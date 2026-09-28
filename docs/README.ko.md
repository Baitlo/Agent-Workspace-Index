# AWI: Agent Workspace Index

**[English](../README.md)** · [中文](README.zh.md) · [日本語](README.ja.md) · **한국어** · [Русский](README.ru.md) · [Français](README.fr.md) · [Deutsch](README.de.md)

AWI는 코드, 데이터 및 에이전트 운영 지식을 위한 로컬 우선 통합 인덱스입니다. 하나의 제한된 CLI 및 Model Context Protocol (MCP) 인터페이스를 통해 소스 코드, SQL, 문서, 로그, JSON/JSONL, CSV/TSV, Parquet, `AGENTS.md`, 에이전트 스킬 및 프로젝트 범위의 크로스 에이전트 메모리를 인덱싱합니다.

하나의 명령으로 해당 인덱스를 Codex, Claude Code, Gemini CLI, GitHub Copilot CLI, OpenCode, Qwen Code, Cline, Zed, Amazon Q Developer 및 Crush를 포함한 16개의 코딩 에이전트 하니스에 연결할 수 있습니다.

## 기능

- 경로, 텍스트, 심볼 및 데이터셋 스키마를 대상으로 하는 Tantivy 하이브리드 검색.
- Rust, Python 및 Go용 Tree-sitter 심볼 추출.
- 명시적으로 허용된 파일에 대한 내장 읽기 전용 DuckDB 쿼리.
- 범위 인식 `AGENTS.md` 검색 및 구조화된 `SKILL.md` 메타데이터.
- Trae, Codex, Zcode, Gemini 및 Claude에 걸친 프로젝트 인식 메모리 검색.
- 오래된 검색 결과를 거부하는 SQLite 카탈로그 검사.
- NFS 안전 `notify` 및 `reconcile` 업데이트 경로.
- 원자적 데몬 활성화를 통한 불변 세대 스냅샷.
- MCP 도구: `workspace_search`, `workspace_inspect` 및 `workspace_query`.

검색, 범위, 순위 지정, 중복 제거 및 안전 의미에 대해서는 [에이전트 지식 인덱싱](agent-knowledge.md)을 참조하세요. 메모리 소스 및 원시 기록 경계에 대해서는 [크로스 에이전트 메모리 인덱싱](agent-memory.md)을 참조하세요.

## 사전 빌드된 Linux 및 macOS 바이너리

GitHub Releases는 다음의 네이티브 바이너리를 제공합니다:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

하나의 명령으로 현재 리포지토리에 AWI를 설치하고 구성합니다:

```bash
curl -fsSL \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh |
  bash -s -- --workspace "$PWD"
```

설치 프로그램은 Linux 아키텍처를 감지하고, 릴리스 아카이브와 `SHA256SUMS`를 다운로드하고, 체크섬을 확인하고, 바이너리를 설치하고, 작업 공간의 최상위 `AGENTS.md`에서 관리되는 AWI 섹션을 업데이트하고, 초기 인덱스를 구축하고, 에이전트 지식과 선별된 프로젝트 메모리를 검색하고, 감지된 모든 지원되는 에이전트 클라이언트를 등록합니다. 기존 `AGENTS.md` 내용은 보존되며 설치 프로그램을 다시 실행하면 관리 섹션이 복제되지 않고 대체됩니다.

실행 전에 다운로드한 스크립트를 검토해야 하는 환경의 경우:

```bash
curl -fsSLO \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh
less install-release.sh
bash install-release.sh --workspace /absolute/path/to/your/repository
```

`--workspace`를 생략하면 `awi` 바이너리만 설치됩니다. `--version v0.2.0`으로 릴리스를 고정하고, `--bin-dir`로 다른 대상을 선택하고, `--clients`로 클라이언트를 제한하거나, `--skip-agent-instructions`로 관리 명령 블록을 비활성화합니다. Linux 바이너리는 Ubuntu 22.04 GitHub 호스트 러너에서 네이티브로 빌드되며 호환되는 glibc 및 `libstdc++`가 필요합니다. macOS 바이너리는 macOS 15 러너에서 Apple Silicon 및 Intel용으로 네이티브로 빌드됩니다.

시맨틱 검색은 선택 사항으로 유지됩니다. 추가로 Python, LanceDB, `llama-cpp-python` 및 호환되는 GGUF 임베딩 모델이 필요합니다. 유지 관리자는 [릴리스 체크리스트](releasing.md)를 따라야 합니다.

## 에이전트 지원 설치

[권장 설치 프롬프트](agent-install-prompt.md)를 코딩 에이전트에 제공하거나 AWI 체크아웃에서 설치 프로그램을 실행합니다:

```bash
bash scripts/install.sh --workspace /absolute/path/to/your/repository
```

첫 실행은 `awi`를 빌드 및 설치하고, 작업 공간의 관리되는 AWI 지침을 업데이트하고, 작업 공간 외부에 로컬 인덱스를 생성하고, 허용된 에이전트 디렉토리에서 발견된 상위 `AGENTS.md` 파일 및 `SKILL.md` 매니페스트를 인덱싱하고, 해당 작업 공간과 연관된 선별된 메모리를 인덱싱하고, 설치된 에이전트 클라이언트를 감지하고, 지원되는 각 클라이언트에 AWI MCP 서버를 등록합니다. 작업은 멱등입니다. 이러한 단계를 비활성화하려면 `--skip-agent-instructions`, `--skip-agent-knowledge` 또는 `--skip-agent-memory`를 전달합니다. `--include-raw-memory`가 제공되지 않는 한 원시 채팅은 제외된 상태로 유지됩니다. Rust와 Cargo는 소스에서 빌드할 때만 필요합니다.

Pi가 감지되면 설치 프로그램은 고정된 `pi-mcp-adapter@2.36.0`도 설치합니다. Pi에는 의도적으로 내장 MCP 클라이언트가 없기 때문입니다. 이것은 타사 Pi 패키지입니다. `--skip-pi-adapter`를 전달하여 별도로 검토하거나 설치하거나 `AWI_PI_MCP_ADAPTER_SPEC`을 설정하여 검토된 다른 버전을 선택합니다. 사용자 지정 바이너리, 인덱스 및 클라이언트 옵션은 `scripts/install.sh --help`를 실행하세요.

## 빌드 및 테스트

```bash
export CARGO_TARGET_DIR=/tmp/awi-target
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
python3 -m unittest tests/test_semantic_sidecar.py
python3 tests/test_audit_session_adoption.py
```

## 기본 사용법

```bash
# 인덱스를 구축하거나 새로 고칩니다.
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace --json

# 영구 데몬을 실행합니다.
awi --index-dir /tmp/my-awi-index serve

# 검색 및 검사.
awi --index-dir /tmp/my-awi-index search "workspace query" --limit 10 --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file \
  --symbol exact_function_name --json

# 특정 작업 공간 경로에 적용되는 지침을 해결합니다.
awi --index-dir /tmp/my-awi-index search "build and test rules" \
  --kind agent_instructions --context-path /path/to/workspace/src/lib.rs --json

# 독립 실행형 전역 스킬 매니페스트를 인덱싱하고 검색합니다.
awi --index-dir /tmp/my-awi-index reconcile ~/.agents/skills/example/SKILL.md --json
awi --index-dir /tmp/my-awi-index search "diagnose deployment failures" \
  --kind agent_skill --json

# 이 프로젝트에 대한 선별된 메모리를 검색하고 검색합니다.
awi --index-dir /tmp/my-awi-index memory --project-root /path/to/workspace
awi --index-dir /tmp/my-awi-index search "previous rollout decision" \
  --kind agent_memory --context-path /path/to/workspace --json

# MCP stdio 어댑터를 시작합니다.
awi --index-dir /tmp/my-awi-index mcp

# 나중에 검색 분석을 위해 실제 MCP 도구 호출을 유지합니다.
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

메모리는 별도의 Tantivy 인덱스를 사용하며 `--kind agent_memory`가 요청될 때만 검색되므로 메모리를 추가해도 일반 코드/데이터 순위가 변경되지 않습니다. MCP 검색은 1,000자 미리보기, 안정적인 `file_id`, 요청 범위의 `search_id` 및 정확한 심볼 히트에 대한 정의 메타데이터가 있는 컴팩트한 `compact_v3` 형식을 사용합니다. 기본값은 5개 히트이며 20개를 초과하는 요청은 20개로 압축됩니다. 반환된 `search_id`와 선택적 정확한 `symbol`을 `workspace_inspect`에 전달하여 검사를 해당 검색과 연결하고 정의에 중심을 둔 발췌를 표시합니다. 병렬 동의어 검색 대신 식별자가 풍부한 하나의 쿼리로 시작하고 첫 번째 결과 세트에 증거가 없을 때만 확장합니다. 정확한 경로를 이미 알고 있는 경우 호스트의 파일 도구로 직접 읽습니다. 검색 스니펫은 제한된 저장 소스 창에서 생성되며 경량 디렉토리 다양성 재순위 지정으로 하나의 아티팩트 폴더가 결과 세트를 채우는 것을 방지합니다. stdio 서버는 표준 레거시 MCP `ping`을 지원하고 초기화 기반 협상을 프로토콜 `2025-11-25`로 제한합니다.

### 시맨틱 검색

AWI는 MCP 도구 인터페이스를 변경하지 않고 Harrier GGUF Q8 시맨틱 레인을 추가할 수 있습니다. 문서 임베딩은 `reconcile`, `notify` 또는 초기 `semantic-build` 중에 계산됩니다. 일반 검색은 쿼리 임베딩만 계산합니다. SQLite는 권한을 유지하며 파일 세대가 최신이 아닌 시맨틱 후보는 삭제됩니다.

전용 Python 환경을 만들고 사이드카를 활성화합니다:

```bash
python3 -m venv ~/.cache/awi/semantic-venv
~/.cache/awi/semantic-venv/bin/pip install -r requirements-semantic.txt

export AWI_SEMANTIC_MODEL=/path/to/harrier-oss-v1-270M-Q8_0.gguf
export AWI_SEMANTIC_MODEL_SHA256=fe12f3583dbbb832def4cffeb46c0d0ab49a3288542d5cdbaeb1741315a01b87
export AWI_SEMANTIC_PYTHON="$HOME/.cache/awi/semantic-venv/bin/python"
export AWI_SEMANTIC_THREADS=16
export AWI_SEMANTIC_EMBED_WORKERS=1
```

기존 카탈로그의 경우 모든 적격 파일을 한 번 사전 계산합니다:

```bash
awi --index-dir /tmp/awi-writer semantic-build \
  --publish-dir /shared/awi-publication --json
```

중단된 대량 빌드는 LanceDB에 커밋된 완전한 파일 세대를 재사용할 수 있습니다. 실패한 시도 중에 작업 공간 내용이 변경된 경우 먼저 게시하지 않고 카탈로그를 조정한 다음 재개합니다:

```bash
env -u AWI_SEMANTIC_MODEL \
  awi --index-dir /tmp/awi-writer reconcile /path/to/workspace --json
awi --index-dir /tmp/awi-writer semantic-build --resume \
  --publish-dir /shared/awi-publication --json
```

재개 모드는 여전히 모든 소스를 SQLite에 대해 다시 해시하고 정확한 `(file_id, generation)` 쌍만 재사용합니다. 봉인은 동일한 전체 커버리지 검사를 수행하고 게시 전에 오래된 쌍을 제거합니다.

빌드는 480 모델 토큰으로 제한된 토크나이저 인식, 구조 민감 청크를 사용하고 파일당 최대 4개의 청크를 유지하며 Harrier 쿼리 명령을 쿼리에만 적용하고 임베딩을 L2 정규화합니다. 소스, 텍스트, 반구조화 및 테이블 콘텐츠가 적격합니다. 민감하거나 메타데이터 전용 파일 및 에이전트 메모리는 제외됩니다. 추출된 페이로드가 비어 있는 0이 아닌 적격 파일은 빌드 및 게시 커버리지가 동일하게 유지되도록 경로 및 유형 헤더 벡터를 받습니다. 0바이트 파일은 제외된 상태로 유지됩니다.

대량 빌더는 `AWI_SEMANTIC_EMBED_WORKERS`를 1 이상으로 설정할 수 있습니다. 추가 GGUF 모델 인스턴스는 문서 임베딩 전용으로 지연 로드됩니다. 쿼리 서비스는 단일 모델을 유지합니다. AWI는 `AWI_SEMANTIC_THREADS`를 이러한 작업자에게 분할하므로 사이드카가 사용할 수 있는 총 CPU 할당량으로 설정하세요.

후속 생산자 주기는 변경된 파일만 업데이트합니다. 변경 없음 주기는 커버리지를 검증하고 로컬 시맨틱 매니페스트만 진행합니다. 중복된 NFS 스냅샷을 게시하지 않습니다. 게시 전에 AWI는 현재 모든 적격 `(file_id, generation)`에 대한 LanceDB 커버리지를 요구합니다. 시맨틱 데이터베이스와 해당 매니페스트는 SQLite 및 Tantivy와 동일한 불변 스냅샷에 복사됩니다. 리더는 일치하지 않는 세대를 거부하고 사이드카를 사용할 수 없을 때 어휘 검색으로 폴백합니다. `AWI_SEMANTIC_MODEL`을 설정 해제하여 시맨틱 레인을 비활성화합니다.

쿼리 시간 시맨틱 검색은 어휘 레인과 동시에 실행됩니다. 벡터 오버페치는 구성된 파일당 최대 청크 수에 의해 제한되어 중복 청크 후보를 반환하지 않고 파일 수준 Top-K 커버리지를 유지합니다. 영구 리더는 쿼리를 제공하기 전에 하나의 전체 하이브리드 워밍업을 실행합니다. 사이드카는 Linux 부모 사망 신호를 사용하므로 리더를 중지하면 모델도 해제됩니다.

봉인된 세대는 IVF_FLAT 인덱스를 사용하고 모든 파티션을 프로브합니다. 이것은 AWI의 현재 코퍼스 크기에서 곱 양자화의 재현율 손실을 피하면서 원래 정규화된 Q8 벡터와 정확한 Top-K 순서를 유지합니다.

NFS 지원 작업 공간의 경우 로컬 스토리지에 가변 인덱스를 구축하고 불변 스냅샷을 게시합니다:

```bash
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace \
  --publish-dir /shared/awi-publication --json

awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

클라이언트 호스트에 GGUF 지연 시간 목표를 위한 충분한 CPU 할당량이 없는 경우 CPU 작업자에서 스냅샷 리더를 실행하고 재연결 스트림 로컬 터널로 로컬 MCP 소켓을 유지합니다:

```bash
scripts/remote-reader-tunnel.sh \
  /tmp/local-awi.sock /tmp/remote-awi.sock <worker-ip> <ssh-port>
```

### 자동 업데이트 체인

수동 조정 없이 공유 게시를 최신 상태로 유지하려면 리더와 함께 영구 생산자를 실행합니다. 생산자는 루트를 조정하고 내용이 변경된 경우에만 새 불변 스냅샷을 게시합니다. 스냅샷 추적 리더는 다음 요청에서 각 새 세대로 원자적으로 전환합니다. 이것은 루프를 종단 간 닫습니다: 파일을 편집하면 리더가 자동으로 반영합니다.

스냅샷 다운로드 및 체크섬 검증은 백그라운드 새로 고침 작업자에서 실행됩니다. 새 스냅샷이 구체화되는 동안 요청은 마지막 유효한 세대에 대해 계속되어 쿼리 경로에서 NFS 새로 고침 일시 중지를 방지합니다. 개별 클라이언트 연결 끊기 또는 쓰기 실패는 공유 데몬을 종료하지 않고 기록됩니다.

로컬 디스크 루트는 실시간으로 모니터링되므로 (inotify) 편집은 디바운스 창 내에서 게시됩니다. 원격 루트 (NFS 및 유사, `/proc/mounts`를 통해 감지됨) 및 `--interval-ms`마다 주기적인 안전망 틱이 나머지를 구동합니다. 파일 시스템 이벤트는 원격 쓰기에 신뢰할 수 없기 때문입니다. 감시자가 시작할 수 없는 경우 생산자는 순수한 주기적 조정으로 깔끔하게 저하됩니다. 액세스 및 메타데이터 전용 이벤트는 자체 트리거 스캔을 방지하기 위해 무시됩니다. 추가 중심 `.log`, `.jsonl`, `.ndjson`, `.csv`, `.tsv` 및 `.parquet` 업데이트는 모든 쓰기에 대해 스냅샷을 재구축하는 대신 주기적 전달로 연기됩니다. 모든 주기는 `awi memory`로 등록된 프로젝트의 소스도 재검색합니다. 새 에이전트 메모리 루트는 자동으로 인덱싱되고 사라진 등록된 루트는 조정되므로 삭제된 증거가 제공되지 않습니다.

```bash
# 생산자: 로컬 루트를 실시간으로 감시하고 각 루트를 최대 5초마다 조정하고
# 500밀리초의 편집 버스트를 병합하고 변경 시 자동 게시하고 3세대를 유지합니다.
awi --index-dir /tmp/awi-writer watch \
  --publish-dir /shared/awi-publication \
  --interval-ms 5000 --debounce-ms 500 --retain 3

# 리더: 게시를 추적하고 새 세대를 자동으로 활성화합니다.
awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

`watch`는 기본적으로 등록된 비메모리 루트입니다. 등록된 메모리 프로젝트는 모든 주기에서 별도로 재검색됩니다. 세트를 제한하려면 `--root <path>`를 한 번 이상 전달합니다. 보존 프루닝은 각 게시 후 이전 세대를 제거하고 포인터가 현재 참조하는 세대를 절대 삭제하지 않으므로 공유 디렉토리가 무한정 커질 수 없습니다.

Codex의 경우 `scripts/awi-mcp-snapshot-wrapper.sh`는 stdio 어댑터를 시작하기 전에 로컬 스냅샷 데몬을 시작하거나 재사용합니다. `AWI_SNAPSHOT_SOURCE`와 `AWI_MCP_AUDIT_LOG`를 구성한 다음 래퍼를 전역 MCP 서버로 등록합니다. 가변 스냅샷 데이터는 `AWI_RUNTIME_DIR` 아래에 유지되고 Unix 소켓 및 시작 잠금은 기본적으로 `~/.local/state/awi-$UID` 아래에 있으므로 임시 캐시 정리로 라이브 컨트롤 소켓을 연결 해제할 수 없습니다. 콜드 구체화는 기본적으로 최대 10분 동안 기다립니다. `AWI_START_TIMEOUT_MS`로 재정의합니다.

### 원클릭 에이전트 통합

`awi integrate`는 설치된 에이전트 클라이언트를 감지하고 한 번의 패스로 지원되는 모든 MCP 호스트에 AWI를 등록합니다:

```bash
# 구성을 수정하지 않고 미리 봅니다.
awi integrate --project-root /path/to/workspace --dry-run --json

# 감지된 모든 지원되는 클라이언트를 구성합니다.
awi integrate --project-root /path/to/workspace

# 작업을 선택한 클라이언트로 제한합니다.
awi integrate --client codex,gemini,opencode,qwen,cline,zed,amazon-q,crush \
  --project-root /path/to/workspace
```

명령은 멱등이며 클라이언트당 하나의 상태를 보고합니다: `configured`, `already_configured`, `would_configure`, `needs_attention`, `not_installed`, `unsupported` 또는 `failed`. 지원되는 각 클라이언트의 등록 방법 또는 구성 대상은 다음과 같습니다:

| 클라이언트 | 등록 방법 또는 대상 |
|---|---|
| Codex | 공식 `codex mcp add` CLI; `~/.codex/config.toml`에 유지됨 |
| Gemini CLI | 공식 `gemini mcp add --scope user` CLI; `~/.gemini/settings.json`에 유지됨 |
| Claude Code | 공식 `claude mcp add --scope user` CLI; `~/.claude.json`에 유지됨 |
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers) | `~/.copilot/mcp-config.json` |
| TraeCode | `<project>/.trae/mcp.json` |
| Zcode | `~/.zcode/cli/config.json`의 `mcp.servers` |
| Kimi Code | `$KIMI_CODE_HOME/mcp.json`, 기본값은 `~/.kimi-code/mcp.json` |
| [OpenCode](https://opencode.ai/docs/en/mcp-servers/) | `$OPENCODE_CONFIG` 또는 `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.json` |
| [Pi](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent) | [`pi-mcp-adapter`](https://pi.dev/packages/pi-mcp-adapter)를 통한 `$PI_CODING_AGENT_DIR/mcp.json` |
| [Cursor](https://cursor.com/help/customization/mcp) | `~/.cursor/mcp.json` |
| [Windsurf](https://docs.windsurf.com/windsurf/cascade/mcp) | `~/.codeium/windsurf/mcp_config.json` |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/) | `~/.qwen/settings.json` |
| [Cline CLI](https://docs.cline.bot/mcp/mcp-overview) | `~/.cline/data/settings/cline_mcp_settings.json` (현재 CLI); `~/.cline/mcp.json` (레거시 IDE 전용 폴백) |
| [Zed](https://zed.dev/docs/ai/mcp) | `${XDG_CONFIG_HOME:-~/.config}/zed/settings.json`의 `context_servers` |
| [Amazon Q Developer](https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-mcp-configuration.html) | `~/.aws/amazonq/mcp.json` |
| [Crush](https://www.mintlify.com/charmbracelet/crush/configuration/mcp) | `${XDG_CONFIG_HOME:-~/.config}/crush/crush.json` |

플러그인은 네이티브 MCP 지원이 있는 클라이언트의 선택적 패키징입니다. Pi는 예외입니다: 코어가 의도적으로 MCP를 생략하므로 확장이 필요합니다. 새 세션은 생성된 사용자 수준 항목을 자동으로 로드합니다. TraeCode 프로젝트 수준 MCP는 설정에서 한 번 활성화해야 합니다. 신뢰할 수 없는 것으로 표시된 Gemini 작업 공간은 Gemini가 사용자가 작업 공간을 명시적으로 신뢰할 때까지 MCP 서버를 억제하기 때문에 `needs_attention`으로 보고됩니다.

기본적으로 AWI는 현재 바이너리를 `awi --index-dir <absolute-path> mcp`로 등록하며 로컬 가변 인덱스에 대해 자체 포함되어 있습니다. 스냅샷 기반 프로덕션 배포는 `--server-command`로 래퍼를 명시적으로 선택해야 합니다. 사용자 지정 런처에는 반복 `--server-arg` 옵션을 사용할 수 있습니다.

## 안전

`workspace_query`는 등록된 루트 아래의 명시적 입력에 대해 하나의 읽기 전용 `SELECT` 또는 `WITH` 문만 허용합니다. DuckDB 확장 로드 및 외부 액세스를 비활성화하면서 시간 초과, 행 및 출력 바이트 제한을 적용합니다.

검색 및 검사 응답은 제한됩니다. 에이전트 지식 검색은 알려진 클라이언트별 디렉토리 아래의 상위 `AGENTS.md` 파일 및 `SKILL.md` 매니페스트로 제한됩니다. 메모리 검색은 선택한 프로젝트에 매핑된 선별된 파일로 제한됩니다. 원시 기록은 명시적 옵트인이 필요합니다. AWI는 전체 홈 디렉토리를 인덱싱하지 않습니다. 민감한 파일, 생성된 디렉토리, 대용량 콘텐츠 및 심볼릭 링크 이스케이프는 기본적으로 제외됩니다. 기본 제외 디렉토리는 `.git`, `.hg`, `.svn`, `.awi-index`, `node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.ipynb_checkpoints`, `.venv`, `.idea`, `.vscode` 및 `.cache`이며 에이전트 생성 `.codex-work` 및 `.worktrees`와 함께 `.gitignore` 및 `.awiignore` 규칙도 포함합니다. `watch` 생산자는 파일 시스템 이벤트에 동일한 제외를 적용하므로 해당 디렉토리의 처닝은 조정을 깨우지 않습니다. 검색 루트 필터는 인덱싱된 루트 또는 인덱싱된 루트를 포함하는 기존 상위 범위를 허용합니다. 구조화된 쿼리 루트는 정확한 허용 목록 항목으로 유지됩니다.

MCP 감사 로깅은 선택 사항입니다. 활성화되면 AWI는 제한되고 자격 증명이 편집된 인수, 호출자 프로세스, 표준화된 클라이언트 이름, 선택적 세션 ID, 합성 호출 마커, 기간, 결과, 오류 세부 정보, 응답 바이트, 텍스트/구조화된 페이로드 바이트, 히트/행 수 및 별도의 미리보기 잘림 및 제한 압축 플래그를 포함하는 비공개 (`0600`) JSONL 레코드를 씁니다. 스키마 v5는 v4 검색/검사 연결 필드를 유지하고 `client_name`, `session_id` 및 `synthetic`을 추가합니다. `AWI_MCP_CLIENT_NAME`, `AWI_MCP_SESSION_ID` 및 `AWI_MCP_SYNTHETIC`으로 명시적 메타데이터를 설정합니다. MCP 초기화 `clientInfo.name`은 프로세스 추론을 재정의합니다. 활성 로그는 64 MiB에서 순환하고 이전 파일 하나를 유지합니다. 프로젝트 채택 보고서는 모든 프로젝트 세션이 아닌 [`eligible_session_adoption`](session-adoption.md)을 분모로 사용합니다.

## 평가

현재 개발 전용 평가 보고서:

- Recall@10: 1.0
- 검색 P95: 6.83 ms
- 오래된 결과율: 0
- 실제 에이전트 페어 도구 호출 감소: 68.4%
- 실제 에이전트 페어 검색 감소: 54.0%
- 실제 에이전트 페어 벽시간 감소: 41.3%

프로토콜, 카테고리 분석, 주의 사항 및 재현성 아티팩트는 [`evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md`](../evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md)를 참조하세요. 평가 세트는 이중 인간 검토 대기 중인 개발 후보로 남아 있습니다. 동결된 릴리스 벤치마크가 아닙니다. 이러한 값은 진단이며 공식적인 정밀도/재현율 주장이 아닙니다. 두 명의 다른 검토자가 쿼리 세트에 독립적으로 레이블을 지정하고 불일치가 판결되며 승인된 레이블과 검토자 신원이 기록될 때까지 AWI는 공식 P/R, Inspect@K 또는 MRR을 게시해서는 안 됩니다.

## 라이선스

비상업적 라이선스. 개인, 학술 및 비영리 사용은 무료. 어떤 형태의 상업적 사용도 사전 서면 동의가 필요. 자세한 내용은 [LICENSE](../LICENSE) 참조.
