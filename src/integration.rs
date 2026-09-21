use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::str::FromStr;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};

const SERVER_NAME: &str = "awi";
const MCP_TIMEOUT_MS: u64 = 45_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationClient {
    All,
    Codex,
    Gemini,
    Claude,
    Trae,
    Zcode,
    Kimi,
}

impl IntegrationClient {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Claude => "claude",
            Self::Trae => "trae",
            Self::Zcode => "zcode",
            Self::Kimi => "kimi",
        }
    }
}

impl FromStr for IntegrationClient {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "codex" => Ok(Self::Codex),
            "gemini" => Ok(Self::Gemini),
            "claude" | "claude-code" => Ok(Self::Claude),
            "trae" | "traecode" | "trae-code" | "traecli" => Ok(Self::Trae),
            "zcode" => Ok(Self::Zcode),
            "kimi" | "kimi-code" => Ok(Self::Kimi),
            other => anyhow::bail!(
                "unsupported integration client {other:?}; expected all, codex, gemini, \
                 claude, trae, zcode, or kimi"
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IntegrationOptions {
    pub clients: Vec<IntegrationClient>,
    pub server: McpServerSpec,
    pub project_root: PathBuf,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpServerSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus {
    Configured,
    AlreadyConfigured,
    WouldConfigure,
    NeedsAttention,
    NotInstalled,
    Unsupported,
    Failed,
}

impl IntegrationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::AlreadyConfigured => "already_configured",
            Self::WouldConfigure => "would_configure",
            Self::NeedsAttention => "needs_attention",
            Self::NotInstalled => "not_installed",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientIntegration {
    pub client: String,
    pub status: IntegrationStatus,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntegrationReport {
    pub server: McpServerSpec,
    pub clients: Vec<ClientIntegration>,
}

impl IntegrationReport {
    pub fn has_failures(&self) -> bool {
        self.clients
            .iter()
            .any(|client| client.status == IntegrationStatus::Failed)
    }
}

pub fn default_server_spec(
    index_dir: &Path,
    explicit_command: Option<&Path>,
    explicit_args: &[String],
) -> Result<McpServerSpec> {
    if let Some(command) = explicit_command {
        return Ok(McpServerSpec {
            command: resolve_command(command)?,
            args: explicit_args.to_vec(),
        });
    }

    let current_exe = env::current_exe().context("locate current AWI executable")?;
    let index_dir = absolute_path(index_dir)?;
    let mut args = if explicit_args.is_empty() {
        vec![
            "--index-dir".to_owned(),
            index_dir.to_string_lossy().into_owned(),
            "mcp".to_owned(),
        ]
    } else {
        explicit_args.to_vec()
    };
    args.shrink_to_fit();
    Ok(McpServerSpec {
        command: current_exe,
        args,
    })
}

pub fn integrate(options: &IntegrationOptions) -> Result<IntegrationReport> {
    let clients = expand_clients(&options.clients);
    let mut results = Vec::with_capacity(clients.len());
    for client in clients {
        let result = match client {
            IntegrationClient::Codex => integrate_codex(&options.server, options.dry_run),
            IntegrationClient::Gemini => {
                integrate_gemini(&options.project_root, &options.server, options.dry_run)
            }
            IntegrationClient::Claude => integrate_claude(&options.server, options.dry_run),
            IntegrationClient::Trae => {
                integrate_trae(&options.project_root, &options.server, options.dry_run)
            }
            IntegrationClient::Zcode => unsupported_client(
                client,
                is_installed(&["zcode"], &[home_path(".zcode")]),
                "this Zcode installation has no stable user-level external MCP registration API",
            ),
            IntegrationClient::Kimi => unsupported_client(
                client,
                is_installed(
                    &["kimi", "kimi-code"],
                    &[home_path(".kimi-code/bin/kimi"), home_path(".kimi-code")],
                ),
                "the installed Kimi Code CLI exposes ACP and built-in services but no MCP client",
            ),
            IntegrationClient::All => unreachable!("all is expanded before integration"),
        };
        results.push(result);
    }
    Ok(IntegrationReport {
        server: options.server.clone(),
        clients: results,
    })
}

pub fn render_human(report: &IntegrationReport) -> String {
    let mut lines = vec![format!(
        "AWI MCP: {} {}",
        report.server.command.display(),
        shell_join(&report.server.args)
    )];
    for client in &report.clients {
        let path = client
            .config_path
            .as_ref()
            .map(|path| format!(" [{}]", path.display()))
            .unwrap_or_default();
        lines.push(format!(
            "{:<7} {:<20} {}{}",
            client.client,
            client.status.as_str(),
            client.detail,
            path
        ));
    }
    lines.join("\n")
}

fn expand_clients(clients: &[IntegrationClient]) -> Vec<IntegrationClient> {
    let requested = if clients.is_empty() {
        &[IntegrationClient::All][..]
    } else {
        clients
    };
    if requested.contains(&IntegrationClient::All) {
        return vec![
            IntegrationClient::Codex,
            IntegrationClient::Gemini,
            IntegrationClient::Claude,
            IntegrationClient::Trae,
            IntegrationClient::Zcode,
            IntegrationClient::Kimi,
        ];
    }
    let mut output = Vec::new();
    for client in requested {
        if !output.contains(client) {
            output.push(*client);
        }
    }
    output
}

fn integrate_codex(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Codex;
    let Some(executable) = find_executable("codex") else {
        return not_installed(client);
    };
    let existing = run(&executable, ["mcp", "get", SERVER_NAME, "--json"]);
    if existing
        .as_ref()
        .ok()
        .is_some_and(|output| output.status.success() && codex_output_matches(output, server))
    {
        return success(
            client,
            IntegrationStatus::AlreadyConfigured,
            "user-scope MCP entry already matches",
            Some(home_path(".codex/config.toml")),
        );
    }
    if dry_run {
        return success(
            client,
            IntegrationStatus::WouldConfigure,
            "would register through `codex mcp add`",
            Some(home_path(".codex/config.toml")),
        );
    }
    if existing.is_ok_and(|output| output.status.success())
        && let Err(error) = checked_run(&executable, ["mcp", "remove", SERVER_NAME])
    {
        return failed(client, error);
    }
    let mut args = vec![
        "mcp".to_owned(),
        "add".to_owned(),
        SERVER_NAME.to_owned(),
        "--".to_owned(),
        server.command.to_string_lossy().into_owned(),
    ];
    args.extend(server.args.iter().cloned());
    if let Err(error) = checked_run(&executable, &args) {
        return failed(client, error);
    }
    match run(&executable, ["mcp", "get", SERVER_NAME, "--json"]) {
        Ok(output) if output.status.success() && codex_output_matches(&output, server) => success(
            client,
            IntegrationStatus::Configured,
            "registered through `codex mcp add`",
            Some(home_path(".codex/config.toml")),
        ),
        Ok(output) => failed(client, command_failure(&executable, &output)),
        Err(error) => failed(client, error),
    }
}

fn integrate_gemini(
    project_root: &Path,
    server: &McpServerSpec,
    dry_run: bool,
) -> ClientIntegration {
    let client = IntegrationClient::Gemini;
    let Some(executable) = find_executable("gemini") else {
        return not_installed(client);
    };
    let config_path = home_path(".gemini/settings.json");
    let existing = read_json(&config_path).ok();
    if existing
        .as_ref()
        .is_some_and(|value| json_server_matches(value.pointer("/mcpServers/awi"), server))
    {
        return gemini_readiness(
            &executable,
            project_root,
            client,
            IntegrationStatus::AlreadyConfigured,
            config_path,
        );
    }
    if dry_run {
        return success(
            client,
            IntegrationStatus::WouldConfigure,
            "would register through `gemini mcp add --scope user`",
            Some(config_path),
        );
    }
    if existing
        .as_ref()
        .is_some_and(|value| value.pointer("/mcpServers/awi").is_some())
        && let Err(error) = checked_run(
            &executable,
            ["mcp", "remove", SERVER_NAME, "--scope", "user"],
        )
    {
        return failed(client, error);
    }
    let mut args = vec![
        "mcp".to_owned(),
        "add".to_owned(),
        SERVER_NAME.to_owned(),
        server.command.to_string_lossy().into_owned(),
    ];
    args.extend(server.args.iter().cloned());
    args.extend([
        "--scope".to_owned(),
        "user".to_owned(),
        "--transport".to_owned(),
        "stdio".to_owned(),
        "--timeout".to_owned(),
        MCP_TIMEOUT_MS.to_string(),
        "--description".to_owned(),
        "AWI workspace code and data retrieval".to_owned(),
    ]);
    if let Err(error) = checked_run(&executable, &args) {
        return failed(client, error);
    }
    match read_json(&config_path) {
        Ok(value) if json_server_matches(value.pointer("/mcpServers/awi"), server) => {
            gemini_readiness(
                &executable,
                project_root,
                client,
                IntegrationStatus::Configured,
                config_path,
            )
        }
        Ok(_) => failed(
            client,
            anyhow::anyhow!("Gemini command succeeded but the user MCP entry did not match"),
        ),
        Err(error) => failed(client, error),
    }
}

fn gemini_readiness(
    executable: &Path,
    project_root: &Path,
    client: IntegrationClient,
    ready_status: IntegrationStatus,
    config_path: PathBuf,
) -> ClientIntegration {
    let output = Command::new(executable)
        .args(["mcp", "list"])
        .current_dir(project_root)
        .output();
    if let Ok(output) = output {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_ascii_lowercase();
        if text.contains("untrusted") || text.contains("disabled") {
            return success(
                client,
                IntegrationStatus::NeedsAttention,
                "MCP entry is installed but Gemini disables it in this untrusted workspace; \
                 run `/permissions trust` once in Gemini",
                Some(config_path),
            );
        }
    }
    success(
        client,
        ready_status,
        "user-scope MCP entry is installed and enabled",
        Some(config_path),
    )
}

fn integrate_claude(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Claude;
    let Some(executable) = find_executable("claude") else {
        return not_installed(client);
    };
    if dry_run {
        return success(
            client,
            IntegrationStatus::WouldConfigure,
            "would register through `claude mcp add --scope user`",
            Some(home_path(".claude.json")),
        );
    }
    let _ = run(
        &executable,
        ["mcp", "remove", SERVER_NAME, "--scope", "user"],
    );
    let mut args = vec![
        "mcp".to_owned(),
        "add".to_owned(),
        "--transport".to_owned(),
        "stdio".to_owned(),
        "--scope".to_owned(),
        "user".to_owned(),
        SERVER_NAME.to_owned(),
        "--".to_owned(),
        server.command.to_string_lossy().into_owned(),
    ];
    args.extend(server.args.iter().cloned());
    match checked_run(&executable, &args) {
        Ok(_) => success(
            client,
            IntegrationStatus::Configured,
            "registered through `claude mcp add --scope user`",
            Some(home_path(".claude.json")),
        ),
        Err(error) => failed(client, error),
    }
}

fn integrate_trae(project_root: &Path, server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Trae;
    let installed = is_installed(
        &["traecli"],
        &[home_path(".trae-cn"), home_path(".traecli")],
    );
    if !installed {
        return not_installed(client);
    }
    let path = project_root.join(".trae/mcp.json");
    let existing = if path.exists() {
        match read_json(&path) {
            Ok(value) => value,
            Err(error) => return failed(client, error),
        }
    } else {
        json!({})
    };
    if json_server_matches(existing.pointer("/mcpServers/awi"), server) {
        return success(
            client,
            IntegrationStatus::AlreadyConfigured,
            "project MCP entry already matches; TraeCode IDE and CLI can share it",
            Some(path),
        );
    }
    if dry_run {
        return success(
            client,
            IntegrationStatus::WouldConfigure,
            "would merge a project MCP entry for TraeCode IDE and CLI",
            Some(path),
        );
    }
    let merged = match merge_json_mcp(existing, server) {
        Ok(value) => value,
        Err(error) => return failed(client, error),
    };
    if let Err(error) = write_json_atomic(&path, &merged) {
        return failed(client, error);
    }
    success(
        client,
        IntegrationStatus::Configured,
        "project MCP entry added; enable project-level MCP once in TraeCode settings",
        Some(path),
    )
}

fn unsupported_client(
    client: IntegrationClient,
    installed: bool,
    detail: &str,
) -> ClientIntegration {
    if installed {
        success(client, IntegrationStatus::Unsupported, detail, None)
    } else {
        not_installed(client)
    }
}

fn merge_json_mcp(mut root: Value, server: &McpServerSpec) -> Result<Value> {
    if !root.is_object() {
        anyhow::bail!("MCP config root must be a JSON object");
    }
    let root_object = root.as_object_mut().expect("checked above");
    let servers = root_object.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        anyhow::bail!("mcpServers must be a JSON object");
    }
    servers.as_object_mut().expect("checked above").insert(
        SERVER_NAME.to_owned(),
        json!({
            "command": server.command,
            "args": server.args,
            "env": {
                "START_MCP_TIMEOUT_MS": MCP_TIMEOUT_MS.to_string(),
                "RUN_MCP_TIMEOUT_MS": MCP_TIMEOUT_MS.to_string()
            }
        }),
    );
    Ok(root)
}

fn codex_output_matches(output: &Output, server: &McpServerSpec) -> bool {
    serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .is_some_and(|value| {
            let transport = value.get("transport");
            let command = transport
                .and_then(|value| value.get("command"))
                .and_then(Value::as_str);
            let args = transport
                .and_then(|value| value.get("args"))
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            command == Some(server.command.to_string_lossy().as_ref())
                && args == server.args.iter().map(String::as_str).collect::<Vec<_>>()
        })
}

fn json_server_matches(value: Option<&Value>, server: &McpServerSpec) -> bool {
    let Some(value) = value else {
        return false;
    };
    let command = value.get("command").and_then(Value::as_str);
    let args = value
        .get("args")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    command == Some(server.command.to_string_lossy().as_ref())
        && args == server.args.iter().map(String::as_str).collect::<Vec<_>>()
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decode {}", path.display()))
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("config path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("mcp.json"),
        std::process::id()
    ));
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes).with_context(|| format!("write {}", temporary.display()))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("activate MCP config {}", path.display()))?;
    Ok(())
}

fn checked_run<I, S>(executable: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run(executable, args)?;
    if !output.status.success() {
        return Err(command_failure(executable, &output));
    }
    Ok(output)
}

fn run<I, S>(executable: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(executable)
        .args(args)
        .output()
        .with_context(|| format!("run {}", executable.display()))
}

fn command_failure(executable: &Path, output: &Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::anyhow!(
        "{} exited with {}: {}{}",
        executable.display(),
        output.status,
        stderr.trim(),
        if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            ""
        }
    )
}

fn success(
    client: IntegrationClient,
    status: IntegrationStatus,
    detail: impl Into<String>,
    config_path: Option<PathBuf>,
) -> ClientIntegration {
    ClientIntegration {
        client: client.as_str().to_owned(),
        status,
        detail: detail.into(),
        config_path,
    }
}

fn failed(client: IntegrationClient, error: anyhow::Error) -> ClientIntegration {
    success(
        client,
        IntegrationStatus::Failed,
        format!("{error:#}"),
        None,
    )
}

fn not_installed(client: IntegrationClient) -> ClientIntegration {
    success(
        client,
        IntegrationStatus::NotInstalled,
        "client is not installed on this machine",
        None,
    )
}

fn is_installed(commands: &[&str], paths: &[PathBuf]) -> bool {
    commands
        .iter()
        .any(|command| find_executable(command).is_some())
        || paths.iter().any(|path| path.exists())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return is_executable(path).then(|| path.to_owned());
    }
    env::var_os("PATH")
        .into_iter()
        .flat_map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

fn resolve_command(path: &Path) -> Result<PathBuf> {
    if path.components().count() == 1
        && let Some(found) = find_executable(&path.to_string_lossy())
    {
        return Ok(found);
    }
    let absolute = absolute_path(path)?;
    if !is_executable(&absolute) {
        anyhow::bail!("MCP command is not executable: {}", absolute.display());
    }
    Ok(absolute)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn home_path(path: impl AsRef<Path>) -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(path)
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_./:".contains(character))
            {
                arg.clone()
            } else {
                format!("{arg:?}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_client_aliases_and_rejects_unknown_values() {
        assert_eq!(
            "claude-code".parse::<IntegrationClient>().unwrap(),
            IntegrationClient::Claude
        );
        assert_eq!(
            "traecli".parse::<IntegrationClient>().unwrap(),
            IntegrationClient::Trae
        );
        assert!("other".parse::<IntegrationClient>().is_err());
    }

    #[test]
    fn expands_all_and_deduplicates_explicit_clients() {
        assert_eq!(expand_clients(&[]).len(), 6);
        assert_eq!(
            expand_clients(&[
                IntegrationClient::Gemini,
                IntegrationClient::Gemini,
                IntegrationClient::Codex,
            ]),
            vec![IntegrationClient::Gemini, IntegrationClient::Codex]
        );
    }

    #[test]
    fn merges_trae_mcp_without_overwriting_other_servers() {
        let server = McpServerSpec {
            command: PathBuf::from("/opt/awi"),
            args: vec!["--index-dir".into(), "/data/index".into(), "mcp".into()],
        };
        let merged = merge_json_mcp(
            json!({
                "mcpServers": {
                    "other": {
                        "command": "/bin/other",
                        "args": []
                    }
                },
                "preserved": true
            }),
            &server,
        )
        .unwrap();
        assert_eq!(merged["preserved"], true);
        assert_eq!(merged["mcpServers"]["other"]["command"], "/bin/other");
        assert!(json_server_matches(
            merged.pointer("/mcpServers/awi"),
            &server
        ));
    }

    #[test]
    fn writes_private_atomic_trae_config() {
        let fixture = tempdir().unwrap();
        let path = fixture.path().join(".trae/mcp.json");
        let value = json!({"mcpServers": {"awi": {"command": "/opt/awi"}}});
        write_json_atomic(&path, &value).unwrap();
        assert_eq!(read_json(&path).unwrap(), value);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn default_server_uses_current_binary_and_absolute_index() {
        let fixture = tempdir().unwrap();
        let index = fixture.path().join("index");
        let server = default_server_spec(&index, None, &[]).unwrap();
        assert_eq!(server.command, env::current_exe().unwrap());
        assert_eq!(
            server.args,
            vec![
                "--index-dir".to_owned(),
                index.to_string_lossy().into_owned(),
                "mcp".to_owned()
            ]
        );
    }

    #[test]
    fn explicit_server_command_keeps_only_explicit_arguments() {
        let server = default_server_spec(
            Path::new("ignored"),
            Some(Path::new("/bin/sh")),
            &["wrapper.sh".to_owned()],
        )
        .unwrap();
        assert_eq!(server.command, PathBuf::from("/bin/sh"));
        assert_eq!(server.args, vec!["wrapper.sh"]);
    }
}
