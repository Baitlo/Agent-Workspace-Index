# Recommended Agent Installation Prompt

Paste the prompt below into a coding agent while its working directory is the
repository that you want AWI to index:

```text
Install Agent Workspace Index (AWI) for this repository from
https://github.com/Baitlo/Agent-Workspace-Index.

1. Resolve the current repository root and keep it as the target workspace.
2. Download and review
   https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh.
3. Run `install-release.sh --workspace "<absolute repository root>"`.
4. Let the installer select and checksum-verify the native Release binary,
   install AWI, add or update the managed AWI section in the repository's
   top-level AGENTS.md, create the first local index, index applicable ancestor
   AGENTS.md files and SKILL.md manifests from allowlisted Agent directories,
   discover curated cross-Agent memory for this project, detect installed coding
   agents, and register AWI as their MCP server.
5. If Pi is installed, allow the installer to add pi-mcp-adapter; Pi has no
   native MCP client. Do not install unrelated packages.
6. Verify that the reported index exists and rerun `awi integrate --dry-run`
   to confirm configured clients are idempotent.
7. Report the binary path, index path, per-client status, and any manual action
   still required. Do not claim success for clients reported as not_installed,
   needs_attention, unsupported, or failed.
```

The installer defaults to:

- binary: `~/.local/bin/awi`
- index: `${XDG_CACHE_HOME:-$HOME/.cache}/awi/indexes/<workspace>-<hash>`
- workspace instructions: managed AWI block in top-level `AGENTS.md`
- Agent knowledge: ancestor `AGENTS.md` files and `SKILL.md` manifests from
  known per-client directories
- Agent memory: curated project memory and summaries; raw history is opt-in
- clients: every detected supported harness (Codex, Gemini CLI, Claude Code,
  GitHub Copilot CLI, TraeCode, Zcode, Kimi Code, OpenCode, Pi, Cursor,
  Windsurf, Qwen Code, Cline, Zed, Amazon Q Developer, and Crush)

For a restricted install:

```bash
bash install-release.sh \
  --workspace /absolute/path/to/repository \
  --clients codex,claude,opencode
```

Use `--skip-pi-adapter` when third-party Pi extensions must be reviewed and
installed separately. Use `--skip-agent-instructions` to leave the repository
AGENTS.md unchanged, `--skip-agent-knowledge` when only repository files and
memory should be indexed, or `--skip-agent-memory` to omit memory. Add
`--include-raw-memory` only when raw chat/session indexing is explicitly wanted.
