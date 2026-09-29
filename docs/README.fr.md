# AWI: Agent Workspace Index

[English](../README.md) · [中文](README.zh.md) · [繁體中文](README.zh-TW.md) · [日本語](README.ja.md) · [한국어](README.ko.md) · [Русский](README.ru.md) · **Français** · [Deutsch](README.de.md) · [Português](README.pt.md) · [Español](README.es.md) · [العربية](README.ar.md) · [Italiano](README.it.md) · [Ελληνικά](README.el.md) · [ไทย](README.th.md) · [Bahasa Melayu](README.ms.md)

AWI est un index unifié local-first pour le code, les données et les connaissances opérationnelles des agents. Il indexe le code source, SQL, les documents, les journaux, JSON/JSONL, CSV/TSV, Parquet, `AGENTS.md`, les compétences d'agent et la mémoire inter-agents à l'échelle du projet via une interface CLI et Model Context Protocol (MCP) unique et limitée.

Une seule commande connecte cet index à 16 harnais d'agents de codage, dont Codex, Claude Code, Gemini CLI, GitHub Copilot CLI, OpenCode, Qwen Code, Cline, Zed, Amazon Q Developer et Crush.

## Fonctionnalités

- Recherche hybride Tantivy sur les chemins, le texte, les symboles et les schémas de jeux de données.
- Extraction de symboles Tree-sitter pour Rust, Python et Go.
- Requêtes DuckDB intégrées en lecture seule sur des fichiers explicitement autorisés.
- Récupération `AGENTS.md` sensible à la portée et métadonnées structurées `SKILL.md`.
- Découverte de mémoire sensible au projet à travers Trae, Codex, Zcode, Gemini et Claude.
- Vérifications du catalogue SQLite qui rejettent les résultats de recherche obsolètes.
- Chemins de mise à jour `notify` et `reconcile` sécurisés pour NFS.
- Instantanés de génération immuables avec activation atomique du démon.
- Outils MCP : `workspace_search`, `workspace_inspect` et `workspace_query`.

Voir [Indexation des connaissances des agents](agent-knowledge.md) pour la découverte, la portée, le classement, la déduplication et la sémantique de sécurité. Voir [Indexation de la mémoire inter-agents](agent-memory.md) pour les sources de mémoire et la limite de l'historique brut.

## Binaires Linux et macOS précompilés

GitHub Releases fournit des binaires natifs pour :

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Installez et configurez AWI pour le dépôt actuel en une seule commande :

```bash
curl -fsSL \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh |
  bash -s -- --workspace "$PWD"
```

L'installateur détecte l'architecture Linux, télécharge l'archive de version et `SHA256SUMS`, vérifie la somme de contrôle, installe le binaire, met à jour une section AWI gérée dans le `AGENTS.md` de niveau supérieur de l'espace de travail, construit l'index initial, découvre les connaissances des agents et la mémoire de projet conservée, et enregistre chaque client d'agent pris en charge détecté. Le contenu `AGENTS.md` existant est préservé et la ré-exécution de l'installateur remplace plutôt que de dupliquer la section gérée.

Pour les environnements qui nécessitent l'examen d'un script téléchargé avant exécution :

```bash
curl -fsSLO \
  https://raw.githubusercontent.com/Baitlo/Agent-Workspace-Index/main/scripts/install-release.sh
less install-release.sh
bash install-release.sh --workspace /absolute/path/to/your/repository
```

Omettez `--workspace` pour installer uniquement le binaire `awi`. Épinglez une version avec `--version v0.2.0`, choisissez une autre destination avec `--bin-dir`, restreignez les clients avec `--clients`, ou désactivez le bloc d'instructions géré avec `--skip-agent-instructions`. Les binaires Linux sont construits nativement sur des runners hébergés par GitHub Ubuntu 22.04 et nécessitent une glibc et `libstdc++` compatibles. Les binaires macOS sont construits nativement sur des runners macOS 15 pour Apple Silicon et Intel.

La récupération sémantique reste optionnelle. Elle nécessite en outre Python, LanceDB, `llama-cpp-python` et un modèle d'intégration GGUF compatible. Les mainteneurs doivent suivre la [checklist de version](releasing.md).

## Installation assistée par agent

Donnez à votre agent de codage l'[invite d'installation recommandée](agent-install-prompt.md), ou exécutez l'installateur depuis un checkout AWI :

```bash
bash scripts/install.sh --workspace /absolute/path/to/your/repository
```

La première exécution construit et installe `awi`, met à jour les instructions AWI gérées de l'espace de travail, crée un index local en dehors de l'espace de travail, indexe les fichiers `AGENTS.md` ancêtres et les manifestes `SKILL.md` trouvés dans les répertoires d'agents autorisés, indexe la mémoire conservée associée à cet espace de travail, détecte les clients d'agents installés et enregistre le serveur MCP AWI auprès de chaque client pris en charge. L'opération est idempotente. Passez `--skip-agent-instructions`, `--skip-agent-knowledge` ou `--skip-agent-memory` pour désactiver ces étapes. Les chats bruts restent exclus à moins que `--include-raw-memory` ne soit fourni. Rust et Cargo ne sont requis que lors de la construction à partir des sources.

Si Pi est détecté, l'installateur installe également le `pi-mcp-adapter@2.36.0` épinglé, car Pi n'a intentionnellement pas de client MCP intégré. Il s'agit d'un package Pi tiers ; passez `--skip-pi-adapter` pour l'examiner ou l'installer séparément, ou définissez `AWI_PI_MCP_ADAPTER_SPEC` pour sélectionner une autre version examinée. Exécutez `scripts/install.sh --help` pour les options binaires, d'index et de client personnalisées.

## Construction et test

```bash
export CARGO_TARGET_DIR=/tmp/awi-target
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
python3 -m unittest tests/test_semantic_sidecar.py
python3 tests/test_audit_session_adoption.py
```

## Utilisation de base

```bash
# Construire ou rafraîchir un index.
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace --json

# Exécuter le démon persistant.
awi --index-dir /tmp/my-awi-index serve

# Rechercher et inspecter.
awi --index-dir /tmp/my-awi-index search "workspace query" --limit 10 --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file \
  --symbol exact_function_name --json

# Résoudre les instructions applicables à un chemin d'espace de travail concret.
awi --index-dir /tmp/my-awi-index search "build and test rules" \
  --kind agent_instructions --context-path /path/to/workspace/src/lib.rs --json

# Indexer et rechercher un manifeste de compétence global autonome.
awi --index-dir /tmp/my-awi-index reconcile ~/.agents/skills/example/SKILL.md --json
awi --index-dir /tmp/my-awi-index search "diagnose deployment failures" \
  --kind agent_skill --json

# Découvrir la mémoire conservée pour ce projet et la rechercher.
awi --index-dir /tmp/my-awi-index memory --project-root /path/to/workspace
awi --index-dir /tmp/my-awi-index search "previous rollout decision" \
  --kind agent_memory --context-path /path/to/workspace --json

# Démarrer l'adaptateur MCP stdio.
awi --index-dir /tmp/my-awi-index mcp

# Persister les appels d'outils MCP réels pour une analyse de récupération ultérieure.
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

Les recherches sans `--kind` couvrent tous les types indexés : code, texte, données structurées, instructions Agent, Skills et mémoire liée au projet. Fournir `--kind` restreint les résultats ; par exemple, `--kind agent_memory` recherche uniquement la mémoire. La mémoire utilise un index Tantivy séparé, donc son vocabulaire ne modifie pas les statistiques IDF ordinaires du code et des données. La recherche MCP utilise la forme compacte `compact_v3` avec un aperçu de 1 000 caractères, un `file_id` stable, un `search_id` à portée de requête et des métadonnées de définition pour les correspondances de symboles exactes. Elle retourne par défaut 5 résultats et compresse les requêtes au-dessus de 20 à 20. Passez le `search_id` retourné et le `symbol` exact optionnel à `workspace_inspect` pour lier l'inspection à sa récupération et centrer l'extrait sur la définition. Commencez par une requête riche en identifiants au lieu de recherches parallèles de quasi-synonymes, et n'étendez que lorsque le premier ensemble de résultats manque de preuves. Lorsque le chemin exact est déjà connu, lisez-le directement avec les outils de fichiers de l'hôte. Les extraits de recherche sont générés à partir de fenêtres source stockées limitées, et un reclassement léger de diversité de répertoires empêche un dossier d'artefacts de remplir l'ensemble des résultats. Le serveur stdio prend en charge le `ping` MCP hérité standard et limite la négociation basée sur l'initialisation au protocole `2025-11-25`.

### Récupération sémantique

AWI peut ajouter une voie sémantique Harrier GGUF Q8 sans changer l'interface des outils MCP. Les intégrations de documents sont calculées pendant `reconcile`, `notify` ou un `semantic-build` initial ; les recherches normales ne calculent que l'intégration de la requête. SQLite reste autoritaire, et les candidats sémantiques dont la génération de fichier n'est pas actuelle sont rejetés.

Créez un environnement Python dédié et activez le sidecar :

```bash
python3 -m venv ~/.cache/awi/semantic-venv
~/.cache/awi/semantic-venv/bin/pip install -r requirements-semantic.txt

export AWI_SEMANTIC_MODEL=/path/to/harrier-oss-v1-270M-Q8_0.gguf
export AWI_SEMANTIC_MODEL_SHA256=fe12f3583dbbb832def4cffeb46c0d0ab49a3288542d5cdbaeb1741315a01b87
export AWI_SEMANTIC_PYTHON="$HOME/.cache/awi/semantic-venv/bin/python"
export AWI_SEMANTIC_THREADS=16
export AWI_SEMANTIC_EMBED_WORKERS=1
```

Pour un catalogue existant, précalculez chaque fichier éligible une fois :

```bash
awi --index-dir /tmp/awi-writer semantic-build \
  --publish-dir /shared/awi-publication --json
```

Une construction en masse interrompue peut réutiliser les générations de fichiers complètes déjà validées dans LanceDB. Si le contenu de l'espace de travail a changé pendant la tentative échouée, réconciliez d'abord le catalogue sans publier, puis reprenez :

```bash
env -u AWI_SEMANTIC_MODEL \
  awi --index-dir /tmp/awi-writer reconcile /path/to/workspace --json
awi --index-dir /tmp/awi-writer semantic-build --resume \
  --publish-dir /shared/awi-publication --json
```

Le mode de reprise re-hashe toujours chaque source par rapport à SQLite et ne réutilise que les paires exactes `(file_id, generation)`. Le scellement effectue la même vérification de couverture complète et élague les paires obsolètes avant publication.

La construction utilise des morceaux sensibles au tokeniseur et à la structure, plafonnés à 480 tokens de modèle, conserve au maximum quatre morceaux par fichier, applique l'instruction de requête Harrier uniquement aux requêtes et normalise L2 les intégrations. Le contenu source, texte, semi-structuré et tabulaire est éligible ; les fichiers sensibles ou uniquement des métadonnées et la mémoire de l'agent sont exclus. Un fichier éligible non nul dont la charge utile extraite est vide reçoit un vecteur d'en-tête de chemin et de type, de sorte que la couverture de construction et de publication reste identique ; les fichiers de zéro octet restent exclus.

Les constructeurs en masse peuvent définir `AWI_SEMANTIC_EMBED_WORKERS` au-dessus de un. Des instances de modèle GGUF supplémentaires sont chargées paresseusement pour l'intégration de documents uniquement ; le service de requêtes conserve un seul modèle. AWI divise `AWI_SEMANTIC_THREADS` entre ces workers, donc définissez-le sur le quota CPU total disponible pour le sidecar.

Les cycles de producteur ultérieurs ne mettent à jour que les fichiers modifiés. Un cycle sans changement valide la couverture et n'avance que le manifeste sémantique local ; il ne publie pas d'instantané NFS redondant. Avant publication, AWI exige une couverture LanceDB pour chaque paire `(file_id, generation)` éligible actuelle. La base de données sémantique et son manifeste sont copiés dans le même instantané immuable que SQLite et Tantivy. Les lecteurs rejettent une génération incompatible et retombent à la recherche lexicale lorsque le sidecar est indisponible. Désactivez `AWI_SEMANTIC_MODEL` pour désactiver la voie sémantique.

La récupération sémantique au moment de la requête s'exécute en même temps que les voies lexicales. Le suréchantillonnage vectoriel est limité par le nombre maximum de morceaux configuré par fichier, préservant la couverture Top-K au niveau du fichier sans retourner de candidats de morceaux redondants. Les lecteurs persistants exécutent un échauffement hybride complet avant de servir les requêtes. Le sidecar utilise un signal de mort parent Linux, donc l'arrêt du lecteur libère également le modèle.

Les générations scellées utilisent un index IVF_FLAT et sondent chaque partition. Cela conserve les vecteurs Q8 normalisés d'origine et l'ordre exact Top-K tout en évitant la perte de rappel de la quantification de produit à la taille actuelle du corpus d'AWI.

Pour les espaces de travail soutenus par NFS, construisez des index mutables sur le stockage local et publiez des instantanés immuables :

```bash
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace \
  --publish-dir /shared/awi-publication --json

awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

Lorsqu'un hôte client manque de quota CPU suffisant pour les cibles de latence GGUF, exécutez le lecteur d'instantanés sur un worker CPU et conservez le socket MCP local avec le tunnel stream-local reconnectant :

```bash
scripts/remote-reader-tunnel.sh \
  /tmp/local-awi.sock /tmp/remote-awi.sock <worker-ip> <ssh-port>
```

### Chaîne de mise à jour automatique

Pour maintenir la publication partagée à jour sans réconciliations manuelles, exécutez le producteur persistant aux côtés du lecteur. Le producteur réconcilie les racines et publie un nouvel instantané immuable uniquement lorsque le contenu a changé ; le lecteur suivant les instantanés bascule atomiquement vers chaque nouvelle génération à sa requête suivante. Cela boucle la boucle de bout en bout : modifiez un fichier, et le lecteur le reflète automatiquement.

Le téléchargement d'instantanés et la validation de somme de contrôle s'exécutent sur un worker de rafraîchissement en arrière-plan. Les requêtes continuent contre la dernière génération valide pendant qu'un nouvel instantané est matérialisé, évitant les pauses de rafraîchissement NFS sur le chemin des requêtes. Une déconnexion ou un échec d'écriture d'un client individuel est enregistré sans terminer le démon partagé.

Les racines de disque local sont surveillées en temps réel (inotify), donc les modifications sont publiées dans la fenêtre de débouncement. Les racines distantes (NFS et similaires, détectées via `/proc/mounts`) et un tic de sécurité périodique tous les `--interval-ms` entraînent le reste, car les événements du système de fichiers ne sont pas fiables pour les écritures distantes. Si le surveillant ne peut pas démarrer, le producteur dégrade proprement vers une réconciliation purement périodique. Les événements d'accès et de métadonnées uniquement sont ignorés pour éviter les analyses auto-déclenchées ; les mises à jour `.log`, `.jsonl`, `.ndjson`, `.csv`, `.tsv` et `.parquet` à forte intensité d'ajout sont différées au passage périodique au lieu de reconstruire un instantané pour chaque écriture. Chaque cycle redécouvre également les sources pour les projets enregistrés par `awi memory`. Les nouvelles racines de mémoire d'agent sont indexées automatiquement, et les racines enregistrées qui disparaissent sont réconciliées afin que les preuves supprimées ne soient pas servies.

```bash
# Producteur : surveiller les racines locales en direct, réconcilier chaque racine au maximum toutes les 5 s,
# coalescer les rafales d'édition sur 500 ms, publier automatiquement sur changement, conserver 3 générations.
awi --index-dir /tmp/awi-writer watch \
  --publish-dir /shared/awi-publication \
  --interval-ms 5000 --debounce-ms 500 --retain 3

# Lecteur : suivre la publication et activer automatiquement les nouvelles générations.
awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

`watch` utilise par défaut les racines non-mémoire enregistrées ; les projets de mémoire enregistrés sont redécouverts séparément à chaque cycle. Passez `--root <path>` une ou plusieurs fois pour restreindre l'ensemble. L'élagage de rétention supprime les générations plus anciennes après chaque publication et ne supprime jamais la génération actuellement référencée par le pointeur, de sorte que le répertoire partagé ne peut pas croître sans limite.

Pour Codex, `scripts/awi-mcp-snapshot-wrapper.sh` démarre ou réutilise un démon d'instantanés local avant de lancer l'adaptateur stdio. Configurez `AWI_SNAPSHOT_SOURCE` et `AWI_MCP_AUDIT_LOG`, puis enregistrez le wrapper en tant que serveur MCP global. Les données d'instantané mutables restent sous `AWI_RUNTIME_DIR`, tandis que le socket Unix et le verrou de démarrage par défaut sous `~/.local/state/awi-$UID` afin que le nettoyage des caches temporaires ne puisse pas délier le socket de contrôle en direct. La matérialisation à froid attend jusqu'à 10 minutes par défaut ; remplacez-la par `AWI_START_TIMEOUT_MS`.

### Intégration d'agent en une commande

`awi integrate` détecte les clients d'agents installés et enregistre AWI auprès de chaque hôte MCP pris en charge en un seul passage :

```bash
# Aperçu sans modifier la configuration.
awi integrate --project-root /path/to/workspace --dry-run --json

# Configurer chaque client pris en charge détecté.
awi integrate --project-root /path/to/workspace

# Restreindre l'opération aux clients sélectionnés.
awi integrate --client codex,gemini,opencode,qwen,cline,zed,amazon-q,crush \
  --project-root /path/to/workspace
```

La commande est idempotente et rapporte un statut par client : `configured`, `already_configured`, `would_configure`, `needs_attention`, `not_installed`, `unsupported` ou `failed`. La méthode d'enregistrement ou la cible de configuration pour chaque client pris en charge est :

| Client | Méthode d'enregistrement ou cible |
|---|---|
| Codex | CLI officiel `codex mcp add` ; persisté dans `~/.codex/config.toml` |
| Gemini CLI | CLI officiel `gemini mcp add --scope user` ; persisté dans `~/.gemini/settings.json` |
| Claude Code | CLI officiel `claude mcp add --scope user` ; persisté dans `~/.claude.json` |
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers) | `~/.copilot/mcp-config.json` |
| TraeCode | `<project>/.trae/mcp.json` |
| Zcode | `~/.zcode/cli/config.json` à `mcp.servers` |
| Kimi Code | `$KIMI_CODE_HOME/mcp.json`, par défaut `~/.kimi-code/mcp.json` |
| [OpenCode](https://opencode.ai/docs/en/mcp-servers/) | `$OPENCODE_CONFIG`, ou `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.json` |
| [Pi](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent) | `$PI_CODING_AGENT_DIR/mcp.json` via [`pi-mcp-adapter`](https://pi.dev/packages/pi-mcp-adapter) |
| [Cursor](https://cursor.com/help/customization/mcp) | `~/.cursor/mcp.json` |
| [Windsurf](https://docs.windsurf.com/windsurf/cascade/mcp) | `~/.codeium/windsurf/mcp_config.json` |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/) | `~/.qwen/settings.json` |
| [Cline CLI](https://docs.cline.bot/mcp/mcp-overview) | `~/.cline/data/settings/cline_mcp_settings.json` (CLI actuel) ; `~/.cline/mcp.json` (repli hérité IDE uniquement) |
| [Zed](https://zed.dev/docs/ai/mcp) | `${XDG_CONFIG_HOME:-~/.config}/zed/settings.json` à `context_servers` |
| [Amazon Q Developer](https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-mcp-configuration.html) | `~/.aws/amazonq/mcp.json` |
| [Crush](https://www.mintlify.com/charmbracelet/crush/configuration/mcp) | `${XDG_CONFIG_HOME:-~/.config}/crush/crush.json` |

Les plugins sont un packaging optionnel pour les clients avec prise en charge MCP native. Pi est l'exception : son cœur omet délibérément MCP, donc une extension est requise. Les nouvelles sessions chargent automatiquement les entrées de niveau utilisateur générées. Le MCP au niveau projet TraeCode doit être activé une fois dans les paramètres. Les espaces de travail Gemini marqués comme non fiables sont rapportés comme `needs_attention` car Gemini supprime tous les serveurs MCP jusqu'à ce que l'utilisateur fasse explicitement confiance à l'espace de travail.

Par défaut, AWI enregistre le binaire actuel comme `awi --index-dir <absolute-path> mcp`, ce qui est autonome pour un index mutable local. Les déploiements de production basés sur des instantanés doivent explicitement sélectionner leur wrapper avec `--server-command` ; des options `--server-arg` répétées sont disponibles pour les lanceurs personnalisés.

## Sécurité

`workspace_query` n'accepte qu'une seule instruction `SELECT` ou `WITH` en lecture seule sur des entrées explicites sous des racines enregistrées. Il applique des limites de délai d'attente, de lignes et d'octets de sortie tout en désactivant le chargement des extensions DuckDB et l'accès externe.

Les réponses de recherche et d'inspection sont limitées. La découverte des connaissances des agents est limitée aux fichiers `AGENTS.md` ancêtres et aux manifestes `SKILL.md` sous des répertoires connus par client. La découverte de la mémoire est limitée aux fichiers conservés mappés au projet sélectionné ; les historiques bruts nécessitent une adhésion explicite. AWI n'indexe jamais un répertoire personnel entier. Les fichiers sensibles, les répertoires générés, le contenu surdimensionné et les échappements de liens symboliques sont exclus par défaut. Les répertoires exclus par défaut sont `.git`, `.hg`, `.svn`, `.awi-index`, `node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.ipynb_checkpoints`, `.venv`, `.idea`, `.vscode` et `.cache`, plus les `.codex-work` et `.worktrees` créés par l'agent, ainsi que les règles `.gitignore` et `.awiignore`. Le producteur `watch` applique les mêmes exclusions aux événements du système de fichiers, de sorte que le brassage dans ces répertoires ne réveille jamais une réconciliation. Les filtres de racine de recherche acceptent soit une racine indexée, soit une portée parent existante contenant des racines indexées. Les racines de requêtes structurées restent des entrées de liste d'autorisation exactes.

La journalisation d'audit MCP est optionnelle. Lorsqu'elle est activée, AWI écrit des enregistrements JSONL privés (`0600`) contenant des arguments limités et rédigés pour les informations d'identification, le processus appelant, le nom de client standardisé, l'ID de session optionnel, le marqueur d'appel synthétique, la durée, le résultat, les détails de l'erreur, les octets de réponse, les octets de charge utile texte/structurée, les comptes de résultats/lignes, ainsi que des indicateurs séparés de troncature d'aperçu et de compactage de limite. Le schéma v5 conserve les champs de liaison recherche/inspection v4 et ajoute `client_name`, `session_id` et `synthetic`. Définissez des métadonnées explicites avec `AWI_MCP_CLIENT_NAME`, `AWI_MCP_SESSION_ID` et `AWI_MCP_SYNTHETIC` ; l'initialisation MCP `clientInfo.name` remplace l'inférence de processus. Le journal actif tourne à 64 Mio et conserve un fichier précédent. Les rapports d'adoption de projet utilisent [`eligible_session_adoption`](session-adoption.md), pas toutes les sessions de projet comme dénominateur.

## Évaluation

Le rapport d'évaluation actuel réservé au développement :

- Recall@10 : 1.0
- Recherche P95 : 6,83 ms
- Taux de résultats obsolètes : 0
- Réduction des appels d'outils appariés par agent réel : 68,4 %
- Réduction des recherches appariées par agent réel : 54,0 %
- Réduction du temps de paroi apparié par agent réel : 41,3 %

Voir [`evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md`](../evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md) pour le protocole, la répartition par catégorie, les mises en garde et les artefacts de reproductibilité. L'ensemble d'évaluation reste un candidat de développement en attente d'un double examen humain ; ce n'est pas un benchmark de version figé. Ces valeurs sont des diagnostics, pas des affirmations formelles de précision/rappel. AWI ne doit pas publier de P/R, Inspect@K ou MRR formels tant que deux réviseurs distincts n'ont pas étiqueté indépendamment l'ensemble de requêtes, que les désaccords ne sont pas arbitrés et que les étiquettes approuvées et les identités des réviseurs ne sont pas enregistrées.

## Licence

Licence non commerciale. Gratuit pour un usage personnel, académique et non lucratif. Toute utilisation commerciale nécessite un accord écrit préalable. Voir [LICENSE](../LICENSE) pour plus de détails.
