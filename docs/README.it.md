# AWI: Agent Workspace Index

[English](../README.md) · [中文](README.zh.md) · [繁體中文](README.zh-TW.md) · [日本語](README.ja.md) · [한국어](README.ko.md) · [Русский](README.ru.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [Português](README.pt.md) · [Español](README.es.md) · [العربية](README.ar.md) · **Italiano** · [Ελληνικά](README.el.md) · [ไทย](README.th.md) · [Bahasa Melayu](README.ms.md)

AWI é um índice unificado local-first para código, dados e conhecimento operacional de Agentes. Ele indexa código-fonte, SQL, documentos, logs, JSON/JSONL, CSV/TSV, Parquet, `AGENTS.md`, Skills de Agentes e memória entre Agentes com escopo de projeto através de uma interface CLI e Model Context Protocol (MCP) limitada.

Um comando conecta esse índice a 16 harnesses de agentes de codificação, incluindo Codex, Claude Code, Gemini CLI, GitHub Copilot CLI, OpenCode, Qwen Code, Cline, Zed, Amazon Q Developer e Crush.

## Funcionalidades

- Recuperação híbrida Tantivy sobre caminhos, texto, símbolos e esquemas de conjuntos de dados.
- Extração de símbolos Tree-sitter para Rust, Python e Go.
- Consultas DuckDB incorporadas e somente leitura sobre arquivos explicitamente permitidos.
- Recuperação `AGENTS.md` sensível ao escopo e metadados estruturados `SKILL.md`.
- Descoberta de memória sensível ao projeto entre Trae, Codex, Zcode, Gemini e Claude.
- Verificações de catálogo SQLite que rejeitam resultados de pesquisa obsoletos.
- Caminhos de atualização `notify` e `reconcile` seguros para NFS.
- Instantâneos de geração imutáveis com ativação atômica de daemon.
- Ferramentas MCP: `workspace_search`, `workspace_inspect` e `workspace_query`.

Consulte [Indexação de conhecimento do Agente](agent-knowledge.md) para descoberta, escopo, classificação, deduplicação e semântica de segurança. Consulte [Indexação de memória entre Agentes](agent-memory.md) para fontes de memória e o limite de histórico bruto.

## Binários Linux e macOS pré-compilados

GitHub Releases fornece binários nativos para:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Instale e configure o AWI para o repositório atual com um comando:

```bash
curl -fsSL \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh |
  bash -s -- --workspace "$PWD"
```

O instalador detecta a arquitetura Linux, baixa o arquivo de release e `SHA256SUMS`, verifica a soma de verificação, instala o binário, atualiza uma seção AWI gerenciada no `AGENTS.md` de nível superior do workspace, constrói o índice inicial, descobre conhecimento do Agente e memória de projeto com curadoria, e registra cada cliente de Agente suportado detectado. O conteúdo `AGENTS.md` existente é preservado e a reexecução do instalador substitui em vez de duplicar a seção gerenciada.

Para ambientes que exigem revisão de um script baixado antes da execução:

```bash
curl -fsSLO \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh
less install-release.sh
bash install-release.sh --workspace /absolute/path/to/your/repository
```

Omite `--workspace` para instalar apenas o binário `awi`. Fixe uma release com `--version v0.2.0`, escolha outro destino com `--bin-dir`, restrinja clientes com `--clients` ou desative o bloco de instruções gerenciado com `--skip-agent-instructions`. Os binários Linux são construídos nativamente em runners hospedados no GitHub Ubuntu 22.04 e requerem glibc e `libstdc++` compatíveis. Os binários macOS são construídos nativamente em runners macOS 15 para Apple Silicon e Intel.

A recuperação semântica permanece opcional. Ela requer adicionalmente Python, LanceDB, `llama-cpp-python` e um modelo de embedding GGUF compatível. Os mantenedores devem seguir a [lista de verificação de release](releasing.md).

## Instalação assistida por Agente

Dê ao seu agente de codificação o [prompt de instalação recomendado](agent-install-prompt.md), ou execute o instalador a partir de um checkout AWI:

```bash
bash scripts/install.sh --workspace /absolute/path/to/your/repository
```

A primeira execução constrói e instala `awi`, atualiza as instruções AWI gerenciadas do workspace, cria um índice local fora do workspace, indexa arquivos `AGENTS.md` ancestrais e manifestos `SKILL.md` encontrados em diretórios de Agentes permitidos, indexa memória com curadoria associada a esse workspace, detecta clientes de Agentes instalados e registra o servidor MCP AWI com cada cliente suportado. A operação é idempotente. Passe `--skip-agent-instructions`, `--skip-agent-knowledge` ou `--skip-agent-memory` para desativar essas etapas. Chats brutos permanecem excluídos a menos que `--include-raw-memory` seja fornecido. Rust e Cargo são necessários apenas ao construir a partir do código-fonte.

Se Pi for detectado, o instalador também instala o `pi-mcp-adapter@2.36.0` fixado, porque Pi intencionalmente não tem cliente MCP embutido. Este é um pacote Pi de terceiros; passe `--skip-pi-adapter` para revisá-lo ou instalá-lo separadamente, ou defina `AWI_PI_MCP_ADAPTER_SPEC` para selecionar outra versão revisada. Execute `scripts/install.sh --help` para opções personalizadas de binário, índice e cliente.

## Construção e Teste

```bash
export CARGO_TARGET_DIR=/tmp/awi-target
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
python3 -m unittest tests/test_semantic_sidecar.py
python3 tests/test_audit_session_adoption.py
```

## Uso Básico

```bash
# Construir ou atualizar um índice.
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace --json

# Executar o daemon persistente.
awi --index-dir /tmp/my-awi-index serve

# Pesquisar e inspecionar.
awi --index-dir /tmp/my-awi-index search "workspace query" --limit 10 --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file \
  --symbol exact_function_name --json

# Resolver instruções aplicáveis a um caminho de workspace concreto.
awi --index-dir /tmp/my-awi-index search "build and test rules" \
  --kind agent_instructions --context-path /path/to/workspace/src/lib.rs --json

# Indexar e pesquisar um manifesto Skill global autônomo.
awi --index-dir /tmp/my-awi-index reconcile ~/.agents/skills/example/SKILL.md --json
awi --index-dir /tmp/my-awi-index search "diagnose deployment failures" \
  --kind agent_skill --json

# Descobrir memória com curadoria para este projeto e pesquisá-la.
awi --index-dir /tmp/my-awi-index memory --project-root /path/to/workspace
awi --index-dir /tmp/my-awi-index search "previous rollout decision" \
  --kind agent_memory --context-path /path/to/workspace --json

# Iniciar o adaptador MCP stdio.
awi --index-dir /tmp/my-awi-index mcp

# Persistir chamadas de ferramentas MCP reais para análise de recuperação posterior.
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

A memória usa um índice Tantivy separado e é pesquisada apenas quando `--kind agent_memory` é solicitado, portanto, adicionar memória não pode alterar a classificação comum de código/dados. A pesquisa MCP usa a forma compacta `compact_v3` com uma prévia de 1.000 caracteres, `file_id` estável, `search_id` com escopo de solicitação e metadados de definição para correspondências exatas de símbolos. O padrão é 5 resultados e solicitações acima de 20 são compactadas para 20. Passe o `search_id` retornado e o `symbol` exato opcional para `workspace_inspect` para vincular a inspeção à sua recuperação e centralizar o trecho na definição. Comece com uma consulta rica em identificadores em vez de pesquisas paralelas de quase-sinônimos, e expanda apenas quando o primeiro conjunto de resultados não tiver evidências. Quando o caminho exato já é conhecido, leia-o diretamente com as ferramentas de arquivo do host. Os trechos de pesquisa são gerados a partir de janelas de origem armazenadas limitadas, e um reclassificação leve de diversidade de diretório impede que uma pasta de artefatos preencha o conjunto de resultados. O servidor stdio suporta `ping` MCP legado padrão e limita a negociação baseada em inicialização ao protocolo `2025-11-25`.

### Recuperação Semântica

AWI pode adicionar uma faixa semântica Harrier GGUF Q8 sem alterar a superfície de ferramentas MCP. Os embeddings de documentos são computados durante `reconcile`, `notify` ou um `semantic-build` inicial; pesquisas normais computam apenas o embedding da consulta. SQLite permanece autoritativo, e candidatos semânticos cuja geração de arquivo não é atual são descartados.

Crie um ambiente Python dedicado e habilite o sidecar:

```bash
python3 -m venv ~/.cache/awi/semantic-venv
~/.cache/awi/semantic-venv/bin/pip install -r requirements-semantic.txt

export AWI_SEMANTIC_MODEL=/path/to/harrier-oss-v1-270M-Q8_0.gguf
export AWI_SEMANTIC_MODEL_SHA256=fe12f3583dbbb832def4cffeb46c0d0ab49a3288542d5cdbaeb1741315a01b87
export AWI_SEMANTIC_PYTHON="$HOME/.cache/awi/semantic-venv/bin/python"
export AWI_SEMANTIC_THREADS=16
export AWI_SEMANTIC_EMBED_WORKERS=1
```

Para um catálogo existente, pré-compute cada arquivo elegível uma vez:

```bash
awi --index-dir /tmp/awi-writer semantic-build \
  --publish-dir /shared/awi-publication --json
```

Uma construção em massa interrompida pode reutilizar gerações de arquivos completos já confirmados no LanceDB. Se o conteúdo do workspace mudou durante a tentativa falha, reconcilie o catálogo sem publicar primeiro, depois retome:

```bash
env -u AWI_SEMANTIC_MODEL \
  awi --index-dir /tmp/awi-writer reconcile /path/to/workspace --json
awi --index-dir /tmp/awi-writer semantic-build --resume \
  --publish-dir /shared/awi-publication --json
```

O modo de retomada ainda re-hash cada fonte contra o SQLite e reutiliza apenas pares `(file_id, generation)` exatos. O selamento executa a mesma verificação de cobertura completa e poda pares obsoletos antes da publicação.

A construção usa chunks sensíveis ao tokenizador e à estrutura, limitados a 480 tokens de modelo, mantém no máximo quatro chunks por arquivo, aplica a instrução de consulta Harrier apenas a consultas e normaliza embeddings L2. Conteúdo de origem, texto, semi-estruturado e tabular é elegível; arquivos sensíveis ou somente de metadados e memória do Agente são excluídos. Um arquivo elegível não-zero cuja carga útil extraída está vazia recebe um vetor de cabeçalho de caminho e tipo para que a cobertura de construção e publicação permaneça idêntica; arquivos de zero bytes permanecem excluídos.

Construtores em massa podem definir `AWI_SEMANTIC_EMBED_WORKERS` acima de um. Instâncias extras do modelo GGUF são carregadas preguiçosamente apenas para embedding de documentos; o serviço de consulta mantém um único modelo. AWI divide `AWI_SEMANTIC_THREADS` entre esses trabalhadores, então defina-o para a cota total de CPU disponível ao sidecar.

Ciclos subsequentes do produtor atualizam apenas arquivos alterados. Um ciclo sem alteração valida a cobertura e avança apenas o manifesto semântico local; não publica um instantâneo NFS redundante. Antes da publicação, AWI requer cobertura LanceDB para cada `(file_id, generation)` elegível atual. O banco de dados semântico e seu manifesto são copiados para o mesmo instantâneo imutável que SQLite e Tantivy. Leitores rejeitam uma geração incompatível e retornam à pesquisa lexical quando o sidecar está indisponível. Desative `AWI_SEMANTIC_MODEL` para desativar a faixa semântica.

A recuperação semântica em tempo de consulta é executada simultaneamente com as faixas lexicais. O overfetch de vetor é limitado pelo número máximo de chunks por arquivo configurado, preservando a cobertura Top-K em nível de arquivo sem retornar candidatos de chunk redundantes. Leitores persistentes executam um aquecimento híbrido completo antes de servir consultas. O sidecar usa um sinal de morte do pai Linux, então parar o leitor também libera o modelo.

Gerações seladas usam um índice IVF_FLAT e sondam cada partição. Isso preserva os vetores Q8 normalizados originais e a ordem Top-K exata, evitando a perda de recall da quantização de produto no tamanho atual do corpus do AWI.

Para workspaces apoiados por NFS, construa índices mutáveis em armazenamento local e publique instantâneos imutáveis:

```bash
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace \
  --publish-dir /shared/awi-publication --json

awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

Quando um host cliente não tem cota de CPU suficiente para metas de latência GGUF, execute o leitor de instantâneos em um trabalhador de CPU e preserve o socket MCP local com o túnel stream-local de reconexão:

```bash
scripts/remote-reader-tunnel.sh \
  /tmp/local-awi.sock /tmp/remote-awi.sock <worker-ip> <ssh-port>
```

### Cadeia de Atualização Automática

Para manter a publicação compartilhada atual sem reconciliações manuais, execute o produtor persistente ao lado do leitor. O produtor reconcilia raízes e publica um novo instantâneo imutável apenas quando o conteúdo muda; o leitor que segue instantâneos muda atomicamente para cada nova geração em sua próxima solicitação. Isso fecha o loop de ponta a ponta: edite um arquivo e o leitor o reflete automaticamente.

O download do instantâneo e a validação da soma de verificação são executados em um trabalhador de atualização em segundo plano. As solicitações continuam contra a última geração válida enquanto um novo instantâneo é materializado, evitando pausas de atualização NFS no caminho da consulta. Uma desconexão ou falha de gravação de cliente individual é registrada sem terminar o daemon compartilhado.

Raízes de disco local são observadas em tempo real (inotify), então edições são publicadas dentro da janela de debounce. Raízes remotas (NFS e similares, detectadas via `/proc/mounts`) e um tique de segurança periódico a cada `--interval-ms` impulsionam o resto, porque eventos de sistema de arquivos não são confiáveis para gravações remotas. Se o observador não puder iniciar, o produtor degrada graciosamente para reconciliação puramente periódica. Eventos de acesso e somente metadados são ignorados para evitar varreduras auto-disparadas; atualizações pesadas de anexação de `.log`, `.jsonl`, `.ndjson`, `.csv`, `.tsv` e `.parquet` são adiadas para a passagem periódica em vez de reconstruir um instantâneo para cada gravação. Cada ciclo também redescobre fontes para projetos registrados por `awi memory`. Novas raízes de memória de Agentes são indexadas automaticamente, e raízes registradas que desaparecem são reconciliadas para que evidências excluídas não sejam servidas.

```bash
# Produtor: observar raízes locais ao vivo, reconciliar cada raiz no máximo a cada 5s,
# coalescer rajadas de edição em 500ms, publicar automaticamente na mudança, reter 3 gerações.
awi --index-dir /tmp/awi-writer watch \
  --publish-dir /shared/awi-publication \
  --interval-ms 5000 --debounce-ms 500 --retain 3

# Leitor: seguir a publicação e ativar automaticamente novas gerações.
awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

`watch` usa por padrão raízes não-memória registradas; projetos de memória registrados são redescobertos separadamente a cada ciclo. Passe `--root <path>` uma ou mais vezes para restringir o conjunto. A poda de retenção remove gerações mais antigas após cada publicação e nunca exclui a geração que o ponteiro referencia atualmente, então o diretório compartilhado não pode crescer sem limites.

Para Codex, `scripts/awi-mcp-snapshot-wrapper.sh` inicia ou reutiliza um daemon de instantâneo local antes de lançar o adaptador stdio. Configure `AWI_SNAPSHOT_SOURCE` e `AWI_MCP_AUDIT_LOG`, então registre o wrapper como um servidor MCP global. Dados de instantâneo mutáveis permanecem sob `AWI_RUNTIME_DIR`, enquanto o socket Unix e o bloqueio de inicialização padrão estão sob `~/.local/state/awi-$UID`, para que a limpeza de caches temporários não possa desvincular o socket de controle ativo. A materialização a frio aguarda até 10 minutos por padrão; substitua-a com `AWI_START_TIMEOUT_MS`.

### Integração de Agente com Um Comando

`awi integrate` detecta clientes de Agentes instalados e registra AWI com cada host MCP suportado em uma passagem:

```bash
# Pré-visualizar sem modificar a configuração.
awi integrate --project-root /path/to/workspace --dry-run --json

# Configurar cada cliente suportado detectado.
awi integrate --project-root /path/to/workspace

# Restringir a operação a clientes selecionados.
awi integrate --client codex,gemini,opencode,qwen,cline,zed,amazon-q,crush \
  --project-root /path/to/workspace
```

O comando é idempotente e relata um status por cliente: `configured`, `already_configured`, `would_configure`, `needs_attention`, `not_installed`, `unsupported` ou `failed`. O método de registro ou alvo de configuração para cada cliente suportado é:

| Cliente | Método de registro ou alvo |
|---|---|
| Codex | CLI oficial `codex mcp add`; persistido em `~/.codex/config.toml` |
| Gemini CLI | CLI oficial `gemini mcp add --scope user`; persistido em `~/.gemini/settings.json` |
| Claude Code | CLI oficial `claude mcp add --scope user`; persistido em `~/.claude.json` |
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers) | `~/.copilot/mcp-config.json` |
| TraeCode | `<project>/.trae/mcp.json` |
| Zcode | `~/.zcode/cli/config.json` em `mcp.servers` |
| Kimi Code | `$KIMI_CODE_HOME/mcp.json`, padrão `~/.kimi-code/mcp.json` |
| [OpenCode](https://opencode.ai/docs/en/mcp-servers/) | `$OPENCODE_CONFIG`, ou `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.json` |
| [Pi](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent) | `$PI_CODING_AGENT_DIR/mcp.json` através de [`pi-mcp-adapter`](https://pi.dev/packages/pi-mcp-adapter) |
| [Cursor](https://cursor.com/help/customization/mcp) | `~/.cursor/mcp.json` |
| [Windsurf](https://docs.windsurf.com/windsurf/cascade/mcp) | `~/.codeium/windsurf/mcp_config.json` |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/) | `~/.qwen/settings.json` |
| [Cline CLI](https://docs.cline.bot/mcp/mcp-overview) | `~/.cline/data/settings/cline_mcp_settings.json` (CLI atual); `~/.cline/mcp.json` (fallback legado somente IDE) |
| [Zed](https://zed.dev/docs/ai/mcp) | `${XDG_CONFIG_HOME:-~/.config}/zed/settings.json` em `context_servers` |
| [Amazon Q Developer](https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-mcp-configuration.html) | `~/.aws/amazonq/mcp.json` |
| [Crush](https://www.mintlify.com/charmbracelet/crush/configuration/mcp) | `${XDG_CONFIG_HOME:-~/.config}/crush/crush.json` |

Plugins são empacotamento opcional para clientes com suporte MCP nativo. Pi é a exceção: seu núcleo omite deliberadamente MCP, então uma extensão é necessária. Novas sessões carregam automaticamente as entradas de nível de usuário geradas. MCP em nível de projeto TraeCode deve ser habilitado uma vez nas configurações. Workspaces Gemini marcados como não confiáveis são relatados como `needs_attention` porque Gemini suprime todos os servidores MCP até que o usuário confie explicitamente no workspace.

Por padrão, AWI registra o binário atual como `awi --index-dir <absolute-path> mcp`, que é autossuficiente para um índice mutável local. Implantações de produção baseadas em instantâneos devem selecionar explicitamente seu wrapper com `--server-command`; opções `--server-arg` repetidas estão disponíveis para lançadores personalizados.

## Segurança

`workspace_query` aceita apenas uma instrução `SELECT` ou `WITH` somente leitura sobre entradas explícitas sob raízes registradas. Ele impõe limites de tempo limite, linhas e bytes de saída enquanto desativa o carregamento de extensões DuckDB e acesso externo.

Respostas de pesquisa e inspeção são limitadas. A descoberta de conhecimento do Agente é limitada a arquivos `AGENTS.md` ancestrais e manifestos `SKILL.md` sob diretórios conhecidos por cliente. A descoberta de memória é limitada a arquivos com curadoria mapeados para o projeto selecionado; históricos brutos requerem opt-in explícito. AWI nunca indexa um diretório home inteiro. Arquivos sensíveis, diretórios gerados, conteúdo superdimensionado e escapes de link simbólico são excluídos por padrão. Diretórios excluídos por padrão são `.git`, `.hg`, `.svn`, `.awi-index`, `node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.ipynb_checkpoints`, `.venv`, `.idea`, `.vscode` e `.cache`, mais `.codex-work` e `.worktrees` criados pelo Agente, junto com regras `.gitignore` e `.awiignore`. O produtor `watch` aplica as mesmas exclusões a eventos do sistema de arquivos, então a movimentação nesses diretórios nunca acorda uma reconciliação. Filtros de raiz de pesquisa aceitam uma raiz indexada ou um escopo pai existente contendo raízes indexadas. Raízes de consulta estruturada permanecem entradas exatas da lista de permissões.

O log de auditoria MCP é opcional. Quando habilitado, AWI grava registros JSONL privados (`0600`) contendo argumentos limitados e editados com credenciais, processo chamador, nome de cliente padronizado, ID de sessão opcional, marcador de chamada sintética, duração, resultado, detalhes do erro, bytes de resposta, bytes de carga útil de texto/estruturados, contagens de acertos/linhas e flags separados de truncamento de prévia e compactação de limite. O esquema v5 retém os campos de vínculo de pesquisa/inspeção v4 e adiciona `client_name`, `session_id` e `synthetic`. Defina metadados explícitos com `AWI_MCP_CLIENT_NAME`, `AWI_MCP_SESSION_ID` e `AWI_MCP_SYNTHETIC`; a inicialização MCP `clientInfo.name` substitui a inferência de processo. O log ativo gira em 64 MiB e retém um arquivo anterior. Relatórios de adoção de projeto usam [`eligible_session_adoption`](session-adoption.md), não todas as sessões de projeto como denominador.

## Avaliação

O relatório de avaliação atual somente para desenvolvimento:

- Recall@10: 1.0
- Pesquisa P95: 6.83 ms
- Taxa de resultados obsoletos: 0
- Redução de chamadas de ferramentas emparelhadas de Agente real: 68.4%
- Redução de pesquisa emparelhada de Agente real: 54.0%
- Redução de tempo de parede emparelhado de Agente real: 41.3%

Consulte [`evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md`](../evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md) para o protocolo, detalhamento por categoria, ressalvas e artefatos de reprodutibilidade. O conjunto de avaliação permanece um candidato de desenvolvimento aguardando revisão humana dupla; não é um benchmark de release congelado. Esses valores são diagnósticos, não alegações formais de precisão/recall. AWI não deve publicar P/R, Inspect@K ou MRR formais até que dois revisores distintos tenham rotulado independentemente o conjunto de consultas, as divergências sejam arbitradas e os rótulos aprovados e identidades dos revisores sejam registrados.

## Licença

Licenza Non Commerciale. Gratuita per uso personale, accademico e non profit. Qualsiasi forma di uso commerciale richiede consenso scritto preventivo. Vedere [LICENSE](../LICENSE) per i dettagli.
