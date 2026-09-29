# 跨 Agent 記憶索引

[English](agent-memory.md) · [中文](agent-memory.zh.md) · **繁體中文** · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## 用法

```bash
# 僅精選記憶和摘要。
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# 顯式包含匹配的原始聊天/會話歷史。
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# 僅搜尋與當前專案關聯的記憶。
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

安裝程式預設執行精選形式。使用 `--skip-agent-memory` 停用它，或使用 `--include-raw-memory` 選擇加入原始歷史。

## 來源

發現僅限於所選規範專案：

| Agent | 預設精選來源 | 原始選擇加入 |
|---|---|---|
| Trae | 使用者設定檔、匹配的專案記憶、主題摘要、會話摘要 | 除了這些摘要之外無 |
| Codex | `MEMORY.md`、`memory_summary.md`、匹配的 rollout 摘要 | 連結的原始 rollout JSONL 和 `raw_memories.md` |
| Zcode | 由 Zcode 執行時元數據映射到專案的記憶根目錄 | 匹配的 rollout/代理檔案 |
| Gemini CLI | 無 | 映射的專案歷史和聊天目錄 |
| Claude Code | 匹配的專案記憶目錄 | 標識專案的會話檔案 |

Argos/SRE 會話目錄被有意排除，因為它們需要 Argos 診斷工作流程而不是批量檔案索引。AWI 從不掃描整個主目錄。

## 元數據和排名

每個記憶命中包括：

- 來源 Agent；
- 層級：`user_profile`、`project_summary`、`topic_summary`、`session_summary`、`memory_note` 或 `raw_history`；
- 可選的 YAML frontmatter `name` 和 `description`；
- 可用時的規範工作區根目錄和提供者專案金鑰；
- 可派生時的會話 ID；
- 觀察時間和原始歷史旗標。

當提供 `context_path` 或專案根篩選器時，來自另一個專案的記憶會被拒絕。精確專案記憶排名領先於全域記憶。精確檔案名和 frontmatter `name` 匹配獲得最強的元數據提升；描述重疊提供較小的提升。標識符形狀的查詢會降低廣泛的 `MEMORY.md`/專案摘要的權重，除非摘要本身與實體完全匹配。對於廣泛查詢，正常的層級順序保持不變，精選摘要排名領先於原始歷史，而新近度只是一個小平局決勝因素。排名後，來自不同 Agent 的內容相同副本會被折疊。未提供類型篩選器時，記憶預設參與檢索；`--kind agent_memory` 僅用於將結果縮小為記憶。記憶文件使用專用的 Tantivy 索引，因此它們的詞彙不會改變普通代碼/數據 IDF 統計或排名。

## 解析和安全

Markdown 記憶被索引為有界文本。JSON/JSONL 記憶被規範化為人類相關欄位，如意圖、行動、結果、學到的事實、角色、訊息和內容；傳輸 ID 和內部摘要元數據從搜尋文件中省略。記憶 JSONL 不會發送到 DuckDB 分析。

正常的大小、UTF-8、忽略和敏感檔案名檢查仍然適用。高置信度的憑證或私鑰會導致整個記憶檔案僅元數據。除非顯式請求，否則原始歷史被停用，超大的原始檔案在配置的內容限制下保持僅元數據。

`awi memory` 持久化規範專案根目錄和原始歷史策略。每個生產者週期在發布前重新發現該專案的 Agent 來源，因此新的提供者專案目錄無需重新安裝 AWI 即可出現。先前註冊的根目錄在消失時也會被協調，防止已刪除的記憶保持可搜尋。追加的摘要 JSONL 在下次定期協調時刷新；追加密集的原始檔案不會觸發每次寫入的快照重建。

`awi status --json` 公開一個 `memory` 物件，其中包含註冊的專案/來源計數、活動檔案、最新記憶 generation 和相對 generation 滯後、檔案系統 `stale_files`/`missing_files`、最大來源滯後和最舊來源年齡。檔案系統過期和缺失計數是權威的新鮮度信號；僅低 generation 滯後並不能證明外部記憶是最新的。

## 開發驗證

一次隔離的 Bona 執行發現了 35 個與專案相關的根目錄，並從 Codex、Trae 和 Zcode 索引了 489 個精選檔案，提取失敗為零；Gemini 原始聊天保持排除。現有的 16 個用例代碼/數據檢索集在 Recall@10 `1.0`、MRR `0.6006`、nDCG@10 `0.6950` 和過期率 `0` 上保持逐位穩定。發布構建 P95 為 `24.06 ms`。此集合仍是 `candidate_pending_dual_review`，不是凍結的發布門檻。在兩名不同的審查員獨立標記查詢集並裁決分歧之前，不要報告正式的精確率/召回率、Inspect@K 或 MRR。
