# AWI Harness Compatibility And Stress Evaluation (2026-09-22)

## Scope

This development evaluation exercises the 16 clients that AWI advertises:
Codex, Gemini CLI, Claude Code, GitHub Copilot CLI, TraeCode, Zcode,
Kimi Code, OpenCode, Pi, Cursor, Windsurf, Qwen Code, Cline, Zed,
Amazon Q Developer, and Crush.

The run used MLX CPU worker `4276696` (`dc05-p13-t402-n050`), with 64
allocated CPU cores, 256 GiB allocated memory, and 2.8 TiB free under
worker-local `/tmp`. Mutable homes, npm packages, indexes, sockets, and logs
were isolated under `/tmp/awi-harness-eval`. Durable results are under:

```text
/mnt/bn/baiweikang/qianchuan_distill/awi/harness_eval/20260922-worker4276696
```

The worker host had high unrelated load. Latency results therefore describe
the observed shared-host environment, not dedicated-hardware limits.

## Installed Harnesses

The following real CLI versions were installed on the worker:

| Harness | Version |
|---|---:|
| Codex | 0.155.1 |
| Gemini CLI | 0.60.0 |
| Claude Code | 2.1.278 |
| GitHub Copilot CLI | 1.0.87 |
| Kimi Code | 2.0.2 |
| OpenCode | 1.18.32 |
| Pi | 0.73.1 |
| Cursor Agent | 2026.09.18-9a7762b |
| Qwen Code | 0.24.3 |
| Cline | 3.0.64 |
| Amazon Q Developer CLI | 1.19.7 |
| Crush | 0.96.1 |

TraeCode, Zcode, Windsurf, and Zed are GUI-first clients. Their current
official configuration contracts were checked against generated files, but
full GUI Agent sessions were not run on the headless CPU worker.

## Compatibility Results

Every integration ran twice in a fresh isolated home. Unrelated configuration
keys were preserved and generated files were private (`0600`).

| Harness | Verification |
|---|---|
| Codex | `codex mcp get awi --json` reports enabled stdio server |
| Gemini | Server discovered; intentionally disabled until workspace trust |
| Claude Code | `claude mcp get awi` reports connected |
| Copilot | `copilot mcp get awi` reports enabled, all tools |
| TraeCode | Official project `.trae/mcp.json` shape validated |
| Zcode | Official `mcp.servers` shape validated |
| Kimi Code | Runtime `strace` confirms it reads `mcp.json` and executes AWI |
| OpenCode | `opencode mcp list` reports connected |
| Pi | Adapter and AWI config detected after writable-prefix fallback |
| Cursor | `cursor-agent mcp list` reports `awi: ready` |
| Windsurf | Official `mcpServers` shape and path validated |
| Qwen Code | `qwen mcp list` reports connected |
| Cline | `cline config mcp --json` reports enabled stdio server |
| Zed | Current upstream settings schema/source validated |
| Amazon Q | `qchat mcp status --name awi` reports enabled server |
| Crush | Current binary reads generated XDG config without parse errors |

Ten native CLI status probes were repeated 20 times at concurrency 20:
`200/200` passed, with no timeout or nonzero exit. Cursor was the slowest
discovery probe (P95 `4.42 s`); Codex was the fastest (P95 `51.5 ms`).

## MCP Stress

The realistic persistent-client run opened 64 MCP clients per level and waited
for every response before closing stdin:

| Concurrent clients | Search calls | Completed | Client P95 |
|---:|---:|---:|---:|
| 1 | 256 | 256 | 160 ms |
| 8 | 256 | 256 | 1.10 s |
| 32 | 256 | 256 | 3.46 s |
| 64 | 256 | 256 | 5.09 s |

Across these 1,024 calls:

- zero timeout, nonzero exit, parse failure, or tool error;
- audit P50/P95/P99 was `274/1300/1330 ms`;
- no response exceeded 100 KiB;
- daemon RSS changed from `61,384` to `62,080 KiB`;
- 200 abrupt disconnects, malformed JSON, and a request over 1 MiB did not
  terminate the daemon;
- malformed and oversized requests returned bounded protocol errors;
- `limit=50` was compacted to the effective maximum of 20.

An intentionally unrealistic pipelined-EOF run sent all requests and
immediately closed stdin. At 32 and 64 concurrent adapters, RMCP stopped the
stdio service before all queued responses were written. Persistent harness
connections did not reproduce this. Keep this as a protocol-edge limitation:
clients must not close the transport before receiving outstanding responses.

## Real-Agent Runs

A fresh one-repetition Codex A/B ran all 12 development tasks (24 trials):

- both baseline and AWI answered `12/12` correctly;
- no timeout, nonzero exit, or policy violation;
- AWI tool calls fell from 48 to 17 (`64.6%`);
- discovery searches fell from 22 to 12 (`45.5%`);
- total wall time fell from `262.06 s` to `192.89 s` (`26.4%`);
- input tokens fell from 1,431,815 to 617,339 (`56.9%`).

AWI was faster in 7/12 pairs. The wall-time bootstrap interval crosses zero,
so this single-repetition run does not establish a significant speed gain.

Gemini 0.60.0 initially ignored the prompt's "AWI only" instruction and used
built-in grep. Five follow-up runs all called `mcp_awi_workspace_search` successfully,
but some still mixed in built-in retrieval. Attempts to deny those tools exposed a
Gemini 0.60.0 policy-schema mismatch: documented wildcard/array forms and an
observed `read_file` tool name were rejected. This is a harness routing/policy
issue rather than an AWI connection failure.

## Problems Found

### Fixed

1. **Cline 3.x config path and schema drift**

   AWI wrote legacy `~/.cline/mcp.json`; Cline 3.0.64 reads
   `~/.cline/data/settings/cline_mcp_settings.json` and wraps stdio fields in
   `transport`. Before the fix `cline config mcp --json` returned `[]`.

2. **Pi adapter install fails with an unwritable global npm prefix**

   `pi install npm:pi-mcp-adapter` attempted `/usr/local/lib/node_modules` and
   failed with `EACCES`. The installer now retries under
   `$PI_CODING_AGENT_DIR/npm` and registers that local package.

3. **JSONC client configs were rejected**

   OpenCode officially accepts `opencode.jsonc`, and Zed settings commonly use
   comments/trailing commas. AWI's strict JSON parser rejected both. Parsing now
   tries JSON first and JSON5/JSONC second.

4. **Claude Code integration was not idempotent**

   A matching entry was removed and recreated on every run. AWI now compares
   `~/.claude.json` first and returns `already_configured`.

### Open Or Environmental

1. **High-concurrency daemon serialization**

   Correctness remained intact at 64 concurrent clients, but the single daemon
   request loop creates queueing: audit P95 reached `1.30 s`.

2. **Pipelined EOF drops outstanding responses**

   Closing MCP stdin immediately after a request burst can cancel queued RMCP
   work. Real persistent clients were unaffected.

3. **JSONC formatting and comments are not retained**

   JSONC input is accepted and all data keys survive, but AWI serializes the
   merged file as standard pretty JSON.

4. **Gemini workspace trust is a required manual safety gate**

   User MCP servers are intentionally suppressed in untrusted workspaces.

5. **GUI-only harnesses lack headless end-to-end coverage**

   TraeCode, Zcode, Windsurf, and Zed were validated against current config
   contracts, but not through GUI Agent tool calls on this worker.

6. **Gemini does not reliably obey prose-only AWI routing**

   In unrestricted headless runs Gemini sometimes used built-in grep despite an
   explicit AWI-only prompt. All five follow-up runs also used AWI, but the
   tested 0.60.0 policy engine rejected documented deny-rule forms, preventing
   strict tool isolation.

## Artifacts

- `run_harness_matrix.py`: isolated install/config/idempotency and native probes.
- `stress_harness_probes.py`: repeated native harness discovery.
- `stress_mcp.py`: pipelined EOF and protocol-edge run.
- `stress_mcp_persistent.py`: realistic persistent-client stress run.
- `run_gemini_real.py`: Gemini headless Agent-use runner.
- Shared `matrix*.json`, `stress*.json`, raw logs, and real-Agent traces under
  the durable result directory above.
