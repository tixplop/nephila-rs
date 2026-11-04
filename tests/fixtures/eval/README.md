# Banc d'évaluation

Ce banc sert à développer nephila, pas à l'utiliser. Il mesure la qualité des résumés et des réponses de `query ask` sur un petit dépôt figé (`corpus/`, 20 questions dans `questions.json`). Il ne juge rien : il écrit un rapport JSON que l'on compare d'une version ou d'un modèle à l'autre.

**Chaque passe fait des appels réels payants** vers le fournisseur configuré. Elle n'est jamais lancée par `cargo test`, la CI ni la release.

## Lancer une passe

Le test ignoré `eval_passe` lance le binaire `nephila` sur une copie du corpus :

```
rm -rf .test-data && mkdir .test-data && TMPDIR=$PWD/.test-data NEPHILA_EVAL_CONFIG=<config> \
  timeout -k 10 1860 cargo nextest run --release --cargo-quiet --color never --test-threads 1 \
  --run-ignored only --success-output final eval_passe
```

Variables :

- `NEPHILA_EVAL_CONFIG` (obligatoire) : fichier de configuration nephila de la passe (fournisseur, modèle, `api_key_env`).
- `NEPHILA_EVAL_OUT` (facultatif) : chemin du rapport, écrasé s'il existe. Défaut : `target/eval/<version>.json`.
- `NEPHILA_EVAL_JUDGE_CONFIG` (facultatif) : configuration du juge, distincte de celle de la passe, pour que le même juge note deux modèles comparés. Absente, il n'y a pas de juge et pas d'appel de plus. Illisible, la passe échoue avant le premier `build`.

## Coût

Environ un appel d'enrichissement par fichier du corpus (moins avec `batch_files`), plus les nouveaux essais de format, plus 20 appels pour les questions, plus au plus 20 appels pour le juge. Bornez la dépense avec `max_calls` dans la configuration de la passe.

## Rapport

Les clés sont triées, les nombres non entiers arrondis à 4 décimales, sans date ni durée ni texte de réponse : deux passes aux réponses identiques donnent le même fichier.

- `schema` : version du format du rapport (1).
- `version` : version de nephila.
- `provider`, `model` : fournisseur et modèle de la passe.
- `prompt_versions` : versions des prompts (`code`, `build`, `data`, `markdown`, `ask`).
- `corpus` : empreinte du banc, nombre de fichiers et de questions. Rapports comparables seulement à empreinte égale.
- `enrich` : code de sortie de `build`, nœuds demandés et couverts (`coverage`), appels réussis du premier coup (`first_try_rate`), appels, nouveaux essais, envois HTTP, jetons, longueur des résumés en mots.
- `ask` : par question, `recall` (attendus cités / attendus) et `precision` (attendus cités / cités), `status` (`ok`, `without_citation`, `not_found`, `error`), puis moyennes, compteurs, appels et jetons.
- `judge` : `null` sans juge ; sinon modèle, taille de l'échantillon (20 résumés au plus), note moyenne de 1 à 5, notes illisibles et note de chaque nœud.

Si `build` échoue, aucune question n'est posée : `ask` et `judge` valent `null` et la passe échoue.

## Comparer deux rapports

```
diff target/eval/ancien.json target/eval/nouveau.json
```

`jq` suffit pour extraire une clé, par exemple `jq .ask.recall`.

## Limites

Une passe a du bruit : le même modèle ne répond pas deux fois pareil. Le juge aussi est bruité, et son échantillon est petit. Comparez des tendances, pas des écarts de quelques centièmes.
