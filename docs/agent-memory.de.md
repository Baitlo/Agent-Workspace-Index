# Agentenübergreifende Gedächtnisindizierung

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · **Deutsch** · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## Verwendung

```bash
# Nur kuratiertes Gedächtnis und Zusammenfassungen.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# Passenden Rohtext-Chat/Sitzungsverlauf explizit einbeziehen.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# Nur mit dem aktuellen Projekt verknüpftes Gedächtnis durchsuchen.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

Das Installationsprogramm führt standardmäßig die kuratierte Form aus. Verwenden Sie `--skip-agent-memory`, um es zu deaktivieren, oder `--include-raw-memory`, um sich für den Rohverlauf zu entscheiden.

## Quellen

Die Entdeckung ist auf das ausgewählte kanonische Projekt beschränkt:

| Agent | Standardmäßig kuratierte Quellen | Opt-in für Rohverlauf |
|---|---|---|
| Trae | Benutzerprofil, passendes Projektgedächtnis, Themenzusammenfassungen, Sitzungszusammenfassungen | nichts über diese Zusammenfassungen hinaus |
| Codex | `MEMORY.md`, `memory_summary.md`, passende Rollout-Zusammenfassungen | verlinkte Raw-Rollout-JSONL und `raw_memories.md` |
| Zcode | durch Zcode-Laufzeitmetadaten auf das Projekt abgebildeter Gedächtnisstamm | passende Rollout/Agent-Dateien |
| Gemini CLI | keine | abgebildeter Projektverlauf und Chat-Verzeichnisse |
| Claude Code | passendes Projektgedächtnisverzeichnis | Sitzungsdateien, die das Projekt identifizieren |

Argos/SRE-Sitzungsverzeichnisse werden absichtlich ausgeschlossen, da sie den Argos-Diagnose-Workflow anstelle der Massendateiindizierung erfordern. AWI scannt niemals das gesamte Home-Verzeichnis.

## Metadaten und Rangfolge

Jeder Gedächtnistreffer enthält:

- Quellagent;
- Ebene: `user_profile`, `project_summary`, `topic_summary`, `session_summary`, `memory_note` oder `raw_history`;
- optionales YAML-Frontmatter `name` und `description`;
- kanonische Arbeitsbereichswurzel und Anbieterprojektschlüssel, wenn verfügbar;
- Sitzungs-ID, wenn sie abgeleitet werden kann;
- Beobachtungszeit und Rohhistorie-Flag.

Wenn `context_path` oder ein Projektwurzelfilter bereitgestellt wird, wird Gedächtnis aus einem anderen Projekt abgelehnt. Gedächtnis des genauen Projekts wird vor globalem Gedächtnis eingestuft. Exakte Dateinamen- und Frontmatter-`name`-Treffer erhalten den stärksten Metadaten-Boost; Beschreibungsüberlappung bietet einen kleineren Boost. Identifikator-förmige Abfragen stufen breite `MEMORY.md`/Projektzusammenfassungen herab, es sei denn, die Zusammenfassung selbst stimmt exakt mit der Entität überein. Die normale Ebenenreihenfolge bleibt für breite Abfragen unverändert, kuratierte Zusammenfassungen werden vor Rohhistorie eingestuft, und Neuigkeit ist nur ein kleiner Tie-Breaker. Inhaltlich identische Kopien verschiedener Agenten werden nach dem Ranking zusammengefaltet. Gedächtnisabruf wird mit `--kind agent_memory` aktiviert. Gedächtnisdokumente verwenden einen dedizierten Tantivy-Index, sodass ihr Vokabular die gewöhnlichen Code-/Daten-IDF-Statistiken oder das Ranking nicht ändern kann.

## Parsing und Sicherheit

Markdown-Gedächtnis wird als begrenzter Text indiziert. JSON/JSONL-Gedächtnis wird auf menschenrelevante Felder wie Absicht, Aktionen, Ergebnis, gelernte Fakten, Rolle, Nachricht und Inhalt normalisiert; Transport-IDs und interne Digest-Metadaten werden aus dem Suchdokument weggelassen. Gedächtnis-JSONL wird nicht an das DuckDB-Profiling gesendet.

Normale Größen-, UTF-8-, Ignore- und sensible Dateinamensprüfungen gelten weiterhin. Hochvertrauenswürdige Anmeldeinformationen oder private Schlüssel führen dazu, dass die gesamte Gedächtnisdatei nur aus Metadaten besteht. Rohhistorie ist deaktiviert, es sei denn, sie wird explizit angefordert, und überdimensionale Rohdateien bleiben unter dem konfigurierten Inhaltslimit nur Metadaten.

`awi memory` persistiert die kanonische Projektwurzel und die Rohhistorie-Richtlinie. Jeder Produzentenzyklus entdeckt die Agentenquellen dieses Projekts vor der Veröffentlichung neu, sodass neue Anbieterprojektverzeichnisse ohne Neuinstallation von AWI erscheinen. Zuvor registrierte Wurzeln werden ebenfalls abgestimmt, wenn sie verschwinden, wodurch verhindert wird, dass gelöschtes Gedächtnis durchsuchbar bleibt. Angehängte Zusammenfassungs-JSONL wird beim nächsten periodischen Abgleich aktualisiert; anhängungsintensive Rohdateien lösen keinen Snapshot-Neuaufbau pro Schreibvorgang aus.

`awi status --json` enthält ein `memory`-Objekt mit registrierten Projekt-/Quellenzählungen, aktiven Dateien, der neuesten Gedächtnisgeneration und dem relativen Generationsverzug, Dateisystem-`stale_files`/`missing_files`, maximalem Quellenverzug und ältestem Quellenalter. Veraltete und fehlende Dateisystemzählungen sind die maßgeblichen Frische-Signale; ein geringer Generationsverzug allein beweist nicht, dass das externe Gedächtnis aktuell ist.

## Entwicklungsvalidierung

Ein isolierter Bona-Lauf entdeckte 35 projektrelevante Wurzeln und indizierte 489 kuratierte Dateien von Codex, Trae und Zcode mit null Extraktionsfehlern; Rohe Gemini-Chats blieben ausgeschlossen. Der bestehende 16-Fälle-Code-/Daten-Abrufset blieb bit-genau stabil bei Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950` und Veraltungsrate `0`. Der Release-Build-P95 lag bei `24.06 ms`. Dieses Set ist noch `candidate_pending_dual_review`, kein eingefrorenes Release-Gate. Berichten Sie keine formelle Präzision/Recall, Inspect@K oder MRR, bis zwei unterschiedliche Reviewer den Abfragesatz unabhängig gekennzeichnet und Meinungsverschiedenheiten entschieden haben.
