use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use serde_json::{Value, json};

use crate::metrics::LatencyWindow;
use crate::model::{IndexStatus, ServingStatus};
use crate::protocol::{PROTOCOL_VERSION, Request, RequestEnvelope, ResponseEnvelope};
use crate::reader::ReaderIndex;
use crate::snapshot::ActivatedSnapshot;

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_QUERY_WORKERS: usize = 4;
const DEFAULT_QUERY_QUEUE_CAPACITY: usize = 64;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(2);
/// How often the background thread recomputes the heavy full-catalog status.
const STATUS_REFRESH_INTERVAL_SECS: u64 = 5;
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_secs(STATUS_REFRESH_INTERVAL_SECS);

/// Shared, asynchronously-refreshed status snapshot. The full-catalog scan can
/// take tens of seconds over NFS; serving it synchronously on the request path
/// trips the IO budget. Instead a background thread keeps this fresh and the
/// `Status` request returns the last computed value immediately.
type SharedStatusCache = Arc<RwLock<Option<IndexStatus>>>;

struct DaemonMetrics {
    workers: u64,
    queue_capacity: u64,
    accepted: AtomicU64,
    completed: AtomicU64,
    rejected: AtomicU64,
    active: AtomicU64,
    queued: AtomicU64,
    queue_wait: LatencyWindow,
    reader_snapshot_open: LatencyWindow,
    /// Real on-wire response serialization cost (the single `serde_json::to_vec`
    /// whose bytes are written to the socket). Recorded post-write to the shared
    /// metrics window; exposure to ServingStatus is the status agent's boundary.
    response_serialization: LatencyWindow,
    /// Monotonic per-request sequence, stamped into the response stages and the
    /// post-serialization audit event so concurrent requests correlate exactly.
    request_seq: AtomicU64,
}

impl DaemonMetrics {
    fn new(workers: usize, queue_capacity: usize) -> Self {
        Self {
            workers: workers as u64,
            queue_capacity: queue_capacity as u64,
            accepted: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            active: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            queue_wait: LatencyWindow::default(),
            reader_snapshot_open: LatencyWindow::default(),
            response_serialization: LatencyWindow::default(),
            request_seq: AtomicU64::new(0),
        }
    }

    fn next_request_seq(&self) -> u64 {
        self.request_seq.fetch_add(1, Ordering::Relaxed)
    }

    fn record_reader_snapshot_open(&self, duration: Duration) {
        self.reader_snapshot_open.record(duration);
    }

    fn record_response_serialization(&self, duration: Duration) {
        self.response_serialization.record(duration);
    }

    fn snapshot(&self) -> ServingStatus {
        ServingStatus {
            workers: self.workers,
            queue_capacity: self.queue_capacity,
            accepted: self.accepted.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            active: self.active.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
            queue_wait: self.queue_wait.snapshot(),
            reader_snapshot_open: self.reader_snapshot_open.snapshot(),
            response_serialization: self.response_serialization.snapshot(),
        }
    }
}

struct ActiveRequest<'a> {
    metrics: &'a DaemonMetrics,
}

impl Drop for ActiveRequest<'_> {
    fn drop(&mut self) {
        self.metrics.active.fetch_sub(1, Ordering::Relaxed);
        self.metrics.completed.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn default_socket_path(index_dir: &Path) -> PathBuf {
    index_dir.join("awi.sock")
}

pub fn serve(index_dir: &Path, socket_path: &Path) -> Result<()> {
    serve_with_snapshots(index_dir, socket_path, None, Duration::from_secs(5))
}

pub fn serve_with_snapshots(
    index_dir: &Path,
    socket_path: &Path,
    snapshot_source: Option<&Path>,
    snapshot_poll_interval: Duration,
) -> Result<()> {
    fs::create_dir_all(index_dir)
        .with_context(|| format!("create AWI index directory {}", index_dir.display()))?;
    let daemon_lock = acquire_daemon_lock(index_dir)?;
    remove_stale_socket(socket_path)?;

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind AWI socket {}", socket_path.display()))?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set permissions on {}", socket_path.display()))?;
    let _socket_guard = SocketGuard(socket_path.to_owned());
    let _daemon_lock = daemon_lock;
    let mut follower = snapshot_source
        .map(|source| {
            SnapshotFollower::new(
                source,
                &index_dir.join("snapshot-cache"),
                snapshot_poll_interval,
            )
        })
        .transpose()?;
    let workspace_path = follower
        .as_ref()
        .map_or_else(|| index_dir.to_owned(), |value| value.active_path.clone());
    // The daemon bootstrap open IS a genuine reader snapshot open; record it in
    // the shared metrics once the non-snapshot loop creates its metrics below.
    let open_started = Instant::now();
    let mut workspace = ReaderIndex::open(workspace_path.clone())?;
    if let Err(error) = workspace.warm_retrieval() {
        eprintln!("AWI semantic sidecar unavailable; serving lexical search: {error:#}");
    }

    eprintln!("AWI daemon listening on {}", socket_path.display());
    if let Some(follower) = &mut follower {
        return serve_snapshot_readers(listener, &mut workspace, follower);
    }
    let metrics = DaemonMetrics::new(1, 0);
    metrics.record_reader_snapshot_open(open_started.elapsed());
    // Plain (non-snapshot) serve: a fixed index dir, no follower. Still populate the
    // shared status cache from a bounded background refresher so `status` returns a
    // real populated body (not an all-zero skeleton) without running the catalog scan
    // on the request path.
    let status_cache: SharedStatusCache = Arc::new(RwLock::new(None));
    let status_state = Arc::new(AtomicU8::new(0));
    let active_path = Arc::new(RwLock::new(workspace_path.clone()));
    // Chain off the already-warm served reader so the refresher shares the SAME
    // warm semantic sidecar Arc (and its readiness/metrics) instead of opening a
    // cold ReaderIndex that would report runtime_ready=false forever.
    let warm_reader = workspace.open_reader_at(workspace.index_dir())?;
    spawn_status_refresher(
        warm_reader,
        Arc::clone(&active_path),
        Arc::clone(&status_cache),
        Arc::clone(&status_state),
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                metrics.accepted.fetch_add(1, Ordering::Relaxed);
                metrics.active.fetch_add(1, Ordering::Relaxed);
                let active = ActiveRequest { metrics: &metrics };
                let status_cache = Arc::clone(&status_cache);
                let shutdown = handle_connection_resilient(
                    stream,
                    &mut workspace,
                    false,
                    &metrics,
                    &status_cache,
                    RequestStages::default(),
                );
                drop(active);
                if shutdown {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("accept AWI daemon connection"),
        }
    }
    Ok(())
}

struct ConnectionWork {
    stream: UnixStream,
    enqueued_at: Instant,
}

fn serve_snapshot_readers(
    listener: UnixListener,
    workspace: &mut ReaderIndex,
    follower: &mut SnapshotFollower,
) -> Result<()> {
    let worker_count = env_usize("AWI_DAEMON_QUERY_WORKERS", DEFAULT_QUERY_WORKERS)?;
    let queue_capacity = env_usize(
        "AWI_DAEMON_QUERY_QUEUE_CAPACITY",
        DEFAULT_QUERY_QUEUE_CAPACITY,
    )?;
    let active_path = Arc::new(RwLock::new(workspace.index_dir().to_owned()));
    let metrics = Arc::new(DaemonMetrics::new(worker_count, queue_capacity));
    let status_cache: SharedStatusCache = Arc::new(RwLock::new(None));
    // 0=initial, 1=loading, 2=populated, 3=error. Exposed so a cold/failed
    // status never masquerades as a healthy all-zero index.
    let status_state = Arc::new(AtomicU8::new(0));
    // Chain off the warm served reader so the refresher shares the live semantic
    // sidecar Arc (readiness + metrics), not a cold duplicate that misreports
    // semantic as down.
    let warm_reader = workspace.open_reader_at(workspace.index_dir())?;
    spawn_status_refresher(
        warm_reader,
        Arc::clone(&active_path),
        Arc::clone(&status_cache),
        Arc::clone(&status_state),
    );
    let (sender, receiver) = mpsc::sync_channel::<ConnectionWork>(queue_capacity);
    let receiver = Arc::new(Mutex::new(receiver));
    let (shutdown_sender, shutdown_receiver) = mpsc::channel();
    let mut workers = Vec::with_capacity(worker_count);
    for worker_id in 0..worker_count {
        // Worker startup IS a real reader snapshot open; record it in the shared
        // window. Warm reused readers contribute no further samples.
        let open_started = Instant::now();
        let mut reader = workspace.open_reader_at(workspace.index_dir())?;
        metrics.record_reader_snapshot_open(open_started.elapsed());
        let receiver = Arc::clone(&receiver);
        let active_path = Arc::clone(&active_path);
        let metrics = Arc::clone(&metrics);
        let status_cache = Arc::clone(&status_cache);
        let shutdown_sender = shutdown_sender.clone();
        workers.push(
            thread::Builder::new()
                .name(format!("awi-query-{worker_id}"))
                .spawn(move || {
                    let mut reader_path = reader.index_dir().to_owned();
                    loop {
                        let work = {
                            let receiver = receiver.lock().expect("AWI query queue mutex poisoned");
                            receiver.recv()
                        };
                        let Ok(work) = work else {
                            break;
                        };
                        metrics.queued.fetch_sub(1, Ordering::Relaxed);
                        let queue_wait = work.enqueued_at.elapsed();
                        metrics.queue_wait.record(queue_wait);
                        metrics.active.fetch_add(1, Ordering::Relaxed);
                        let active = ActiveRequest { metrics: &metrics };
                        let requested_path = active_path
                            .read()
                            .expect("AWI active snapshot lock poisoned")
                            .clone();
                        // Only a genuine rotation open measures reader_snapshot_open_ms.
                        // A warm reused reader reports null + reopened=false; we never
                        // replay the previous open duration onto this query.
                        let mut request_reader_open_ms: Option<f64> = None;
                        if requested_path != reader_path {
                            let open_started = Instant::now();
                            match reader.open_reader_at(&requested_path) {
                                Ok(replacement) => {
                                    reader = replacement;
                                    reader_path = requested_path;
                                    let elapsed = open_started.elapsed();
                                    metrics.record_reader_snapshot_open(elapsed);
                                    request_reader_open_ms = Some(elapsed.as_secs_f64() * 1_000.0);
                                }
                                Err(error) => {
                                    eprintln!(
                                        "AWI query worker could not open active snapshot {}; \
                                         retaining {}: {error:#}",
                                        requested_path.display(),
                                        reader_path.display()
                                    );
                                }
                            }
                        }
                        let stages = RequestStages {
                            queue_wait_ms: queue_wait.as_secs_f64() * 1_000.0,
                            reader_snapshot_open_ms: request_reader_open_ms,
                            reader_reopened: request_reader_open_ms.is_some(),
                        };
                        let shutdown = handle_connection_resilient(
                            work.stream,
                            &mut reader,
                            true,
                            &metrics,
                            &status_cache,
                            stages,
                        );
                        drop(active);
                        if shutdown {
                            let _ = shutdown_sender.send(());
                            break;
                        }
                    }
                })
                .context("spawn AWI query worker")?,
        );
    }
    drop(shutdown_sender);
    listener
        .set_nonblocking(true)
        .context("set AWI listener nonblocking")?;
    let result = snapshot_accept_loop(
        &listener,
        workspace,
        follower,
        &active_path,
        &sender,
        &shutdown_receiver,
        &metrics,
    );
    drop(sender);
    for worker in workers {
        if worker.join().is_err() {
            eprintln!("AWI query worker panicked during shutdown");
        }
    }
    result
}

fn snapshot_accept_loop(
    listener: &UnixListener,
    workspace: &mut ReaderIndex,
    follower: &mut SnapshotFollower,
    active_path: &RwLock<PathBuf>,
    sender: &SyncSender<ConnectionWork>,
    shutdown: &Receiver<()>,
    metrics: &DaemonMetrics,
) -> Result<()> {
    loop {
        if shutdown.try_recv().is_ok() {
            return Ok(());
        }
        if let Err(error) = follower.refresh(workspace) {
            eprintln!("AWI snapshot refresh failed; keeping last valid snapshot: {error:#}");
        }
        if workspace.index_dir()
            != active_path
                .read()
                .expect("AWI active snapshot lock poisoned")
                .as_path()
        {
            *active_path
                .write()
                .expect("AWI active snapshot lock poisoned") = workspace.index_dir().to_owned();
        }
        // The status scan runs in a dedicated thread (see spawn_status_refresher)
        // so the accept loop never blocks on the multi-minute catalog scan.
        match listener.accept() {
            Ok((stream, _)) => {
                metrics.accepted.fetch_add(1, Ordering::Relaxed);
                metrics.queued.fetch_add(1, Ordering::Relaxed);
                match sender.try_send(ConnectionWork {
                    stream,
                    enqueued_at: Instant::now(),
                }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(work)) => {
                        metrics.queued.fetch_sub(1, Ordering::Relaxed);
                        metrics.rejected.fetch_add(1, Ordering::Relaxed);
                        write_failure(work.stream, "server_busy", "AWI query queue is at capacity");
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        metrics.queued.fetch_sub(1, Ordering::Relaxed);
                        anyhow::bail!("AWI query workers disconnected");
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("accept AWI daemon connection"),
        }
    }
}

fn write_failure(mut stream: UnixStream, code: &str, message: &str) {
    let response = ResponseEnvelope::failure(code, message);
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let _ = serde_json::to_writer(&mut stream, &response);
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

/// Per-request stage context measured by the query worker, layered on top of the
/// sidecar diagnostics the workspace returns with each search.
#[derive(Debug, Clone, Copy, Default)]
struct RequestStages {
    queue_wait_ms: f64,
    reader_snapshot_open_ms: Option<f64>,
    reader_reopened: bool,
}

fn handle_connection_resilient(
    stream: UnixStream,
    workspace: &mut ReaderIndex,
    snapshot_read_only: bool,
    metrics: &DaemonMetrics,
    status_cache: &SharedStatusCache,
    stages: RequestStages,
) -> bool {
    match handle_connection(
        stream,
        workspace,
        snapshot_read_only,
        metrics,
        status_cache,
        stages,
    ) {
        Ok(shutdown) => shutdown,
        Err(error) => {
            eprintln!("AWI daemon connection failed; continuing: {error:#}");
            false
        }
    }
}

fn env_usize(name: &str, default: usize) -> Result<usize> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(default);
    };
    let parsed = value
        .to_string_lossy()
        .parse::<usize>()
        .with_context(|| format!("{name} must be a positive integer"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be positive");
    }
    Ok(parsed)
}

pub fn try_request(
    socket_path: &Path,
    request: &Request,
) -> Result<Option<(Value, Option<Value>)>> {
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("connect to AWI daemon {}", socket_path.display()));
        }
    };
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let envelope = RequestEnvelope::new(request.clone());
    serde_json::to_writer(&mut stream, &envelope)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .context("read AWI daemon response")?;
    if line.len() > MAX_RESPONSE_BYTES {
        anyhow::bail!("AWI daemon response exceeded {MAX_RESPONSE_BYTES} bytes");
    }
    if line.is_empty() {
        anyhow::bail!("AWI daemon closed the connection without a response");
    }
    let response: ResponseEnvelope =
        serde_json::from_str(&line).context("decode AWI daemon response")?;
    response.into_parts().map(Some)
}

fn handle_connection(
    mut stream: UnixStream,
    workspace: &mut ReaderIndex,
    snapshot_read_only: bool,
    metrics: &DaemonMetrics,
    status_cache: &SharedStatusCache,
    stages: RequestStages,
) -> Result<bool> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut response = if line.len() > MAX_REQUEST_BYTES {
        ResponseEnvelope::failure(
            "request_too_large",
            format!("request exceeded {MAX_REQUEST_BYTES} bytes"),
        )
    } else {
        match serde_json::from_str::<RequestEnvelope>(&line) {
            Ok(envelope) if envelope.protocol_version == PROTOCOL_VERSION => dispatch(
                workspace,
                envelope.request,
                snapshot_read_only,
                metrics,
                status_cache,
                stages,
            ),
            Ok(envelope) => ResponseEnvelope::failure(
                "protocol_mismatch",
                format!(
                    "client={}, server={PROTOCOL_VERSION}",
                    envelope.protocol_version
                ),
            ),
            Err(error) => ResponseEnvelope::failure("invalid_request", error.to_string()),
        }
    };
    let shutdown = matches!(
        serde_json::from_str::<RequestEnvelope>(&line),
        Ok(RequestEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request: Request::Shutdown,
        })
    );

    // Stamp a per-request sequence into the response stages BEFORE serialization
    // so it lands on the wire; the same sequence tags the audit event below.
    // This lets concurrent requests correlate exactly (method+bytes alone is not
    // enough). The seq is a known value, not self-referential timing.
    let request_seq = metrics.next_request_seq();
    if let Some(stages) = response.stages.as_mut() {
        stages["daemon_request_seq"] = serde_json::json!(request_seq);
    }

    // Serialize the response payload ONCE to its real on-wire bytes, time that
    // single serialization, then write those exact bytes. The measured cost is
    // recorded to the shared DaemonMetrics window (not self-referenced into the
    // same bytes we are about to write). No probe, no second full re-serialization.
    // Ser-stage口径: this is the daemon JSON response actual serialization;
    // the MCP JSONRPC stdio framing is owned by the rmcp transport and NOT timed here.
    let serialize_start = std::time::Instant::now();
    let body = serde_json::to_vec(&response)?;
    let serialization_ms = serialize_start.elapsed().as_secs_f64() * 1_000.0;
    metrics.record_response_serialization(Duration::from_secs_f64(serialization_ms / 1_000.0));
    // Post-serialization audit event: a small independent JSON line carrying the
    // request seq, real ser value, byte size and the per-query stages. It does
    // NOT re-serialize the response; it logs the already-built stages Value.
    eprintln!(
        "awi-audit-response-serialization {}",
        serde_json::json!({
            "event": "daemon_response_serialization",
            "request_seq": request_seq,
            "serialization_ms": serialization_ms,
            "bytes": body.len(),
            "stages": response.stages,
        })
    );

    let mut writer = BufWriter::new(&mut stream);
    writer.write_all(&body)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(shutdown && response.ok)
}

fn dispatch(
    workspace: &mut ReaderIndex,
    request: Request,
    snapshot_read_only: bool,
    metrics: &DaemonMetrics,
    status_cache: &SharedStatusCache,
    stages: RequestStages,
) -> ResponseEnvelope {
    if snapshot_read_only && matches!(request, Request::Index { .. } | Request::Notify { .. }) {
        return ResponseEnvelope::failure(
            "snapshot_read_only",
            "index mutations are disabled while serving published snapshots",
        );
    }
    let result: Result<ResponseEnvelope> = match request {
        Request::Ping => ResponseEnvelope::success(json!({
            "service": "awi",
            "protocol_version": PROTOCOL_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "build_id": env!("AWI_BUILD_ID"),
            "capabilities": ["source_context", "miss_diagnostics", "compact_v4"]
        }))
        .map_err(anyhow::Error::from),
        Request::Index { root, options } => workspace
            .index_root(root, &options)
            .and_then(|value| ResponseEnvelope::success(value).map_err(Into::into)),
        Request::Notify { paths, options } => workspace
            .notify_paths(&paths, &options)
            .and_then(|value| ResponseEnvelope::success(value).map_err(Into::into)),
        Request::Search {
            query,
            limit,
            roots,
            kinds,
            path_prefix,
            context_path,
        } => workspace
            .search_filtered_stages(
                &query,
                limit,
                &roots,
                &kinds,
                path_prefix.as_deref(),
                context_path.as_deref(),
            )
            .and_then(|(hits, mut sidecar)| {
                // Layer the worker-measured queue wait and reader snapshot open on
                // top of the sidecar sub-stages. reader_snapshot_open_ms stays null
                // when the warm reader was reused for this request.
                if let Some(obj) = sidecar.as_object_mut() {
                    obj.insert("queue_wait_ms".to_owned(), json!(stages.queue_wait_ms));
                    obj.insert(
                        "reader_snapshot_open_ms".to_owned(),
                        json!(stages.reader_snapshot_open_ms),
                    );
                    obj.insert(
                        "reader_snapshot_reopened".to_owned(),
                        json!(stages.reader_reopened),
                    );
                    // sidecar startup never happens per query; report warm state
                    // rather than fabricating a startup duration.
                    obj.insert("sidecar_startup_ms".to_owned(), Value::Null);
                    obj.insert("sidecar_startup_state".to_owned(), json!("warm"));
                }
                ResponseEnvelope::success_with_stages(hits, sidecar).map_err(anyhow::Error::from)
            }),
        Request::DiagnoseSearch {
            anchors,
            roots,
            context_path,
        } => workspace
            .diagnose_search(&anchors, &roots, context_path.as_deref())
            .and_then(|value| ResponseEnvelope::success(value).map_err(Into::into)),
        Request::Inspect {
            path,
            symbol,
            start_line,
            max_lines,
            max_chars,
        } => match symbol {
            Some(symbol) => workspace.inspect_symbol(&path, &symbol, max_lines, max_chars),
            None => workspace.inspect_excerpt(&path, start_line, max_lines, max_chars),
        }
        .and_then(|value| ResponseEnvelope::success(value).map_err(Into::into)),
        Request::Query { request } => workspace
            .query(&request)
            .and_then(|value| ResponseEnvelope::success(value).map_err(Into::into)),
        Request::Status => {
            // Plain (read-write, local) serve has no multi-minute NFS scan, so a
            // status request computes the real body synchronously on the warm,
            // served workspace — this always reflects the latest index. In snapshot
            // mode the catalog may live on NFS, so we NEVER compute on the request
            // path; the background refresher owns the scan and we serve its cache.
            let mut value = if snapshot_read_only {
                let cached = status_cache
                    .read()
                    .expect("AWI status cache lock poisoned")
                    .clone();
                cached.unwrap_or_default()
            } else {
                match workspace.status() {
                    Ok(mut status) => {
                        if status.status_state.is_empty() || status.status_state == "initial" {
                            status.status_state = "populated".to_owned();
                        }
                        status.status_sampled_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or_default();
                        status
                    }
                    Err(error) => IndexStatus {
                        status_state: "error".to_owned(),
                        status_error: Some(format!("{error:#}")),
                        ..IndexStatus::default()
                    },
                }
            };
            value.serving = metrics.snapshot();
            ResponseEnvelope::success(value).map_err(Into::into)
        }
        Request::Shutdown => {
            ResponseEnvelope::success(json!({"stopping": true})).map_err(anyhow::Error::from)
        }
    };

    result.unwrap_or_else(|error| ResponseEnvelope::failure("request_failed", format!("{error:#}")))
}

/// Dedicated thread that keeps the shared status cache fresh. The catalog scan
/// is multi-minute over NFS, so it must never run on the accept loop or on a
/// query worker. We open a base reader on the *collection* (the same object the
/// main workspace opened) and follow snapshot cutovers with `open_reader_at`,
/// which reuses already-warm shard readers — not a bare `ReaderIndex::open`
/// that would miss `collection.local.json` and fall back to an empty scratch
/// reader.
fn spawn_status_refresher(
    initial_reader: ReaderIndex,
    active_path: Arc<RwLock<PathBuf>>,
    status_cache: SharedStatusCache,
    status_state: Arc<AtomicU8>,
) {
    thread::Builder::new()
        .name("awi-status-refresh".to_owned())
        .spawn(move || {
            // Seed the cache with an explicit "loading" body so a cold readback
            // sees loading (not an empty status_state string) while the first scan
            // runs. It shares the warm semantic sidecar Arc with the serving path.
            let mut reader: Option<ReaderIndex> = Some(initial_reader);
            let seed = || IndexStatus {
                status_state: "loading".to_owned(),
                ..IndexStatus::default()
            };
            if let Ok(mut guard) = status_cache.write() {
                *guard = Some(seed());
            }
            loop {
                let path = active_path
                    .read()
                    .expect("AWI active snapshot lock poisoned")
                    .clone();
                let reopen = match &reader {
                    None => true,
                    Some(current) => current.index_dir() != path.as_path(),
                };
                if reopen {
                    status_state.store(1, Ordering::Relaxed);
                    // Re-chain from the current warm reader: open_reader_at reuses
                    // already-warm shard readers and the shared semantic Arc.
                    let next = match reader.take() {
                        Some(current) => current.open_reader_at(&path),
                        None => ReaderIndex::open(&path),
                    };
                    match next {
                        Ok(built) => reader = Some(built),
                        Err(error) => {
                            status_state.store(3, Ordering::Relaxed);
                            eprintln!(
                                "AWI status refresher follow {} failed: {error:#}",
                                path.display()
                            );
                        }
                    }
                }
                if let Some(current) = &reader {
                    let sampled_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or_default();
                    match current.status() {
                        Ok(mut status) => {
                            status.status_state = "populated".to_owned();
                            status.status_sampled_ms = sampled_ms;
                            status.status_error = None;
                            if let Ok(mut guard) = status_cache.write() {
                                *guard = Some(status);
                            }
                            status_state.store(2, Ordering::Relaxed);
                        }
                        Err(error) => {
                            status_state.store(3, Ordering::Relaxed);
                            eprintln!("AWI status refresher status compute failed: {error:#}");
                            if let Ok(mut guard) = status_cache.write() {
                                let fallback = IndexStatus {
                                    status_state: "error".to_owned(),
                                    status_error: Some(format!("{error:#}")),
                                    status_sampled_ms: sampled_ms,
                                    ..IndexStatus::default()
                                };
                                *guard = Some(fallback);
                            }
                        }
                    }
                }
                // Once populated, poll on the slow steady-state interval. While still
                // cold (or after an error), retry quickly so a freshly-started daemon
                // converges to populated in well under a second without ever running
                // the scan on the request path.
                let settled = status_state.load(Ordering::Relaxed) == 2;
                thread::sleep(if settled {
                    STATUS_REFRESH_INTERVAL
                } else {
                    Duration::from_millis(200)
                });
            }
        })
        .ok();
}

struct SnapshotFollower {
    publish_dir: PathBuf,
    cache_dir: PathBuf,
    active_generation: i64,
    active_identity: String,
    active_path: PathBuf,
    poll_interval: Duration,
    next_poll: Instant,
    pending: Option<Receiver<Result<Option<ActivatedSnapshot>>>>,
}

impl SnapshotFollower {
    fn new(publish_dir: &Path, cache_dir: &Path, poll_interval: Duration) -> Result<Self> {
        let activated = crate::collection::refresh(publish_dir, cache_dir, None)?
            .context("snapshot bootstrap returned no activation")?;
        Ok(Self {
            publish_dir: publish_dir.to_owned(),
            cache_dir: cache_dir.to_owned(),
            active_generation: activated.generation,
            active_identity: activated.identity,
            active_path: activated.path,
            poll_interval,
            next_poll: Instant::now() + poll_interval,
            pending: None,
        })
    }

    fn refresh(&mut self, workspace: &mut ReaderIndex) -> Result<()> {
        if let Some(receiver) = self.pending.take() {
            match receiver.try_recv() {
                Ok(Ok(Some(activated))) => {
                    let replacement = workspace.open_reader_at(&activated.path)?;
                    if let Err(error) = replacement.warm_retrieval() {
                        eprintln!(
                            "AWI semantic sidecar unavailable for generation {}; \
                             serving lexical search: {error:#}",
                            activated.generation
                        );
                    }
                    *workspace = replacement;
                    self.active_generation = activated.generation;
                    self.active_identity = activated.identity;
                    self.active_path = activated.path;
                    eprintln!(
                        "AWI daemon activated snapshot generation {}",
                        self.active_generation
                    );
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => return Err(error),
                Err(TryRecvError::Empty) => {
                    self.pending = Some(receiver);
                    return Ok(());
                }
                Err(TryRecvError::Disconnected) => {
                    anyhow::bail!("AWI snapshot refresh worker disconnected");
                }
            }
        }
        if Instant::now() < self.next_poll {
            return Ok(());
        }
        self.next_poll = Instant::now() + self.poll_interval;
        let publish_dir = self.publish_dir.clone();
        let cache_dir = self.cache_dir.clone();
        let previous = ActivatedSnapshot {
            generation: self.active_generation,
            path: self.active_path.clone(),
            identity: self.active_identity.clone(),
        };
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("awi-snapshot-refresh".to_owned())
            .spawn(move || {
                let result = crate::collection::refresh(&publish_dir, &cache_dir, Some(&previous));
                let _ = sender.send(result);
            })
            .context("spawn AWI snapshot refresh worker")?;
        self.pending = Some(receiver);
        Ok(())
    }
}

fn acquire_daemon_lock(index_dir: &Path) -> Result<File> {
    let path = index_dir.join("daemon.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open daemon lock {}", path.display()))?;
    if !file.try_lock_exclusive()? {
        anyhow::bail!("another AWI daemon already owns {}", path.display());
    }
    Ok(file)
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path)
                .with_context(|| format!("remove stale AWI socket {}", path.display()))?;
        }
        Ok(_) => anyhow::bail!("refusing to replace non-socket path at {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspect socket {}", path.display()));
        }
    }
    Ok(())
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;
    use crate::{IndexOptions, WorkspaceIndex};

    #[test]
    fn snapshot_refresh_is_scheduled_without_blocking_the_request_path() {
        let fixture = tempdir().unwrap();
        let root = fixture.path().join("workspace");
        let writer_index = fixture.path().join("writer");
        let publish_dir = fixture.path().join("published");
        let cache_dir = fixture.path().join("cache");
        let source = root.join("service.rs");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, "fn version_one() {}\n").unwrap();

        let mut writer = WorkspaceIndex::open(&writer_index).unwrap();
        writer.index_root(&root, &IndexOptions::default()).unwrap();
        writer.publish_snapshot(&publish_dir).unwrap();

        let mut follower = SnapshotFollower::new(&publish_dir, &cache_dir, Duration::ZERO).unwrap();
        let mut reader = ReaderIndex::open(&follower.active_path).unwrap();
        fs::write(&source, "fn version_two() {}\n").unwrap();
        writer.index_root(&root, &IndexOptions::default()).unwrap();
        writer.publish_snapshot(&publish_dir).unwrap();

        follower.refresh(&mut reader).unwrap();
        assert_eq!(follower.active_generation, 1);
        assert!(follower.pending.is_some());

        let deadline = Instant::now() + Duration::from_secs(5);
        while follower.active_generation == 1 {
            follower.refresh(&mut reader).unwrap();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(follower.active_generation, 2);
        assert_eq!(
            reader
                .search_filtered("version_two", 5, &[], &[], None, None)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn response_is_serialized_once_and_written_bytes_match_timed_buffer() {
        use std::io::Write;
        // Mirror handle_connection: to_vec once (timed), then write those exact
        // bytes. Assert the writer receives exactly the bytes we serialized — no
        // second full re-serialization, no discarded probe buffer.
        let envelope = ResponseEnvelope::success_with_stages::<Vec<serde_json::Value>>(
            Vec::new(),
            serde_json::json!({"queue_wait_ms": 0.0, "semantic_available": false}),
        )
        .expect("envelope");
        let body = serde_json::to_vec(&envelope).expect("serialize once");
        let mut cursor = Vec::new();
        cursor.write_all(&body).expect("write exact bytes");
        cursor.write_all(b"\n").unwrap();
        // The written payload must equal the single serialized buffer.
        assert!(cursor.starts_with(&body));
        // And it must be valid JSON (one envelope, not two concatenated).
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("valid envelope");
        assert_eq!(parsed["ok"], serde_json::Value::Bool(true));
        // DaemonMetrics accepts a response-serialization record without panic.
        let metrics = DaemonMetrics::new(1, 1);
        metrics.record_response_serialization(Duration::from_micros(66));
    }

    #[test]
    fn request_seq_is_unique_across_concurrent_requests() {
        use std::sync::Arc;
        use std::thread;
        let metrics = Arc::new(DaemonMetrics::new(4, 16));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let m = Arc::clone(&metrics);
            handles.push(thread::spawn(move || m.next_request_seq()));
        }
        let mut seqs: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(
            seqs.len(),
            8,
            "request_seq must be unique per concurrent request"
        );
    }
}
