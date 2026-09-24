use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use serde_json::{Value, json};

use crate::WorkspaceIndex;
use crate::metrics::LatencyWindow;
use crate::model::ServingStatus;
use crate::protocol::{PROTOCOL_VERSION, Request, RequestEnvelope, ResponseEnvelope};
use crate::snapshot::{ActivatedSnapshot, materialize_latest, read_pointer};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_QUERY_WORKERS: usize = 4;
const DEFAULT_QUERY_QUEUE_CAPACITY: usize = 64;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(2);

struct DaemonMetrics {
    workers: u64,
    queue_capacity: u64,
    accepted: AtomicU64,
    completed: AtomicU64,
    rejected: AtomicU64,
    active: AtomicU64,
    queued: AtomicU64,
    queue_wait: LatencyWindow,
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
        }
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
    let mut workspace = WorkspaceIndex::open(workspace_path)?;
    if let Err(error) = workspace.warm_retrieval() {
        eprintln!("AWI semantic sidecar unavailable; serving lexical search: {error:#}");
    }

    eprintln!("AWI daemon listening on {}", socket_path.display());
    if let Some(follower) = &mut follower {
        return serve_snapshot_readers(listener, &mut workspace, follower);
    }
    let metrics = DaemonMetrics::new(1, 0);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                metrics.accepted.fetch_add(1, Ordering::Relaxed);
                metrics.active.fetch_add(1, Ordering::Relaxed);
                let active = ActiveRequest { metrics: &metrics };
                let shutdown = handle_connection_resilient(stream, &mut workspace, false, &metrics);
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
    workspace: &mut WorkspaceIndex,
    follower: &mut SnapshotFollower,
) -> Result<()> {
    let worker_count = env_usize("AWI_DAEMON_QUERY_WORKERS", DEFAULT_QUERY_WORKERS)?;
    let queue_capacity = env_usize(
        "AWI_DAEMON_QUERY_QUEUE_CAPACITY",
        DEFAULT_QUERY_QUEUE_CAPACITY,
    )?;
    let active_path = Arc::new(RwLock::new(workspace.index_dir().to_owned()));
    let metrics = Arc::new(DaemonMetrics::new(worker_count, queue_capacity));
    let (sender, receiver) = mpsc::sync_channel::<ConnectionWork>(queue_capacity);
    let receiver = Arc::new(Mutex::new(receiver));
    let (shutdown_sender, shutdown_receiver) = mpsc::channel();
    let mut workers = Vec::with_capacity(worker_count);
    for worker_id in 0..worker_count {
        let mut reader = workspace.open_reader_at(workspace.index_dir())?;
        let receiver = Arc::clone(&receiver);
        let active_path = Arc::clone(&active_path);
        let metrics = Arc::clone(&metrics);
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
                        metrics.queue_wait.record(work.enqueued_at.elapsed());
                        metrics.active.fetch_add(1, Ordering::Relaxed);
                        let active = ActiveRequest { metrics: &metrics };
                        let requested_path = active_path
                            .read()
                            .expect("AWI active snapshot lock poisoned")
                            .clone();
                        if requested_path != reader_path {
                            match reader.open_reader_at(&requested_path) {
                                Ok(replacement) => {
                                    reader = replacement;
                                    reader_path = requested_path;
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
                        let shutdown =
                            handle_connection_resilient(work.stream, &mut reader, true, &metrics);
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
    workspace: &mut WorkspaceIndex,
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

fn handle_connection_resilient(
    stream: UnixStream,
    workspace: &mut WorkspaceIndex,
    snapshot_read_only: bool,
    metrics: &DaemonMetrics,
) -> bool {
    match handle_connection(stream, workspace, snapshot_read_only, metrics) {
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

pub fn try_request(socket_path: &Path, request: &Request) -> Result<Option<Value>> {
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
    response.into_result().map(Some)
}

fn handle_connection(
    mut stream: UnixStream,
    workspace: &mut WorkspaceIndex,
    snapshot_read_only: bool,
    metrics: &DaemonMetrics,
) -> Result<bool> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let response = if line.len() > MAX_REQUEST_BYTES {
        ResponseEnvelope::failure(
            "request_too_large",
            format!("request exceeded {MAX_REQUEST_BYTES} bytes"),
        )
    } else {
        match serde_json::from_str::<RequestEnvelope>(&line) {
            Ok(envelope) if envelope.protocol_version == PROTOCOL_VERSION => {
                dispatch(workspace, envelope.request, snapshot_read_only, metrics)
            }
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

    let mut writer = BufWriter::new(&mut stream);
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(shutdown && response.ok)
}

fn dispatch(
    workspace: &mut WorkspaceIndex,
    request: Request,
    snapshot_read_only: bool,
    metrics: &DaemonMetrics,
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
            "protocol_version": PROTOCOL_VERSION
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
            .search_filtered(
                &query,
                limit,
                &roots,
                &kinds,
                path_prefix.as_deref(),
                context_path.as_deref(),
            )
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
        Request::Status => workspace.status().and_then(|mut value| {
            value.serving = metrics.snapshot();
            ResponseEnvelope::success(value).map_err(Into::into)
        }),
        Request::Shutdown => {
            ResponseEnvelope::success(json!({"stopping": true})).map_err(anyhow::Error::from)
        }
    };

    result.unwrap_or_else(|error| ResponseEnvelope::failure("request_failed", format!("{error:#}")))
}

struct SnapshotFollower {
    publish_dir: PathBuf,
    cache_dir: PathBuf,
    active_generation: i64,
    active_path: PathBuf,
    poll_interval: Duration,
    next_poll: Instant,
    pending: Option<Receiver<Result<Option<ActivatedSnapshot>>>>,
}

impl SnapshotFollower {
    fn new(publish_dir: &Path, cache_dir: &Path, poll_interval: Duration) -> Result<Self> {
        let activated = materialize_latest(publish_dir, cache_dir)?;
        Ok(Self {
            publish_dir: publish_dir.to_owned(),
            cache_dir: cache_dir.to_owned(),
            active_generation: activated.generation,
            active_path: activated.path,
            poll_interval,
            next_poll: Instant::now() + poll_interval,
            pending: None,
        })
    }

    fn refresh(&mut self, workspace: &mut WorkspaceIndex) -> Result<()> {
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
        let active_generation = self.active_generation;
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("awi-snapshot-refresh".to_owned())
            .spawn(move || {
                let result = (|| {
                    let pointer = read_pointer(&publish_dir)?;
                    if pointer.generation == active_generation {
                        return Ok(None);
                    }
                    materialize_latest(&publish_dir, &cache_dir).map(Some)
                })();
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
    use crate::IndexOptions;

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
        let mut reader = WorkspaceIndex::open(&follower.active_path).unwrap();
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
        assert_eq!(reader.search("version_two", 5).unwrap().len(), 1);
    }
}
