# Indexation des connaissances de l'agent

[English](agent-knowledge.md) · [中文](agent-knowledge.zh.md) · [繁體中文](agent-knowledge.zh-TW.md) · [日本語](agent-knowledge.ja.md) · [한국어](agent-knowledge.ko.md) · [Русский](agent-knowledge.ru.md) · **Français** · [Deutsch](agent-knowledge.de.md) · [Português](agent-knowledge.pt.md) · [Español](agent-knowledge.es.md) · [العربية](agent-knowledge.ar.md) · [Italiano](agent-knowledge.it.md) · [Ελληνικά](agent-knowledge.el.md) · [ไทย](agent-knowledge.th.md) · [Bahasa Melayu](agent-knowledge.ms.md)

L'historique du projet inter-agents est traité séparément en tant que `agent_memory` ; voir [Indexation de la mémoire inter-agents](agent-memory.md).

## Types de documents

| Fichier | Type AWI | Métadonnées structurées |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | répertoire de portée, profondeur de priorité, titres, références locales |
| `SKILL.md` | `agent_skill` | `name` et `description` YAML, titres, références locales |

Le texte Markdown complet et limité reste consultable. `workspace_inspect` renvoie les métadonnées structurées ainsi que les métadonnées de fichier normales et l'extrait de contenu.

## Découverte

Une réconciliation d'espace de travail normale indexe les fichiers `AGENTS.md` et `SKILL.md` sous cet espace de travail, sous réserve de `.gitignore`, `.awiignore` et des exclusions par défaut d'AWI.

L'installateur de première exécution découvre en outre :

- les fichiers `AGENTS.md` dans les répertoires ancêtres au-dessus de l'espace de travail sélectionné ;
- les manifestes `SKILL.md` sous les répertoires de projet et d'utilisateur connus pour les clients d'agent pris en charge.

Chaque document externe est enregistré en tant que racine de fichier unique. AWI n'indexe pas un répertoire personnel entier ou le contenu complet des packages Skill globaux. `--skip-agent-knowledge` désactive cette découverte supplémentaire.

Les liens Markdown locaux d'un document d'agent ne sont enregistrés que lorsque leur cible existe et reste à l'intérieur de la racine du document. Les fichiers référencés sont consultables lorsqu'ils sont déjà couverts par une racine d'espace de travail ; sinon, le chemin de référence est renvoyé en tant que métadonnées pour une inspection explicite avec un autre outil de fichier.

## Portée et classement

Passez `context_path` à `workspace_search` lors de la résolution des instructions du dépôt :

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI exclut les fichiers `AGENTS.md` dont la portée n'est pas un ancêtre du chemin de contexte. Les fichiers applicables sont classés par profondeur de portée afin que les instructions les plus proches arrivent en premier. Après ce classement, les documents d'agent au contenu identique sont réduits, ce qui supprime les doublons de Skill et de worktree copiés sans perdre l'instruction applicable la plus proche.

Sans filtre de type, les instructions Agent et les Skills sont recherchés avec le code et les données. Utilisez `--kind agent_skill` ou `--kind agent_instructions` uniquement pour limiter la recherche à un type de document Agent. `context_path` applique le filtrage et le classement par portée des instructions.

## Sécurité

Les documents d'agent restent soumis aux vérifications normales de taille, UTF-8, liens symboliques et noms de fichiers sensibles. Les modèles de clés privées et de jetons de fournisseur à haute confiance dans leur contenu provoquent un résultat uniquement de métadonnées avec `metadata_only_sensitive_content` ; le contenu, l'aperçu et les métadonnées d'agent analysées ne sont pas stockés.

Un frontmatter Skill mal formé est enregistré comme un échec d'extraction, tandis que le nouveau contenu Markdown remplace toujours tout texte indexé plus ancien. Cela évite de servir du contenu obsolète lorsque seule l'analyse des métadonnées structurées échoue.

## Validation de développement

Une installation isolée sur l'environnement Bona actuel a découvert 136 documents d'agent canoniques avec zéro échec d'extraction. Les requêtes de fumée ciblées ont sélectionné `wukong-dag-failure-debugger` en premier pour une requête d'échec Wukong DAG/LogID et le `AGENTS.md` Bona pour un contexte source AWI.

Sur l'ensemble de récupération de développement existant de 16 cas, l'ajout de ces documents d'agent a maintenu Recall@10 à `1.0` et le taux de résultats obsolètes à `0` ; le MRR est passé de `0.5975` à `0.6027`, le nDCG@10 de `0.6922` à `0.6969`, et le P95 de build de version de `8,07 ms` à `25,88 ms`. La latence reste inférieure à l'objectif de recherche hybride de `150 ms`. Cet ensemble de développement reste en attente de double examen et n'est pas un benchmark de version figé.
