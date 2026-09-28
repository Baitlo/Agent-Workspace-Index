# Indicizzazione della Memoria tra Agenti

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · **Italiano** · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## Uso

```bash
# Solo memoria curata e riepiloghi.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# Includere esplicitamente la cronologia grezza di chat/sessione corrispondente.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# Cercare solo memoria associata al progetto corrente.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

L'installatore esegue la forma curata per impostazione predefinita. Usa `--skip-agent-memory` per disattivarla o `--include-raw-memory` per aderire alla cronologia grezza.

## Fonti

La scoperta è limitata al progetto canonico selezionato:

| Agente | Fonti curate predefinite | Opt-in grezzo |
|---|---|---|
| Trae | profilo utente, memoria di progetto corrispondente, riepiloghi degli argomenti, riepiloghi delle sessioni | nulla oltre a questi riepiloghi |
| Codex | `MEMORY.md`, `memory_summary.md`, riepiloghi di rollout corrispondenti | JSONL di rollout grezzo collegato e `raw_memories.md` |
| Zcode | radice di memoria mappata al progetto dai metadati di runtime di Zcode | file di rollout/agente corrispondenti |
| Gemini CLI | nessuna | cronologia di progetto mappata e directory di chat |
| Claude Code | directory di memoria di progetto corrispondente | file di sessione che identificano il progetto |

Le directory di sessione Argos/SRE sono intenzionalmente escluse perché richiedono il flusso di lavoro diagnostico Argos anziché l'indicizzazione di massa dei file. AWI non scansiona mai l'intera directory home.

## Metadati e Classificazione

Ogni risultato di memoria include:

- Agente di origine;
- livello: `user_profile`, `project_summary`, `topic_summary`, `session_summary`, `memory_note` o `raw_history`;
- `name` e `description` YAML frontmatter opzionali;
- radice workspace canonico e chiave progetto provider quando disponibili;
- ID sessione quando può essere derivato;
- tempo di osservazione e flag cronologia grezza.

Quando viene fornito `context_path` o un filtro radice progetto, la memoria di un altro progetto viene rifiutata. La memoria del progetto esatto si classifica prima della memoria globale. Le corrispondenze esatte di nome file e `name` frontmatter ricevono il maggior boost di metadati; la sovrapposizione di descrizione fornisce un boost minore. Le query a forma di identificatore degradano i riepiloghi ampi `MEMORY.md`/progetto a meno che il riepilogo stesso non corrisponda esattamente all'entità. L'ordine normale dei livelli rimane invariato per query ampie, i riepiloghi curati si classificano prima della cronologia grezza, e la recenza è solo un piccolo tie-breaker. Le copie con contenuto identico da diversi Agenti vengono compresse dopo la classificazione. Il recupero della memoria è attivato con `--kind agent_memory`. I documenti di memoria usano un indice Tantivy dedicato in modo che il loro vocabolario non possa cambiare le statistiche IDF o la classificazione comune di codice/dati.

## Analisi e Sicurezza

La memoria Markdown è indicizzata come testo limitato. La memoria JSON/JSONL è normalizzata in campi rilevanti per l'uomo come intento, azioni, risultato, fatti appresi, ruolo, messaggio e contenuto; gli ID di trasporto e i metadati di riepilogo interni sono omessi dal documento di ricerca. La memoria JSONL non viene inviata al profiling DuckDB.

I normali controlli di dimensione, UTF-8, ignore e nome file sensibile si applicano comunque. Credenziali o chiavi private ad alta confidenza rendono l'intero file di memoria solo metadati. La cronologia grezza è disabilitata a meno che non sia esplicitamente richiesta, e i file grezzi sovradimensionati rimangono solo metadati sotto il limite di contenuto configurato.

`awi memory` persiste la radice del progetto canonico e la politica della cronologia grezza. Ogni ciclo di produttore riscopre le fonti Agente di quel progetto prima della pubblicazione, quindi nuove directory di progetto provider appaiono senza reinstallare AWI. Le radici precedentemente registrate vengono riconciliate anche quando scompaiono, impedendo che la memoria eliminata rimanga consultabile. Il JSONL di riepilogo annesso viene aggiornato alla prossima riconciliazione periodica; i file grezzi pesanti di annesso non attivano ricostruzioni di snapshot per scrittura.

`awi status --json` espone un oggetto `memory` con conteggi di progetto/fonte registrati, file attivi, la generazione di memoria più recente e il ritardo di generazione relativo, `stale_files`/`missing_files` del filesystem, ritardo massimo della fonte ed età della fonte più vecchia. I conteggi di file obsoleti e mancanti del filesystem sono i segnali di freschezza autorevoli; un basso ritardo di generazione da solo non prova che la memoria esterna sia aggiornata.

## Validazione dello Sviluppo

Un'esecuzione Bona isolata ha scoperto 35 radici rilevanti al progetto e indicizzato 489 file curati da Codex, Trae e Zcode con zero fallimenti di estrazione; le chat grezze di Gemini sono rimaste escluse. Il set esistente di 16 casi di recupero codice/dati è rimasto stabile bit a bit a Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950` e tasso di obsolescenza `0`. Il P95 della build di release era `24.06 ms`. Questo set è ancora `candidate_pending_dual_review`, non un cancello di release congelato. Non riportare precisione/richiamo formale, Inspect@K o MRR finché due revisori distinti non abbiano etichettato indipendentemente il set di query e arbitrato le divergenze.
