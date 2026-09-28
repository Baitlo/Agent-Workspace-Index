# Indexation de la mémoire inter-agents

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · **Français** · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · [Bahasa Melayu](agent-memory.ms.md)

## Utilisation

```bash
# Mémoire conservée et résumés uniquement.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# Inclure explicitement l'historique brut de chat/session correspondant.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# Rechercher uniquement la mémoire associée au projet actuel.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

L'installateur exécute la forme conservée par défaut. Utilisez `--skip-agent-memory` pour la désactiver ou `--include-raw-memory` pour adhérer à l'historique brut.

## Sources

La découverte est limitée au projet canonique sélectionné :

| Agent | Sources conservées par défaut | Opt-in brut |
|---|---|---|
| Trae | profil utilisateur, mémoire de projet correspondante, résumés de sujets, résumés de sessions | rien au-delà de ces résumés |
| Codex | `MEMORY.md`, `memory_summary.md`, résumés de déploiement correspondants | JSONL bruts de déploiement liés et `raw_memories.md` |
| Zcode | racine de mémoire mappée au projet par les métadonnées d'exécution Zcode | fichiers de déploiement/agent correspondants |
| Gemini CLI | aucune | historique de projet mappé et répertoires de chats |
| Claude Code | répertoire de mémoire de projet correspondant | fichiers de session identifiant le projet |

Les répertoires de sessions Argos/SRE sont intentionnellement exclus car ils nécessitent le flux de travail de diagnostic Argos plutôt que l'indexation en masse de fichiers. AWI ne scanne jamais l'intégralité du répertoire personnel.

## Métadonnées et classement

Chaque résultat de mémoire inclut :

- l'agent source ;
- la couche : `user_profile`, `project_summary`, `topic_summary`, `session_summary`, `memory_note` ou `raw_history` ;
- le YAML frontmatter `name` et `description` optionnels ;
- la racine de l'espace de travail canonique et la clé de projet du fournisseur, quand disponibles ;
- l'ID de session quand il peut être dérivé ;
- le temps d'observation et le drapeau d'historique brut.

Quand `context_path` ou un filtre de racine de projet est fourni, la mémoire d'un autre projet est rejetée. La mémoire du projet exact se classe avant la mémoire globale. Les correspondances exactes de nom de fichier et de `name` frontmatter reçoivent le plus fort boost de métadonnées ; le chevauchement de description fournit un boost plus faible. Les requêtes en forme d'identifiant rétrogradent les résumés larges `MEMORY.md`/projet, sauf si le résumé correspond exactement à l'entité. L'ordre normal des couches reste inchangé pour les requêtes larges, les résumés conservés se classent avant l'historique brut, et la récence n'est qu'un petit tie-breaker. Les copies identiques en contenu de différents agents sont réduites après le classement. La récupération de mémoire est activée avec `--kind agent_memory`. Les documents de mémoire utilisent un index Tantivy dédié, donc leur vocabulaire ne peut pas changer les statistiques IDF ou le classement ordinaire du code/données.

## Analyse et sécurité

La mémoire Markdown est indexée comme du texte borné. La mémoire JSON/JSONL est normalisée vers des champs pertinents pour l'humain, tels que l'intention, les actions, le résultat, les faits appris, le rôle, le message et le contenu ; les IDs de transport et les métadonnées de résumé internes sont omis du document de recherche. La mémoire JSONL n'est pas envoyée au profilage DuckDB.

Les vérifications normales de taille, UTF-8, ignore et de noms de fichiers sensibles s'appliquent toujours. Les identifiants ou clés privées à haute confiance rendent le fichier de mémoire entier métadonnées seules. L'historique brut est désactivé sauf demande explicite, et les fichiers bruts surdimensionnés restent métadonnées seules sous la limite de contenu configurée.

`awi memory` persiste la racine de projet canonique et la politique d'historique brut. Chaque cycle de producteur redécouvre les sources agent de ce projet avant de publier, de sorte que les nouveaux répertoires de projet de fournisseur apparaissent sans réinstaller AWI. Les racines précédemment enregistrées sont également réconciliées lorsqu'elles disparaissent, empêchant la mémoire supprimée de rester consultable. Les résumés JSONL ajoutés sont rafraîchis lors de la prochaine réconciliation périodique ; les fichiers bruts à forte intensité d'ajout ne déclenchent pas de reconstruction de snapshot à chaque écriture.

`awi status --json` expose un objet `memory` avec les comptes de projets/sources enregistrés, les fichiers actifs, la génération de mémoire la plus récente et le retard de génération relatif, les `stale_files`/`missing_files` du système de fichiers, le retard de source maximal et l'âge de la source la plus ancienne. Les comptes de fichiers obsolètes et manquants du système de fichiers sont les signaux d'actualité faisant autorité ; un faible retard de génération seul ne prouve pas que la mémoire externe est à jour.

## Validation de développement

Un exécution Bona isolée a découvert 35 racines pertinentes au projet et indexé 489 fichiers conservés de Codex, Trae et Zcode avec zéro échec d'extraction ; les chats bruts de Gemini sont restés exclus. L'ensemble existant de 16 cas de récupération code/données est resté stable bit-à-bit à Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950` et taux d'obsolescence `0`. Le P95 de build de version était de `24.06 ms`. Cet ensemble est toujours `candidate_pending_dual_review`, pas une porte de version figée. Ne rapportez pas de précision/rappel formel, Inspect@K ou MRR tant que deux réviseurs distincts n'ont pas étiqueté indépendamment l'ensemble de requêtes et arbitré les désaccords.
