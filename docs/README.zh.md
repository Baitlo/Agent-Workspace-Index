# AWI: Agent Workspace Index

[English](../README.md) · **中文** · [繁體中文](README.zh-TW.md) · [日本語](README.ja.md) · [한국어](README.ko.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [Português](README.pt.md) · [Español](README.es.md) · [العربية](README.ar.md) · [Italiano](README.it.md) · [Ελληνικά](README.el.md) · [ไทย](README.th.md) · [Bahasa Melayu](README.ms.md)

AWI 是一个本地优先的统一索引，用于代码、数据和 Agent 操作知识。它通过一个受限的 CLI 和 Model Context Protocol (MCP) 界面索引源代码、SQL、文档、日志、JSON/JSONL、CSV/TSV、Parquet、`AGENTS.md`、Agent Skills 以及项目级的跨 Agent 记忆。

一条命令即可将该索引连接到 16 种编码 Agent 宿主，包括 Codex、Claude Code、Gemini CLI、GitHub Copilot CLI、OpenCode、Qwen Code、Cline、Zed、Amazon Q Developer 和 Crush。

## 功能特性

- 基于 Tantivy 的混合检索，覆盖路径、文本、符号和数据集 schema。
- 基于 Tree-sitter 的 Rust、Python 和 Go 符号提取。
- 嵌入式只读 DuckDB 查询，仅限显式允许的文件。
- 作用域感知的 `AGENTS.md` 检索和结构化 `SKILL.md` 元数据。
- 跨 Trae、Codex、Zcode、Gemini 和 Claude 的项目感知记忆发现。
- SQLite 目录检查，拒绝过期的搜索结果。
- NFS 安全的 `notify` 和 `reconcile` 更新路径。
- 不可变 generation 快照，支持原子性 daemon 激活。
- MCP 工具：`workspace_search`、`workspace_inspect` 和 `workspace_query`。

有关发现、作用域、排序、去重和安全语义，请参阅 [Agent 知识索引](agent-knowledge.md)。有关记忆来源和原始历史边界，请参阅 [跨 Agent 记忆索引](agent-memory.md)。

## 预编译 Linux 和 macOS 二进制文件

GitHub Releases 提供以下平台的原生二进制文件：

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

一条命令即可为当前仓库安装和配置 AWI：

```bash
curl -fsSL \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh |
  bash -s -- --workspace "$PWD"
```

安装程序会检测 Linux 架构、下载发布包和 `SHA256SUMS`、校验校验和、安装二进制文件、更新工作区顶层 `AGENTS.md` 中的托管 AWI 部分、构建初始索引、发现 Agent 知识和精选项目记忆，并注册每个检测到的受支持 Agent 客户端。现有 `AGENTS.md` 内容会被保留，重新运行安装程序会替换而非重复托管部分。

对于需要审查下载脚本后再执行的环境：

```bash
curl -fsSLO \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh
less install-release.sh
bash install-release.sh --workspace /absolute/path/to/your/repository
```

省略 `--workspace` 仅安装 `awi` 二进制文件。使用 `--version v0.2.0` 固定版本，使用 `--bin-dir` 选择其他目标目录，使用 `--clients` 限制客户端，或使用 `--skip-agent-instructions` 禁用托管指令块。Linux 二进制文件在 Ubuntu 22.04 GitHub 托管运行器上原生构建，需要兼容的 glibc 和 `libstdc++`。macOS 二进制文件在 macOS 15 运行器上为 Apple Silicon 和 Intel 原生构建。

语义检索仍然是可选的。它额外需要 Python、LanceDB、`llama-cpp-python` 和兼容的 GGUF 嵌入模型。维护者应遵循 [发布清单](releasing.md)。

## Agent 辅助安装

将 [推荐安装提示](agent-install-prompt.md) 提供给您的编码 Agent，或从 AWI 检出运行安装程序：

```bash
bash scripts/install.sh --workspace /absolute/path/to/your/repository
```

首次运行会构建并安装 `awi`、更新工作区的托管 AWI 指令、在工作区外创建本地索引、索引在允许的 Agent 目录中找到的祖先 `AGENTS.md` 文件和 `SKILL.md` 清单、索引与该工作区关联的精选记忆、检测已安装的 Agent 客户端，并向每个受支持的客户端注册 AWI MCP 服务器。该操作是幂等的。传递 `--skip-agent-instructions`、`--skip-agent-knowledge` 或 `--skip-agent-memory` 可禁用这些步骤。除非提供 `--include-raw-memory`，否则原始聊天记录保持排除。仅在从源代码构建时需要 Rust 和 Cargo。

如果检测到 Pi，安装程序还会安装固定的 `pi-mcp-adapter@2.36.0`，因为 Pi 故意没有内置 MCP 客户端。这是第三方 Pi 软件包；传递 `--skip-pi-adapter` 可单独审查或安装，或设置 `AWI_PI_MCP_ADAPTER_SPEC` 选择其他已审查版本。运行 `scripts/install.sh --help` 查看自定义二进制文件、索引和客户端选项。

## 构建和测试

```bash
export CARGO_TARGET_DIR=/tmp/awi-target
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
python3 -m unittest tests/test_semantic_sidecar.py
python3 tests/test_audit_session_adoption.py
```

## 基本用法

```bash
# 构建或刷新索引。
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace --json

# 运行持久化 daemon。
awi --index-dir /tmp/my-awi-index serve

# 搜索和检查。
awi --index-dir /tmp/my-awi-index search "workspace query" --limit 10 --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file \
  --symbol exact_function_name --json

# 解析适用于具体工作区路径的指令。
awi --index-dir /tmp/my-awi-index search "build and test rules" \
  --kind agent_instructions --context-path /path/to/workspace/src/lib.rs --json

# 索引和搜索独立的全局 Skill 清单。
awi --index-dir /tmp/my-awi-index reconcile ~/.agents/skills/example/SKILL.md --json
awi --index-dir /tmp/my-awi-index search "diagnose deployment failures" \
  --kind agent_skill --json

# 发现此项目的精选记忆，然后搜索所有已索引类型。
awi --index-dir /tmp/my-awi-index memory --project-root /path/to/workspace
awi --index-dir /tmp/my-awi-index search "previous rollout decision" \
  --context-path /path/to/workspace --json

# 启动 MCP stdio 适配器。
awi --index-dir /tmp/my-awi-index mcp

# 持久化实际 MCP 工具调用以供后续检索分析。
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

不传 `--kind` 的搜索会包含所有已索引类型：代码、文本、结构化数据、Agent 指令、Skills 和项目级记忆。传入 `--kind` 会收窄结果，例如 `--kind agent_memory` 表示仅检索记忆。记忆使用独立的 Tantivy 索引，因此其词汇不会改变普通代码/数据的 IDF 统计。MCP 搜索使用紧凑的 `compact_v3` 格式，具有 1,000 字符预览、稳定的 `file_id`、请求范围的 `search_id` 以及精确符号命中的定义元数据。默认返回 5 条结果，超过 20 的请求会压缩到 20。将返回的 `search_id` 和可选的精确 `symbol` 传递给 `workspace_inspect`，以将检查链接到其检索并居中显示定义处的摘录。从一条包含丰富标识符的查询开始，而不是并行发送近义查询，仅在第一批结果缺乏证据时才扩展。当已知确切路径时，直接使用宿主的文件工具读取。搜索片段是从有界存储的源窗口生成的，轻量级目录多样性重新排序可防止一个构件文件夹填满结果集。stdio 服务器支持标准旧版 MCP `ping`，并将基于初始化的协商限制在协议 `2025-11-25`。

### 语义检索

AWI 可以在不改变 MCP 工具界面的情况下添加 Harrier GGUF Q8 语义通道。文档嵌入在 `reconcile`、`notify` 或初始 `semantic-build` 期间计算；普通搜索仅计算查询嵌入。SQLite 保持权威，文件 generation 不是最新的语义候选会被丢弃。

创建专用 Python 环境并启用 sidecar：

```bash
python3 -m venv ~/.cache/awi/semantic-venv
~/.cache/awi/semantic-venv/bin/pip install -r requirements-semantic.txt

export AWI_SEMANTIC_MODEL=/path/to/harrier-oss-v1-270M-Q8_0.gguf
export AWI_SEMANTIC_MODEL_SHA256=fe12f3583dbbb832def4cffeb46c0d0ab49a3288542d5cdbaeb1741315a01b87
export AWI_SEMANTIC_PYTHON="$HOME/.cache/awi/semantic-venv/bin/python"
export AWI_SEMANTIC_THREADS=16
export AWI_SEMANTIC_EMBED_WORKERS=1
```

对于现有目录，预计算每个符合条件的文件一次：

```bash
awi --index-dir /tmp/awi-writer semantic-build \
  --publish-dir /shared/awi-publication --json
```

中断的批量构建可以重用已提交到 LanceDB 的完整文件 generation。如果工作区内容在失败尝试期间发生变化，请先不发布地协调目录，然后恢复：

```bash
env -u AWI_SEMANTIC_MODEL \
  awi --index-dir /tmp/awi-writer reconcile /path/to/workspace --json
awi --index-dir /tmp/awi-writer semantic-build --resume \
  --publish-dir /shared/awi-publication --json
```

恢复模式仍会针对 SQLite 重新哈希每个源，并仅重用精确的 `(file_id, generation)` 对。封存执行相同的完整覆盖检查，并在发布前修剪过期的对。

构建使用分词器感知、结构敏感的块，上限为 480 个模型 token，每个文件最多保留四个块，仅对查询应用 Harrier 查询指令，并对嵌入进行 L2 归一化。源代码、文本、半结构化和表格内容均符合条件；敏感或仅元数据文件以及 Agent 记忆被排除。提取的有效载荷为空的非零合格文件会获得路径和类型标头向量，因此构建和发布覆盖范围保持一致；零字节文件保持排除。

批量构建器可以将 `AWI_SEMANTIC_EMBED_WORKERS` 设置为大于 1。额外的 GGUF 模型实例仅针对文档嵌入延迟加载；查询服务保持单个模型。AWI 将 `AWI_SEMANTIC_THREADS` 分配给这些工作线程，因此请将其设置为 sidecar 可用的总 CPU 配额。

后续的生产者周期仅更新更改的文件。无更改周期验证覆盖范围并仅推进本地语义清单；不会发布冗余的 NFS 快照。发布前，AWI 要求 LanceDB 覆盖每个当前符合条件的 `(file_id, generation)`。语义数据库及其清单被复制到与 SQLite 和 Tantivy 相同的不可变快照中。读取器拒绝不匹配的 generation，并在 sidecar 不可用时回退到词汇搜索。取消设置 `AWI_SEMANTIC_MODEL` 以禁用语义通道。

查询时语义检索与词汇通道并发运行。向量超取受配置的最大每文件块数限制，保留文件级 Top-K 覆盖而不返回冗余块候选。持久化读取器在服务查询之前运行一次完整的混合预热。sidecar 使用 Linux 父进程死亡信号，因此停止读取器也会释放模型。

封存的 generation 使用 IVF_FLAT 索引并探测每个分区。这保留了原始的归一化 Q8 向量和精确的 Top-K 排序，同时避免了 AWI 当前语料库规模下乘积量化的召回损失。

对于 NFS 支持的工作区，在本地存储上构建可变索引并发布不可变快照：

```bash
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace \
  --publish-dir /shared/awi-publication --json

awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

当客户端主机缺乏足够的 CPU 配额来满足 GGUF 延迟目标时，在 CPU worker 上运行快照读取器，并使用重连流本地隧道保留本地 MCP socket：

```bash
scripts/remote-reader-tunnel.sh \
  /tmp/local-awi.sock /tmp/remote-awi.sock <worker-ip> <ssh-port>
```

### 自动更新链

要在无需手动协调的情况下保持共享发布最新，请在读取器旁边运行持久化生产者。生产者协调根目录并仅在内容更改时发布新的不可变快照；快照跟随读取器在下次请求时原子地切换到每个新 generation。这端到端地闭合了循环：编辑文件，读取器会自动反映。

快照下载和校验和验证在后台刷新工作线程上运行。在新快照具体化时，请求继续针对最后一个有效 generation 运行，避免查询路径上的 NFS 刷新暂停。单个客户端断开连接或写入失败会被记录而不会终止共享 daemon。

本地磁盘根目录被实时监控（inotify），因此编辑会在去抖窗口内发布。远程根目录（NFS 及类似，通过 `/proc/mounts` 检测）和每 `--interval-ms` 的定期安全网滴答驱动其余部分，因为文件系统事件对远程写入不可靠。如果监视器无法启动，生产者会干净地降级为纯定期协调。访问和仅元数据事件被忽略以防止自触发扫描；追加密集的 `.log`、`.jsonl`、`.ndjson`、`.csv`、`.tsv` 和 `.parquet` 更新被推迟到定期传递，而不是为每次写入重建快照。每个周期还会重新发现由 `awi memory` 注册的项目的来源。新的 Agent 记忆根目录会自动索引，消失的注册根目录会被协调，因此不会提供已删除的证据。

```bash
# 生产者：实时监控本地根目录，每 5 秒最多协调一次每个根目录，
# 在 500 毫秒内合并编辑突发，更改时自动发布，保留 3 个 generation。
awi --index-dir /tmp/awi-writer watch \
  --publish-dir /shared/awi-publication \
  --interval-ms 5000 --debounce-ms 500 --retain 3

# 读取器：跟随发布并自动激活新 generation。
awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

`watch` 默认注册的非记忆根目录；注册的记忆项目在每个周期单独重新发现。传递 `--root <path>` 一次或多次以限制集合。保留修剪在每次发布后删除较旧的 generation，并且永远不会删除指针当前引用的 generation，因此共享目录不会无限增长。

对于 Codex，`scripts/awi-mcp-snapshot-wrapper.sh` 在启动 stdio 适配器之前启动或重用一个本地快照 daemon。配置 `AWI_SNAPSHOT_SOURCE` 和 `AWI_MCP_AUDIT_LOG`，然后将包装器注册为全局 MCP 服务器。可变快照数据保留在 `AWI_RUNTIME_DIR` 下，而 Unix socket 和启动锁默认在 `~/.local/state/awi-$UID` 下，因此临时缓存的清理无法取消链接实时控制 socket。冷具体化默认等待最多 10 分钟；使用 `AWI_START_TIMEOUT_MS` 覆盖它。

### 一键 Agent 集成

`awi integrate` 检测已安装的 Agent 客户端，并在一次传递中向每个受支持的 MCP 宿主注册 AWI：

```bash
# 预览而不修改配置。
awi integrate --project-root /path/to/workspace --dry-run --json

# 配置每个检测到的受支持客户端。
awi integrate --project-root /path/to/workspace

# 将操作限制为选定的客户端。
awi integrate --client codex,gemini,opencode,qwen,cline,zed,amazon-q,crush \
  --project-root /path/to/workspace
```

该命令是幂等的，并为每个客户端报告一个状态：`configured`、`already_configured`、`would_configure`、`needs_attention`、`not_installed`、`unsupported` 或 `failed`。每个受支持客户端的注册方法或配置目标是：

| 客户端 | 注册方法或目标 |
|---|---|
| Codex | 官方 `codex mcp add` CLI；持久化在 `~/.codex/config.toml` |
| Gemini CLI | 官方 `gemini mcp add --scope user` CLI；持久化在 `~/.gemini/settings.json` |
| Claude Code | 官方 `claude mcp add --scope user` CLI；持久化在 `~/.claude.json` |
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers) | `~/.copilot/mcp-config.json` |
| TraeCode | `<project>/.trae/mcp.json` |
| Zcode | `~/.zcode/cli/config.json` 中的 `mcp.servers` |
| Kimi Code | `$KIMI_CODE_HOME/mcp.json`，默认为 `~/.kimi-code/mcp.json` |
| [OpenCode](https://opencode.ai/docs/en/mcp-servers/) | `$OPENCODE_CONFIG`，或 `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.json` |
| [Pi](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent) | 通过 [`pi-mcp-adapter`](https://pi.dev/packages/pi-mcp-adapter) 的 `$PI_CODING_AGENT_DIR/mcp.json` |
| [Cursor](https://cursor.com/help/customization/mcp) | `~/.cursor/mcp.json` |
| [Windsurf](https://docs.windsurf.com/windsurf/cascade/mcp) | `~/.codeium/windsurf/mcp_config.json` |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/) | `~/.qwen/settings.json` |
| [Cline CLI](https://docs.cline.bot/mcp/mcp-overview) | `~/.cline/data/settings/cline_mcp_settings.json`（当前 CLI）；`~/.cline/mcp.json`（旧版仅 IDE 回退） |
| [Zed](https://zed.dev/docs/ai/mcp) | `${XDG_CONFIG_HOME:-~/.config}/zed/settings.json` 中的 `context_servers` |
| [Amazon Q Developer](https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-mcp-configuration.html) | `~/.aws/amazonq/mcp.json` |
| [Crush](https://www.mintlify.com/charmbracelet/crush/configuration/mcp) | `${XDG_CONFIG_HOME:-~/.config}/crush/crush.json` |

插件是具有原生 MCP 支持的客户端的可选包装。Pi 是个例外：其核心故意省略 MCP，因此需要扩展。新会话自动加载生成的用户级条目。TraeCode 项目级 MCP 必须在设置中启用一次。标记为不受信任的 Gemini 工作区报告为 `needs_attention`，因为 Gemini 会抑制所有 MCP 服务器，直到用户明确信任该工作区。

默认情况下，AWI 将当前二进制文件注册为 `awi --index-dir <absolute-path> mcp`，这对于本地可变索引是自包含的。基于快照的生产部署必须显式使用 `--server-command` 选择其包装器；重复的 `--server-arg` 选项可用于自定义启动器。

## 安全

`workspace_query` 仅接受一条只读 `SELECT` 或 `WITH` 语句，针对注册根目录下的显式输入。它强制执行超时、行和输出字节限制，同时禁用 DuckDB 扩展加载和外部访问。

搜索和检查响应是有界的。Agent 知识发现仅限于已知每客户端目录下的祖先 `AGENTS.md` 文件和 `SKILL.md` 清单。记忆发现仅限于映射到所选项目的精选文件；原始历史需要显式选择加入。AWI 永远不会索引整个主目录。敏感文件、生成的目录、超大内容和符号链接逃逸默认被排除。默认排除的目录是 `.git`、`.hg`、`.svn`、`.awi-index`、`node_modules`、`target`、`__pycache__`、`.pytest_cache`、`.mypy_cache`、`.ruff_cache`、`.ipynb_checkpoints`、`.venv`、`.idea`、`.vscode` 和 `.cache`，加上 Agent 创建的 `.codex-work` 和 `.worktrees`，以及 `.gitignore` 和 `.awiignore` 规则。`watch` 生产者对文件系统事件应用相同的排除，因此这些目录中的变动永远不会唤醒协调。搜索根过滤器接受索引根目录或包含索引根目录的现有父作用域。结构化查询根目录保持精确的允许列表条目。

MCP 审计日志记录是可选的。启用时，AWI 写入私有（`0600`）JSONL 记录，包含有界和凭据编辑的参数、调用者进程、标准化客户端名称、可选会话 ID、合成调用标记、持续时间、结果、错误详情、响应字节、文本/结构化有效载荷字节、命中/行计数以及单独的预览截断和限制压缩标志。Schema v5 保留了 v4 搜索/检查链接字段，并添加了 `client_name`、`session_id` 和 `synthetic`。使用 `AWI_MCP_CLIENT_NAME`、`AWI_MCP_SESSION_ID` 和 `AWI_MCP_SYNTHETIC` 设置显式元数据；MCP 初始化 `clientInfo.name` 覆盖进程推断。活动日志在 64 MiB 时轮换并保留一个以前的文件。项目采用报告使用 [`eligible_session_adoption`](session-adoption.md)，而不是所有项目会话作为分母。

## 评估

当前的开发专用评估报告：

- Recall@10: 1.0
- 搜索 P95: 6.83 ms
- 过期结果率: 0
- 真实 Agent 配对工具调用减少: 68.4%
- 真实 Agent 配对搜索减少: 54.0%
- 真实 Agent 配对墙钟时间减少: 41.3%

有关协议、类别细分、注意事项和可复现性工件，请参阅 [`evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md`](../evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md)。评估集仍是待双重人工审查的开发候选；它不是冻结的发布基准。这些值是诊断，不是正式的精确率/召回率声明。在两名不同的审查员独立标记查询集、裁决分歧并记录批准的标签和审查员身份之前，AWI 不得发布正式的 P/R、Inspect@K 或 MRR。

## 许可证

非商用许可证。个人、学术和非营利使用免费。任何形式的商用都需要事先获得书面同意。详见 [LICENSE](../LICENSE)。
