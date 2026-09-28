# 크로스 에이전트 메모리 인덱싱

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · **한국어** · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## 사용법

```bash
# 선별된 메모리 및 요약만.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# 일치하는 원시 채팅/세션 기록을 명시적으로 포함합니다.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# 현재 프로젝트와 연관된 메모리만 검색합니다.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

설치 프로그램은 기본적으로 선별된 형태를 실행합니다. 비활성화하려면 `--skip-agent-memory`를 사용하거나 원시 기록을 선택하려면 `--include-raw-memory`를 사용하세요.

## 소스

검색은 선택된 정식 프로젝트로 제한됩니다:

| 에이전트 | 기본 선별 소스 | 원시 옵트인 |
|---|---|---|
| Trae | 사용자 프로필, 일치하는 프로젝트 메모리, 주제 요약, 세션 요약 | 이러한 요약 외에는 없음 |
| Codex | `MEMORY.md`, `memory_summary.md`, 일치하는 롤아웃 요약 | 연결된 원시 롤아웃 JSONL 및 `raw_memories.md` |
| Zcode | Zcode 런타임 메타데이터에 의해 프로젝트에 매핑된 메모리 루트 | 일치하는 롤아웃/에이전트 파일 |
| Gemini CLI | 없음 | 매핑된 프로젝트 기록 및 채팅 디렉토리 |
| Claude Code | 일치하는 프로젝트 메모리 디렉토리 | 프로젝트를 식별하는 세션 파일 |

Argos/SRE 세션 디렉토리는 대량 파일 인덱싱이 아닌 Argos 진단 워크플로가 필요하기 때문에 의도적으로 제외됩니다. AWI는 전체 홈 디렉토리를 스캔하지 않습니다.

## 메타데이터 및 순위

모든 메모리 히트에는 다음이 포함됩니다:

- 소스 에이전트;
- 계층: `user_profile`, `project_summary`, `topic_summary`, `session_summary`, `memory_note` 또는 `raw_history`;
- 선택적 YAML frontmatter `name` 및 `description`;
- 사용 가능한 경우 정식 작업 공간 루트 및 공급자 프로젝트 키;
- 파생 가능한 경우 세션 ID;
- 관찰 시간 및 원시 기록 플래그.

`context_path` 또는 프로젝트 루트 필터가 제공되면 다른 프로젝트의 메모리가 거부됩니다. 정확한 프로젝트 메모리는 전역 메모리보다 앞서 순위가 매겨집니다. 정확한 파일 이름 및 frontmatter `name` 일치는 가장 강력한 메타데이터 부스트를 받습니다. 설명 겹침은 더 작은 부스트를 제공합니다. 식별자 모양의 쿼리는 요약 자체가 엔티티와 정확히 일치하지 않는 한 광범위한 `MEMORY.md`/프로젝트 요약을 강등시킵니다. 광범위한 쿼리의 경우 일반 계층 순서는 변경되지 않으며 선별된 요약은 원시 기록보다 앞서 순위가 매겨지고 최신성은 작은 타이브레이커일 뿐입니다. 순위 지정 후 다른 에이전트의 콘텐츠 동일 복사본은 축소됩니다. 메모리 검색은 `--kind agent_memory`로 활성화됩니다. 메모리 문서는 전용 Tantivy 인덱스를 사용하므로 해당 어휘는 일반 코드/데이터 IDF 통계 또는 순위를 변경할 수 없습니다.

## 구문 분석 및 안전

Markdown 메모리는 제한된 텍스트로 인덱싱됩니다. JSON/JSONL 메모리는 의도, 작업, 결과, 학습된 사실, 역할, 메시지 및 콘텐츠와 같은 인간 관련 필드로 정규화됩니다. 전송 ID 및 내부 다이제스트 메타데이터는 검색 문서에서 생략됩니다. 메모리 JSONL은 DuckDB 프로파일링으로 전송되지 않습니다.

일반 크기, UTF-8, 무시 및 민감한 파일 이름 검사는 여전히 적용됩니다. 고신뢰도 자격 증명 또는 개인 키는 전체 메모리 파일을 메타데이터 전용으로 만듭니다. 명시적으로 요청되지 않는 한 원시 기록은 비활성화되며, 대용량 원시 파일은 구성된 콘텐츠 제한 내에서 메타데이터 전용으로 유지됩니다.

`awi memory`는 정식 프로젝트 루트 및 원시 기록 정책을 유지합니다. 모든 생산자 주기는 게시 전에 해당 프로젝트의 에이전트 소스를 재검색하므로 AWI를 다시 설치하지 않고도 새 공급자 프로젝트 디렉토리가 나타납니다. 이전에 등록된 루트가 사라지면 조정되어 삭제된 메모리가 검색 가능한 상태로 남아 있는 것을 방지합니다. 추가된 요약 JSONL은 다음 주기적 조정에서 새로 고쳐집니다. 추가 중심 원시 파일은 쓰기별 스냅샷 재구축을 트리거하지 않습니다.

`awi status --json`은 등록된 프로젝트/소스 수, 활성 파일, 최신 메모리 세대 및 상대적 세대 지연, 파일 시스템 `stale_files`/`missing_files`, 최대 소스 지연 및 가장 오래된 소스 연령을 포함하는 `memory` 객체를 노출합니다. 파일 시스템 오래된 파일 및 누락된 파일 수는 권위 있는 최신성 신호입니다. 낮은 세대 지연만으로는 외부 메모리가 최신임을 증명하지 않습니다.

## 개발 검증

격리된 Bona 실행은 35개의 프로젝트 관련 루트를 발견하고 Codex, Trae 및 Zcode에서 489개의 선별된 파일을 인덱싱했으며 추출 실패는 0건이었습니다. Gemini 원시 채팅은 제외된 상태로 유지되었습니다. 기존 16개 사례 코드/데이터 검색 세트는 Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950` 및 오래된 비율 `0`에서 비트 단위로 안정적으로 유지되었습니다. 릴리스 빌드 P95는 `24.06 ms`였습니다. 이 세트는 여전히 `candidate_pending_dual_review`이며 동결된 릴리스 게이트가 아닙니다. 두 명의 다른 검토자가 쿼리 세트에 독립적으로 레이블을 지정하고 불일치를 판결할 때까지 공식적인 정밀도/재현율, Inspect@K 또는 MRR을 보고하지 마세요.
