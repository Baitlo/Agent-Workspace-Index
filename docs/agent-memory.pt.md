# Indexação de Memória entre Agentes

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · **Português** · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## Uso

```bash
# Apenas memória com curadoria e resumos.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# Incluir explicitamente histórico bruto de chat/sessão correspondente.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# Pesquisar apenas memória associada ao projeto atual.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

O instalador executa a forma com curadoria por padrão. Use `--skip-agent-memory` para desativá-la ou `--include-raw-memory` para optar pelo histórico bruto.

## Fontes

A descoberta é restrita ao projeto canônico selecionado:

| Agente | Fontes com curadoria padrão | Opt-in bruto |
|---|---|---|
| Trae | perfil do usuário, memória de projeto correspondente, resumos de tópicos, resumos de sessões | nada além desses resumos |
| Codex | `MEMORY.md`, `memory_summary.md`, resumos de rollout correspondentes | JSONL de rollout bruto vinculado e `raw_memories.md` |
| Zcode | raiz de memória mapeada ao projeto pelos metadados de runtime do Zcode | arquivos de rollout/agente correspondentes |
| Gemini CLI | nenhuma | histórico de projeto mapeado e diretórios de chat |
| Claude Code | diretório de memória de projeto correspondente | arquivos de sessão que identificam o projeto |

Diretórios de sessão Argos/SRE são intencionalmente excluídos porque exigem o fluxo de trabalho de diagnóstico Argos em vez de indexação em massa de arquivos. AWI nunca escaneia todo o diretório home.

## Metadados e Classificação

Cada resultado de memória inclui:

- Agente de origem;
- camada: `user_profile`, `project_summary`, `topic_summary`, `session_summary`, `memory_note` ou `raw_history`;
- `name` e `description` YAML frontmatter opcionais;
- raiz de workspace canônico e chave de projeto do provedor quando disponíveis;
- ID de sessão quando pode ser derivado;
- tempo de observação e flag de histórico bruto.

Quando `context_path` ou um filtro de raiz de projeto é fornecido, memória de outro projeto é rejeitada. Memória de projeto exato classifica antes de memória global. Correspondências exatas de nome de arquivo e `name` frontmatter recebem o maior impulso de metadados; sobreposição de descrição fornece um impulso menor. Consultas em forma de identificador rebaixam resumos amplos de `MEMORY.md`/projeto, a menos que o resumo em si corresponda exatamente à entidade. A ordem normal de camadas permanece inalterada para consultas amplas, resumos com curadoria classificam antes do histórico bruto, e recência é apenas um pequeno desempate. Cópias com conteúdo idêntico de diferentes Agentes são colapsadas após a classificação. A recuperação de memória é ativada com `--kind agent_memory`. Documentos de memória usam um índice Tantivy dedicado para que seu vocabulário não possa alterar estatísticas IDF ou classificação comuns de código/dados.

## Análise e Segurança

Memória Markdown é indexada como texto limitado. Memória JSON/JSONL é normalizada para campos relevantes ao humano, como intenção, ações, resultado, fatos aprendidos, papel, mensagem e conteúdo; IDs de transporte e metadados de resumo internos são omitidos do documento de pesquisa. Memória JSONL não é enviada para perfilamento DuckDB.

Verificações normais de tamanho, UTF-8, ignorar e nome de arquivo sensível ainda se aplicam. Credenciais ou chaves privadas de alta confiança tornam o arquivo de memória inteiro somente metadados. Histórico bruto é desativado a menos que explicitamente solicitado, e arquivos brutos superdimensionados permanecem somente metadados sob o limite de conteúdo configurado.

`awi memory` persiste a raiz do projeto canônico e a política de histórico bruto. Cada ciclo de produtor redescobre as fontes de Agente desse projeto antes de publicar, então novos diretórios de projeto de provedor aparecem sem reinstalar AWI. Raízes registradas anteriormente também são reconciliadas quando desaparecem, evitando que memória excluída permaneça pesquisável. JSONL de resumo anexado é atualizado na próxima reconciliação periódica; arquivos brutos pesados de anexação não acionam reconstruções de instantâneo por gravação.

`awi status --json` expõe um objeto `memory` com contagens de projeto/fonte registradas, arquivos ativos, a geração de memória mais recente e o atraso de geração relativo, `stale_files`/`missing_files` do sistema de arquivos, atraso máximo de fonte e idade da fonte mais antiga. Contagens de arquivos obsoletos e ausentes do sistema de arquivos são os sinais de frescor autoritativos; um baixo atraso de geração sozinho não prova que a memória externa está atualizada.

## Validação de Desenvolvimento

Uma execução Bona isolada descobriu 35 raízes relevantes ao projeto e indexou 489 arquivos com curadoria de Codex, Trae e Zcode com zero falhas de extração; chats brutos do Gemini permaneceram excluídos. O conjunto existente de 16 casos de recuperação de código/dados permaneceu estável bit a bit em Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950` e taxa de obsolescência `0`. P95 de build de release foi `24.06 ms`. Este conjunto ainda é `candidate_pending_dual_review`, não um portão de release congelado. Não relate precisão/recall formal, Inspect@K ou MRR até que dois revisores distintos tenham rotulado independentemente o conjunto de consultas e arbitrado divergências.
