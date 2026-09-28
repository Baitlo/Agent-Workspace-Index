# Agent 知识索引

AWI 在源代码和数据旁边索引 Agent 操作指令，而无需添加另一个 MCP 工具。

跨 Agent 项目历史单独作为 `agent_memory` 处理；请参阅[跨 Agent 记忆索引](agent-memory.md)。

## 文档类型

| 文件 | AWI 类型 | 结构化元数据 |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | 作用域目录、优先级深度、标题、本地引用 |
| `SKILL.md` | `agent_skill` | YAML `name` 和 `description`、标题、本地引用 |

完整的受限 Markdown 文本保持可搜索。`workspace_inspect` 返回结构化元数据以及正常的文件元数据和内容摘录。

## 发现

普通工作区协调会索引该工作区下的 `AGENTS.md` 和 `SKILL.md` 文件，受 `.gitignore`、`.awiignore` 和 AWI 默认排除的约束。

首次运行安装程序还会额外发现：

- 所选工作区上方祖先目录中的 `AGENTS.md` 文件；
- 支持的 Agent 客户端的已知项目和用户目录下的 `SKILL.md` 清单。

每个外部文档都注册为单文件根目录。AWI 不会索引整个主目录或全局 Skill 软件包的完整内容。`--skip-agent-knowledge` 禁用此额外发现。

来自 Agent 文档的本地 Markdown 链接仅在其目标存在且保留在文档根目录内时才被记录。当被引用的文件已被工作区根目录覆盖时，它们是可搜索的；否则，引用路径将作为元数据返回，以便用另一个文件工具进行显式检查。

## 作用域和排名

在解析存储库指令时，将 `context_path` 传递给 `workspace_search`：

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI 排除其作用域不是上下文路径祖先的 `AGENTS.md` 文件。适用的文件按作用域深度排名，因此最近的指令排在最前面。在此排名之后，内容相同的 Agent 文档会被折叠，这会抑制复制的 Skill 和 worktree 重复项，而不会丢失最近的适用指令。

使用 `--kind agent_skill` 进行以 Skill 为中心的检索。Agent 文档被排除在普通代码/数据搜索之外；专用 Agent 通道由 Agent 类型过滤器或 `context_path` 激活。

## 安全

Agent 文档仍受正常大小、UTF-8、符号链接和敏感文件名检查。其内容中高置信度的私钥和提供商令牌模式会导致仅元数据结果，并带有 `metadata_only_sensitive_content`；内容、预览和解析的 Agent 元数据不会被存储。

格式错误的 Skill frontmatter 被记录为提取失败，而新的 Markdown 内容仍会替换任何较旧的索引文本。这避免了仅在结构化元数据解析失败时提供过期内容。

## 开发验证

在当前 Bona 环境上的隔离安装发现了 136 个规范的 Agent 文档，提取失败为零。有针对性的冒烟查询为 Wukong DAG/LogID 故障查询选择了 `wukong-dag-failure-debugger`，为 AWI 源上下文选择了 Bona `AGENTS.md`。

在现有的 16 个用例开发检索集上，添加这些 Agent 文档使 Recall@10 保持在 `1.0`，过期结果率为 `0`；MRR 从 `0.5975` 变为 `0.6027`，nDCG@10 从 `0.6922` 变为 `0.6969`，发布构建 P95 从 `8.07 ms` 变为 `25.88 ms`。延迟仍低于 `150 ms` 混合搜索目标。此开发集仍待双重审查，不是冻结的发布基准。
