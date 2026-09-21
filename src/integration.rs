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
    Copilot,
    Trae,
    Zcode,
    Kimi,
    OpenCode,
    Pi,
    Cursor,
    Windsurf,
}

impl IntegrationClient {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Claude => "claude",
            Self::Copilot => "copilot",
            Self::Trae => "trae",
            Self::Zcode => "zcode",
            Self::Kimi => "kimi",
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
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
            "copilot" | "github-copilot" | "github-copilot-cli" => Ok(Self::Copilot),
            "trae" | "traecode" | "trae-code" | "traecli" => Ok(Self::Trae),
            "zcode" => Ok(Self::Zcode),
            "kimi" | "kimi-code" => Ok(Self::Kimi),
            "opencode" | "open-code" => Ok(Self::OpenCode),
            "pi" | "pi-coding-agent" => Ok(Self::Pi),
            "cursor" | "cursor-agent" => Ok(Self::Cursor),
            "windsurf" | "windsurf-cascade" => Ok(Self::Windsurf),
            other => anyhow::bail!(
                "unsupported integration client {other:?}; expected all, codex, gemini, \
                 claude, copilot, trae, zcode, kimi, opencode, pi, cursor, or windsurf"
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
            IntegrationClient::Copilot => integrate_copilot(&options.server, options.dry_run),
            IntegrationClient::Trae => {
                integrate_trae(&options.project_root, &options.server, options.dry_run)
            }
            IntegrationClient::Zcode => integrate_zcode(&options.server, options.dry_run),
            IntegrationClient::Kimi => integrate_kimi(&options.server, options.dry_run),
            IntegrationClient::OpenCode => integrate_opencode(&options.server, options.dry_run),
            IntegrationClient::Pi => integrate_pi(&options.server, options.dry_run),
            IntegrationClient::Cursor => integrate_cursor(&options.server, options.dry_run),
            IntegrationClient::Windsurf => integrate_windsurf(&options.server, options.dry_run),
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
            IntegrationClient::Copilot,
            IntegrationClient::Trae,
            IntegrationClient::Zcode,
            IntegrationClient::Kimi,
            IntegrationClient::OpenCode,
            IntegrationClient::Pi,
            IntegrationClient::Cursor,
            IntegrationClient::Windsurf,
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

fn integrate_copilot(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Copilot;
    let path = home_path(".copilot/mcp-config.json");
    integrate_json_config(
        client,
        is_installed(&["copilot"], std::slice::from_ref(&path)),
        path,
        "/mcpServers/awi",
        server,
        dry_run,
        merge_copilot_mcp,
        copilot_server_matches,
        "native user-scope MCP entry added; Copilot CLI can load it immediately",
    )
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

fn integrate_zcode(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Zcode;
    if !is_installed(&["zcode"], &[home_path(".zcode")]) {
        return not_installed(client);
    }
    let path = home_path(".zcode/cli/config.json");
    let existing = match read_optional_json(&path) {
        Ok(value) => value,
        Err(error) => return failed(client, error),
    };
    if zcode_server_matches(existing.pointer("/mcp/servers/awi"), server) {
        return success(
            client,
            IntegrationStatus::AlreadyConfigured,
            "native user-scope MCP entry already matches",
            Some(path),
        );
    }
    if dry_run {
        return success(
            client,
            IntegrationStatus::WouldConfigure,
            "would merge ~/.zcode/cli/config.json at mcp.servers.awi",
            Some(path),
        );
    }
    let merged = match merge_zcode_mcp(existing, server) {
        Ok(value) => value,
        Err(error) => return failed(client, error),
    };
    if let Err(error) = write_json_atomic(&path, &merged) {
        return failed(client, error);
    }
    success(
        client,
        IntegrationStatus::Configured,
        "native user-scope MCP entry added; new Zcode sessions auto-connect",
        Some(path),
    )
}

fn integrate_kimi(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Kimi;
    let home = kimi_home_path();
    if !is_installed(
        &["kimi", "kimi-code"],
        &[home.join("bin/kimi"), home.clone()],
    ) {
        return not_installed(client);
    }
    let path = home.join("mcp.json");
    let existing = match read_optional_json(&path) {
        Ok(value) => value,
        Err(error) => return failed(client, error),
    };
    if kimi_server_matches(existing.pointer("/mcpServers/awi"), server) {
        return success(
            client,
            IntegrationStatus::AlreadyConfigured,
            "user-scope mcp.json entry already matches",
            Some(path),
        );
    }
    if dry_run {
        return success(
            client,
            IntegrationStatus::WouldConfigure,
            "would merge the user-level Kimi Code mcp.json",
            Some(path),
        );
    }
    let merged = match merge_kimi_mcp(existing, server) {
        Ok(value) => value,
        Err(error) => return failed(client, error),
    };
    if let Err(error) = write_json_atomic(&path, &merged) {
        return failed(client, error);
    }
    success(
        client,
        IntegrationStatus::Configured,
        "user-scope MCP entry added; start a new Kimi Code session to load it",
        Some(path),
    )
}

fn integrate_opencode(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::OpenCode;
    let path = opencode_config_path();
    let installed = is_installed(
        &["opencode"],
        &[path.clone(), xdg_config_home().join("opencode")],
    );
    integrate_json_config(
        client,
        installed,
        path,
        "/mcp/awi",
        server,
        dry_run,
        merge_opencode_mcp,
        opencode_server_matches,
        "native global MCP entry added; restart OpenCode or start a new session",
    )
}

fn integrate_pi(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Pi;
    let home = pi_agent_home_path();
    let executable = find_executable("pi");
    let installed = executable.is_some() || home.exists();
    if !installed {
        return not_installed(client);
    }
    let adapter_installed = pi_mcp_adapter_installed(executable.as_deref(), &home);
    let mut result = integrate_json_config(
        client,
        true,
        home.join("mcp.json"),
        "/mcpServers/awi",
        server,
        dry_run,
        merge_pi_mcp,
        pi_server_matches,
        "Pi MCP adapter config added; restart Pi to load AWI",
    );
    if !adapter_installed && result.status != IntegrationStatus::Failed {
        result.status = if dry_run {
            IntegrationStatus::WouldConfigure
        } else {
            IntegrationStatus::NeedsAttention
        };
        result.detail = if dry_run {
            "would merge the Pi MCP config; Pi also requires `pi install \
             npm:pi-mcp-adapter`"
                .to_owned()
        } else {
            "MCP config is ready, but Pi core has no native MCP client; run `pi install \
             npm:pi-mcp-adapter`, then restart Pi"
                .to_owned()
        };
    }
    result
}

fn integrate_cursor(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Cursor;
    let path = home_path(".cursor/mcp.json");
    integrate_json_config(
        client,
        is_installed(&["cursor", "cursor-agent"], &[home_path(".cursor")]),
        path,
        "/mcpServers/awi",
        server,
        dry_run,
        merge_json_mcp,
        json_server_matches,
        "native global MCP entry added; restart Cursor or start a new Agent session",
    )
}

fn integrate_windsurf(server: &McpServerSpec, dry_run: bool) -> ClientIntegration {
    let client = IntegrationClient::Windsurf;
    let path = home_path(".codeium/windsurf/mcp_config.json");
    integrate_json_config(
        client,
        is_installed(&["windsurf"], &[home_path(".codeium/windsurf")]),
        path,
        "/mcpServers/awi",
        server,
        dry_run,
        merge_json_mcp,
        json_server_matches,
        "native global MCP entry added; refresh MCP servers in Windsurf",
    )
}

#[allow(clippy::too_many_arguments)]
fn integrate_json_config(
    client: IntegrationClient,
    installed: bool,
    path: PathBuf,
    pointer: &str,
    server: &McpServerSpec,
    dry_run: bool,
    merge: fn(Value, &McpServerSpec) -> Result<Value>,
    matches: fn(Option<&Value>, &McpServerSpec) -> bool,
    configured_detail: &str,
) -> ClientIntegration {
    if !installed {
        return not_installed(client);
    }
    let existing = match read_optional_json(&path) {
        Ok(value) => value,
        Err(error) => return failed(client, error),
    };
    if matches(existing.pointer(pointer), server) {
        return success(
            client,
            IntegrationStatus::AlreadyConfigured,
            "native MCP entry already matches",
            Some(path),
        );
    }
    if dry_run {
        return success(
            client,
            IntegrationStatus::WouldConfigure,
            "would merge a native MCP entry",
            Some(path),
        );
    }
    let merged = match merge(existing, server) {
        Ok(value) => value,
        Err(error) => return failed(client, error),
    };
    if let Err(error) = write_json_atomic(&path, &merged) {
        return failed(client, error);
    }
    success(
        client,
        IntegrationStatus::Configured,
        configured_detail,
        Some(path),
    )
}

fn merge_zcode_mcp(mut root: Value, server: &McpServerSpec) -> Result<Value> {
    if !root.is_object() {
        anyhow::bail!("Zcode config root must be a JSON object");
    }
    let root_object = root.as_object_mut().expect("checked above");
    let mcp = root_object.entry("mcp").or_insert_with(|| json!({}));
    if !mcp.is_object() {
        anyhow::bail!("Zcode mcp config must be a JSON object");
    }
    let servers = mcp
        .as_object_mut()
        .expect("checked above")
        .entry("servers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        anyhow::bail!("Zcode mcp.servers must be a JSON object");
    }
    servers.as_object_mut().expect("checked above").insert(
        SERVER_NAME.to_owned(),
        json!({
            "type": "stdio",
            "command": server.command,
            "args": server.args,
            "env": {},
            "enabled": true,
            "timeoutMs": MCP_TIMEOUT_MS
        }),
    );
    Ok(root)
}

fn merge_opencode_mcp(mut root: Value, server: &McpServerSpec) -> Result<Value> {
    if !root.is_object() {
        anyhow::bail!("OpenCode config root must be a JSON object");
    }
    let mcp = root
        .as_object_mut()
        .expect("checked above")
        .entry("mcp")
        .or_insert_with(|| json!({}));
    if !mcp.is_object() {
        anyhow::bail!("OpenCode mcp config must be a JSON object");
    }
    let mut command = Vec::with_capacity(server.args.len() + 1);
    command.push(server.command.to_string_lossy().into_owned());
    command.extend(server.args.iter().cloned());
    mcp.as_object_mut().expect("checked above").insert(
        SERVER_NAME.to_owned(),
        json!({
            "type": "local",
            "command": command,
            "enabled": true,
            "timeout": MCP_TIMEOUT_MS
        }),
    );
    Ok(root)
}

fn merge_pi_mcp(mut root: Value, server: &McpServerSpec) -> Result<Value> {
    if !root.is_object() {
        anyhow::bail!("Pi MCP config root must be a JSON object");
    }
    let servers = root
        .as_object_mut()
        .expect("checked above")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        anyhow::bail!("Pi mcpServers must be a JSON object");
    }
    servers.as_object_mut().expect("checked above").insert(
        SERVER_NAME.to_owned(),
        json!({
            "transport": "stdio",
            "command": server.command,
            "args": server.args,
            "env": {},
            "lifecycle": "eager"
        }),
    );
    Ok(root)
}

fn merge_copilot_mcp(mut root: Value, server: &McpServerSpec) -> Result<Value> {
    if !root.is_object() {
        anyhow::bail!("GitHub Copilot MCP config root must be a JSON object");
    }
    let servers = root
        .as_object_mut()
        .expect("checked above")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        anyhow::bail!("GitHub Copilot mcpServers must be a JSON object");
    }
    servers.as_object_mut().expect("checked above").insert(
        SERVER_NAME.to_owned(),
        json!({
            "type": "local",
            "command": server.command,
            "args": server.args,
            "env": {},
            "tools": ["*"],
            "timeout": MCP_TIMEOUT_MS
        }),
    );
    Ok(root)
}

fn merge_kimi_mcp(mut root: Value, server: &McpServerSpec) -> Result<Value> {
    if !root.is_object() {
        anyhow::bail!("Kimi Code MCP config root must be a JSON object");
    }
    let servers = root
        .as_object_mut()
        .expect("checked above")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        anyhow::bail!("Kimi Code mcpServers must be a JSON object");
    }
    servers.as_object_mut().expect("checked above").insert(
        SERVER_NAME.to_owned(),
        json!({
            "transport": "stdio",
            "command": server.command,
            "args": server.args,
            "env": {},
            "enabled": true,
            "startupTimeoutMs": MCP_TIMEOUT_MS,
            "toolTimeoutMs": MCP_TIMEOUT_MS
        }),
    );
    Ok(root)
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

fn zcode_server_matches(value: Option<&Value>, server: &McpServerSpec) -> bool {
    json_server_matches(value, server)
        && value.is_some_and(|value| {
            matches!(
                value.get("type").and_then(Value::as_str),
                None | Some("stdio")
            ) && value.get("enabled").and_then(Value::as_bool) != Some(false)
        })
}

fn kimi_server_matches(value: Option<&Value>, server: &McpServerSpec) -> bool {
    json_server_matches(value, server)
        && value.is_some_and(|value| {
            matches!(
                value.get("transport").and_then(Value::as_str),
                None | Some("stdio")
            ) && value.get("enabled").and_then(Value::as_bool) != Some(false)
        })
}

fn copilot_server_matches(value: Option<&Value>, server: &McpServerSpec) -> bool {
    json_server_matches(value, server)
        && value.is_some_and(|value| {
            matches!(
                value.get("type").and_then(Value::as_str),
                None | Some("local") | Some("stdio")
            ) && value.get("enabled").and_then(Value::as_bool) != Some(false)
        })
}

fn opencode_server_matches(value: Option<&Value>, server: &McpServerSpec) -> bool {
    let Some(value) = value else {
        return false;
    };
    let command = value
        .get("command")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut expected = Vec::with_capacity(server.args.len() + 1);
    expected.push(server.command.to_string_lossy().into_owned());
    expected.extend(server.args.iter().cloned());
    value.get("type").and_then(Value::as_str) == Some("local")
        && command == expected.iter().map(String::as_str).collect::<Vec<_>>()
        && value.get("enabled").and_then(Value::as_bool) != Some(false)
}

fn pi_server_matches(value: Option<&Value>, server: &McpServerSpec) -> bool {
    json_server_matches(value, server)
        && value.is_some_and(|value| {
            matches!(
                value.get("transport").and_then(Value::as_str),
                None | Some("stdio")
            )
        })
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decode {}", path.display()))
}

fn read_optional_json(path: &Path) -> Result<Value> {
    if path.exists() {
        read_json(path)
    } else {
        Ok(json!({}))
    }
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

fn kimi_home_path() -> PathBuf {
    env::var_os("KIMI_CODE_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_path(".kimi-code"))
}

fn xdg_config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_path(".config"))
}

fn opencode_config_path() -> PathBuf {
    if let Some(path) = env::var_os("OPENCODE_CONFIG").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let directory = xdg_config_home().join("opencode");
    let json = directory.join("opencode.json");
    let jsonc = directory.join("opencode.jsonc");
    if json.exists() || !jsonc.exists() {
        json
    } else {
        jsonc
    }
}

fn pi_agent_home_path() -> PathBuf {
    env::var_os("PI_CODING_AGENT_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_path(".pi/agent"))
}

fn pi_mcp_adapter_installed(executable: Option<&Path>, home: &Path) -> bool {
    let listed = executable
        .and_then(|executable| run(executable, ["list"]).ok())
        .filter(|output| output.status.success())
        .map(|output| {
            format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .is_some_and(|output| output.contains("pi-mcp-adapter"));
    listed
        || home.join("npm/pi-mcp-adapter").exists()
        || home.join("npm/node_modules/pi-mcp-adapter").exists()
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
        assert_eq!(
            "github-copilot".parse::<IntegrationClient>().unwrap(),
            IntegrationClient::Copilot
        );
        assert_eq!(
            "open-code".parse::<IntegrationClient>().unwrap(),
            IntegrationClient::OpenCode
        );
        assert_eq!(
            "pi-coding-agent".parse::<IntegrationClient>().unwrap(),
            IntegrationClient::Pi
        );
        assert!("other".parse::<IntegrationClient>().is_err());
    }

    #[test]
    fn expands_all_and_deduplicates_explicit_clients() {
        assert_eq!(expand_clients(&[]).len(), 11);
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
    fn merges_zcode_mcp_without_overwriting_other_config() {
        let server = McpServerSpec {
            command: PathBuf::from("/opt/awi"),
            args: vec!["--index-dir".into(), "/data/index".into(), "mcp".into()],
        };
        let merged = merge_zcode_mcp(
            json!({
                "mcp": {
                    "servers": {
                        "other": {
                            "type": "http",
                            "url": "https://example.invalid/mcp"
                        }
                    }
                },
                "plugins": {
                    "example": true
                }
            }),
            &server,
        )
        .unwrap();
        let merged_again = merge_zcode_mcp(merged.clone(), &server).unwrap();
        assert_eq!(merged_again, merged);
        assert_eq!(merged["plugins"]["example"], true);
        assert_eq!(
            merged["mcp"]["servers"]["other"]["url"],
            "https://example.invalid/mcp"
        );
        assert!(zcode_server_matches(
            merged.pointer("/mcp/servers/awi"),
            &server
        ));
        assert_eq!(merged["mcp"]["servers"]["awi"]["timeoutMs"], 45_000);
    }

    #[test]
    fn merges_opencode_mcp_using_local_command_array() {
        let server = McpServerSpec {
            command: PathBuf::from("/opt/awi"),
            args: vec!["--index-dir".into(), "/data/index".into(), "mcp".into()],
        };
        let merged = merge_opencode_mcp(
            json!({
                "$schema": "https://opencode.ai/config.json",
                "mcp": {
                    "other": {
                        "type": "remote",
                        "url": "https://example.invalid/mcp"
                    }
                }
            }),
            &server,
        )
        .unwrap();
        let merged_again = merge_opencode_mcp(merged.clone(), &server).unwrap();
        assert_eq!(merged_again, merged);
        assert_eq!(merged["$schema"], "https://opencode.ai/config.json");
        assert_eq!(merged["mcp"]["other"]["url"], "https://example.invalid/mcp");
        assert_eq!(
            merged["mcp"]["awi"]["command"],
            json!(["/opt/awi", "--index-dir", "/data/index", "mcp"])
        );
        assert!(opencode_server_matches(merged.pointer("/mcp/awi"), &server));
    }

    #[test]
    fn merges_pi_mcp_for_the_adapter_without_overwriting_settings() {
        let server = McpServerSpec {
            command: PathBuf::from("/opt/awi"),
            args: vec!["mcp".into()],
        };
        let merged = merge_pi_mcp(
            json!({
                "settings": {
                    "toolPrefix": "mcp"
                },
                "mcpServers": {
                    "other": {
                        "url": "https://example.invalid/mcp"
                    }
                }
            }),
            &server,
        )
        .unwrap();
        assert_eq!(merged["settings"]["toolPrefix"], "mcp");
        assert_eq!(
            merged["mcpServers"]["other"]["url"],
            "https://example.invalid/mcp"
        );
        assert_eq!(merged["mcpServers"]["awi"]["lifecycle"], "eager");
        assert!(pi_server_matches(
            merged.pointer("/mcpServers/awi"),
            &server
        ));
    }

    #[test]
    fn merges_copilot_mcp_with_all_tools_enabled() {
        let server = McpServerSpec {
            command: PathBuf::from("/opt/awi"),
            args: vec!["mcp".into()],
        };
        let merged = merge_copilot_mcp(json!({}), &server).unwrap();
        assert_eq!(merged["mcpServers"]["awi"]["type"], "local");
        assert_eq!(merged["mcpServers"]["awi"]["tools"], json!(["*"]));
        assert!(copilot_server_matches(
            merged.pointer("/mcpServers/awi"),
            &server
        ));
    }

    #[test]
    fn merges_kimi_mcp_idempotently_without_overwriting_other_servers() {
        let server = McpServerSpec {
            command: PathBuf::from("/opt/awi"),
            args: vec!["--index-dir".into(), "/data/index".into(), "mcp".into()],
        };
        let existing = json!({
            "mcpServers": {
                "other": {
                    "url": "https://example.invalid/mcp"
                }
            },
            "preserved": true
        });
        let merged = merge_kimi_mcp(existing, &server).unwrap();
        let merged_again = merge_kimi_mcp(merged.clone(), &server).unwrap();
        assert_eq!(merged_again, merged);
        assert_eq!(merged["preserved"], true);
        assert_eq!(
            merged["mcpServers"]["other"]["url"],
            "https://example.invalid/mcp"
        );
        assert!(kimi_server_matches(
            merged.pointer("/mcpServers/awi"),
            &server
        ));
        assert_eq!(merged["mcpServers"]["awi"]["startupTimeoutMs"], 45_000);
        assert_eq!(merged["mcpServers"]["awi"]["toolTimeoutMs"], 45_000);
    }

    #[test]
    fn native_client_matches_reject_disabled_or_wrong_transport_entries() {
        let server = McpServerSpec {
            command: PathBuf::from("/opt/awi"),
            args: vec!["mcp".into()],
        };
        let disabled = json!({
            "command": "/opt/awi",
            "args": ["mcp"],
            "enabled": false
        });
        let remote = json!({
            "type": "http",
            "command": "/opt/awi",
            "args": ["mcp"]
        });
        assert!(!zcode_server_matches(Some(&disabled), &server));
        assert!(!zcode_server_matches(Some(&remote), &server));
        assert!(!kimi_server_matches(Some(&disabled), &server));
        assert!(kimi_server_matches(
            Some(&json!({
                "transport": "stdio",
                "command": "/opt/awi",
                "args": ["mcp"]
            })),
            &server
        ));
    }

    #[test]
    fn writes_private_atomic_json_config() {
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
