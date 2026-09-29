# 跨 Agent 记忆索引

[English](agent-memory.md) · **中文** · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## 用法

```bash
# 仅精选记忆和摘要。
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# 显式包含匹配的原始聊天/会话历史。
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# 搜索所有已索引类型，包括与当前项目关联的记忆。
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --context-path /absolute/path/to/workspace \
  --json

# 将同一查询收窄为仅搜索记忆。
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

安装程序默认运行精选形式。使用 `--skip-agent-memory` 禁用它，或使用 `--include-raw-memory` 选择加入原始历史。

## 来源

发现仅限于所选规范项目：

| Agent | 默认精选来源 | 原始选择加入 |
|---|---|---|
| Trae | 用户配置文件、匹配的项目记忆、主题摘要、会话摘要 | 除了这些摘要之外无 |
| Codex | `MEMORY.md`、`memory_summary.md`、匹配的 rollout 摘要 | 链接的原始 rollout JSONL 和 `raw_memories.md` |
| Zcode | 由 Zcode 运行时元数据映射到项目的记忆根目录 | 匹配的 rollout/代理文件 |
| Gemini CLI | 无 | 映射的项目历史和聊天目录 |
| Claude Code | 匹配的项目记忆目录 | 标识项目的会话文件 |

Argos/SRE 会话目录被有意排除，因为它们需要 Argos 诊断工作流而不是批量文件索引。AWI 从不扫描整个主目录。

## 元数据和排名

每个记忆命中包括：

- 来源 Agent；
- 层级：`user_profile`、`project_summary`、`topic_summary`、`session_summary`、`memory_note` 或 `raw_history`；
- 可选的 YAML frontmatter `name` 和 `description`；
- 可用时的规范工作区根目录和提供商项目密钥；
- 可派生时的会话 ID；
- 观察时间和原始历史标志。

当提供 `context_path` 或项目根过滤器时，来自另一个项目的记忆会被拒绝。精确项目记忆排名领先于全局记忆。精确文件名和 frontmatter `name` 匹配获得最强的元数据提升；描述重叠提供较小的提升。标识符形状的查询会降低广泛的 `MEMORY.md`/项目摘要的权重，除非摘要本身与实体完全匹配。对于广泛查询，正常的层级顺序保持不变，精选摘要排名领先于原始历史，而新近度只是一个小平局决胜因素。排名后，来自不同 Agent 的内容相同副本会被折叠。不提供类型过滤器时，记忆默认参与检索；`--kind agent_memory` 仅用于将结果收窄为记忆。记忆文档使用专用的 Tantivy 索引，因此它们的词汇不会改变普通代码/数据的 IDF 统计。

## 解析和安全

Markdown 记忆被索引为有界文本。JSON/JSONL 记忆被规范化为人类相关字段，如意图、行动、结果、学到的事实、角色、消息和内容；传输 ID 和内部摘要元数据从搜索文档中省略。记忆 JSONL 不会发送到 DuckDB 分析。

正常的大小、UTF-8、忽略和敏感文件名检查仍然适用。高置信度的凭据或私钥会导致整个记忆文件仅元数据。除非显式请求，否则原始历史被禁用，超大的原始文件在配置的内容限制下保持仅元数据。

`awi memory` 持久化规范项目根目录和原始历史策略。每个生产者周期在发布前重新发现该项目的 Agent 来源，因此新的提供商项目目录无需重新安装 AWI 即可出现。先前注册的根目录在消失时也会被协调，防止已删除的记忆保持可搜索。追加的摘要 JSONL 在下次定期协调时刷新；追加密集的原始文件不会触发每次写入的快照重建。

`awi status --json` 公开一个 `memory` 对象，其中包含注册的项目/来源计数、活动文件、最新记忆 generation 和相对 generation 滞后、文件系统 `stale_files`/`missing_files`、最大来源滞后和最旧来源年龄。文件系统过期和缺失计数是权威的新鲜度信号；仅低 generation 滞后并不能证明外部记忆是最新的。

## 开发验证

一次隔离的 Bona 运行发现了 35 个与项目相关的根目录，并从 Codex、Trae 和 Zcode 索引了 489 个精选文件，提取失败为零；Gemini 原始聊天保持排除。现有的 16 个用例代码/数据检索集在 Recall@10 `1.0`、MRR `0.6006`、nDCG@10 `0.6950` 和过期率 `0` 上保持逐位稳定。发布构建 P95 为 `24.06 ms`。此集合仍是 `candidate_pending_dual_review`，不是冻结的发布门槛。在两名不同的审查员独立标记查询集并裁决分歧之前，不要报告正式的精确率/召回率、Inspect@K 或 MRR。
