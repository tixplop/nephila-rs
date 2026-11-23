# Carte du code

Format : `chemin — rôle — points d'entrée (3 au plus)`. Crate `nephila-rs` (lib `nephila`, binaire `nephila`).

## Binaire et commandes
- src/main.rs — binaire : analyse clap puis `cli::run` — `main`
- src/lib.rs — déclaration des modules de la bibliothèque
- src/cli.rs — arbre de commandes clap, options d'indexation et répartition — `Cli`, `IndexOpts`, `run`
- src/error.rs — type d'erreur unique, codes de sortie — `Error`, `report_error`
- src/out.rs — écrivain unique de stdout — `Out`
- src/status.rs — commande `status` — `run`
- src/export.rs — commande `export` (JSON du graphe) — `run`
- src/summary.rs — résumé global calculé au build, lu par `summary` — `compute`, `run`
- src/cache.rs — commande `cache export|import` — `export`, `import`
- src/config.rs — configuration du fournisseur LLM et délais — `AskLimits`, `summary_language_of`
- src/progress.rs, src/trace.rs, src/interrupt.rs, src/diag.rs, src/workers.rs — progression, traces `--verbose`, Ctrl-C, compteurs de test, fils de travail

## Analyse
- src/scan.rs — parcours du dépôt, empreintes BLAKE3, modules — `scan`
- src/filter.rs — filtres de types, `.gitignore`, `.nephila.toml` — `Matcher`, `compile`
- src/diff.rs — comparaison parcours / table `files` — `compute`
- src/chunk.rs, src/excerpt.rs, src/escape.rs — découpage des gros fichiers, extrait des données, échappement d'affichage
- src/model.rs — genres de nœuds et de liens — `Node`, `Edge`, `node_id`
- src/resolve.rs — résolution des liens sans LLM — `ResolveStats`
- src/community.rs, src/subsystems.rs — communautés (Leiden), noms de sous-systèmes
- src/parse/ — extracteurs par langage (Java, Kotlin, Scala, Groovy, JS/TS, Gradle, Maven, Ivy, npm, SQL, JSON, YAML, XML, Markdown) — `extract` (parse/mod.rs), `Lang::parse` (parse/lang.rs)

## Index et enrichissement
- src/index/mod.rs — plafond et défaut de `--jobs` — `default_jobs`, `MAX_JOBS`
- src/index/build.rs — commande `build` — `run`, `build`, `build_with_dry_run`
- src/index/update.rs — commande `update` incrémentale — `run`, `update`
- src/index/structure.rs — phase structure parallèle, un seul écrivain — `analyze`
- src/index/enrich.rs — phase enrichissement en lots parallèles, bornes de lots — `enrich_all`, `enrich_pass`
- src/index/batch.rs — enrichissement par le mode lot du fournisseur (`--batch`)
- src/index/estimate.rs — estimation du plan d'enrichissement (`--dry-run`)
- src/index/lock.rs — verrou d'écriture `.nephila/lock` — `RepoLock`

## LLM
- src/llm/mod.rs — abstraction du fournisseur, trait `Llm` — `Llm`
- src/llm/ — adaptateurs `anthropic.rs`, `openai.rs`, `mock.rs` ; `http.rs`, `sse.rs`, `retry.rs`, `usage.rs` ; prompts et schémas `enrich.rs`, `ask.rs`, `subsystem.rs`, `schema.rs`, `tidy.rs`, `compact.rs`, `redact.rs`, `batch.rs`

## Stockage
- src/store/mod.rs — SQLite `.nephila/graph.db` : réglages, schéma, FTS5, migrations — `Store`, `open_or_create`, `open_readonly`
- src/store/schema.sql — schéma des tables
- src/store/cache.rs — fichier d'enrichissements (cache export/import)
- src/store/prepare.rs — structure préparée d'un fichier avant écriture

## Requêtes
- src/query/mod.rs — ouverture du graphe et formats communs — `open`, `describe`
- src/query/ — `search.rs`, `neighbors.rs`, `path.rs`, `community.rs`, `ask.rs` (+ `ask_cache.rs`), `graph.rs`, `lookup.rs`, `words.rs`

## Vue HTML
- src/view.rs — page HTML autonome d'exploration — `render`
- src/view/ — `index.html`, `style.css`, `app.js`, `model.js` (embarqués)

## Chaîne d'outils et CI
- Cargo.toml, Cargo.lock — dépendances (rusqlite, tree-sitter, ureq, rustls…), `rust-version`
- rust-toolchain.toml — chaîne Rust épinglée
- .github/workflows/ci.yml, release.yml, .github/dependabot.yml — CI (jobs dont `deny`), publication, mises à jour
- .config/nextest.toml — réglages nextest
- examples/ — exemples de configuration

## Tests
- tests/*.rs — un fichier par sujet (intégration, `assert_cmd`), noms `REQnnn_ACk` — ex. `tests/batch.rs`, `tests/toolchain.rs`
- tests/support/ — serveurs factices, générateurs de dépôts, schémas anciens (`schemaN.rs`)
- tests/view/ — tests JS de la vue (`app.test.js`, `model.test.js`)
- tests/fixtures/ — dépôts d'exemple

## Fonctionnalités
- Enrichissement en lots parallèles, lots bornés en caractères : REQ-027, REQ-287 → src/index/enrich.rs, src/llm/enrich.rs, src/config.rs, tests/batch.rs
- Mode lot du fournisseur : REQ-211 → src/index/batch.rs, src/llm/batch.rs
- Estimation / dry-run : REQ-184 → src/index/estimate.rs, src/index/enrich.rs
- Stockage SQLite, réglages d'ouverture : REQ-059 → src/store/mod.rs
- Index plein texte FTS5, schéma 16 : REQ-284 → src/store/mod.rs, src/query/search.rs, tests/schema_upgrade.rs
- Dépendance rusqlite, audit des avis : REQ-296, REQ-297 → Cargo.toml, Cargo.lock, tests/toolchain.rs, .github/workflows/ci.yml
- Chaîne d'outils Rust 1.99 : REQ-076, REQ-123 → Cargo.toml, rust-toolchain.toml, .github/workflows/ci.yml, .github/workflows/release.yml, tests/toolchain.rs
- CI, job `deny` : REQ-117 → .github/workflows/ci.yml, tests/workflows.rs
- Analyse des langages : REQ-006, REQ-034 → src/parse/
- Statut, mise à jour incrémentale : REQ-011, REQ-012 → src/status.rs, src/diff.rs, src/index/update.rs
- Recherche, voisins, chemin, communautés : REQ-013, REQ-014, REQ-015, REQ-148 → src/query/
- Question en langage naturel : REQ-016, REQ-281 → src/query/ask.rs, src/llm/ask.rs
- Export JSON, résumé : REQ-018, REQ-019 → src/export.rs, src/summary.rs
- Vue HTML : REQ-025 → src/view.rs, src/view/
- Cache d'enrichissements : REQ-295 → src/cache.rs, src/store/cache.rs
- Sous-systèmes nommés : REQ-261, REQ-262 → src/subsystems.rs, src/llm/subsystem.rs
- LLM simulé pour les tests : REQ-003 → src/llm/mock.rs, src/llm/mod.rs
