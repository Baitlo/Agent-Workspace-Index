# Indexação de Conhecimento do Agente

[English](agent-knowledge.md) · [中文](agent-knowledge.zh.md) · [繁體中文](agent-knowledge.zh-TW.md) · [日本語](agent-knowledge.ja.md) · [한국어](agent-knowledge.ko.md) · [Русский](agent-knowledge.ru.md) · [Français](agent-knowledge.fr.md) · [Deutsch](agent-knowledge.de.md) · **Português** · [Español](agent-knowledge.es.md) · [العربية](agent-knowledge.ar.md) · [Italiano](agent-knowledge.it.md) · [Ελληνικά](agent-knowledge.el.md) · [ไทย](agent-knowledge.th.md) · [Bahasa Melayu](agent-knowledge.ms.md)

O histórico de projetos entre Agentes é tratado separadamente como `agent_memory`; consulte [Indexação de Memória entre Agentes](agent-memory.md).

## Tipos de Documentos

| Arquivo | Tipo AWI | Metadados estruturados |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | diretório de escopo, profundidade de precedência, títulos, referências locais |
| `SKILL.md` | `agent_skill` | `name` e `description` YAML, títulos, referências locais |

O texto Markdown completo e limitado permanece pesquisável. `workspace_inspect` retorna os metadados estruturados junto com os metadados normais do arquivo e o trecho de conteúdo.

## Descoberta

Uma reconciliação normal de workspace indexa arquivos `AGENTS.md` e `SKILL.md` sob esse workspace, sujeito a `.gitignore`, `.awiignore` e exclusões padrão do AWI.

O instalador de primeira execução descobre adicionalmente:

- arquivos `AGENTS.md` em diretórios ancestrais acima do workspace selecionado;
- manifestos `SKILL.md` sob diretórios de projeto e usuário conhecidos para clientes de Agente suportados.

Cada documento externo é registrado como uma raiz de arquivo único. AWI não indexa um diretório home inteiro ou o conteúdo completo de pacotes Skill globais. `--skip-agent-knowledge` desativa essa descoberta adicional.

Links Markdown locais de um documento do Agente são registrados apenas quando seu alvo existe e permanece dentro da raiz do documento. Arquivos referenciados são pesquisáveis quando já estão cobertos por uma raiz de workspace; caso contrário, o caminho de referência é retornado como metadados para inspeção explícita com outra ferramenta de arquivo.

## Escopo e Classificação

Passe `context_path` para `workspace_search` ao resolver instruções de repositório:

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI exclui arquivos `AGENTS.md` cujo escopo não é um ancestral do caminho de contexto. Arquivos aplicáveis são classificados por profundidade de escopo para que as instruções mais próximas venham primeiro. Após essa classificação, documentos de Agente com conteúdo idêntico são colapsados, o que suprime duplicatas de Skill e worktree copiadas sem perder a instrução aplicável mais próxima.

Use `--kind agent_skill` para recuperação focada em Skill. Documentos de Agente são mantidos fora de pesquisas comuns de código/dados; a faixa dedicada do Agente é ativada por um filtro de tipo de Agente ou `context_path`.

## Segurança

Documentos de Agente permanecem sujeitos a verificações normais de tamanho, UTF-8, link simbólico e nome de arquivo sensível. Padrões de chave privada e token de provedor de alta confiança em seu conteúdo causam um resultado somente de metadados com `metadata_only_sensitive_content`; o conteúdo, prévia e metadados de Agente analisados não são armazenados.

Frontmatter Skill malformado é registrado como uma falha de extração, enquanto o novo conteúdo Markdown ainda substitui qualquer texto indexado mais antigo. Isso evita servir conteúdo obsoleto quando apenas a análise de metadados estruturados falha.

## Validação de Desenvolvimento

Uma instalação isolada sobre o ambiente Bona atual descobriu 136 documentos de Agente canônicos com zero falhas de extração. Consultas de fumaça direcionadas selecionaram `wukong-dag-failure-debugger` primeiro para uma consulta de falha Wukong DAG/LogID e o `AGENTS.md` Bona para um contexto de origem AWI.

No conjunto de recuperação de desenvolvimento existente de 16 casos, adicionar esses documentos de Agente manteve Recall@10 em `1.0` e taxa de resultados obsoletos em `0`; MRR mudou de `0.5975` para `0.6027`, nDCG@10 de `0.6922` para `0.6969`, e P95 de build de release de `8.07 ms` para `25.88 ms`. A latência permanece abaixo da meta de pesquisa híbrida de `150 ms`. Este conjunto de desenvolvimento permanece pendente de revisão dupla e não é um benchmark de release congelado.
