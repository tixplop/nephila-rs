# Nephila

**Graphe de connaissances d'un dépôt de code.**

> *Nephila* est le genre des néphiles dorées, des araignées qui tissent de très grandes toiles d'une soie couleur d'or. Cet outil tisse de la même manière les liens entre tous les éléments d'un dépôt.

`nephila` (crate `nephila-rs`) analyse un dépôt de code JVM, JavaScript ou TypeScript et sa documentation, en tire un graphe de connaissances stocké localement, et permet de l'interroger en ligne de commande, de l'exporter ou de l'explorer dans le navigateur.

- **Analyse locale, sans LLM** : la structure (modules, packages, types, méthodes, documents, sections) et les liens (`imports`, `extends`, `implements`, `calls`, `depends_on`, `links_to`, `mentions`, `references`, `contains`) viennent d'une analyse syntaxique. Ni Maven, ni Gradle, ni compilateur ne sont exécutés.
- **LLM facultatif** : s'il est configuré, il enrichit le graphe (résumés des nœuds, concepts des documents) et répond aux questions en langage naturel. Anthropic, OpenAI et tout serveur compatible OpenAI (Ollama, llama.cpp, vLLM…) sont pris en charge.
- **Incrémental** : `nephila update` ne retraite que ce qui a changé.
- **Pensé pour les agents** : sorties `--json` stables, codes de sortie explicites, messages sur stderr.
- **Exploration visuelle** : `nephila export --format html` produit une page autonome, utilisable hors ligne.

Langages et formats pris en charge : Java, Kotlin, Groovy, Scala, JavaScript et TypeScript, Maven (`pom.xml`), Ivy (`ivy.xml`), Gradle (Groovy et Kotlin DSL), npm (`package.json`), Markdown, et les fichiers de données SQL, XML, JSON et YAML.

## Sommaire

- [Démarrage rapide](#démarrage-rapide)
- [Installation](#installation)
- [Commandes](#commandes)
- [Usage par un agent](#usage-par-un-agent)
- [Codes de sortie](#codes-de-sortie)
- [Filtrer l'analyse](#filtrer-lanalyse)
- [Configuration du LLM](#configuration-du-llm)
- [Enrichissement](#enrichissement)
- [Export et vue HTML](#export-et-vue-html)
- [Communautés](#communautés)
- [Ce que l'analyse extrait](#ce-que-lanalyse-extrait)
- [Limites connues](#limites-connues)
- [Mise à niveau](#mise-à-niveau)
- [Développement](#développement)
- [Licence](#licence)

## Démarrage rapide

```sh
cargo install --locked --path .             # installe le binaire nephila
cd /chemin/vers/mon-depot
echo ".nephila/" >> .gitignore              # le graphe est stocké dans .nephila/
nephila build --no-llm                      # structure seule, aucun appel réseau
nephila query search OrderService           # chercher un élément
nephila query neighbors type:com.acme.core.OrderService
xdg-open "$(nephila export --format html)"  # explorer le graphe dans le navigateur
```

Exemple sur un petit dépôt Maven de trois modules :

```
$ nephila -C demo build --no-llm
build terminé : 11 fichiers traités, 23 nœuds, 30 liens, 0 références non résolues, 0 fichiers en attente, 3 fichiers à enrichir, enrichissement : désactivé

$ nephila -C demo summary
Modules (3)
  com.acme:api -> module:com.acme:core
  com.acme:core -> dependency:org.slf4j:slf4j-api
  com.acme:store -> module:com.acme:core
Nœuds les plus reliés
  type:com.acme.core.OrderRepository (degré 6)
  module:com.acme:core (degré 5)
  ...
```

Les modules sont désignés par leur nom qualifié (`com.acme:core`), celui qu'acceptent les requêtes.

## Installation

### Binaires précompilés

Chaque version publiée sur la page [Releases](https://github.com/tixplop/nephila-rs/releases) fournit une archive par plateforme : Linux (x86_64, aarch64 ; binaire statique musl) et macOS (Intel, Apple Silicon). Windows n'est pas pris en charge. Chaque archive contient le binaire, le README et deux exemples de configuration : `config.example.toml` (copie de `examples/config.toml`) et `nephila.example.toml` (copie de `examples/nephila.toml`).

```sh
V=v0.25.3                                      # version voulue
T=x86_64-unknown-linux-musl                    # ou aarch64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin
curl -LO https://github.com/tixplop/nephila-rs/releases/download/$V/nephila-$V-$T.tar.gz
curl -LO https://github.com/tixplop/nephila-rs/releases/download/$V/SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS       # macOS : shasum -a 256 --ignore-missing -c SHA256SUMS
tar xzf nephila-$V-$T.tar.gz
install nephila-$V-$T/nephila ~/.local/bin/
```

### Depuis les sources

Prérequis : Rust 1.99 ou plus récent (`cargo`). Aucune dépendance système : SQLite est intégré au binaire.

```sh
cargo install --locked --path . # installe nephila dans ~/.cargo/bin
# ou, sans installation :
cargo build --release           # binaire dans target/release/nephila
```

Vérification : `nephila --version`.

## Commandes

Option globale : `-C, --repo <dossier>`, la racine du dépôt (défaut : dossier courant). `nephila <commande> --help` détaille chaque commande.

| Commande | Rôle | LLM |
|---|---|---|
| `nephila build` | Reconstruit entièrement le graphe | facultatif |
| `nephila update` | Retraite les fichiers ajoutés, modifiés, supprimés, en attente ou à enrichir | facultatif |
| `nephila status [--json]` | Compare le dépôt au graphe | non |
| `nephila query search <terme>` | Cherche des nœuds par nom | non |
| `nephila query neighbors <nœud>` | Voisins d'un nœud | non |
| `nephila query path <source> <cible>` | Plus court chemin entre deux nœuds | non |
| `nephila query community [<numéro>]` | Communautés du graphe : liste, ou membres d'une communauté | non |
| `nephila query ask "<question>"` | Question en langage naturel | requis |
| `nephila summary [--json]` | Modules, nœuds les plus reliés, thèmes, communautés | non |
| `nephila export` | Export JSON ou page HTML | non |

Les commandes sans LLM ne lisent aucune configuration. Les requêtes (`query`) ne parcourent pas le dépôt : elles répondent d'après le graphe, même s'il est en retard sur les fichiers. Avec `--check-stale`, elles le vérifient d'abord et le signalent sur stderr (`avertissement: graphe pas à jour : 1 fichier modifié`).

### `nephila build` et `nephila update`

```
nephila build  [--no-llm] [-v|--verbose | --show-progress] [--enrich-code] [--jobs <N>] [--batch-files <N>] [--format-retries <N>] [--exclude <MOTIF>] [--exclude-type <TYPES>]
nephila update [--no-llm] [-v|--verbose | --show-progress] [--enrich-code] [--jobs <N>] [--batch-files <N>] [--format-retries <N>] [--exclude <MOTIF>] [--exclude-type <TYPES>]
```

`build` reconstruit le graphe ; `update` ne retraite que les fichiers ajoutés, modifiés ou supprimés, plus ceux en attente ou à enrichir. Enrichissement : voir [Enrichissement](#enrichissement).

| Option | Effet |
|---|---|
| `--no-llm` | Structure seule : la configuration n'est pas lue, aucun appel LLM |
| `--enrich-code` | Enrichit aussi le code, pour cette commande (voir [Enrichissement](#enrichissement)) |
| `-v, --verbose` | Traces sur stderr (voir [Traces et progression](#traces-et-progression)) |
| `--show-progress` | Avancement des phases sur stderr ; incompatible avec `--verbose` |
| `--jobs <N>` | Fils du parcours et de l'extraction, de 1 à 256 ; défaut : nombre de cœurs. Sans effet sur `concurrency` (appels LLM) |
| `--batch-files <N>` | Fichiers par appel d'enrichissement, pour cette commande |
| `--format-retries <N>` | Nouveaux essais d'une réponse non conforme, pour cette commande |
| `--exclude <MOTIF>` | Exclut les fichiers qui correspondent au motif, pour cette commande ; répétable |
| `--exclude-type <TYPES>` | Exclut des types, séparés par des virgules, pour cette commande ; répétable |

```
$ nephila -C repo update
update terminé : 2 fichiers traités, 24 nœuds, 28 liens, 2 références non résolues, 0 fichiers en attente, 0 fichiers à enrichir
```

Le bilan donne deux compteurs :

- **en attente** : l'analyse du fichier n'a pas abouti ; code de sortie 1 ; `nephila update` le reprend.
- **à enrichir** : la structure du fichier est dans le graphe, mais le LLM ne l'a pas enrichie (réponse non conforme ou tronquée, trop d'appels, erreur, `--no-llm`, fichier modifié depuis) ; code 1 avec le LLM actif, 0 sans lui ; `nephila update` le reprend.

Un fichier en échec est nommé sur stderr avec sa cause, et le traitement des autres continue ; si l'envoi au LLM s'arrête, voir [Arrêt de l'envoi](#arrêt-de-lenvoi).
Un seul `build` ou `update` à la fois par dépôt : une seconde commande échoue aussitôt (`erreur: indexation déjà en cours sur ce dépôt`).

### `nephila status`

```
$ nephila status
graphe pas à jour
ajouté	core/Extra.java
supprimé	core/Order.java
...

$ nephila status --json
{"a_jour":false,"fichiers":[{"chemin":"core/Extra.java","état":"ajouté"},...]}
```

Graphe à jour : `graphe à jour`.

Graphe à résoudre : `graphe à résoudre : lancez nephila update`, code 0, sans fichier listé (`--json` : `"a_jour":false`, `fichiers` vide). Une commande a été interrompue, ou la passe de résolution des liens a échoué : lancez `nephila update`, puis `nephila build` si l'erreur `résolution` revient.

`status` compare les contenus (empreintes) ; c'est la vérification exacte.

### `nephila query`

Un nœud se désigne par son nom (`com.acme.core.OrderService`) ou par son identifiant `genre:nom` (`method:com.acme.core.OrderService.place`). Toutes les requêtes acceptent `--json` et `--check-stale`.

```
$ nephila query search order [--limit N]
com.acme.domain.Order (class, core/src/main/java/com/acme/domain/Order.java)
...

$ nephila query neighbors method:com.acme.core.OrderService.place [--depth N] [--limit N]
utilise :
  calls com.acme.core.OrderRepository.save (method, core/src/main/java/com/acme/core/OrderRepository.java)
...

$ nephila query path method:com.acme.core.OrderService.place type:com.acme.core.OrderRepository
com.acme.core.OrderService.place calls com.acme.core.OrderRepository.save
...
```

`--depth` vaut 1 par défaut. Une recherche sans résultat sort avec le code 1 (`erreur: aucun résultat pour « zzz »`). La liste des communautés, ou celle d'un nœud et ses membres : [`nephila query community`](#communautés).

`query ask` est la seule requête qui appelle le LLM. Son contexte est fait des nœuds du graphe trouvés par les mots de la question et de leurs voisins, jamais du contenu des fichiers.

```
$ nephila query ask "Que fait place ?"
OrderService.place appelle OrderRepository.save.
Nœuds cités :
- com.acme.core.OrderService.place (method, core/src/main/java/com/acme/core/OrderService.java)
```

`--format-retries <N>` renvoie une réponse non conforme jusqu'à `N` fois (voir [Échecs et nouveaux essais](#échecs-et-nouveaux-essais)).

**Graphe en retard** : une requête ne lit que le graphe et ne dit pas s'il est en retard. Avant une série de requêtes, `nephila status` le vérifie et `nephila update` le remet à jour. Avec `--check-stale`, la requête compare d'abord la taille et la date de chaque fichier à celles du graphe ; si des fichiers diffèrent, la première ligne de stderr est `avertissement: graphe pas à jour : <n> fichiers modifiés`. Ce compte est approché : un fichier seulement touché (date changée, contenu identique) est compté, jusqu'au prochain `nephila update`.

### `nephila summary`

```
$ nephila summary
Modules (3)
  com.acme:api -> module:com.acme:core
  ...
```

### Traces et progression

Avec `-v`, `build` et `update` écrivent sur stderr une ligne `trace:` par événement, horodatée : configuration effective (et réglages venus de `.nephila.toml`), parcours, migration du schéma, phases, chaque appel LLM avec son résultat et ses nouveaux essais, découpes, politique d'arrêt. stdout est inchangé : `nephila build -v 2>trace.log` garde le bilan lisible.

```
trace: [0.000s] config: llm=mock model=m-projet enrich_code=false thinking=false effort=low max_output_tokens=16384 concurrency=4 timeout_secs=300 max_chunk_chars=60000 batch_files=1 format_retries=1 (.nephila.toml : model,format_retries)
trace: [0.009s] structure: fin 7 ms, 0 en attente, 3 enrichissements conservés
trace: [0.012s] llm enrich départ src/shop/Order.java [1/1] 812 car.
trace: [0.031s] llm enrich fin src/shop/Order.java [1/1] ok (1 résumés, 0 concepts) 19 ms
```

Les traces ne contiennent jamais de contenu de fichier, de prompt, de réponse du LLM ni de clé d'API.

Avec `--show-progress`, `build` et `update` écrivent l'avancement de l'extraction puis de l'enrichissement, une ligne au départ puis une par palier de 10 %, sur le gabarit `progression: <phase> <p> % (<k>/<n> fichiers)`. Les lignes ne sont jamais réécrites : elles se filtrent par `grep '^progression:'`. `--verbose` et `--show-progress` sont incompatibles (erreur d'usage).

## Usage par un agent

Les sorties `--json` sont stables et sans décoration ; les messages vont sur stderr et le code de sortie dit si la commande a réussi (voir [Codes de sortie](#codes-de-sortie)). Séquence type :

1. `nephila status --json` : si `a_jour` vaut `false`, lancer `nephila update`.
2. `nephila query search --json <terme>` pour localiser un élément, `nephila query neighbors --json <nœud> --depth 2` pour explorer ses dépendances, `nephila query path --json <a> <b>` pour relier deux éléments, `nephila query community --json --node <nœud>` pour situer un élément dans son sous-système.
3. `nephila summary --json` pour une vue d'ensemble, `nephila export --output -` pour charger tout le graphe.

Les requêtes ne vérifient pas que le graphe est à jour : faites l'étape 1 avant une série de requêtes, ou ajoutez `--check-stale` à une requête isolée. Désignez un nœud par son identifiant `genre:nom` (`method:shop.Order.place`), qui n'est jamais ambigu, et une communauté par un de ses nœuds (`--node`) : les numéros changent à chaque calcul. Le détail de chaque commande est dans `nephila <commande> --help` ; chaque ligne `erreur:` dit quoi corriger.

## Codes de sortie

| Code | Signification |
|---|---|
| 0 | Succès |
| 1 | Erreur d'exécution : configuration, graphe absent, aucun résultat, arrêt de l'envoi au LLM, fichiers en attente ou à enrichir avec le LLM actif… |
| 2 | Erreur d'usage : commande ou option inconnue, valeur invalide |

stdout ne contient que le résultat. Les messages vont sur stderr, préfixés `erreur:` ou `avertissement:` (et `trace:` ou `progression:` sur demande). Une erreur tient sur une ligne et dit quoi corriger. Si stdout est fermée avant la fin (`| head`), la commande s'arrête sur `erreur: écriture sur stdout impossible : <cause>` et le code 1 ; `build` et `update` ont déjà enregistré le graphe, et `nephila status` donne son état.

## Filtrer l'analyse

Par défaut, `nephila` analyse tout ce qu'il prend en charge, hors des [exclusions par défaut](#exclusions-par-défaut) (sorties de build, dépendances copiées, réglages d'IDE, JavaScript minifié). Les filtres retirent de l'analyse des fichiers sans intérêt (code généré, fichiers minifiés, données). Ils ne font qu'exclure : aucun filtre ne fait indexer un fichier que `.gitignore` ou les exclusions fixes (dossiers `.git/`, `target/`, `node_modules/`, fichiers de verrouillage…) écartent déjà.

### Exclusions par défaut

| Exclu par défaut | Contenu habituel |
|---|---|
| `dist/`, `out/` | sorties de build |
| `coverage/` | rapports de couverture des tests |
| `.next/`, `.nuxt/` | sorties de Next.js et de Nuxt |
| `vendor/` | dépendances copiées dans le dépôt |
| `.idea/` | réglages des IDE JetBrains |
| `*.min.js` | JavaScript minifié |

Le nom doit être exact, casse comprise, à tout niveau (`web/dist/` comme `dist/`) ; ni un filtre ni `!motif` ne les annule. Si l'un de ces dossiers contient votre code (un package Java `out` ou `vendor`, par exemple), désactivez-les toutes dans `.nephila.toml` :

```toml
exclude_defaults = false
```

Pour en garder certains exclus, ajoutez-les à `exclude`. `target/`, `build/`, `.gradle/`, `node_modules/` et les fichiers de verrouillage restent toujours exclus.

### Motifs et types

Un motif suit la syntaxe `.gitignore`, relativement à la racine du dépôt, et est sensible à la casse :

- sans `/`, il vaut à tout niveau : `*.min.js` ;
- un `/` en tête ou au milieu l'ancre à la racine : `/src/gen/`, `docs/legacy/` ;
- un `/` final désigne un dossier : tout son contenu est exclu ;
- `**` traverse les dossiers : `**/generated/**` ;
- `!motif` réinclut un fichier exclu par un motif précédent de la liste : `!docs/garde.md`.

Chaque fichier pris en charge appartient à un seul type.

| Type | Fichiers |
|---|---|
| `markdown` | `.md`, `.markdown` |
| `java` | `.java` |
| `kotlin` | `.kt`, `.kts` (sauf `build.gradle.kts` et `settings.gradle.kts`) |
| `groovy` | `.groovy` |
| `scala` | `.scala` |
| `javascript` | `.js`, `.mjs`, `.cjs`, `.jsx` |
| `typescript` | `.ts`, `.mts`, `.cts`, `.tsx`, `.d.ts` |
| `sql` | `.sql` |
| `xml` | `.xml` (sauf `pom.xml` et `ivy.xml`) |
| `json` | `.json` (sauf `package.json`) |
| `yaml` | `.yaml`, `.yml` |
| `maven` | `pom.xml` |
| `ivy` | `ivy.xml` |
| `gradle` | tout `.gradle` (scripts appliqués compris), `build.gradle.kts`, `settings.gradle.kts` |
| `npm` | `package.json` |

`--exclude-type sql,xml,json,yaml` exclut les fichiers de données.

En ligne de commande, `--exclude` et `--exclude-type` (de `build` et `update`) valent pour la commande seulement, avec un avertissement ; pour un filtre durable, écrivez-les dans `.nephila.toml`. Les motifs du fichier puis ceux de l'option forment une seule liste, où le dernier motif qui s'applique l'emporte. Un fichier indexé puis exclu est retiré par `nephila update`, comme un fichier supprimé.

### Fichier de projet `.nephila.toml`

Pour un filtre durable, créez `.nephila.toml` à la racine du dépôt et versionnez-le :

```toml
# Filtres d'analyse de nephila (exclusions seulement)
exclude = ["docs/legacy/", "*.min.js"]   # motifs .gitignore, relatifs à la racine
exclude_types = ["json"]                 # types : voir nephila build --help
```

Le même fichier porte les réglages LLM du dépôt :

```toml
# Réglages LLM du dépôt (prioritaires sur la configuration personnelle)
model = "qwen2.5-coder:14b"
format_retries = 4
```

Clés permises : les filtres `exclude`, `exclude_types` et `exclude_defaults`, et les six réglages `model`, `max_chunk_chars`, `concurrency`, `timeout_secs`, `batch_files` et `format_retries`, au premier niveau, avec le nom et le type de la configuration personnelle. Toute autre clé est une erreur. Priorité : option > fichier de projet > configuration personnelle > défaut.

| Réglage | Plafond dans `.nephila.toml` |
|---|---|
| `max_chunk_chars` | 1000000 |
| `concurrency` | 64 |
| `timeout_secs` | 3600 |
| `batch_files` | 100 |
| `format_retries` | 10 |

Ces plafonds ne valent que pour .nephila.toml : la configuration personnelle et les options --batch-files et --format-retries n'en ont pas. Un dépôt cloné ne peut ainsi imposer ni plus de fils, ni des appels plus longs ou plus gros, ni plus d'essais.

- **Clés de connexion refusées** : `provider`, `base_url`, `api_key_env`, `tls_ca_cert` et `tls_insecure` ne sont permises que dans la configuration personnelle : un dépôt cloné ne doit pas pouvoir envoyer votre code ou votre clé à un autre serveur, ni couper le contrôle des certificats.
- **Sans LLM actif**, les réglages n'ont aucun effet ni message.
- **Changer `model` réenrichit une fois** chaque fichier de la portée ; les autres réglages ne réenrichissent rien. Pour vérifier les valeurs effectives : `nephila build --verbose` (ligne `config`).

Un exemple commenté est fourni dans [`examples/nephila.toml`](examples/nephila.toml). Un fichier invalide arrête toute commande qui parcourt le dépôt, avant toute analyse, sur une ligne qui nomme la clé et la valeur attendue :

```
erreur: ./.nephila.toml : clé `concurrency` : valeur invalide `-1` (entier strictement positif attendu, au plus 64)
```

## Configuration du LLM

La configuration est facultative. Le LLM est actif quand un fichier de configuration existe et que `--no-llm` est absent. `nephila` cherche ce fichier TOML dans cet ordre :

1. le chemin donné par la variable `NEPHILA_CONFIG` ;
2. `$XDG_CONFIG_HOME/nephila/config.toml` ;
3. `~/.config/nephila/config.toml`.

[`examples/config.toml`](examples/config.toml) documente toutes les clés et variables.

| Clé | Rôle | Défaut |
|---|---|---|
| `provider` | `anthropic`, `openai` (compatible OpenAI) ou `mock` | obligatoire |
| `model` | Nom du modèle | obligatoire, sauf `mock` |
| `api_key_env` | **Nom** de la variable d'environnement qui contient la clé | obligatoire pour `anthropic` |
| `base_url` | URL du serveur, **sans** `/v1` final | celle du fournisseur |
| `max_chunk_chars` | Taille maximale d'un morceau envoyé au LLM | 60000 |
| `concurrency` | Appels LLM simultanés ; l'extraction est réglée par `--jobs` | 4 |
| `enrich_code` | `true` enrichit aussi le code ; configuration personnelle seulement | `false` |
| `thinking` | `true` demande au modèle de réfléchir ; configuration personnelle seulement ; chez anthropic, modèle Claude 4.6 ou suivant | `false` |
| `effort` | `"low"`, `"medium"` ou `"high"`, avec `thinking = true` ; à `"medium"` ou `"high"`, augmentez `timeout_secs` ; configuration personnelle seulement | `"low"` |
| `max_output_tokens` | Limite de jetons de la réponse, entier > 0 ; configuration personnelle seulement | 16384 |
| `batch_files` | Fichiers par appel d'enrichissement, entier > 0 | 10 |
| `format_retries` | Nouveaux essais d'une réponse non conforme, entier ≥ 0 | 2 |
| `timeout_secs` | Durée maximale d'un appel HTTP, entier > 0 | 300 |
| `tls_ca_cert` | Fichier PEM d'autorités ajoutées aux autorités publiques ; chemin absolu, `~/…` ou relatif | aucun |
| `tls_insecure` | `true` désactive le contrôle du certificat du serveur, comme `curl -k` | `false` |

Ce que chaque clé envoie à chaque fournisseur :

| Clé | `anthropic` | `openai` | `mock` |
|---|---|---|---|
| `max_output_tokens` | `max_tokens` | `max_completion_tokens` | sans effet |
| `thinking = true` | `"thinking": {"type": "adaptive"}` | aucun champ propre (voir `effort`) | sans effet |
| `effort` (avec `thinking = true` seulement) | `output_config.effort` | `reasoning_effort` | sans effet |

Un modèle ou un serveur qui refuse ces champs, ou une limite de 16384 jetons, refuse la requête (HTTP 400) : voir [Arrêt de l'envoi](#arrêt-de-lenvoi).

La clé d'API n'est jamais écrite dans le fichier. `nephila` ne la vérifie pas avant le traitement : une clé refusée est constatée au premier appel, après l'extraction.

<details>
<summary>Exemples : Anthropic, OpenAI, modèle local, simulé</summary>

```toml
# Anthropic — puis : export ANTHROPIC_API_KEY=...
provider = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
```

```toml
# OpenAI — puis : export OPENAI_API_KEY=...
provider = "openai"
model = "gpt-4o-mini"
api_key_env = "OPENAI_API_KEY"
```

```toml
# Modèle local (Ollama, llama.cpp, vLLM…), sans clé
provider = "openai"
model = "llama3.1"
base_url = "http://localhost:11434"   # pas http://localhost:11434/v1
```

```toml
# Fournisseur simulé (tests, démonstration sans réseau)
provider = "mock"
```

</details>

`nephila` ajoute lui-même `/v1/chat/completions` à `base_url` : avec `/v1`, les requêtes partiraient vers `/v1/v1/…`.

**Comportement selon la configuration**

| Situation | `build` / `update` | `query ask` |
|---|---|---|
| Aucun fichier, ou `NEPHILA_CONFIG` vers un fichier absent | structure seule, code 0, `enrichissement : désactivé` | échoue, demande une configuration |
| Fichier valide | structure et enrichissement | répond |
| Fichier invalide | échoue (sauf avec `--no-llm`) | échoue |

### Certificats TLS

Si le serveur (proxy d'entreprise, serveur local) présente un certificat signé par une autorité privée, donnez le PEM de cette autorité :

```toml
provider = "openai"
model = "llama3.1"
base_url = "https://127.0.0.1:8443"
tls_ca_cert = "certs/ca.pem"   # relatif au fichier de configuration
```

Les autorités publiques restent reconnues. En dernier recours, `tls_insecure = true` désactive le contrôle. Un certificat refusé donne une erreur qui nomme `tls_ca_cert` et `tls_insecure`. Ni `mock`, ni `http://`, ni TLS mutuel.

## Enrichissement

Par défaut, seuls les documents Markdown sont enrichis (résumés des nœuds, concepts des documents). `enrich_code = true` dans la configuration personnelle, ou `--enrich-code` pour une commande, enrichit aussi le code et les fichiers de build et de données ; sans lui, un fichier de code n'est ni envoyé ni compté « à enrichir ». Un fichier garde ses enrichissements tant que son contenu et le modèle ne changent pas : un second `build` sans changement ne fait aucun appel, et changer `model` réenrichit une fois chaque fichier de la portée.

### Lots et gros fichiers

- **Lots** : `batch_files` fichiers par appel (défaut 10), dans la limite de `max_chunk_chars` ; jusqu'à `concurrency` appels en même temps.
- **Gros fichier** : envoyé seul, en morceaux de `max_chunk_chars`, au plus 150 nœuds demandés par appel.
- **Plafond** : un fichier qui demanderait plus de 50 appels n'est pas envoyé et reste « à enrichir », nommé par l'avertissement `trop d'appels d'enrichissement`. Deux remèdes : `exclude` dans `.nephila.toml`, qui retire aussi sa structure du graphe, ou un `max_chunk_chars` plus grand, qui réduit le nombre de morceaux pour tous les fichiers. Au-delà de 7500 nœuds, seul `exclude` suffit.

### Échecs et nouveaux essais

- **Réponse non conforme** : renvoyée aussitôt, jusqu'à `format_retries` fois (défaut 2) ; l'échec final nomme le fichier avec `(<k> essais sur <t>)` ; si le modèle échoue souvent, changez-le ou réduisez `batch_files`.
- **Surcharge passagère** (HTTP 429, 500, 502, 503, 504, 529, erreur réseau transitoire) : jusqu'à 4 nouveaux essais, avec une attente croissante ; un `retry-after` en secondes est respecté, et au-delà de 60 s le fichier échoue aussitôt.
- **Réponse tronquée** : `réponse tronquée par la limite de jetons`, jamais renvoyée ; un lot tronqué est découpé en deux, jusqu'au fichier seul ; pour un fichier seul, réduisez `max_chunk_chars` ou augmentez `max_output_tokens`.
- **Ce qui reste** : un fichier en échec reste « à enrichir », et `nephila update` ne renvoie que ceux-là. Aucune de ces causes n'arrête l'envoi tant qu'elle reste isolée.

### Arrêt de l'envoi

Six causes arrêtent l'envoi au LLM. Chacune a sa ligne `erreur:`, la dernière de stderr, après la ligne de bilan, avec le code 1 :

| Cause | Quand | Que faire |
|---|---|---|
| Clé refusée | HTTP 401 ou 403, dès le premier appel | corriger la variable nommée par `api_key_env` |
| Requête refusée | HTTP 400, 404, 413 ou 422, dès le premier appel | corriger `model`, `thinking`, `effort` ou `max_output_tokens`, d'après le message du fournisseur |
| Redirection | HTTP 300 à 399, jamais suivie | corriger `base_url` d'après l'adresse affichée |
| Fournisseur saturé | 3 appels de suite en échec sur HTTP 429 ou 529 | relancer plus tard, ou réduire `concurrency` |
| Panne réseau | 3 erreurs réseau de suite | vérifier le réseau et `base_url` |
| Délais dépassés | 3 appels de suite sans réponse dans `timeout_secs` | augmenter `timeout_secs`, ou vérifier le fournisseur |

Les appels en cours se terminent et sont enregistrés. Les fichiers non envoyés restent « à enrichir » : une fois la cause corrigée, `nephila update` ne renvoie qu'eux. Sans fichier à enrichir, aucun appel n'a lieu, et une clé fausse passe inaperçue.

## Export et vue HTML

```
nephila export [--format json|html] [--output <fichier>|-]
```

Sans `--output`, l'export est écrit **dans le dossier courant** (pas dans le dépôt analysé), sous le nom `<slug>.json` ou `<slug>.html`, et stdout ne contient que le chemin du fichier écrit. Un fichier existant est remplacé.

```
$ nephila -C repo export
repo.json
$ nephila export --output graphe.json
graphe.json
$ nephila export --output - | jq '.nodes | length'
23
$ xdg-open "$(nephila export --format html)"
```

Le slug est le nom du dossier en minuscules ASCII, chaque suite de caractères hors `[a-z0-9]` devenant un `-` : `Mon Dépôt_2` donne `mon-d-p-t-2`. **Format JSON** : objet avec `version`, `nodes`, `edges` et `unresolved`.

Le document est écrit au fil de la lecture du graphe. Seul le code de sortie 0 garantit un document complet : en cas d'échec, le fichier commencé est supprimé, et avec `--output -` stdout peut contenir un début de document.

**Page HTML** : fichier unique, hors ligne, qui affiche une vue partielle du graphe que l'on étend.

- **Vue initiale** : les 100 modules les plus reliés.
- **Recherche** : sur le nom court ou qualifié, sans casse ; choisir un résultat affiche le nœud et ses voisins.
- **Détails** : un clic sur un nœud ouvre un panneau (genre, fichier, résumé, liens).
- **Extension** : « Étendre » (ou un double-clic) ajoute les voisins directs ; « Tout étendre » affiche tout le graphe, dans la limite de 5000 nœuds.
- **Filtres** : une case par genre de nœud et par genre de lien ; « Vue initiale » ramène au départ.
- **Couleur** : par genre (défaut) ou par communauté ; voir [Communautés](#communautés).

## Communautés

Une communauté est un groupe de nœuds plus reliés entre eux qu'avec le reste du graphe : un sous-système. `nephila` les calcule sans LLM, par l'algorithme de Leiden ; un même graphe donne les mêmes communautés.

- **Numéro** : 0 pour la plus grande, puis par taille décroissante.
- **Nom** : le nom court du nœud le plus relié de la communauté (son pivot), ou `communauté <numéro>` si ce nom est vide.
- **Cohésion** : la part des paires de membres directement reliées, de 0 à 1.

**Quand** : à chaque `build` et à chaque `update` qui change le graphe. Les numéros sont refaits à chaque calcul : désignez une communauté par un de ses nœuds (`--node`).

```
$ nephila query community [<numéro>] [--node <nœud>] [--limit N]
$ nephila query community
0 OrderRepository (6 nœuds, cohésion 0.40)
...

$ nephila query community --node com.acme.core.OrderService
1 OrderService (6 nœuds, cohésion 0.40)
  com.acme.core.OrderService (class, core/src/main/java/com/acme/core/OrderService.java)
  ...
```

Sans argument, `query community` liste les communautés. Avec un numéro, ou `--node` et un nœud, il affiche la communauté puis ses membres, le pivot d'abord. `--limit` vaut 50 par défaut ; `--json` est accepté.

- **Résumé** : `nephila summary` se termine par `Communautés (<total>)` et les 10 plus grandes.
- **Export JSON** : chaque nœud a une clé `community`, et le document une clé `communities`.
- **Vue HTML** : le choix « Couleur » colore les nœuds par communauté.

**Graphe d'une version précédente** : tant que le premier `nephila update` (ou `build`) ne l'a pas migré, les communautés ne sont pas calculées et `query community` sort avec le code 1 (`erreur: communautés non calculées : lancez nephila update`).

## Ce que l'analyse extrait

Chaque fichier est analysé localement : tree-sitter pour les langages JVM, JavaScript et TypeScript, un lecteur XML pour Maven et Ivy, un lecteur lexical pour Gradle et pour SQL, `serde_json` pour `package.json` et pour JSON, un analyseur à événements pour YAML, et CommonMark pour Markdown.

| Fichier | Nœuds | Liens |
|---|---|---|
| `.java`, `.kt`, `.kts` (hors Gradle), `.groovy`, `.scala` | package, types (class, interface, enum, annotation, object, trait Scala), méthodes | `imports`, `extends`, `implements`, `calls` |
| `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx` | package (un module ES), types (class, interface, enum, alias, object), méthodes | `imports`, `extends`, `implements`, `calls` |
| `package.json` | module = champ `name` (sinon nom du dossier) | `depends_on` : toutes les dépendances |
| `pom.xml`, `ivy.xml` | module `groupId:artifactId` ou `organisation:module` | `depends_on` |
| `build.gradle(.kts)`, `settings.gradle(.kts)` | module = chemin de projet (`:services:api`) | `depends_on` : bloc `dependencies { }` de premier niveau |
| autre `.gradle` | analysé comme un script Groovy | comme Groovy |
| `.md`, `.markdown` | document, sections | `links_to` (liens vers un `.md`), `mentions` (noms JVM en code en ligne) |
| `.sql` | document, `table` (ou `view`) | `references` (clé étrangère, table lue par une vue) |
| autre `.xml` | document, sans nœud par élément | `mentions` (noms JVM cités) |
| `.json` | document, `key` par clé feuille | `mentions` (noms JVM cités) |
| `.yaml`, `.yml` | document, `key` par clé feuille | `mentions` (noms JVM cités) |

- **JavaScript et TypeScript** : un fichier est un module ES nommé par son chemin sans extension ; les imports relatifs et les alias `@/`, `~/` sont suivis, un paquet npm est externe.
- **Données** : noms SQL en minuscules ; en JSON et YAML, un nœud `key` par clé feuille, au plus 1000 par fichier.
- **Noms JVM cités** : dans les fichiers XML, JSON et YAML, un nom d'au moins deux segments (`com.acme.Repo#save`) donne une `mention`, non résolue tant que le nom est absent du graphe.
- **Résolution des appels** : le type du receveur est cherché dans le fichier seulement ; s'il est inconnu (chaîne d'appels, `var` sans initialiseur reconnaissable), aucun lien n'est créé, car un lien manquant coûte moins cher qu'un lien faux.

Les références sans cible sont comptées dans « références non résolues ». Un fichier mal formé est exploité au mieux.

## Limites connues

- **`trait` Groovy** : la grammaire ne le reconnaît pas ; un trait ne produit ni nœud ni méthode.
- **Script Groovy** : un appel sans parenthèses (`helper 42`, `println x`) ne crée pas de lien ; écrivez `helper(42)`.
- **Gradle** : `project("core")` est ignoré ; écrivez `project(':core')`.
- **JSONC et JSON5** : non pris en charge (document seul, avec un `avertissement:`).
- **Nom SQL qualifié** : `public.orders` ne trouve pas une table déclarée `orders`.
- **JS/TS** : `export … from`, `export *` et `import()` ne sont pas suivis ; les `paths` de `tsconfig.json` ne sont pas lus.
- **Gros JSON et exports** : indexés dans la limite de 1 000 clés ; excluez-les.
- **`query path`** : ignore le sens des liens.

## Mise à niveau

Les versions qui demandent une action, la plus récente d'abord. L'historique des nouveautés est dans les tags git et sur la page [Releases](https://github.com/tixplop/nephila-rs/releases).

- **Vers 0.25** : le premier `nephila update` (ou `build`) migre le graphe et calcule les communautés, sans appel LLM ; d'ici là, `query community` répond `communautés non calculées`. Pour revenir à une version antérieure, supprimez `.nephila/` : le build suivant réenrichit tous les fichiers de la portée.
- **Vers 0.24** : `dist/`, `out/`, `coverage/`, `.next/`, `.nuxt/`, `vendor/`, `.idea/` et `*.min.js` sont exclus par défaut, et le premier `nephila update` retire leurs fichiers du graphe. Pour les garder : `exclude_defaults = false` dans `.nephila.toml`.
- **Vers 0.23** : l'extraction utilise un fil par cœur, quelle que soit `concurrency`. Pour la limiter comme `concurrency = 1` le faisait : `--jobs 1`.
- **Vers 0.19** : les requêtes n'avertissent plus d'un graphe en retard. Un script qui s'appuyait sur l'avertissement ajoute `--check-stale` à ses requêtes, ou lance `nephila status` avant elles.
- **Vers 0.18** : seuls les documents Markdown sont enrichis par défaut ; pour le code, `enrich_code = true` dans la configuration personnelle. Le premier `nephila build` remplace un graphe 0.17 et enrichit une fois tous les fichiers de la portée.

Un graphe 0.18 à 0.24 est migré sur place par le premier `nephila update` ou `nephila build`, sans appel LLM et sans perdre les résumés. Un graphe plus ancien est refusé par un message qui dit de lancer `nephila build`.

## Développement

La CI (`.github/workflows/ci.yml`) lance le lint, les tests sur Linux et macOS, les tests de la vue et `cargo deny check advisories` à chaque push sur `master` et pull request ; chaque lundi, elle passe aussi l'audit, les performances et clippy beta.

Les actions sont épinglées par SHA et Dependabot (`.github/dependabot.yml`) propose les mises à jour. La chaîne d'outils est épinglée par `rust-toolchain.toml` à Rust 1.99.0 ; une montée change ensemble `channel`, `rust-version` et les `toolchain:` des workflows.

**Publier une version** : mettre à jour `version` dans `Cargo.toml` et le README, puis pousser un tag identique ; `release.yml` publie les archives et `SHA256SUMS`.

```sh
git tag v0.25.3 && git push origin v0.25.3
```

Les tests s'exécutent avec [cargo-nextest](https://nexte.st) (`cargo install cargo-nextest --locked`), en série, avec une limite de temps par test (`.config/nextest.toml`) ; `cargo test` passe aussi.

```sh
cargo nextest run --cargo-quiet --color never --test-threads 1 --no-fail-fast --status-level fail --final-status-level fail   # suite complète
cargo fmt --all -- --check                                      # format
cargo clippy -q --all-targets --color never -- -D warnings      # lint
cargo nextest run --release --cargo-quiet --color never --test-threads 1 --run-ignored only --success-output final perf       # performances
cargo nextest run --release --cargo-quiet --color never --test-threads 1 --run-ignored only --success-output final perf_banc  # banc
node --test --test-timeout=10000 --test-concurrency=1 --test-reporter=dot 'tests/view/*.test.js'   # logique de la vue HTML
```

Sous macOS, les tests des noms de fichiers non UTF-8 ne vérifient rien (APFS les refuse) : seule la CI Linux les exécute.

`NEPHILA_RETRY_BASE_MS` (entier ≥ 0, en millisecondes) remplace la base de 1 s de l'attente entre deux envois HTTP, pour des tests rapides ; `retry-after` reste respecté.

### LLM simulé (`provider = "mock"`)

Le fournisseur `mock` remplace le LLM par des fixtures, piloté par l'environnement :

| Variable | Effet |
|---|---|
| `NEPHILA_MOCK_DIR` | Dossier de fixtures : `<dir>/<chemin relatif>.json` est la réponse d'enrichissement, `_ask.json` celle de `query ask` |
| `NEPHILA_MOCK_LOG` | Journal des appels : une ligne `chemin<TAB>n°appel` par appel |
| `NEPHILA_MOCK_FAIL_AFTER` | Nombre d'appels réussis avant des erreurs réseau simulées |
| `NEPHILA_MOCK_INVALID_KEY` | Si non vide et différent de `0`, chaque appel échoue en « clé refusée » |
| `NEPHILA_MOCK_DELAY_MS` | Délai ajouté à chaque appel |
| `NEPHILA_MOCK_TIMEOUT_FILES` | Chemins relatifs, séparés par des virgules, dont l'appel échoue par délai dépassé |
| `NEPHILA_MOCK_KEY_REVOKED_AFTER` | Nombre d'appels d'enrichissement avant des refus de clé simulés |
| `NEPHILA_MOCK_MALFORMED` | `<chemin>:<k>[,…]` : les k premiers appels de ce fichier reçoivent une réponse non conforme |

Une fixture contient obligatoirement `version`, `summaries` et `concepts` (listes vides au besoin) ; les concepts sont réservés aux fichiers Markdown, et tout le reste est écarté avec un avertissement. Pour `src/shop/Order.java`, le fichier `<dir>/src/shop/Order.java.json` :

```json
{"version":2,
 "summaries":[{"node":"type:shop.Order","summary":"Une commande."}],
 "concepts":[]}
```

Format de `_ask.json` : `{"found":true,"answer":"...","nodes":["method:shop.Order.place"]}` ou `{"found":false}`.

## Licence

Aucune licence n'est encore déclarée pour ce projet.
