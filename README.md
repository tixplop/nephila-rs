# rkg - graphe de connaissances d'un dépôt

`rkg` analyse un dépôt (Java, Kotlin, Groovy, Scala, fichiers de build Maven/Gradle/Ivy, Markdown) avec un LLM, stocke le résultat dans un graphe SQLite local (nœuds : modules, types, méthodes, documents... ; liens : `contains`, `calls`...) et permet de l'interroger sans LLM, sauf pour `query ask`.

## Installation

```sh
cargo install --path .
```

Cela installe le binaire `rkg`. Sans installation : `cargo build --release`, binaire dans `target/release/rkg`.

## Configuration du LLM

`build`, `update` et `query ask` lisent un fichier TOML, cherché dans cet ordre :

1. la variable `RKG_CONFIG` ;
2. `$XDG_CONFIG_HOME/rkg/config.toml` ;
3. `~/.config/rkg/config.toml`.

Un exemple commenté est fourni dans `examples/config.toml`.

| Clé | Rôle |
|---|---|
| `provider` | `anthropic`, `openai` (protocole compatible OpenAI) ou `mock` (obligatoire) |
| `model` | nom du modèle (obligatoire, sauf `mock`) |
| `api_key_env` | NOM de la variable d'environnement qui contient la clé (obligatoire pour `anthropic`, facultatif sinon) |
| `base_url` | URL du serveur (facultatif) |
| `max_chunk_chars` | taille maximale d'un morceau envoyé au LLM (défaut 60000) |
| `concurrency` | appels simultanés (défaut 4) |

La clé n'est jamais écrite dans le fichier : `api_key_env = "ANTHROPIC_API_KEY"` puis `export ANTHROPIC_API_KEY=...`. Avant tout traitement, `rkg` vérifie la clé par un appel de liste des modèles (sans coût de génération).

Anthropic :

```toml
provider = "anthropic"
model = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"
```

OpenAI :

```toml
provider = "openai"
model = "gpt-4o-mini"
api_key_env = "OPENAI_API_KEY"
```

Modèle local (Ollama, llama.cpp, vLLM...), sans clé :

```toml
provider = "openai"
model = "llama3.1"
base_url = "http://localhost:11434"
```

Attention : `base_url` s'écrit SANS `/v1` final. `rkg` ajoute lui-même `/v1/chat/completions` et `/v1/models` ; avec `http://localhost:11434/v1` les requêtes iraient vers `/v1/v1/...`.

Fournisseur simulé (tests, démonstration sans réseau) :

```toml
provider = "mock"
```

## Le dossier `.rkg/`

Le graphe est stocké dans `<dépôt>/.rkg/` (`graph.db`, fichiers WAL, `lock`). Ajoutez `.rkg/` au `.gitignore` du dépôt analysé. Le dossier `.rkg/`, `target/`, `build/`, `.gradle/` et `.git/` ne sont jamais analysés ; le `.gitignore` du dépôt est respecté.

## Commandes

Option globale : `-C/--repo <dossier>` (racine du dépôt, défaut : dossier courant). Les exemples ci-dessous viennent d'un petit dépôt avec le fournisseur `mock`.

### `rkg build`
Reconstruit entièrement le graphe (un appel LLM par fichier ou morceau).
```
$ rkg -C repo build
build terminé : 2 fichiers traités, 4 nœuds, 3 liens, 0 références non résolues, 0 fichiers en attente
```

### `rkg update`
Ne retraite que les fichiers ajoutés, modifiés ou supprimés ; même format de sortie que `build`.
```
$ rkg -C repo update
update terminé : 2 fichiers traités, 2 nœuds, 1 liens, 0 références non résolues, 0 fichiers en attente
```
Si des fichiers échouent (réseau, réponse non conforme), ils restent « en attente », la commande sort avec le code 1 et le message nomme le premier échec ; relancez `rkg update`.

### `rkg status [--json]`
Compare le dépôt au graphe, sans LLM.
```
$ rkg status
graphe pas à jour
supprimé	src/Order.java
ajouté	src/Order2.java
modifié	src/Payment.java

$ rkg status --json
{"a_jour":false,"fichiers":[{"chemin":"src/Order.java","état":"supprimé"},...]}
```
Graphe à jour : `graphe à jour`.

### `rkg query search <terme> [--limit N] [--json]`
```
$ rkg query search order
shop.Order (class, src/Order.java) - Une commande.
shop.Order.place (method, src/Order.java)
```
Sans résultat : `erreur: aucun résultat pour « zzz »` (code 1).

### `rkg query neighbors <nœud> [--depth N] [--limit N] [--json]`
Le nœud est un nom (`shop.Order`) ou un identifiant `genre:nom` (`method:shop.Order.place`).
```
$ rkg query neighbors method:shop.Order.place
utilise :
  calls shop.Payment.pay (method, src/Payment.java)
utilisé par :
  contains shop.Order (class, src/Order.java)
```

### `rkg query path <source> <cible> [--json]`
```
$ rkg query path method:shop.Order.place method:shop.Payment.pay
shop.Order.place calls shop.Payment.pay
```

### `rkg query ask "<question>" [--json]`
Seule requête qui appelle le LLM : des nœuds du graphe trouvés par les mots de la question (et leurs voisins) lui servent de contexte, jamais le contenu des fichiers.
```
$ rkg query ask "Que fait Order ?"
Order.place appelle Payment.pay.
Nœuds cités :
- shop.Order.place (method, src/Order.java)
```
Si rien ne correspond : `erreur: pas trouvé dans le graphe` (code 1).

### `rkg summary [--json]`
Modules, nœuds les plus reliés, thèmes.
```
$ rkg summary
Modules (0)
Nœuds les plus reliés
  method:shop.Order.place (degré 2)
  ...
```

### `rkg export [--output fichier]`
Exporte le graphe en JSON (`version`, `nodes`, `edges`, `unresolved`) sur stdout ou dans le fichier.

`status`, `query` (hors `ask`), `summary` et `export` ne font aucun appel LLM et ne demandent aucune configuration. Si le graphe est en retard sur les fichiers, les requêtes l'indiquent par `avertissement: graphe pas à jour : 1 fichier modifié` sur stderr.

## Codes de sortie

| Code | Signification |
|---|---|
| 0 | succès |
| 1 | erreur d'exécution (configuration, clé refusée, réseau, graphe absent, pas de résultat, fichiers en attente...) |
| 2 | erreur d'usage (commande ou option inconnue) |

Les messages sont préfixés `erreur:` ou `avertissement:` et écrits sur stderr ; stdout ne contient que le résultat.

## Limites à connaître

- **Graphe d'une ancienne version** : un graphe de schéma incompatible est refusé (`... lancez rkg build`) ; reconstruisez-le avec `rkg build`.
- **Un seul `build`/`update` à la fois** : un verrou (`.rkg/lock`) protège le dépôt ; une seconde commande échoue aussitôt avec `erreur: indexation déjà en cours sur ce dépôt`. Le verrou disparaît avec le processus, même après un arrêt brutal.
- **Fenêtre de 2 s après écriture** : un fichier modifié moins de 2 secondes avant son enregistrement dans le graphe est considéré comme « douteux » (même taille et même date possibles). Son empreinte est recalculée à chaque passage jusqu'à ce que cette fenêtre soit passée ; cela coûte une lecture du fichier, pas un appel LLM, et évite de manquer une modification.

## Usage par un agent

Les sorties `--json` sont stables et sans décoration. Séquence type :

1. `rkg status --json` : `a_jour` vaut `false` ? lancer `rkg update`.
2. `rkg query search --json <terme>` pour localiser, `rkg query neighbors --json <nœud> --depth 2` pour explorer les dépendances, `rkg query path` pour relier deux éléments.
3. `rkg summary --json` pour une vue d'ensemble, `rkg export` pour tout charger.

Tester le code retour (0, 1, 2) et lire stderr pour les messages.

## Tests et LLM simulé

```sh
cargo test -q --color never
cargo fmt --all -- --check && cargo clippy -q --all-targets --color never -- -D warnings
cargo test --release -q --color never -- --ignored perf   # performances, 10 000 fichiers
```

Avec `provider = "mock"`, le LLM est simulé et piloté par l'environnement :

| Variable | Effet |
|---|---|
| `RKG_MOCK_DIR` | dossier de fixtures : `<dir>/<chemin relatif>.json` (ou `<chemin>.<n>.json` pour le morceau n, à partir de 1) donne la réponse d'extraction ; `_ask.json` donne la réponse de `query ask` ; sans fixture, réponse vide valide |
| `RKG_MOCK_LOG` | journal des appels, une ligne `chemin<TAB>n°morceau` par appel |
| `RKG_MOCK_FAIL_AFTER` | nombre d'appels réussis avant des erreurs réseau simulées |
| `RKG_MOCK_INVALID_KEY` | si non vide et différent de `0`, la vérification de clé échoue |
| `RKG_MOCK_DELAY_MS` | délai ajouté à chaque appel |

Format d'une fixture d'extraction :

```json
{"version":1,"context":"package shop",
 "declares":[{"kind":"type","name":"shop.Order","subkind":"class","summary":"Une commande."},
             {"kind":"method","name":"shop.Order.place"}],
 "references":[{"source":"method:shop.Order.place","kind":"calls","target":"Payment.pay"}]}
```

Format de `_ask.json` : `{"found":true,"answer":"...","nodes":["method:shop.Order.place"]}` ou `{"found":false}`.
