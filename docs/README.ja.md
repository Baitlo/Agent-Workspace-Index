# AWI: Agent Workspace Index

**[English](../README.md)** · [中文](README.zh.md) · **日本語** · [한국어](README.ko.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Deutsch](README.de.md)

AWI は、コード、データ、エージェント運用知識のためのローカルファースト統合インデックスです。1 つの制限された CLI と Model Context Protocol (MCP) インターフェースを通じて、ソースコード、SQL、ドキュメント、ログ、JSON/JSONL、CSV/TSV、Parquet、`AGENTS.md`、エージェントスキル、およびプロジェクトスコープのクロスエージェントメモリをインデックスします。

1 つのコマンドで、そのインデックスを Codex、Claude Code、Gemini CLI、GitHub Copilot CLI、OpenCode、Qwen Code、Cline、Zed、Amazon Q Developer、Crush を含む 16 のコーディングエージェントハーネスに接続できます。

## 機能

- パス、テキスト、シンボル、データセットスキーマを対象とする Tantivy ハイブリッド検索。
- Rust、Python、Go 用の Tree-sitter シンボル抽出。
- 明示的に許可されたファイルに対する埋め込み読み取り専用 DuckDB クエリ。
- スコープ対応の `AGENTS.md` 検索と構造化 `SKILL.md` メタデータ。
- Trae、Codex、Zcode、Gemini、Claude にまたがるプロジェクト対応メモリ検出。
- 古い検索結果を拒否する SQLite カタログチェック。
- NFS セーフな `notify` および `reconcile` 更新パス。
- 不可変世代スナップショットとアトミックデーモンアクティベーション。
- MCP ツール: `workspace_search`、`workspace_inspect`、`workspace_query`。

検出、スコープ、ランキング、重複排除、安全セマンティクスについては [エージェント知識インデックス](agent-knowledge.md) を参照してください。メモリソースと生履歴境界については [クロスエージェントメモリインデックス](agent-memory.md) を参照してください。

## プレビルド Linux および macOS バイナリ

GitHub Releases は以下のネイティブバイナリを提供します：

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

1 つのコマンドで現在のリポジトリに AWI をインストールおよび設定します：

```bash
curl -fsSL \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh |
  bash -s -- --workspace "$PWD"
```

インストーラーは Linux アーキテクチャを検出し、リリースアーカイブと `SHA256SUMS` をダウンロードし、チェックサムを検証し、バイナリをインストールし、ワークスペースのトップレベル `AGENTS.md` の管理された AWI セクションを更新し、初期インデックスを構築し、エージェント知識と精選されたプロジェクトメモリを検出し、検出されたすべてのサポートされているエージェントクライアントを登録します。既存の `AGENTS.md` コンテンツは保持され、インストーラーの再実行は管理セクションを複製ではなく置き換えます。

実行前にダウンロードしたスクリプトを確認する必要がある環境の場合：

```bash
curl -fsSLO \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh
less install-release.sh
bash install-release.sh --workspace /absolute/path/to/your/repository
```

`--workspace` を省略すると `awi` バイナリのみがインストールされます。`--version v0.2.0` でリリースを固定し、`--bin-dir` で別の宛先を選択し、`--clients` でクライアントを制限するか、`--skip-agent-instructions` で管理命令ブロックを無効にします。Linux バイナリは Ubuntu 22.04 GitHub ホストランナーでネイティブにビルドされ、互換性のある glibc と `libstdc++` が必要です。macOS バイナリは macOS 15 ランナーで Apple Silicon および Intel 用にネイティブにビルドされます。

セマンティック検索はオプションのままです。追加で Python、LanceDB、`llama-cpp-python`、および互換性のある GGUF 埋め込みモデルが必要です。メンテナーは [リリースチェックリスト](releasing.md) に従う必要があります。

## エージェント支援インストール

[推奨インストールプロンプト](agent-install-prompt.md) をコーディングエージェントに提供するか、AWI チェックアウトからインストーラーを実行します：

```bash
bash scripts/install.sh --workspace /absolute/path/to/your/repository
```

初回実行では、`awi` をビルドおよびインストールし、ワークスペースの管理された AWI 命令を更新し、ワークスペース外にローカルインデックスを作成し、許可されたエージェントディレクトリで見つかった祖先 `AGENTS.md` ファイルと `SKILL.md` マニフェストをインデックスし、そのワークスペースに関連付けられた精選メモリをインデックスし、インストールされているエージェントクライアントを検出し、サポートされている各クライアントに AWI MCP サーバーを登録します。操作は冪等です。これらのステップを無効にするには、`--skip-agent-instructions`、`--skip-agent-knowledge`、または `--skip-agent-memory` を渡します。`--include-raw-memory` が指定されない限り、生のチャットは除外されたままです。Rust と Cargo はソースからビルドする場合にのみ必要です。

Pi が検出された場合、Pi には意図的に組み込み MCP クライアントがないため、インストーラーは固定された `pi-mcp-adapter@2.36.0` もインストールします。これはサードパーティの Pi パッケージです。`--skip-pi-adapter` を渡して個別に確認またはインストールするか、`AWI_PI_MCP_ADAPTER_SPEC` を設定して別の確認済みバージョンを選択します。カスタムバイナリ、インデックス、クライアントオプションについては `scripts/install.sh --help` を実行してください。

## ビルドとテスト

```bash
export CARGO_TARGET_DIR=/tmp/awi-target
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
python3 -m unittest tests/test_semantic_sidecar.py
python3 tests/test_audit_session_adoption.py
```

## 基本的な使用方法

```bash
# インデックスを構築または更新します。
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace --json

# 永続デーモンを実行します。
awi --index-dir /tmp/my-awi-index serve

# 検索と検査。
awi --index-dir /tmp/my-awi-index search "workspace query" --limit 10 --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file \
  --symbol exact_function_name --json

# 特定のワークスペースパスに適用される命令を解決します。
awi --index-dir /tmp/my-awi-index search "build and test rules" \
  --kind agent_instructions --context-path /path/to/workspace/src/lib.rs --json

# スタンドアロンのグローバルスキルマニフェストをインデックスして検索します。
awi --index-dir /tmp/my-awi-index reconcile ~/.agents/skills/example/SKILL.md --json
awi --index-dir /tmp/my-awi-index search "diagnose deployment failures" \
  --kind agent_skill --json

# このプロジェクトの精選メモリを検出して検索します。
awi --index-dir /tmp/my-awi-index memory --project-root /path/to/workspace
awi --index-dir /tmp/my-awi-index search "previous rollout decision" \
  --kind agent_memory --context-path /path/to/workspace --json

# MCP stdio アダプターを起動します。
awi --index-dir /tmp/my-awi-index mcp

# 後で検索分析のために実際の MCP ツール呼び出しを永続化します。
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

メモリは別の Tantivy インデックスを使用し、`--kind agent_memory` が要求された場合にのみ検索されるため、メモリの追加は通常のコード/データランキングを変更できません。MCP 検索は、1,000 文字プレビュー、安定した `file_id`、リクエストスコープの `search_id`、および正確なシンボルヒットの定義メタデータを備えたコンパクトな `compact_v3` 形式を使用します。デフォルトは 5 ヒットで、20 を超えるリクエストは 20 に圧縮されます。返された `search_id` とオプションの正確な `symbol` を `workspace_inspect` に渡して、検査をその検索にリンクし、定義に中心を置いた抜粋を表示します。並列の類義語検索ではなく、識別子豊富な 1 つのクエリから開始し、最初の結果セットに証拠がない場合にのみ拡張します。正確なパスがすでにわかっている場合は、ホストのファイルツールで直接読み取ります。検索スニペットは制限された保存ソースウィンドウから生成され、軽量ディレクトリ多様性再ランキングにより、1 つのアーティファクトフォルダが結果セットを埋め尽くすことを防ぎます。stdio サーバーは標準レガシー MCP `ping` をサポートし、初期化ベースのネゴシエーションをプロトコル `2025-11-25` に制限します。

### セマンティック検索

AWI は、MCP ツールインターフェースを変更せずに Harrier GGUF Q8 セマンティックレーンを追加できます。ドキュメント埋め込みは `reconcile`、`notify`、または初期 `semantic-build` 中に計算されます。通常の検索はクエリ埋め込みのみを計算します。SQLite は権威であり続け、ファイル生成が最新でないセマンティック候補は破棄されます。

専用の Python 環境を作成し、サイドカーを有効にします：

```bash
python3 -m venv ~/.cache/awi/semantic-venv
~/.cache/awi/semantic-venv/bin/pip install -r requirements-semantic.txt

export AWI_SEMANTIC_MODEL=/path/to/harrier-oss-v1-270M-Q8_0.gguf
export AWI_SEMANTIC_MODEL_SHA256=fe12f3583dbbb832def4cffeb46c0d0ab49a3288542d5cdbaeb1741315a01b87
export AWI_SEMANTIC_PYTHON="$HOME/.cache/awi/semantic-venv/bin/python"
export AWI_SEMANTIC_THREADS=16
export AWI_SEMANTIC_EMBED_WORKERS=1
```

既存のカタログの場合、すべての適格ファイルを一度事前計算します：

```bash
awi --index-dir /tmp/awi-writer semantic-build \
  --publish-dir /shared/awi-publication --json
```

中断されたバルクビルドは、LanceDB にコミットされた完全なファイル生成を再利用できます。失敗した試行中にワークスペースコンテンツが変更された場合は、最初に公開せずにカタログを調整してから再開します：

```bash
env -u AWI_SEMANTIC_MODEL \
  awi --index-dir /tmp/awi-writer reconcile /path/to/workspace --json
awi --index-dir /tmp/awi-writer semantic-build --resume \
  --publish-dir /shared/awi-publication --json
```

再開モードでも、すべてのソースを SQLite に対して再ハッシュし、正確な `(file_id, generation)` ペアのみを再利用します。シールは同じ完全カバレッジチェックを実行し、公開前に古いペアを削除します。

ビルドは、480 モデルトークンに制限されたトークナイザー対応、構造センシティブなチャンクを使用し、ファイルあたり最大 4 つのチャンクを保持し、Harrier クエリ命令をクエリにのみ適用し、埋め込みを L2 正規化します。ソース、テキスト、半構造化、および表形式コンテンツが適格です。機密またはメタデータのみのファイルとエージェントメモリは除外されます。抽出されたペイロードが空の非ゼロ適格ファイルは、ビルドと公開カバレッジが同一であることを保証するためにパスとタイプヘッダーベクトルを受け取ります。ゼロバイトファイルは除外されたままです。

バルクビルダーは `AWI_SEMANTIC_EMBED_WORKERS` を 1 より大きく設定できます。追加の GGUF モデルインスタンスはドキュメント埋め込み専用に遅延ロードされます。クエリサービングは単一モデルを維持します。AWI は `AWI_SEMANTIC_THREADS` をこれらのワーカーに分割するため、サイドカーが利用できる合計 CPU クォータに設定してください。

後続のプロデューサーサイクルは変更されたファイルのみを更新します。変更なしサイクルはカバレッジを検証し、ローカルセマンティックマニフェストのみを進めます。冗長な NFS スナップショットは公開されません。公開前に、AWI は現在のすべての適格 `(file_id, generation)` に対する LanceDB カバレッジを要求します。セマンティックデータベースとそのマニフェストは、SQLite および Tantivy と同じ不変スナップショットにコピーされます。リーダーは不一致の生成を拒否し、サイドカーが利用できない場合は字句検索にフォールバックします。`AWI_SEMANTIC_MODEL` を未設定にしてセマンティックレーンを無効にします。

クエリ時セマンティック検索は字句レーンと同時に実行されます。ベクトルオーバーフェッチは設定されたファイルあたりの最大チャンク数に制限され、冗長なチャンク候補を返さずにファイルレベルの Top-K カバレッジを保持します。永続リーダーはクエリを提供する前に 1 回の完全なハイブリッドウォームアップを実行します。サイドカーは Linux 親死亡シグナルを使用するため、リーダーを停止するとモデルも解放されます。

シールされた生成は IVF_FLAT インデックスを使用し、すべてのパーティションをプローブします。これにより、AWI の現在のコーパスサイズで積量子化の再現率損失を回避しながら、元の正規化 Q8 ベクトルと正確な Top-K 順序が保持されます。

NFS バックアップワークスペースの場合、ローカルストレージに可変インデックスを構築し、不変スナップショットを公開します：

```bash
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace \
  --publish-dir /shared/awi-publication --json

awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

クライアントホストに GGUF レイテンシ目標に十分な CPU クォータがない場合は、CPU ワーカーでスナップショットリーダーを実行し、再接続ストリームローカルトンネルでローカル MCP ソケットを保持します：

```bash
scripts/remote-reader-tunnel.sh \
  /tmp/local-awi.sock /tmp/remote-awi.sock <worker-ip> <ssh-port>
```

### 自動更新チェーン

手動調整なしで共有公開を最新に保つには、リーダーと並行して永続プロデューサーを実行します。プロデューサーはルートを調整し、コンテンツが変更された場合にのみ新しい不変スナップショットを公開します。スナップショット追跡リーダーは次のリクエストで各新しい生成にアトミックに切り替わります。これによりループがエンドツーエンドで閉じられます：ファイルを編集すると、リーダーは自動的にそれを反映します。

スナップショットのダウンロードとチェックサム検証はバックグラウンド更新ワーカーで実行されます。新しいスナップショットが具体化されている間、リクエストは最後の有効な生成に対して継続し、クエリパスでの NFS 更新一時停止を回避します。個々のクライアント切断または書き込み失敗は、共有デーモンを終了せずにログに記録されます。

ローカルディスグルートはリアルタイムで監視されるため（inotify）、編集はデバウンスウィンドウ内で公開されます。リモートルート（NFS など、`/proc/mounts` で検出）と `--interval-ms` ごとの定期的セーフティネットティックが残りを駆動します。ファイルシステムイベントはリモート書き込みに対して信頼できないためです。ウォッチャーが開始できない場合、プロデューサーは純粋な定期調整にクリーンに低下します。アクセスおよびメタデータのみのイベントは、自己トリガースキャンを防ぐために無視されます。追加中心の `.log`、`.jsonl`、`.ndjson`、`.csv`、`.tsv`、`.parquet` 更新は、すべての書き込みに対してスナップショットを再構築する代わりに定期パスに延期されます。すべてのサイクルは `awi memory` によって登録されたプロジェクトのソースも再検出します。新しいエージェントメモリルートは自動的にインデックスされ、消失した登録ルートは調整され、削除された証拠が提供されません。

```bash
# プロデューサー：ローカルルートをライブで監視し、各ルートを最大 5 秒ごとに調整し、
# 500 ミリ秒の編集バーストを結合し、変更時に自動公開し、3 世代を保持します。
awi --index-dir /tmp/awi-writer watch \
  --publish-dir /shared/awi-publication \
  --interval-ms 5000 --debounce-ms 500 --retain 3

# リーダー：公開に追従し、新しい生成を自動的にアクティブ化します。
awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

`watch` はデフォルトで登録された非メモリルートです。登録されたメモリプロジェクトはすべてのサイクルで個別に再検出されます。セットを制限するには `--root <path>` を 1 回以上渡します。保持プルーニングは各公開後に古い生成を削除し、ポインターが現在参照している生成を決して削除しないため、共有ディレクトリは制限なく増大できません。

Codex の場合、`scripts/awi-mcp-snapshot-wrapper.sh` は stdio アダプターを起動する前にローカルスナップショットデーモンを起動または再利用します。`AWI_SNAPSHOT_SOURCE` と `AWI_MCP_AUDIT_LOG` を設定し、ラッパーをグローバル MCP サーバーとして登録します。可変スナップショットデータは `AWI_RUNTIME_DIR` の下に保持され、Unix ソケットと起動ロックはデフォルトで `~/.local/state/awi-$UID` の下にあるため、一時キャッシュのクリーンアップでライブコントロールソケットをリンク解除できません。コールド具体化はデフォルトで最大 10 分待機します。`AWI_START_TIMEOUT_MS` でオーバーライドします。

### ワンコマンドエージェント統合

`awi integrate` はインストールされているエージェントクライアントを検出し、1 回のパスでサポートされているすべての MCP ホストに AWI を登録します：

```bash
# 設定を変更せずにプレビューします。
awi integrate --project-root /path/to/workspace --dry-run --json

# 検出されたすべてのサポートされているクライアントを設定します。
awi integrate --project-root /path/to/workspace

# 操作を選択したクライアントに制限します。
awi integrate --client codex,gemini,opencode,qwen,cline,zed,amazon-q,crush \
  --project-root /path/to/workspace
```

このコマンドは冪等で、クライアントごとに 1 つのステータスを報告します：`configured`、`already_configured`、`would_configure`、`needs_attention`、`not_installed`、`unsupported`、または `failed`。サポートされている各クライアントの登録方法または設定ターゲットは次のとおりです：

| クライアント | 登録方法またはターゲット |
|---|---|
| Codex | 公式 `codex mcp add` CLI；`~/.codex/config.toml` に永続化 |
| Gemini CLI | 公式 `gemini mcp add --scope user` CLI；`~/.gemini/settings.json` に永続化 |
| Claude Code | 公式 `claude mcp add --scope user` CLI；`~/.claude.json` に永続化 |
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers) | `~/.copilot/mcp-config.json` |
| TraeCode | `<project>/.trae/mcp.json` |
| Zcode | `~/.zcode/cli/config.json` の `mcp.servers` |
| Kimi Code | `$KIMI_CODE_HOME/mcp.json`、デフォルトは `~/.kimi-code/mcp.json` |
| [OpenCode](https://opencode.ai/docs/en/mcp-servers/) | `$OPENCODE_CONFIG`、または `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.json` |
| [Pi](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent) | [`pi-mcp-adapter`](https://pi.dev/packages/pi-mcp-adapter) 経由の `$PI_CODING_AGENT_DIR/mcp.json` |
| [Cursor](https://cursor.com/help/customization/mcp) | `~/.cursor/mcp.json` |
| [Windsurf](https://docs.windsurf.com/windsurf/cascade/mcp) | `~/.codeium/windsurf/mcp_config.json` |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/) | `~/.qwen/settings.json` |
| [Cline CLI](https://docs.cline.bot/mcp/mcp-overview) | `~/.cline/data/settings/cline_mcp_settings.json`（現在の CLI）；`~/.cline/mcp.json`（レガシー IDE 専用フォールバック） |
| [Zed](https://zed.dev/docs/ai/mcp) | `${XDG_CONFIG_HOME:-~/.config}/zed/settings.json` の `context_servers` |
| [Amazon Q Developer](https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-mcp-configuration.html) | `~/.aws/amazonq/mcp.json` |
| [Crush](https://www.mintlify.com/charmbracelet/crush/configuration/mcp) | `${XDG_CONFIG_HOME:-~/.config}/crush/crush.json` |

プラグインはネイティブ MCP サポートを持つクライアントのオプションパッケージです。Pi は例外です：そのコアは意図的に MCP を省略するため、拡張が必要です。新しいセッションは生成されたユーザーレベルエントリを自動的にロードします。TraeCode プロジェクトレベル MCP は設定で一度有効にする必要があります。信頼されていないとマークされた Gemini ワークスペースは、Gemini がユーザーがワークスペースを明示的に信頼するまで MCP サーバーを抑制するため、`needs_attention` と報告されます。

デフォルトでは、AWI は現在のバイナリを `awi --index-dir <absolute-path> mcp` として登録します。これはローカル可変インデックスに対して自己完結しています。スナップショットベースの本番デプロイメントは `--server-command` でラッパーを明示的に選択する必要があります。カスタムランチャーには繰り返し `--server-arg` オプションを使用できます。

## 安全性

`workspace_query` は、登録されたルートの下の明示的な入力に対して 1 つの読み取り専用 `SELECT` または `WITH` ステートメントのみを受け入れます。タイムアウト、行、出力バイト制限を強制し、DuckDB 拡張のロードと外部アクセスを無効にします。

検索および検査応答は制限されています。エージェント知識検出は、既知のクライアントごとのディレクトリの下にある祖先 `AGENTS.md` ファイルと `SKILL.md` マニフェストに限定されます。メモリ検出は、選択したプロジェクトにマップされた精選ファイルに限定されます。生履歴には明示的なオプトインが必要です。AWI はホームディレクトリ全体をインデックスしません。機密ファイル、生成されたディレクトリ、超大容量コンテンツ、シンボリックリンクエスケープはデフォルトで除外されます。デフォルトで除外されるディレクトリは `.git`、`.hg`、`.svn`、`.awi-index`、`node_modules`、`target`、`__pycache__`、`.pytest_cache`、`.mypy_cache`、`.ruff_cache`、`.ipynb_checkpoints`、`.venv`、`.idea`、`.vscode`、`.cache`、およびエージェント作成の `.codex-work` と `.worktrees`、ならびに `.gitignore` と `.awiignore` ルールです。`watch` プロデューサーはファイルシステムイベントに同じ除外を適用するため、これらのディレクトリのチャーンは調整を起動しません。検索ルートフィルターは、インデックスされたルートまたはインデックスされたルートを含む既存の親スコープを受け入れます。構造化クエリルートは正確な許可リストエントリのままです。

MCP 監査ログはオプションです。有効にすると、AWI は制限され資格情報が編集された引数、呼び出しプロセス、標準化されたクライアント名、オプションのセッション ID、合成呼び出しマーカー、期間、結果、エラーの詳細、応答バイト、テキスト/構造化ペイロードバイト、ヒット/行数、および個別のプレビュートランケーションと制限圧縮フラグを含むプライベート（`0600`）JSONL レコードを書き込みます。スキーマ v5 は v4 検索/検査リンクフィールドを保持し、`client_name`、`session_id`、`synthetic` を追加します。`AWI_MCP_CLIENT_NAME`、`AWI_MCP_SESSION_ID`、`AWI_MCP_SYNTHETIC` で明示的なメタデータを設定します。MCP 初期化 `clientInfo.name` はプロセス推論をオーバーライドします。アクティブログは 64 MiB でローテーションし、1 つの以前のファイルを保持します。プロジェクト採用レポートは、すべてのプロジェクトセッションではなく [`eligible_session_adoption`](session-adoption.md) を分母として使用します。

## 評価

現在の開発専用評価レポート：

- Recall@10: 1.0
- 検索 P95: 6.83 ms
- 古い結果率: 0
- 実際のエージェントペアツール呼び出し削減: 68.4%
- 実際のエージェントペア検索削減: 54.0%
- 実際のエージェントペア壁時間削減: 41.3%

プロトコル、カテゴリ内訳、注意事項、再現性アーティファクトについては [`evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md`](../evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md) を参照してください。評価セットは二重人間レビュー待ちの開発候補のままです。これは凍結されたリリースベンチマークではありません。これらの値は診断であり、正式な精度/再現率の主張ではありません。2 人の異なるレビュアーがクエリセットに独立してラベルを付け、意見の相違が裁定され、承認されたラベルとレビュアーの身元が記録されるまで、AWI は正式な P/R、Inspect@K、または MRR を公開してはなりません。

## ライセンス

MIT
