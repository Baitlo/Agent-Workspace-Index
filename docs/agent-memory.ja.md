# クロスエージェントメモリインデックス

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · **日本語** · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## 使用方法

```bash
# 精選メモリと要約のみ。
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# 一致する生のチャット/セッション履歴を明示的に含める。
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# 現在のプロジェクトに関連付けられたメモリのみを検索。
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

インストーラーはデフォルトで精選フォームを実行します。無効にするには `--skip-agent-memory` を、生の履歴を選択するには `--include-raw-memory` を使用します。

## ソース

検出は選択された正規プロジェクトに制限されます：

| エージェント | デフォルトの精選ソース | 生のオプトイン |
|---|---|---|
| Trae | ユーザープロファイル、一致するプロジェクトメモリ、トピック要約、セッション要約 | これらの要約を超えるものなし |
| Codex | `MEMORY.md`、`memory_summary.md`、一致するロールアウト要約 | リンクされた生のロールアウト JSONL と `raw_memories.md` |
| Zcode | Zcode ランタイムメタデータによってプロジェクトにマップされたメモリルート | 一致するロールアウト/エージェントファイル |
| Gemini CLI | なし | マップされたプロジェクト履歴とチャットディレクトリ |
| Claude Code | 一致するプロジェクトメモリディレクトリ | プロジェクトを識別するセッションファイル |

Argos/SRE セッションディレクトリは、バルクファイルインデックスではなく Argos 診断ワークフローを必要とするため、意図的に除外されています。AWI はホームディレクトリ全体をスキャンしません。

## メタデータとランキング

すべてのメモリヒットには以下が含まれます：

- ソースエージェント；
- レイヤー：`user_profile`、`project_summary`、`topic_summary`、`session_summary`、`memory_note`、または `raw_history`；
- オプションの YAML frontmatter `name` と `description`；
- 利用可能な場合の正規ワークスペースルートとプロバイダープロジェクトキー；
- 導出可能な場合のセッション ID；
- 観察時間と生履歴フラグ。

`context_path` またはプロジェクトルートフィルターが提供されると、別のプロジェクトからのメモリは拒否されます。正確なプロジェクトメモリはグローバルメモリよりも上位にランク付けされます。正確なファイル名と frontmatter `name` の一致は最も強いメタデータブーストを受け取ります。説明の重複はより小さなブーストを提供します。識別子形状のクエリは、要約自体がエンティティと正確に一致しない限り、広範な `MEMORY.md`/プロジェクト要約を降格させます。広範なクエリでは通常のレイヤー順序は変更されず、精選要約は生履歴よりも上位にランク付けされ、新近性は小さなタイブレーカーにすぎません。ランキング後、異なるエージェントからの内容同一のコピーは折りたたまれます。種類フィルターを省略するとメモリもデフォルトで検索されます。`--kind agent_memory` は結果をメモリだけに絞り込む場合に使用します。メモリドキュメントは専用の Tantivy インデックスを使用するため、その語彙は通常のコード/データ IDF 統計やランキングを変更できません。

## 解析と安全性

Markdown メモリは制限付きテキストとしてインデックスされます。JSON/JSONL メモリは、意図、アクション、結果、学んだ事実、役割、メッセージ、コンテンツなどの人間に関連するフィールドに正規化されます。転送 ID と内部ダイジェストメタデータは検索ドキュメントから省略されます。メモリ JSONL は DuckDB プロファイリングに送信されません。

通常のサイズ、UTF-8、無視、機密ファイル名チェックは引き続き適用されます。高信頼度の資格情報または秘密鍵は、メモリファイル全体をメタデータのみにします。明示的に要求されない限り生履歴は無効であり、超大容量の生ファイルは構成されたコンテンツ制限の下でメタデータのみのままです。

`awi memory` は正規プロジェクトルートと生履歴ポリシーを永続化します。すべてのプロデューサーサイクルは公開前にそのプロジェクトのエージェントソースを再発見するため、AWI を再インストールせずに新しいプロバイダープロジェクトディレクトリが表示されます。以前に登録されたルートが消失した場合も調整され、削除されたメモリが検索可能なままになることを防ぎます。追加された要約 JSONL は次の定期調整で更新されます。追加中心の生ファイルは書き込みごとのスナップショット再構築をトリガーしません。

`awi status --json` は、登録されたプロジェクト/ソースカウント、アクティブファイル、最新のメモリ generation と相対的な generation ラグ、ファイルシステム `stale_files`/`missing_files`、最大ソースラグ、最古ソース年齢を含む `memory` オブジェクトを公開します。ファイルシステムの古いファイルと欠落ファイルのカウントは権威ある鮮度シグナルです。低い generation ラグだけでは外部メモリが最新であることを証明しません。

## 開発検証

分離された Bona 実行では、35 のプロジェクト関連ルートが発見され、Codex、Trae、Zcode から 489 の精選ファイルがインデックスされ、抽出失敗はゼロでした。Gemini の生チャットは除外されたままでした。既存の 16 ケースコード/データ検索セットは、Recall@10 `1.0`、MRR `0.6006`、nDCG@10 `0.6950`、古い率 `0` でビット単位で安定したままでした。リリースビルド P95 は `24.06 ms` でした。このセットはまだ `candidate_pending_dual_review` であり、凍結されたリリースゲートではありません。2 人の異なるレビュアーがクエリセットに独立してラベルを付け、意見の相違を裁定するまで、正式な精度/再現率、Inspect@K、または MRR を報告しないでください。
