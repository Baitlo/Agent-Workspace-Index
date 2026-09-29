# Indicizzazione della Conoscenza dell'Agente

[English](agent-knowledge.md) · [中文](agent-knowledge.zh.md) · [繁體中文](agent-knowledge.zh-TW.md) · [日本語](agent-knowledge.ja.md) · [한국어](agent-knowledge.ko.md) · [Русский](agent-knowledge.ru.md) · [Français](agent-knowledge.fr.md) · [Deutsch](agent-knowledge.de.md) · [Português](agent-knowledge.pt.md) · [Español](agent-knowledge.es.md) · [العربية](agent-knowledge.ar.md) · **Italiano** · [Ελληνικά](agent-knowledge.el.md) · [ไทย](agent-knowledge.th.md) · [Bahasa Melayu](agent-knowledge.ms.md)

La cronologia del progetto tra Agenti è gestita separatamente come `agent_memory`; vedere [Indicizzazione della Memoria tra Agenti](agent-memory.md).

## Tipi di Documenti

| File | Tipo AWI | Metadati strutturati |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | directory di ambito, profondità di precedenza, titoli, riferimenti locali |
| `SKILL.md` | `agent_skill` | `name` e `description` YAML, titoli, riferimenti locali |

Il testo Markdown completo e limitato rimane consultabile. `workspace_inspect` restituisce i metadati strutturati insieme ai metadati normali del file e all'estratto di contenuto.

## Scoperta

Una riconciliazione normale del workspace indicizza i file `AGENTS.md` e `SKILL.md` sotto quel workspace, soggetto a `.gitignore`, `.awiignore` ed esclusioni predefinite di AWI.

L'installatore della prima esecuzione scopre inoltre:

- file `AGENTS.md` in directory antenate sopra il workspace selezionato;
- manifesti `SKILL.md` sotto directory di progetto e utente note per i client Agente supportati.

Ogni documento esterno è registrato come radice di file singolo. AWI non indicizza un'intera directory home o il contenuto completo di pacchetti Skill globali. `--skip-agent-knowledge` disattiva questa scoperta aggiuntiva.

I collegamenti Markdown locali da un documento Agente vengono registrati solo quando la loro destinazione esiste e rimane all'interno della radice del documento. I file referenziati sono consultabili quando sono già coperti da una radice del workspace; altrimenti, il percorso di riferimento viene restituito come metadati per l'ispezione esplicita con un altro strumento file.

## Ambito e Classificazione

Passa `context_path` a `workspace_search` quando risolvi le istruzioni del repository:

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI esclude i file `AGENTS.md` il cui ambito non è un antenato del percorso di contesto. I file applicabili sono classificati per profondità di ambito in modo che le istruzioni più vicine vengano prima. Dopo questa classificazione, i documenti Agente con contenuto identico vengono compressi, il che sopprime i duplicati di Skill e worktree copiati senza perdere l'istruzione applicabile più vicina.

Senza un filtro di tipo, le istruzioni Agent e le Skills vengono cercate insieme a codice e dati. Usa `--kind agent_skill` o `--kind agent_instructions` solo per limitare la ricerca a un tipo di documento Agent. `context_path` applica il filtro di ambito e il ranking delle istruzioni.

## Sicurezza

I documenti Agente rimangono soggetti a controlli normali di dimensione, UTF-8, collegamento simbolico e nome file sensibile. I modelli di chiave privata e token provider ad alta confidenza nel loro contenuto causano un risultato solo metadati con `metadata_only_sensitive_content`; il contenuto, l'anteprima e i metadati Agente analizzati non vengono memorizzati.

Il frontmatter Skill malformato viene registrato come un fallimento di estrazione, mentre il nuovo contenuto Markdown sostituisce comunque qualsiasi testo indicizzato più vecchio. Ciò evita di servire contenuto obsoleto quando solo l'analisi dei metadati strutturati fallisce.

## Validazione dello Sviluppo

Un'installazione isolata sull'ambiente Bona attuale ha scoperto 136 documenti Agente canonici con zero fallimenti di estrazione. Le query di fumo mirate hanno selezionato `wukong-dag-failure-debugger` prima per una query di fallimento Wukong DAG/LogID e l'`AGENTS.md` di Bona per un contesto di origine AWI.

Sul set di recupero di sviluppo esistente di 16 casi, l'aggiunta di questi documenti Agente ha mantenuto Recall@10 a `1.0` e il tasso di risultati obsoleti a `0`; MRR è cambiato da `0.5975` a `0.6027`, nDCG@10 da `0.6922` a `0.6969`, e P95 della build di release da `8.07 ms` a `25.88 ms`. La latenza rimane al di sotto dell'obiettivo di ricerca ibrida di `150 ms`. Questo set di sviluppo rimane in attesa di doppia revisione e non è un benchmark di release congelato.
