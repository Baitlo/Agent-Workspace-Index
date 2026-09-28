# AWI: Agent Workspace Index

**[English](../README.md)** · [中文](README.zh.md) · [日本語](README.ja.md) · [한국어](README.ko.md) · [Русский](README.ru.md) · [Français](README.fr.md) · **Deutsch**

AWI ist ein lokaler, vereinheitlichter Index für Code, Daten und operative Agentenkenntnisse. Er indiziert Quellcode, SQL, Dokumente, Protokolle, JSON/JSONL, CSV/TSV, Parquet, `AGENTS.md`, Agentenfähigkeiten und projektbezogenes agentenübergreifendes Gedächtnis über eine begrenzte CLI- und Model Context Protocol (MCP)-Oberfläche.

Ein einziger Befehl verbindet diesen Index mit 16 Coding-Agent-Harnesses, darunter Codex, Claude Code, Gemini CLI, GitHub Copilot CLI, OpenCode, Qwen Code, Cline, Zed, Amazon Q Developer und Crush.

## Funktionen

- Tantivy-Hybridabruf über Pfade, Text, Symbole und Datensatzschemata.
- Tree-sitter-Symbolsextraktion für Rust, Python und Go.
- Eingebettete, schreibgeschützte DuckDB-Abfragen über explizit zugelassene Dateien.
- Bereichsbewusster `AGENTS.md`-Abruf und strukturierte `SKILL.md`-Metadaten.
- Projektbewusste Gedächtnisentdeckung über Trae, Codex, Zcode, Gemini und Claude.
- SQLite-Katalogprüfungen, die veraltete Suchergebnisse ablehnen.
- NFS-sichere `notify`- und `reconcile`-Aktualisierungspfade.
- Unveränderliche Generationssnapshots mit atomarer Daemon-Aktivierung.
- MCP-Tools: `workspace_search`, `workspace_inspect` und `workspace_query`.

Siehe [Agentenwissensindizierung](agent-knowledge.md) für Entdeckung, Bereich, Ranking, Deduplizierung und Sicherheitssemantik. Siehe [Agentenübergreifende Gedächtnisindizierung](agent-memory.md) für Gedächtnisquellen und die Rohhistoriegrenze.

## Vorgefertigte Linux- und macOS-Binärdateien

GitHub Releases bietet native Binärdateien für:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Installieren und konfigurieren Sie AWI für das aktuelle Repository mit einem Befehl:

```bash
curl -fsSL \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh |
  bash -s -- --workspace "$PWD"
```

Das Installationsprogramm erkennt die Linux-Architektur, lädt das Release-Archiv und `SHA256SUMS` herunter, verifiziert die Prüfsumme, installiert die Binärdatei, aktualisiert einen verwalteten AWI-Abschnitt in der obersten `AGENTS.md` des Arbeitsbereichs, erstellt den anfänglichen Index, entdeckt Agentenwissen und kuratiertes Projektgedächtnis und registriert jeden erkannten unterstützten Agentenclient. Bestehender `AGENTS.md`-Inhalt bleibt erhalten, und eine erneute Ausführung des Installationsprogramms ersetzt den verwalteten Abschnitt, anstatt ihn zu duplizieren.

Für Umgebungen, die eine Überprüfung eines heruntergeladenen Skripts vor der Ausführung erfordern:

```bash
curl -fsSLO \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh
less install-release.sh
bash install-release.sh --workspace /absolute/path/to/your/repository
```

Lassen Sie `--workspace` weg, um nur die `awi`-Binärdatei zu installieren. Fixieren Sie ein Release mit `--version v0.2.0`, wählen Sie ein anderes Ziel mit `--bin-dir`, schränken Sie Clients mit `--clients` ein oder deaktivieren Sie den verwalteten Anweisungsblock mit `--skip-agent-instructions`. Die Linux-Binärdateien werden nativ auf Ubuntu 22.04 GitHub-gehosteten Runnern erstellt und erfordern eine kompatible glibc und `libstdc++`. Die macOS-Binärdateien werden nativ auf macOS 15 Runnern für Apple Silicon und Intel erstellt.

Der semantische Abruf bleibt optional. Er erfordert zusätzlich Python, LanceDB, `llama-cpp-python` und ein kompatibles GGUF-Einbettungsmodell. Maintainer sollten der [Release-Checkliste](releasing.md) folgen.

## Agentenunterstützte Installation

Geben Sie Ihrem Coding-Agenten die [empfohlene Installationsaufforderung](agent-install-prompt.md), oder führen Sie das Installationsprogramm von einem AWI-Checkout aus:

```bash
bash scripts/install.sh --workspace /absolute/path/to/your/repository
```

Die erste Ausführung erstellt und installiert `awi`, aktualisiert die verwalteten AWI-Anweisungen des Arbeitsbereichs, erstellt einen lokalen Index außerhalb des Arbeitsbereichs, indiziert Vorfahren-`AGENTS.md`-Dateien und `SKILL.md`-Manifeste, die in zugelassenen Agentenverzeichnissen gefunden wurden, indiziert kuratiertes Gedächtnis, das mit diesem Arbeitsbereich verbunden ist, erkennt installierte Agentenclients und registriert den AWI-MCP-Server bei jedem unterstützten Client. Der Vorgang ist idempotent. Übergeben Sie `--skip-agent-instructions`, `--skip-agent-knowledge` oder `--skip-agent-memory`, um diese Schritte zu deaktivieren. Rohchats bleiben ausgeschlossen, es sei denn, `--include-raw-memory` wird angegeben. Rust und Cargo werden nur beim Erstellen aus dem Quellcode benötigt.

Wenn Pi erkannt wird, installiert das Installationsprogramm auch das fixierte `pi-mcp-adapter@2.36.0`, da Pi absichtlich keinen integrierten MCP-Client hat. Dies ist ein Drittanbieter-Pi-Paket; übergeben Sie `--skip-pi-adapter`, um es separat zu überprüfen oder zu installieren, oder setzen Sie `AWI_PI_MCP_ADAPTER_SPEC`, um eine andere überprüfte Version auszuwählen. Führen Sie `scripts/install.sh --help` für benutzerdefinierte Binär-, Index- und Clientoptionen aus.

## Erstellen und Testen

```bash
export CARGO_TARGET_DIR=/tmp/awi-target
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
python3 -m unittest tests/test_semantic_sidecar.py
python3 tests/test_audit_session_adoption.py
```

## Grundlegende Verwendung

```bash
# Index erstellen oder aktualisieren.
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace --json

# Persistente Daemon ausführen.
awi --index-dir /tmp/my-awi-index serve

# Suchen und inspizieren.
awi --index-dir /tmp/my-awi-index search "workspace query" --limit 10 --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file \
  --symbol exact_function_name --json

# Auflösen von Anweisungen, die für einen konkreten Arbeitsbereichspfad gelten.
awi --index-dir /tmp/my-awi-index search "build and test rules" \
  --kind agent_instructions --context-path /path/to/workspace/src/lib.rs --json

# Indizieren und Durchsuchen eines eigenständigen globalen Skill-Manifests.
awi --index-dir /tmp/my-awi-index reconcile ~/.agents/skills/example/SKILL.md --json
awi --index-dir /tmp/my-awi-index search "diagnose deployment failures" \
  --kind agent_skill --json

# Kuratiertes Gedächtnis für dieses Projekt entdecken und durchsuchen.
awi --index-dir /tmp/my-awi-index memory --project-root /path/to/workspace
awi --index-dir /tmp/my-awi-index search "previous rollout decision" \
  --kind agent_memory --context-path /path/to/workspace --json

# MCP-Stdio-Adapter starten.
awi --index-dir /tmp/my-awi-index mcp

# Tatsächliche MCP-Toolaufrufe für spätere Abrufanalysen persistieren.
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

Das Gedächtnis verwendet einen separaten Tantivy-Index und wird nur durchsucht, wenn `--kind agent_memory` angefordert wird, sodass das Hinzufügen von Gedächtnis das gewöhnliche Code-/Daten-Ranking nicht ändern kann. Die MCP-Suche verwendet die kompakte `compact_v3`-Form mit einer 1.000-Zeichen-Vorschau, einer stabilen `file_id`, einer anfragebezogenen `search_id` und Definitionsmetadaten für exakte Symboltreffer. Sie gibt standardmäßig 5 Treffer zurück und komprimiert Anfragen über 20 auf 20. Übergeben Sie die zurückgegebene `search_id` und das optionale exakte `symbol` an `workspace_inspect`, um die Inspektion mit ihrem Abruf zu verknüpfen und den Auszug auf die Definition zu zentrieren. Beginnen Sie mit einer identifierreichen Abfrage anstelle von parallelen Fast-Synonym-Suchen und erweitern Sie nur, wenn dem ersten Ergebnissatz Beweise fehlen. Wenn der genaue Pfad bereits bekannt ist, lesen Sie ihn direkt mit den Dateitools des Hosts. Suchausschnitte werden aus begrenzten gespeicherten Quellfenstern generiert, und ein leichtes Verzeichnis-Diversitäts-Reranking verhindert, dass ein Artefaktordner den Ergebnissatz füllt. Der Stdio-Server unterstützt den standardmäßigen Legacy-MCP-`ping` und begrenzt die initialisierungsbasierte Aushandlung auf Protokoll `2025-11-25`.

### Semantischer Abruf

AWI kann eine Harrier-GGUF-Q8-semantische Spur hinzufügen, ohne die MCP-Tool-Oberfläche zu ändern. Dokumenteneinbettungen werden während `reconcile`, `notify` oder einem anfänglichen `semantic-build` berechnet; normale Suchen berechnen nur die Abfrageeinbettung. SQLite bleibt maßgeblich, und semantische Kandidaten, deren Dateigeneration nicht aktuell ist, werden verworfen.

Erstellen Sie eine dedizierte Python-Umgebung und aktivieren Sie den Sidecar:

```bash
python3 -m venv ~/.cache/awi/semantic-venv
~/.cache/awi/semantic-venv/bin/pip install -r requirements-semantic.txt

export AWI_SEMANTIC_MODEL=/path/to/harrier-oss-v1-270M-Q8_0.gguf
export AWI_SEMANTIC_MODEL_SHA256=fe12f3583dbbb832def4cffeb46c0d0ab49a3288542d5cdbaeb1741315a01b87
export AWI_SEMANTIC_PYTHON="$HOME/.cache/awi/semantic-venv/bin/python"
export AWI_SEMANTIC_THREADS=16
export AWI_SEMANTIC_EMBED_WORKERS=1
```

Für einen bestehenden Katalog berechnen Sie jede förderfähige Datei einmal vorab:

```bash
awi --index-dir /tmp/awi-writer semantic-build \
  --publish-dir /shared/awi-publication --json
```

Ein unterbrochener Massenbuild kann vollständige Dateigenerationen wiederverwenden, die bereits an LanceDB übergeben wurden. Wenn sich der Arbeitsbereichsinhalt während des fehlgeschlagenen Versuchs geändert hat, stimmen Sie den Katalog zuerst ohne Veröffentlichung ab und setzen Sie dann fort:

```bash
env -u AWI_SEMANTIC_MODEL \
  awi --index-dir /tmp/awi-writer reconcile /path/to/workspace --json
awi --index-dir /tmp/awi-writer semantic-build --resume \
  --publish-dir /shared/awi-publication --json
```

Der Fortsetzungsmodus hasht jede Quelle immer noch gegen SQLite und verwendet nur exakte `(file_id, generation)`-Paare wieder. Die Versiegelung führt dieselbe vollständige Abdeckungsprüfung durch und entfernt veraltete Paare vor der Veröffentlichung.

Der Build verwendet tokenizerbewusste, struktursensitive Chunks, die auf 480 Modelltoken begrenzt sind, behält höchstens vier Chunks pro Datei, wendet die Harrier-Abfrageanweisung nur auf Abfragen an und L2-normalisiert Einbettungen. Quell-, Text-, halbstrukturierte und tabellarische Inhalte sind förderfähig; sensible oder nur Metadaten enthaltende Dateien und Agentengedächtnis sind ausgeschlossen. Eine nicht-nullwertige förderfähige Datei, deren extrahierte Nutzlast leer ist, erhält einen Pfad-und-Typ-Header-Vektor, sodass Build- und Veröffentlichungsabdeckung identisch bleiben; null Byte große Dateien bleiben ausgeschlossen.

Massenbuilder können `AWI_SEMANTIC_EMBED_WORKERS` über eins setzen. Zusätzliche GGUF-Modellinstanzen werden nur für die Dokumenteneinbettung träge geladen; der Abfragedienst behält ein einzelnes Modell. AWI teilt `AWI_SEMANTIC_THREADS` auf diese Worker auf, also setzen Sie es auf das gesamte CPU-Kontingent, das dem Sidecar zur Verfügung steht.

Nachfolgende Produzentenzyklen aktualisieren nur geänderte Dateien. Ein Zyklus ohne Änderungen validiert die Abdeckung und schreitet nur im lokalen semantischen Manifest voran; er veröffentlicht keinen redundanten NFS-Snapshot. Vor der Veröffentlichung erfordert AWI eine LanceDB-Abdeckung für jedes aktuelle förderfähige `(file_id, generation)`-Paar. Die semantische Datenbank und ihr Manifest werden in denselben unveränderlichen Snapshot wie SQLite und Tantivy kopiert. Leser lehnen eine nicht übereinstimmende Generation ab und fallen bei nicht verfügbarem Sidecar auf die lexikalische Suche zurück. Entfernen Sie die Einstellung von `AWI_SEMANTIC_MODEL`, um die semantische Spur zu deaktivieren.

Der semantische Abruf zur Abfragezeit läuft gleichzeitig mit den lexikalischen Spuren. Der Vektorüberabruf wird durch die konfigurierte maximale Anzahl von Chunks pro Datei begrenzt, wodurch die Dateiebene-Top-K-Abdeckung erhalten bleibt, ohne redundante Chunk-Kandidaten zurückzugeben. Persistente Leser führen einen vollständigen Hybrid-Warmup durch, bevor sie Abfragen bedienen. Der Sidecar verwendet ein Linux-Elterntod-Signal, sodass das Stoppen des Lesers auch das Modell freigibt.

Versiegelte Generationen verwenden einen IVF_FLAT-Index und sondieren jede Partition. Dies bewahrt die ursprünglichen normalisierten Q8-Vektoren und die exakte Top-K-Reihenfolge und vermeidet gleichzeitig den Recall-Verlust der Produktquantisierung bei AWIs aktueller Korpusgröße.

Für NFS-gestützte Arbeitsbereiche erstellen Sie veränderliche Indizes auf lokalem Speicher und veröffentlichen unveränderliche Snapshots:

```bash
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace \
  --publish-dir /shared/awi-publication --json

awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

Wenn einem Client-Host nicht genügend CPU-Kontingent für GGUF-Latenzziele fehlt, führen Sie den Snapshot-Leser auf einem CPU-Worker aus und bewahren den lokalen MCP-Socket mit dem sich wieder verbindenden Stream-Local-Tunnel auf:

```bash
scripts/remote-reader-tunnel.sh \
  /tmp/local-awi.sock /tmp/remote-awi.sock <worker-ip> <ssh-port>
```

### Automatische Aktualisierungskette

Um die gemeinsame Veröffentlichung ohne manuelle Abstimmungen aktuell zu halten, führen Sie den persistenten Produzenten neben dem Leser aus. Der Produzent stimmt Wurzeln ab und veröffentlicht nur dann einen neuen unveränderlichen Snapshot, wenn sich Inhalt geändert hat; der Snapshot-folgende Leser wechselt bei seiner nächsten Anfrage atomar zu jeder neuen Generation. Dies schließt die Schleife Ende-zu-Ende: Bearbeiten Sie eine Datei, und der Leser spiegelt sie automatisch wider.

Snapshot-Download und Prüfsummenvalidierung laufen auf einem Hintergrundaktualisierungs-Worker. Anfragen laufen gegen die letzte gültige Generation weiter, während ein neuer Snapshot materialisiert wird, wodurch NFS-Aktualisierungspausen auf dem Abfragepfad vermieden werden. Eine einzelne Client-Trennung oder ein Schreibfehler wird protokolliert, ohne den gemeinsamen Daemon zu beenden.

Lokale Datenträgerwurzeln werden in Echtzeit überwacht (inotify), sodass Bearbeitungen innerhalb des Entprellfensters veröffentlicht werden. Entfernte Wurzeln (NFS und ähnliche, über `/proc/mounts` erkannt) und ein periodischer Sicherheitsnetz-Tick alle `--interval-ms` treiben den Rest an, da Dateisystemereignisse für entfernte Schreibvorgänge nicht zuverlässig sind. Wenn der Wächter nicht starten kann, degradiert der Produzent sauber zu reiner periodischer Abstimmung. Zugriffs- und nur-Metadaten-Ereignisse werden ignoriert, um selbstausgelöste Scans zu verhindern; anhängungsintensive `.log`-, `.jsonl`-, `.ndjson`-, `.csv`-, `.tsv`- und `.parquet`-Aktualisierungen werden auf den periodischen Durchlauf verschoben, anstatt für jeden Schreibvorgang einen Snapshot neu zu erstellen. Jeder Zyklus entdeckt auch Quellen für von `awi memory` registrierte Projekte neu. Neue Agentengedächtniswurzeln werden automatisch indiziert, und registrierte Wurzeln, die verschwinden, werden abgestimmt, sodass gelöschte Beweise nicht bereitgestellt werden.

```bash
# Produzent: lokale Wurzeln live überwachen, jede Wurzel höchstens alle 5 s abstimmen,
# Bearbeitungsbursts über 500 ms zusammenfassen, bei Änderung automatisch veröffentlichen, 3 Generationen behalten.
awi --index-dir /tmp/awi-writer watch \
  --publish-dir /shared/awi-publication \
  --interval-ms 5000 --debounce-ms 500 --retain 3

# Leser: der Veröffentlichung folgen und neue Generationen automatisch aktivieren.
awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

`watch` verwendet standardmäßig registrierte Nicht-Gedächtnis-Wurzeln; registrierte Gedächtnisprojekte werden in jedem Zyklus separat neu entdeckt. Übergeben Sie `--root <path>` einmal oder mehrmals, um die Menge einzuschränken. Die Aufbewahrungsbeschneidung entfernt ältere Generationen nach jeder Veröffentlichung und löscht niemals die Generation, auf die der Zeiger aktuell verweist, sodass das gemeinsame Verzeichnis nicht unbegrenzt wachsen kann.

Für Codex startet oder wiederverwendet `scripts/awi-mcp-snapshot-wrapper.sh` einen lokalen Snapshot-Daemon, bevor der Stdio-Adapter gestartet wird. Konfigurieren Sie `AWI_SNAPSHOT_SOURCE` und `AWI_MCP_AUDIT_LOG` und registrieren Sie dann den Wrapper als globalen MCP-Server. Veränderliche Snapshot-Daten bleiben unter `AWI_RUNTIME_DIR`, während der Unix-Socket und die Startsperre standardmäßig unter `~/.local/state/awi-$UID` liegen, sodass die Bereinigung temporärer Caches den Live-Steuersocket nicht trennen kann. Die kalte Materialisierung wartet standardmäßig bis zu 10 Minuten; überschreiben Sie sie mit `AWI_START_TIMEOUT_MS`.

### Ein-Kommando-Agentenintegration

`awi integrate` erkennt installierte Agentenclients und registriert AWI in einem Durchgang bei jedem unterstützten MCP-Host:

```bash
# Vorschau ohne Konfigurationsänderung.
awi integrate --project-root /path/to/workspace --dry-run --json

# Jeden erkannten unterstützten Client konfigurieren.
awi integrate --project-root /path/to/workspace

# Den Vorgang auf ausgewählte Clients beschränken.
awi integrate --client codex,gemini,opencode,qwen,cline,zed,amazon-q,crush \
  --project-root /path/to/workspace
```

Der Befehl ist idempotent und meldet einen Status pro Client: `configured`, `already_configured`, `would_configure`, `needs_attention`, `not_installed`, `unsupported` oder `failed`. Die Registrierungsmethode oder das Konfigurationsziel für jeden unterstützten Client ist:

| Client | Registrierungsmethode oder Ziel |
|---|---|
| Codex | Offizielle `codex mcp add` CLI; persistiert in `~/.codex/config.toml` |
| Gemini CLI | Offizielle `gemini mcp add --scope user` CLI; persistiert in `~/.gemini/settings.json` |
| Claude Code | Offizielle `claude mcp add --scope user` CLI; persistiert in `~/.claude.json` |
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers) | `~/.copilot/mcp-config.json` |
| TraeCode | `<project>/.trae/mcp.json` |
| Zcode | `~/.zcode/cli/config.json` bei `mcp.servers` |
| Kimi Code | `$KIMI_CODE_HOME/mcp.json`, standardmäßig `~/.kimi-code/mcp.json` |
| [OpenCode](https://opencode.ai/docs/en/mcp-servers/) | `$OPENCODE_CONFIG` oder `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.json` |
| [Pi](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent) | `$PI_CODING_AGENT_DIR/mcp.json` über [`pi-mcp-adapter`](https://pi.dev/packages/pi-mcp-adapter) |
| [Cursor](https://cursor.com/help/customization/mcp) | `~/.cursor/mcp.json` |
| [Windsurf](https://docs.windsurf.com/windsurf/cascade/mcp) | `~/.codeium/windsurf/mcp_config.json` |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/) | `~/.qwen/settings.json` |
| [Cline CLI](https://docs.cline.bot/mcp/mcp-overview) | `~/.cline/data/settings/cline_mcp_settings.json` (aktuelle CLI); `~/.cline/mcp.json` (Legacy-Nur-IDE-Fallback) |
| [Zed](https://zed.dev/docs/ai/mcp) | `${XDG_CONFIG_HOME:-~/.config}/zed/settings.json` bei `context_servers` |
| [Amazon Q Developer](https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-mcp-configuration.html) | `~/.aws/amazonq/mcp.json` |
| [Crush](https://www.mintlify.com/charmbracelet/crush/configuration/mcp) | `${XDG_CONFIG_HOME:-~/.config}/crush/crush.json` |

Plugins sind optionale Verpackungen für Clients mit nativer MCP-Unterstützung. Pi ist die Ausnahme: Sein Kern lässt MCP absichtlich weg, daher ist eine Erweiterung erforderlich. Neue Sitzungen laden die generierten benutzerebenen Einträge automatisch. TraeCode-Projektebenen-MCP muss einmal in den Einstellungen aktiviert werden. Als nicht vertrauenswürdig markierte Gemini-Arbeitsbereiche werden als `needs_attention` gemeldet, da Gemini alle MCP-Server unterdrückt, bis der Benutzer dem Arbeitsbereich ausdrücklich vertraut.

Standardmäßig registriert AWI die aktuelle Binärdatei als `awi --index-dir <absolute-path> mcp`, was für einen lokalen veränderlichen Index in sich geschlossen ist. Snapshot-basierte Produktionsbereitstellungen müssen ihren Wrapper explizit mit `--server-command` auswählen; wiederholte `--server-arg`-Optionen stehen für benutzerdefinierte Starter zur Verfügung.

## Sicherheit

`workspace_query` akzeptiert nur eine schreibgeschützte `SELECT`- oder `WITH`-Anweisung über explizite Eingaben unter registrierten Wurzeln. Es erzwingt Timeout-, Zeilen- und Ausgabebytegrenzen, während das Laden von DuckDB-Erweiterungen und der externe Zugriff deaktiviert werden.

Such- und Inspektionsantworten sind begrenzt. Die Agentenwissensentdeckung ist auf Vorfahren-`AGENTS.md`-Dateien und `SKILL.md`-Manifeste unter bekannten clientbezogenen Verzeichnissen beschränkt. Die Gedächtnisentdeckung ist auf kuratierte Dateien beschränkt, die dem ausgewählten Projekt zugeordnet sind; Rohhistorien erfordern eine explizite Einwilligung. AWI indiziert niemals ein ganzes Home-Verzeichnis. Sensible Dateien, generierte Verzeichnisse, übergroße Inhalte und Symlink-Ausbrüche sind standardmäßig ausgeschlossen. Standardmäßig ausgeschlossene Verzeichnisse sind `.git`, `.hg`, `.svn`, `.awi-index`, `node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.ipynb_checkpoints`, `.venv`, `.idea`, `.vscode` und `.cache`, sowie vom Agenten erstellte `.codex-work` und `.worktrees`, neben `.gitignore`- und `.awiignore`-Regeln. Der `watch`-Produzent wendet dieselben Ausschlüsse auf Dateisystemereignisse an, sodass Änderungen in diesen Verzeichnissen niemals eine Abstimmung auslösen. Suchwurzelfilter akzeptieren entweder eine indizierte Wurzel oder einen bestehenden übergeordneten Bereich, der indizierte Wurzeln enthält. Strukturierte Abfragewurzeln bleiben exakte Zulassungslisteneinträge.

Die MCP-Audit-Protokollierung ist optional. Wenn aktiviert, schreibt AWI private (`0600`) JSONL-Datensätze, die begrenzte und anmeldedatenredigierte Argumente, den aufrufenden Prozess, den standardisierten Client-Namen, die optionale Sitzungs-ID, den synthetischen Aufrufmarker, die Dauer, das Ergebnis, Fehlerdetails, Antwortbytes, Text-/strukturierte Nutzlastbytes, Treffer-/Zeilenzahlen sowie separate Vorschauabschneide- und Limitverdichtungsflags enthalten. Schema v5 behält die v4-Such-/Inspektionsverknüpfungsfelder bei und fügt `client_name`, `session_id` und `synthetic` hinzu. Legen Sie explizite Metadaten mit `AWI_MCP_CLIENT_NAME`, `AWI_MCP_SESSION_ID` und `AWI_MCP_SYNTHETIC` fest; die MCP-Initialisierung `clientInfo.name` überschreibt die Prozessableitung. Das aktive Protokoll rotiert bei 64 MiB und behält eine vorherige Datei. Projektübernahmeberichte verwenden [`eligible_session_adoption`](session-adoption.md), nicht alle Projektsitzungen als Nenner.

## Bewertung

Der aktuelle nur für die Entwicklung bestimmte Bewertungsbericht:

- Recall@10: 1,0
- Suche P95: 6,83 ms
- Veraltete-Ergebnis-Rate: 0
- Reale-Agenten-Paar-Toolaufrufreduzierung: 68,4 %
- Reale-Agenten-Paar-Suchreduzierung: 54,0 %
- Reale-Agenten-Paar-Wandzeitreduzierung: 41,3 %

Siehe [`evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md`](../evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md) für Protokoll, Kategorieaufschlüsselung, Vorbehalte und Reproduzierbarkeitsartefakte. Der Bewertungssatz bleibt ein Entwicklungskandidat, der eine doppelte menschliche Überprüfung erwartet; er ist kein eingefrorener Release-Benchmark. Diese Werte sind Diagnosen, keine formellen Präzisions-/Recall-Aussagen. AWI darf keine formellen P/R-, Inspect@K- oder MRR-Werte veröffentlichen, bis zwei verschiedene Gutachter den Abfragesatz unabhängig voneinander gekennzeichnet haben, Meinungsverschiedenheiten beigelegt wurden und die genehmigten Kennzeichnungen und Gutachteridentitäten aufgezeichnet wurden.

## Lizenz

Nicht-kommerzielle Lizenz. Kostenlos für persönliche, akademische und gemeinnützige Nutzung. Jede kommerzielle Nutzung erfordert eine vorherige schriftliche Zustimmung. Details siehe [LICENSE](../LICENSE).
