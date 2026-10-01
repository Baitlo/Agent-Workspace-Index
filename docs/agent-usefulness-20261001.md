# AWI 调用有效性优化 — 2026-10-01

已完成本机候选部署；不是公开版本发布，也不是经过双人评审的帮助率结论。

## 真实记录与改动

首次审计窗口自 2026-09-27 起，共 243 次调用：220 次 search、23 次 inspect，12 次工具错误、16 次零命中搜索。历史 synthetic 标记不完整，因此不把全部调用当成严格的生产流量分母。

Codex 项目会话中识别出 13 个会话、114 次真实 search 调用；95 次输出包含结构化预览，71 次在随后三次工具内出现 shell 检索，4 次直接引用返回路径。这些是诊断信号，不能据此推算帮助率。检查过 Codex、两类 Trae Bona 记忆目录及仓库产物；Zcode 无相关近期命中，Gemini 有旧会话，Claude 会话目录为空。

针对明确证据修改：

- **文本证据交付**：部分客户端只交付 `content[].text`。旧版只给搜索路径，并将 inspect 静默截到 2,000 字符。现在文本包含预览、文件代次、符号和 search_id；inspect 保留请求范围内的正文和 schema；query 不再静默只展示前三行。文本独立限制为 96 Ki 字符，超出会明确标记，并写入 `delivery.text_truncated`。
- **文件名意图**：通过已有 filename postings 找候选，再核对字面文件名/路径，给精确匹配加权，同时保留匹配文件之间的原有相关性。没有引入索引格式迁移或面向个别查询的规则。
- **范围**：允许已注册根下的现有子目录，并在候选截断前过滤旁支文件；保留适用指令和项目记忆的来源校验。`workspace_query` 仍要求精确注册根。
- **避免盲目重试**：明确文件名没有出现在返回结果时，提示检查范围、忽略规则或使用文件工具。实际的 `ms-swift/experiments/.../run_experiment.py` 被 `.gitignore:159` 的 `experiments/` 排除；排序不能找回未入索引的文件，不擅自绕过忽略规则。
- **审计 v6**：增加文本截断标记和命中路径哈希、代次，避免跨分片 file_id 冲突。原始记录和回放结果保持私有权限，不进入仓库。

## 固定索引回放

CPU worker 4368061，复制并经正常快照读取器校验的不可变分片：fast **62382**、NFS **53980**，合计约 **5.4 GiB**。两个版本使用相同索引及语义配置，无重新索引。114 次观测请求去重为 112 条；每条各跑两个版本两次，交换版本顺序。全部保持 `split=dev / candidate_pending_dual_review`。

| 指标 | 旧版 | 新版 |
|---|---:|---:|
| 每轮请求 | 112 | 112 |
| 每轮工具错误 | 3 | 1 |
| 成功但零命中 | 2 | 2 |
| 文本可见预览 / 返回非空预览 | 0 / 535 | 545 / 545 |
| P50，两轮（ms） | 292.5 / 287.0 | 298.8 / 302.4 |
| P95，两轮（ms） | 349.5 / 339.0 | 352.3 / 363.4 |
| 文本字节中位数 | 约 580 | 6,202 |
| 完整响应字节中位数 | 约 8,022 | 14,059 |

两次错误修复均为子目录范围；剩余一次是已经不存在的 context_path。三条请求的命中列表发生变化：两条由错误恢复为有结果，一条使明确指定的文件名排在前面；其余 109 条列表不变。逐项检查了这些变化，未将命中率当作任务正确率。

**545/545 表示证据交付完整，不是 Agent 帮助率 100%。** 文本变长是恢复实际证据的代价。P95 仍未达到 200ms，当前结果不支持宣称检索提速、Agent 调用减少 50% 或端到端帮助率已量化提升。

原有 16 例开发集在同一复制索引上：

| 指标 | 旧版 | 新版 |
|---|---:|---:|
| Recall@10 | 1.000 | 1.000 |
| MRR | 0.6448 | 0.6448 |
| nDCG@10 | 0.7330 | 0.7330 |
| P50 / P95（ms） | 315.5 / 356.4 | 339.0 / 378.9 |
| 陈旧命中率 | 0 | 0 |

开发集 release gate 仍为 false：双人评审未完成，P95 未达标。报告中的历史 baseline tool-call 字段不是本次 Agent A/B 结果。当前没有双盲端到端 A/B，真实帮助率需要任务相关性与证据采用标注。

## 验证与部署

- 107 项 Rust 测试通过，release Clippy 全 targets/features 零警告，fmt 和两个 Python 工具的 Ruff 检查通过。
- 测试覆盖只接收文本的客户端、超过旧 2K 限制的中文正文、查询第四行以后内容、显式截断、字面文件名、子目录 Top-K 前过滤、符号链接、分片所有权、结构化查询边界。
- 已替换 `~/.local/bin/awi` 并重启稳定 socket 的 reader。部署二进制 SHA256：`111e3e76a64eecb017141535cb33c8cc96951406ad3092d38bddf128b78ceb31`。
- 独立 MCP 经安装 wrapper 完成初始化、子目录 search 和 inspect：5 个命中均交付预览，search_id 可见，4,251 字符正文完整，审计 schema=6、synthetic=true。
- **当前 IDE MCP 接口返回 `IDE Command timeout`，无法确认该会话内的 GUI 通道。** 核验时两个旧 stdio adapter 仍持有旧二进制；它们已使用新版 reader 的检索，但文本格式在客户端重连后才更新。没有把独立协议验证写成 GUI 端到端通过。

共享证据：`/mnt/bn/baiweikang/qianchuan_distill/awi/evaluation/agent-usefulness-20261001/`；最终结果在 `replay-v3/`。`replay/` 与 `replay-v2/` 保留了被延迟检查否决的迭代。运行产物和 MCP 验证在 `/mnt/bn/baiweikang/qianchuan_distill/awi/runtime/agent-usefulness-20261001/`。

回滚：停止稳定 socket `~/.local/state/awi/codex-router/awi.sock` 对应的 reader，原子恢复运行目录下 `rollback/awi` 到 `~/.local/bin/awi`，再调用 `awi-mcp`。wrapper 备份也保留在 `rollback/awi-mcp`；本次未改生产者及快照格式。

## 后续复用

`scripts/audit-call-outcomes.py` 汇总真实调用及后续行为代理信号；`scripts/replay-call-outcomes.py` 固定分片并交替回放，可用 `--gold` 附加开发检索集。不要把后续 shell 调用自动标成检索失败，也不要把有预览自动标成证据采用。AWI Skill 及 usage-audit 参考已同步这些规则。
