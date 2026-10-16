# Nephila

**Graphe de connaissances d'un dépôt de code.**

> *Nephila* est le genre des néphiles dorées, des araignées qui tissent de très grandes toiles d'une soie couleur d'or. Cet outil tisse de la même manière les liens entre tous les éléments d'un dépôt.

`nephila` (crate `nephila-rs`) analyse un dépôt de code JVM et sa documentation, en tire un graphe de connaissances stocké localement, et permet de l'interroger en ligne de commande, de l'exporter ou de l'explorer dans le navigateur.

- **Analyse locale, sans LLM** : la structure (modules, packages, types, méthodes, documents, sections) et les liens (`imports`, `extends`, `implements`, `calls`, `depends_on`, `links_to`, `mentions`, `contains`) viennent d'une analyse syntaxique. Ni Maven, ni Gradle, ni compilateur ne sont exécutés.
- **LLM facultatif** : s'il est configuré, il enrichit le graphe (résumés des nœuds, concepts des documents) et répond aux questions en langage naturel. Anthropic, OpenAI et tout serveur compatible OpenAI (Ollama, llama.cpp, vLLM…) sont pris en charge.
- **Incrémental** : `nephila update` ne retraite que ce qui a changé.
- **Pensé pour les agents** : sorties `--json` stables, codes de sortie explicites, messages sur stderr.
- **Exploration visuelle** : `nephila export --format html` produit une page autonome, utilisable hors ligne.

Langages et formats pris en charge : Java, Kotlin, Groovy, Scala, Maven (`pom.xml`), Ivy (`ivy.xml`), Gradle (Groovy et Kotlin DSL), Markdown.

## Sommaire

- [Démarrage rapide](#démarrage-rapide)
- [Installation](#installation)
- [Commandes](#commandes)
- [Export et vue HTML](#export-et-vue-html)
- [Configuration du LLM](#configuration-du-llm)
- [Enrichissement en lots](#enrichissement-en-lots)
- [Ce que l'analyse extrait](#ce-que-lanalyse-extrait)
- [Fonctionnement](#fonctionnement)
- [Codes de sortie](#codes-de-sortie)
- [Usage par un agent](#usage-par-un-agent)
- [Limites connues](#limites-connues)
- [Mise à niveau](#mise-à-niveau)
- [Développement](#développement)
- [Licence](#licence)

## Démarrage rapide

```sh
cargo install --path .                      # installe le binaire nephila
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
build terminé : 11 fichiers traités, 23 nœuds, 30 liens, 0 références non résolues, 0 fichiers en attente, 11 fichiers à enrichir, enrichissement : désactivé

$ nephila -C demo summary
Modules (3)
  api -> module:com.acme:core
  core -> dependency:org.slf4j:slf4j-api
  store -> module:com.acme:core
Nœuds les plus reliés
  type:com.acme.core.OrderRepository (degré 6)
  module:com.acme:core (degré 5)
  ...
```

## Installation

### Binaires précompilés

Chaque version publiée sur la page [Releases](https://github.com/tixplop/nephila-rs/releases) fournit une archive par plateforme : Linux (x86_64, aarch64 ; binaire statique musl) et macOS (Intel, Apple Silicon). Windows n'est pas pris en charge.

```sh
V=v0.8.1                                       # version voulue
T=x86_64-unknown-linux-musl                    # ou aarch64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin
curl -LO https://github.com/tixplop/nephila-rs/releases/download/$V/nephila-$V-$T.tar.gz
curl -LO https://github.com/tixplop/nephila-rs/releases/download/$V/SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS       # macOS : shasum -a 256 --ignore-missing -c SHA256SUMS
tar xzf nephila-$V-$T.tar.gz
install nephila-$V-$T/nephila ~/.local/bin/
```

### Depuis les sources

Prérequis : une chaîne Rust stable récente (`cargo`). Aucune dépendance système : SQLite est intégré au binaire.

```sh
cargo install --path .        # installe nephila dans ~/.cargo/bin
# ou, sans installation :
cargo build --release         # binaire dans target/release/nephila
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
| `nephila query ask "<question>"` | Question en langage naturel | requis |
| `nephila summary [--json]` | Modules, nœuds les plus reliés, thèmes | non |
| `nephila export` | Export JSON ou page HTML | non |

Les commandes sans LLM ne lisent aucune configuration. Si le graphe est en retard sur les fichiers, elles le signalent sur stderr (`avertissement: graphe pas à jour : 1 fichier modifié`).

### `nephila build` et `nephila update`

```
nephila build  [--no-llm] [-v|--verbose] [--batch-files <N>]
nephila update [--no-llm] [-v|--verbose] [--batch-files <N>]
```

`build` analyse chaque fichier puis, si le LLM est actif, enrichit le graphe. `update` ne retraite que les fichiers ajoutés, modifiés ou supprimés, plus ceux restés en attente ou à enrichir.

| Option | Effet |
|---|---|
| `--no-llm` | Structure seule : la configuration n'est pas lue (même invalide) et aucun appel n'est fait |
| `-v, --verbose` | Traces sur stderr (voir [Traces](#traces---verbose)) |
| `--batch-files <N>` | Taille de lot pour cette commande seulement (voir [Enrichissement en lots](#enrichissement-en-lots)) |

```
$ nephila -C repo update
update terminé : 2 fichiers traités, 2 nœuds, 1 liens, 0 références non résolues, 0 fichiers en attente, 0 fichiers à enrichir
```

Le bilan donne deux compteurs :

- **en attente** : l'analyse du fichier n'a pas abouti ou n'a pas été faite. La commande sort avec le code 1 ; `nephila update` le retraite.
- **à enrichir** : la structure du fichier est dans le graphe, mais son contenu actuel n'a pas été enrichi par le LLM (réponse non conforme, erreur, `--no-llm`, ou fichier modifié depuis). Avec le LLM actif, la commande sort avec le code 1 et `nephila update` ne renvoie au LLM que ces fichiers. Sans LLM, ce n'est pas une erreur (code 0, `enrichissement : désactivé`).

Réanalyser un fichier modifié efface ses résumés et concepts.

**Gestion des échecs** :

- un fichier en échec reste « en attente » ou « à enrichir », est nommé sur stderr avec sa cause, et le traitement des autres continue ;
- l'envoi au LLM s'arrête sur une clé refusée (HTTP 401/403, dès le premier refus) ou après 3 erreurs réseau consécutives (une réussite remet le compte à zéro) ;
- les appels déjà en cours se terminent et sont enregistrés, et la ligne de bilan est toujours affichée ;
- une référence dont le genre de lien est inconnu ou non autorisé pour son nœud source est écartée avec un avertissement ; le fichier est indexé normalement.

### `nephila status`

```
$ nephila status
graphe pas à jour
supprimé	src/Order.java
ajouté	src/Order2.java
modifié	src/Payment.java

$ nephila status --json
{"a_jour":false,"fichiers":[{"chemin":"src/Order.java","état":"supprimé"},...]}
```

Graphe à jour : `graphe à jour`.

### `nephila query`

Un nœud se désigne par son nom (`shop.Order`) ou par son identifiant `genre:nom` (`method:shop.Order.place`). Toutes les requêtes acceptent `--json`.

```
$ nephila query search order [--limit N]
shop.Order (class, src/Order.java) - Une commande.
shop.Order.place (method, src/Order.java)

$ nephila query neighbors method:shop.Order.place [--depth N] [--limit N]
utilise :
  calls shop.Payment.pay (method, src/Payment.java)
utilisé par :
  contains shop.Order (class, src/Order.java)

$ nephila query path method:shop.Order.place method:shop.Payment.pay
shop.Order.place calls shop.Payment.pay
```

`--depth` vaut 1 par défaut. Une recherche sans résultat sort avec le code 1 (`erreur: aucun résultat pour « zzz »`).

`query ask` est la seule requête qui appelle le LLM. Le contexte qu'il reçoit est fait des nœuds du graphe trouvés par les mots de la question et de leurs voisins, jamais du contenu des fichiers.

```
$ nephila query ask "Que fait Order ?"
Order.place appelle Payment.pay.
Nœuds cités :
- shop.Order.place (method, src/Order.java)
```

Si rien ne correspond : `erreur: pas trouvé dans le graphe` (code 1).

### `nephila summary`

```
$ nephila summary
Modules (0)
Nœuds les plus reliés
  method:shop.Order.place (degré 2)
  ...
```

### Traces (`--verbose`)

Avec `-v`, `build` et `update` écrivent sur stderr une ligne par événement, préfixée `trace:` et horodatée depuis le lancement. stdout est inchangé : `nephila build -v 2>trace.log` garde le bilan lisible.

Événements tracés :

- la configuration (fournisseur, modèle, `concurrency`, `timeout_secs`, `max_chunk_chars`, `batch_files`, ou `llm inactif`) ;
- la vérification de la clé ;
- le parcours des fichiers ;
- le début et la fin de chaque phase (structure, résolution, enrichissement) ;
- le départ et la fin de chaque appel LLM, avec son résultat et sa durée ;
- les décisions d'arrêt.

```
trace: [0.012s] llm enrich départ src/shop/Order.java [1/1] 812 car.
trace: [0.031s] llm enrich fin src/shop/Order.java [1/1] ok (1 résumés, 0 concepts) 19 ms
trace: [1.040s] llm enrich fin docs/guide.md [1/1] délai dépassé 1003 ms : délai dépassé avec openai sur https://api.openai.com/v1/chat/completions ; augmentez timeout_secs
trace: [1.053s] politique: arrêt de l'envoi (clé d'API refusée par le fournisseur), 3 fichiers non envoyés
```

Les traces ne contiennent jamais de contenu de fichier, de prompt, de réponse du LLM ni de clé d'API.

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

Le slug est le nom du dossier analysé en minuscules ASCII. Chaque suite de caractères hors `[a-z0-9]` y devient un seul `-` : `Mon Dépôt_2` donne `mon-d-p-t-2`. Si le nom est vide, le slug vaut `graphe`. Les fichiers `.json` et `.html` ne sont pas analysés : un export écrit dans le dépôt ne change ni le graphe ni `status`.

**Format JSON** : objet avec `version`, `nodes`, `edges` et `unresolved`.

**Page HTML** : fichier unique, CSS et JS embarqués, sans aucune ressource externe ni requête réseau. Elle s'ouvre en `file://`. Elle n'affiche jamais tout le graphe, mais une vue partielle que l'on étend :

- **Vue initiale** : les 100 modules les plus reliés et les liens entre eux, avec « N modules non affichés » si certains sont écartés. Sans module, ce sont les 100 nœuds les plus reliés.
- **Recherche** : sur le nom court ou qualifié, sans casse, 20 résultats au plus. Choisir un résultat affiche le nœud et ses voisins.
- **Détails** : un clic sur un nœud ouvre un panneau (genre, nom qualifié, fichier, résumé, liens par genre). Les liens y sont cliquables.
- **Extension** : un double-clic ou le bouton « Étendre » ajoute les voisins directs. La vue est plafonnée à 300 nœuds : les voisins les plus reliés sont retenus, et la page indique « N voisins non affichés ». Si la vue est pleine, naviguer vers un nouveau nœud remplace la vue par ce nœud et ses voisins.
- **Filtres** : une case par genre de nœud et par genre de lien. Le bouton « Vue initiale » ramène à la vue de départ.
- Déplacement, zoom et glisser d'un nœud à la souris.

## Configuration du LLM

La configuration est facultative. Le LLM est actif quand un fichier de configuration existe et que `--no-llm` est absent. `nephila` cherche ce fichier TOML dans cet ordre :

1. le chemin donné par la variable `NEPHILA_CONFIG` ;
2. `$XDG_CONFIG_HOME/nephila/config.toml` ;
3. `~/.config/nephila/config.toml`.

Un exemple commenté est fourni dans [`examples/config.toml`](examples/config.toml).

| Clé | Rôle | Défaut |
|---|---|---|
| `provider` | `anthropic`, `openai` (protocole compatible OpenAI) ou `mock` | obligatoire |
| `model` | Nom du modèle | obligatoire, sauf `mock` |
| `api_key_env` | **Nom** de la variable d'environnement qui contient la clé | obligatoire pour `anthropic` |
| `base_url` | URL du serveur, **sans** `/v1` final | celle du fournisseur |
| `max_chunk_chars` | Taille maximale d'un morceau envoyé au LLM | 60000 |
| `concurrency` | Appels simultanés | 4 |
| `batch_files` | Fichiers par appel d'enrichissement, entier > 0 | 1 |
| `timeout_secs` | Durée maximale d'un appel HTTP, entier > 0 (la connexion reste limitée à 10 s) | 300 |

La clé d'API n'est jamais écrite dans le fichier. Avant tout traitement, `nephila` la vérifie par un appel qui liste les modèles, sans coût de génération.

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

`nephila` ajoute lui-même `/v1/chat/completions` et `/v1/models` à `base_url`. Avec `http://localhost:11434/v1`, les requêtes partiraient donc vers `/v1/v1/…`.

**Comportement selon la configuration**

| Situation | `build` / `update` | `query ask` |
|---|---|---|
| Aucun fichier, ou `NEPHILA_CONFIG` vers un fichier absent | structure seule, code 0, `enrichissement : désactivé` | échoue, demande une configuration |
| Fichier valide | structure et enrichissement | répond |
| Fichier invalide | échoue (sauf avec `--no-llm`) | échoue |

## Enrichissement en lots

Par défaut (`batch_files = 1`), un appel LLM porte sur un seul fichier. Pour un dépôt fait de nombreux petits fichiers, `batch_files = 4` à `8` regroupe plusieurs fichiers par appel et réduit le nombre d'appels.

- Les fichiers sont pris dans l'ordre des chemins. Un lot ne dépasse ni `batch_files` fichiers ni `max_chunk_chars` octets au total. Un fichier plus gros que `max_chunk_chars` part seul, en morceaux successifs.
- `--batch-files <N>` remplace la clé pour la commande en cours, sans rien écrire. Priorité : option, puis clé `batch_files`, puis défaut 1. Sans LLM actif, l'option est acceptée sans effet.
- Jusqu'à `concurrency` lots sont en vol en même temps, soit `concurrency` × `batch_files` fichiers.
- Si un lot échoue, tous ses fichiers restent « à enrichir » et sont repris par le prochain `nephila update`. L'échec compte pour un seul appel dans la politique d'arrêt. Un fichier absent de la réponse échoue seul.
- Si la réponse du modèle est tronquée par sa limite de jetons de sortie, tout le lot échoue. Réduisez alors `batch_files`.

## Ce que l'analyse extrait

Chaque fichier est analysé localement : tree-sitter pour les langages JVM, un lecteur XML pour Maven et Ivy, un lecteur lexical pour Gradle, et CommonMark pour Markdown.

| Fichier | Nœuds | Liens |
|---|---|---|
| `.java`, `.kt`, `.kts` (hors Gradle), `.groovy`, `.scala` | package, types (class, interface, enum, annotation, object, trait Scala ; imbriqués `Externe.Interne`), méthodes (constructeurs `<init>`) ; fonctions de premier niveau rattachées au type de fichier conventionnel (`NomFichierKt`, `NomFichier`, `NomFichier$package`) | `imports`, `extends`, `implements`, `calls` |
| `pom.xml`, `ivy.xml` | module `groupId:artifactId` ou `organisation:module` | `depends_on` (hors `dependencyManagement`, plugins et `parent`) |
| `build.gradle(.kts)`, `settings.gradle(.kts)` | module = chemin de projet (`:services:api`) ; `settings` est suivi mais ne crée pas de nœud | `depends_on` : `project(':core')`, chaînes `g:a[:v]`, notation map ; seul le bloc `dependencies { }` de premier niveau compte |
| autre `.gradle` | analysé comme un script Groovy | comme Groovy |
| `.md`, `.markdown` | document, sections (ancres à la GitHub) | `links_to` (liens relatifs vers un `.md`), `mentions` (code en ligne au format de nom JVM : `Order`, `shop.Order`, `place()`, `Order#place`) |

**Résolution des appels.** Le type du receveur est cherché dans le fichier seulement : paramètre, variable locale, ou champ typé ou initialisé par un constructeur. Un receveur non déclaré qui commence par une majuscule est traité comme un appel statique. `this.m()` et `m()` visent le type englobant s'il déclare `m`. Quand le type du receveur est inconnu (chaîne d'appels, `var` sans initialiseur reconnaissable), aucun lien n'est créé : un lien manquant coûte moins cher qu'un lien faux.

Les références qui ne trouvent pas de cible sont comptées dans « références non résolues ». Un fichier mal formé est exploité au mieux, sans faire échouer la commande.

## Fonctionnement

- **Stockage** : le graphe est une base SQLite dans `<dépôt>/.nephila/`. Ajoutez `.nephila/` au `.gitignore` du dépôt analysé.
- **Fichiers ignorés** : `.nephila/`, `target/`, `build/`, `.gradle/`, `.git/` et tout ce qu'exclut le `.gitignore` du dépôt.
- **Détection des changements** : par empreinte du contenu. Un fichier modifié moins de 2 secondes avant son enregistrement est relu au passage suivant, pour ne manquer aucune modification. Cela coûte une lecture, jamais un appel LLM.
- **Concurrence** : un seul `build` ou `update` à la fois par dépôt, grâce au verrou `.nephila/lock`. Une seconde commande échoue aussitôt (`erreur: indexation déjà en cours sur ce dépôt`). Le verrou disparaît avec le processus, même après un arrêt brutal.

## Codes de sortie

| Code | Signification |
|---|---|
| 0 | Succès |
| 1 | Erreur d'exécution : configuration, clé refusée, réseau, graphe absent, aucun résultat, fichiers en attente ou à enrichir avec le LLM actif… |
| 2 | Erreur d'usage : commande ou option inconnue, valeur invalide |

Les messages sont écrits sur stderr, préfixés `erreur:` ou `avertissement:`. stdout ne contient que le résultat.

## Usage par un agent

Les sorties `--json` sont stables et sans décoration. Séquence type :

1. `nephila status --json` : si `a_jour` vaut `false`, lancer `nephila update`.
2. `nephila query search --json <terme>` pour localiser un élément, `nephila query neighbors --json <nœud> --depth 2` pour explorer ses dépendances, `nephila query path --json <a> <b>` pour relier deux éléments.
3. `nephila summary --json` pour une vue d'ensemble, `nephila export --output -` pour charger tout le graphe.

Testez le code de sortie (0, 1, 2) et lisez stderr pour les messages.

## Limites connues

- **`trait` Groovy** : la grammaire utilisée ne reconnaît pas `trait`. Un trait ne produit ni nœud ni méthode, sans avertissement. Une classe qui l'implémente garde une référence `implements` non résolue.
- **Premier niveau d'un script Groovy** : un appel sans parenthèses (`helper 42`, `println "hello"`, `apply plugin: 'java'`) et une fermeture isolée `{ … }` sont ignorés. Écrivez `helper(42)` pour obtenir le lien `calls`. Dans le corps d'une fonction, l'appel sans parenthèses reste reconnu, sauf directement dans le bloc d'une méthode de classe.
- **`println x` en Groovy** : un appel sans parenthèses dont l'argument est un identifiant seul est lu comme une déclaration de variable et ne crée pas de lien.
- **Chemins Gradle relatifs** : `project("core")` (sans `:` initial) est ignoré. Écrivez `project(':core')`.
- **`query path`** : renvoie le plus court chemin sans tenir compte du sens des liens. Il peut donc passer par un `import` ou un package plutôt que par des appels.

## Mise à niveau

- **Renommage `rkg` → `nephila`** : le binaire s'appelle `nephila`, le graphe est stocké dans `.nephila/` (au lieu de `.rkg/`), la configuration dans `~/.config/nephila/config.toml` et les variables d'environnement sont préfixées `NEPHILA_` (`NEPHILA_CONFIG`, `NEPHILA_MOCK_DIR`…). Déplacez votre configuration (`mv ~/.config/rkg ~/.config/nephila`), puis reconstruisez le graphe avec `nephila build` et supprimez l'ancien dossier `.rkg/`.
- **Vers 0.4** : `nephila export` sans option écrit dans un fichier au lieu de stdout. Pour l'ancien comportement, utilisez `nephila export --output -`.
- **Depuis 0.2** : le schéma du graphe a changé. Un graphe ancien est refusé (`… lancez nephila build`) : reconstruisez-le avec `nephila build`. Les fixtures du fournisseur `mock` passent au schéma version 2.

| Version | Nouveautés |
|---|---|
| 0.8.1 | Premiers binaires précompilés (Linux musl, macOS) publiés par la CI |
| 0.8 | Renommage `rkg` → `nephila` (binaire, dossier `.nephila/`, configuration, variables `NEPHILA_*`) |
| 0.7 | Option `--batch-files <N>` de `build` et `update` |
| 0.6 | Enrichissement en lots (clé `batch_files`) |
| 0.5 | Option `-v, --verbose` de `build` et `update` |
| 0.4 | Export dans un fichier, page HTML d'exploration |
| 0.3 | Structure extraite par analyse syntaxique, sans LLM ; LLM facultatif |

## Développement

L'intégration continue (`.github/workflows/ci.yml`) lance le lint, les tests sur Linux et macOS, et les tests de la vue à chaque push sur `master` et à chaque pull request.

**Publier une version** : mettre à jour `version` dans `Cargo.toml` et le README, commiter, puis pousser un tag `vX.Y.Z` identique à cette version. Le workflow `.github/workflows/release.yml` vérifie la correspondance, relance la CI, compile les quatre cibles et publie la release GitHub avec les archives et `SHA256SUMS`.

```sh
git tag v0.8.1 && git push origin v0.8.1
```

```sh
cargo test -q                                                   # suite complète
cargo fmt --all -- --check
cargo clippy -q --all-targets -- -D warnings                    # lint
cargo test --release -q -- --ignored perf                       # performances (dépôt de 10 000 fichiers)
node --test --test-reporter=dot 'tests/view/*.test.js'          # logique de la vue HTML
```

La logique de la vue HTML est un module JS pur, testé par `node --test`. Node 20 ou plus est requis pour ces tests seulement, sans npm ni dépendance : le produit n'en dépend pas. Un test Rust compare les données de la vue à la fixture `tests/view/fixtures/maven3.json`. Après un changement volontaire de l'export, régénérez-la avec `NEPHILA_UPDATE_VIEW_FIXTURE=1 cargo test view_fixture`. Le rendu et les interactions souris se vérifient à la main.

### LLM simulé (`provider = "mock"`)

Le fournisseur `mock` remplace le LLM par des fixtures et se pilote par l'environnement :

| Variable | Effet |
|---|---|
| `NEPHILA_MOCK_DIR` | Dossier de fixtures : `<dir>/<chemin relatif>.json` (ou `<chemin>.<n>.json` pour le morceau n, à partir de 1) est la réponse d'enrichissement ; `_ask.json` est la réponse de `query ask`. Sans fixture, la réponse est vide et valide. |
| `NEPHILA_MOCK_LOG` | Journal des appels : une ligne `chemin<TAB>n°morceau` par appel, ou `chemin_1;chemin_2;…<TAB>1` pour un lot |
| `NEPHILA_MOCK_FAIL_AFTER` | Nombre d'appels réussis avant des erreurs réseau simulées |
| `NEPHILA_MOCK_INVALID_KEY` | Si non vide et différent de `0`, la vérification de clé échoue |
| `NEPHILA_MOCK_DELAY_MS` | Délai ajouté à chaque appel |
| `NEPHILA_MOCK_TIMEOUT_FILES` | Chemins relatifs, séparés par des virgules, dont l'enrichissement échoue par délai dépassé |
| `NEPHILA_MOCK_KEY_REVOKED_AFTER` | Nombre d'appels d'enrichissement avant des refus de clé simulés |

Une fixture d'enrichissement (schéma version 2) contient obligatoirement `version`, `summaries` et `concepts`, avec des listes vides au besoin. Les résumés visent des nœuds du fichier par leur identifiant `genre:nom`. Les concepts ne sont acceptés que pour un fichier Markdown et citent ses sections par leur identifiant complet. Tout le reste est écarté avec un avertissement, et une fixture ne crée jamais de nœud de structure.

Pour `src/shop/Order.java`, le fichier `<dir>/src/shop/Order.java.json` :

```json
{"version":2,
 "summaries":[{"node":"type:shop.Order","summary":"Une commande."}],
 "concepts":[]}
```

Pour `docs/guide.md`, le fichier `<dir>/docs/guide.md.json` :

```json
{"version":2,
 "summaries":[],
 "concepts":[{"name":"Facturation","summary":"Paiement des commandes.","sections":["section:docs/guide.md#facturation"]}]}
```

Format de `_ask.json` : `{"found":true,"answer":"...","nodes":["method:shop.Order.place"]}` ou `{"found":false}`.

## Licence

Aucune licence n'est encore déclarée pour ce projet.
