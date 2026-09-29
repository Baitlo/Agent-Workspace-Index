# エージェント知識インデックス

[English](agent-knowledge.md) · [中文](agent-knowledge.zh.md) · [繁體中文](agent-knowledge.zh-TW.md) · **日本語** · [한국어](agent-knowledge.ko.md) · [Русский](agent-knowledge.ru.md) · [Français](agent-knowledge.fr.md) · [Deutsch](agent-knowledge.de.md) · [Português](agent-knowledge.pt.md) · [Español](agent-knowledge.es.md) · [العربية](agent-knowledge.ar.md) · [Italiano](agent-knowledge.it.md) · [Ελληνικά](agent-knowledge.el.md) · [ไทย](agent-knowledge.th.md) · [Bahasa Melayu](agent-knowledge.ms.md)

クロスエージェントプロジェクト履歴は `agent_memory` として個別に処理されます。[クロスエージェントメモリインデックス](agent-memory.md)を参照してください。

## ドキュメントタイプ

| ファイル | AWI 種別 | 構造化メタデータ |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | スコープディレクトリ、優先度深度、見出し、ローカル参照 |
| `SKILL.md` | `agent_skill` | YAML `name` と `description`、見出し、ローカル参照 |

完全な制限付き Markdown テキストは検索可能なままです。`workspace_inspect` は、通常のファイルメタデータとコンテンツ抜粋とともに構造化メタデータを返します。

## 検出

通常のワークスペース調整は、`.gitignore`、`.awiignore`、および AWI のデフォルト除外に従って、そのワークスペースの下にある `AGENTS.md` および `SKILL.md` ファイルをインデックスします。

初回実行インストーラーはさらに以下を検出します：

- 選択したワークスペースの上の祖先ディレクトリにある `AGENTS.md` ファイル；
- サポートされているエージェントクライアントの既知のプロジェクトおよびユーザーディレクトリの下にある `SKILL.md` マニフェスト。

各外部ドキュメントは単一ファイルルートとして登録されます。AWI はホームディレクトリ全体やグローバルスキルパッケージの完全な内容をインデックスしません。`--skip-agent-knowledge` はこの追加検出を無効にします。

エージェントドキュメントからのローカル Markdown リンクは、ターゲットが存在しドキュメントルート内に留まる場合にのみ記録されます。参照されたファイルは、ワークスペースルートですでにカバーされている場合に検索可能です。それ以外の場合、参照パスはメタデータとして返され、別のファイルツールで明示的に検査できます。

## スコープとランキング

リポジトリ指示を解決するときは、`context_path` を `workspace_search` に渡します：

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI は、スコープがコンテキストパスの祖先ではない `AGENTS.md` ファイルを除外します。該当するファイルはスコープ深度でランク付けされるため、最も近い指示が最初に来ます。このランキングの後、内容が同一のエージェントドキュメントは折りたたまれ、コピーされたスキルとワークツリーの重複を抑制し、最も近い該当する指示を失うことなく抑制します。

種類フィルターを省略すると、Agent 命令と Skills はコードやデータとともに検索されます。`--kind agent_skill` または `--kind agent_instructions` は、特定の Agent 文書種別に絞り込む場合だけ使用します。`context_path` を渡すと、命令のスコープフィルターとランキングが適用されます。

## 安全性

エージェントドキュメントは、通常のサイズ、UTF-8、シンボリックリンク、機密ファイル名チェックの対象のままです。コンテンツ内の高信頼度の秘密鍵およびプロバイダートークンパターンは、`metadata_only_sensitive_content` を伴うメタデータのみの結果を引き起こします。コンテンツ、プレビュー、解析されたエージェントメタデータは保存されません。

不正な形式のスキル frontmatter は抽出失敗として記録されますが、新しい Markdown コンテンツは古いインデックス付きテキストを置き換えます。これにより、構造化メタデータの解析のみが失敗した場合に古いコンテンツが提供されることを回避します。

## 開発検証

現在の Bona 環境での分離インストールでは、136 の正規エージェントドキュメントが発見され、抽出失敗はゼロでした。ターゲットを絞ったスモーククエリは、Wukong DAG/LogID 障害クエリに対して `wukong-dag-failure-debugger` を最初に選択し、AWI ソースコンテキストに対して Bona `AGENTS.md` を選択しました。

既存の 16 ケース開発検索セットでは、これらのエージェントドキュメントを追加しても Recall@10 は `1.0`、古い結果率は `0` のままでした。MRR は `0.5975` から `0.6027` に、nDCG@10 は `0.6922` から `0.6969` に、リリースビルド P95 は `8.07 ms` から `25.88 ms` に変化しました。レイテンシは `150 ms` ハイブリッド検索目標を下回っています。この開発セットは二重レビュー待ちであり、凍結されたリリースベンチマークではありません。
