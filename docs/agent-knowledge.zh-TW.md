# Agent 知識索引

[English](agent-knowledge.md) · [中文](agent-knowledge.zh.md) · **繁體中文** · [日本語](agent-knowledge.ja.md) · [한국어](agent-knowledge.ko.md) · [Русский](agent-knowledge.ru.md) · [Français](agent-knowledge.fr.md) · [Deutsch](agent-knowledge.de.md) · [Português](agent-knowledge.pt.md) · [Español](agent-knowledge.es.md) · [العربية](agent-knowledge.ar.md) · [Italiano](agent-knowledge.it.md) · [Ελληνικά](agent-knowledge.el.md) · [ไทย](agent-knowledge.th.md) · [Bahasa Melayu](agent-knowledge.ms.md)

跨 Agent 專案歷史單獨作為 `agent_memory` 處理；請參閱[跨 Agent 記憶索引](agent-memory.md)。

## 文件類型

| 檔案 | AWI 類型 | 結構化元數據 |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | 作用域目錄、優先級深度、標題、本地引用 |
| `SKILL.md` | `agent_skill` | YAML `name` 和 `description`、標題、本地引用 |

完整的受限 Markdown 文本保持可搜尋。`workspace_inspect` 返回結構化元數據以及正常的檔案元數據和內容摘錄。

## 發現

普通工作區協調會索引該工作區下的 `AGENTS.md` 和 `SKILL.md` 檔案，受 `.gitignore`、`.awiignore` 和 AWI 預設排除的約束。

首次執行安裝程式還會額外發現：

- 所選工作區上方祖先目錄中的 `AGENTS.md` 檔案；
- 支援的 Agent 用戶端的已知專案和使用者目錄下的 `SKILL.md` 清單。

每個外部文件都註冊為單檔案根目錄。AWI 不會索引整個主目錄或全域 Skill 套件的完整內容。`--skip-agent-knowledge` 停用此額外發現。

來自 Agent 文件的本地 Markdown 連結僅在其目標存在且保留在文件根目錄內時才被記錄。當被引用的檔案已被工作區根目錄覆蓋時，它們是可搜尋的；否則，引用路徑將作為元數據返回，以便用另一個檔案工具進行顯式檢查。

## 作用域和排名

在解析儲存庫指令時，將 `context_path` 傳遞給 `workspace_search`：

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI 排除其作用域不是上下文路徑祖先的 `AGENTS.md` 檔案。適用的檔案按作用域深度排名，因此最近的指令排在最前面。在此排名之後，內容相同的 Agent 文件會被折疊，這會抑制複製的 Skill 和 worktree 重複項，而不會丟失最近的適用指令。

未提供類型篩選器時，Agent 指令和 Skills 會與程式碼、資料共同參與檢索。只有需要縮小到某種 Agent 文件時才使用 `--kind agent_skill` 或 `--kind agent_instructions`。傳入 `context_path` 會套用指令作用域篩選和排序。

## 安全

Agent 文件仍受正常大小、UTF-8、符號連結和敏感檔案名檢查。其內容中高置信度的私鑰和提供者權杖模式會導致僅元數據結果，並帶有 `metadata_only_sensitive_content`；內容、預覽和解析的 Agent 元數據不會被儲存。

格式錯誤的 Skill frontmatter 被記錄為提取失敗，而新的 Markdown 內容仍會取代任何較舊的索引文本。這避免了僅在結構化元數據解析失敗時提供過期內容。

## 開發驗證

在目前 Bona 環境上的隔離安裝發現了 136 個規範的 Agent 文件，提取失敗為零。有針對性的冒煙查詢為 Wukong DAG/LogID 故障查詢選擇了 `wukong-dag-failure-debugger`，為 AWI 源上下文選擇了 Bona `AGENTS.md`。

在現有的 16 個用例開發檢索集上，添加這些 Agent 文件使 Recall@10 保持在 `1.0`，過期結果率為 `0`；MRR 從 `0.5975` 變為 `0.6027`，nDCG@10 從 `0.6922` 變為 `0.6969`，發布構建 P95 從 `8.07 ms` 變為 `25.88 ms`。延遲仍低於 `150 ms` 混合搜尋目標。此開發集仍待雙重審查，不是凍結的發布基準。
