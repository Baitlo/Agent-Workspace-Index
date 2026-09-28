# Agentenwissensindizierung

AWI indiziert Agentenbetriebsanweisungen neben Quellcode und Daten, ohne ein weiteres MCP-Tool hinzuzufügen.

Die agentenübergreifende Projekthistorie wird separat als `agent_memory` behandelt; siehe [Agentenübergreifende Gedächtnisindizierung](agent-memory.md).

## Dokumenttypen

| Datei | AWI-Typ | Strukturierte Metadaten |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | Bereichsverzeichnis, Prioritätstiefe, Überschriften, lokale Verweise |
| `SKILL.md` | `agent_skill` | YAML `name` und `description`, Überschriften, lokale Verweise |

Der vollständige begrenzte Markdown-Text bleibt durchsuchbar. `workspace_inspect` gibt die strukturierten Metadaten zusammen mit den normalen Dateimetadaten und dem Inhaltsauszug zurück.

## Entdeckung

Eine normale Arbeitsbereichsabstimmung indiziert `AGENTS.md`- und `SKILL.md`-Dateien unter diesem Arbeitsbereich, vorbehaltlich `.gitignore`, `.awiignore` und AWIs Standardausschlüssen.

Das Erstinstallationsprogramm entdeckt zusätzlich:

- `AGENTS.md`-Dateien in Vorfahrenverzeichnissen über dem ausgewählten Arbeitsbereich;
- `SKILL.md`-Manifeste unter bekannten Projekt- und Benutzerverzeichnissen für unterstützte Agentenclients.

Jedes externe Dokument wird als Einzeldateiwurzel registriert. AWI indiziert nicht ein ganzes Home-Verzeichnis oder den vollständigen Inhalt globaler Skill-Pakete. `--skip-agent-knowledge` deaktiviert diese zusätzliche Entdeckung.

Lokale Markdown-Links aus einem Agentendokument werden nur aufgezeichnet, wenn ihr Ziel existiert und innerhalb der Dokumentwurzel bleibt. Referenzierte Dateien sind durchsuchbar, wenn sie bereits von einer Arbeitsbereichswurzel abgedeckt werden; andernfalls wird der Referenzpfad als Metadaten für eine explizite Inspektion mit einem anderen Dateitool zurückgegeben.

## Bereich und Rangfolge

Übergeben Sie `context_path` an `workspace_search`, wenn Sie Repository-Anweisungen auflösen:

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI schließt `AGENTS.md`-Dateien aus, deren Bereich kein Vorfahre des Kontextpfads ist. Anwendbare Dateien werden nach Bereichstiefe geordnet, sodass die nächstgelegenen Anweisungen zuerst kommen. Nach dieser Rangfolge werden inhaltsidentische Agentendokumente reduziert, was kopierte Skill- und Worktree-Duplikate unterdrückt, ohne die nächstgelegene anwendbare Anweisung zu verlieren.

Verwenden Sie `--kind agent_skill` für einen Skill-zentrierten Abruf. Agentendokumente werden aus gewöhnlichen Code-/Datensuchen herausgehalten; die dedizierte Agentenspur wird durch einen Agententypfilter oder `context_path` aktiviert.

## Sicherheit

Agentendokumente unterliegen weiterhin normalen Größen-, UTF-8-, Symlink- und sensiblen Dateinamensprüfungen. Hochvertrauensmuster für private Schlüssel und Anbietertoken in ihrem Inhalt führen zu einem Nur-Metadaten-Ergebnis mit `metadata_only_sensitive_content`; der Inhalt, die Vorschau und die analysierten Agentenmetadaten werden nicht gespeichert.

Ein fehlerhaftes Skill-Frontmatter wird als Extraktionsfehler aufgezeichnet, während der neue Markdown-Inhalt weiterhin älteren indizierten Text ersetzt. Dies vermeidet die Bereitstellung veralteter Inhalte, wenn nur das Parsen strukturierter Metadaten fehlschlägt.

## Entwicklungsvalidierung

Eine isolierte Installation über die aktuelle Bona-Umgebung entdeckte 136 kanonische Agentendokumente mit null Extraktionsfehlern. Gezielte Smoke-Abfragen wählten `wukong-dag-failure-debugger` zuerst für eine Wukong-DAG/LogID-Fehlerabfrage und die Bona-`AGENTS.md` für einen AWI-Quellkontext.

Im bestehenden 16-Fall-Entwicklungsabrufset hielt das Hinzufügen dieser Agentendokumente Recall@10 bei `1.0` und die Rate veralteter Ergebnisse bei `0`; MRR änderte sich von `0.5975` auf `0.6027`, nDCG@10 von `0.6922` auf `0.6969` und der Release-Build-P95 von `8,07 ms` auf `25,88 ms`. Die Latenz bleibt unter dem `150 ms`-Hybrid-Suchziel. Dieses Entwicklungsset steht noch unter doppelter Überprüfung und ist kein eingefrorener Release-Benchmark.
