# rkg - graphe de connaissances d'un dépôt

`rkg` analyse un dépôt (Java, Kotlin, Groovy, Scala, fichiers de build Maven/Gradle/Ivy, Markdown), stocke le résultat dans un graphe SQLite local (nœuds : modules, types, méthodes, documents... ; liens : `contains`, `calls`...) et permet de l'interroger sans LLM, sauf pour `query ask`.

Depuis la version 0.3.0, la structure du graphe vient d'une analyse syntaxique locale, sans LLM. Le LLM est facultatif : il ne sert qu'à enrichir le graphe (résumés des nœuds, concepts des documents Markdown) et à répondre à `query ask`. Sans configuration, ou avec `--no-llm`, `build` et `update` produisent la structure seule.

## Installation

```sh
cargo install --path .
```

Cela installe le binaire `rkg`. Sans installation : `cargo build --release`, binaire dans `target/release/rkg`.

## Configuration du LLM (facultative)

`build`, `update` (sauf avec `--no-llm`) et `query ask` lisent un fichier TOML, cherché dans cet ordre :

1. la variable `RKG_CONFIG` ;
2. `$XDG_CONFIG_HOME/rkg/config.toml` ;
3. `~/.config/rkg/config.toml`.

Un exemple commenté est fourni dans `examples/config.toml`.

Le LLM est actif quand ce fichier existe et que `--no-llm` est absent. Sans fichier de configuration, `build` et `update` ne font aucun appel, réussissent (code 0) et affichent `enrichissement : désactivé` dans leur bilan ; `query ask` échoue en demandant une configuration. Avec `--no-llm`, la configuration n'est pas lue (même invalide) et aucun appel n'est fait. Si `RKG_CONFIG` pointe vers un fichier absent, le LLM est désactivé sans erreur (comme sans configuration). Un fichier de configuration présent mais invalide fait échouer `build`/`update` (sans `--no-llm`).

| Clé | Rôle |
|---|---|
| `provider` | `anthropic`, `openai` (protocole compatible OpenAI) ou `mock` (obligatoire) |
| `model` | nom du modèle (obligatoire, sauf `mock`) |
| `api_key_env` | NOM de la variable d'environnement qui contient la clé (obligatoire pour `anthropic`, facultatif sinon) |
| `base_url` | URL du serveur (facultatif) |
| `max_chunk_chars` | taille maximale d'un morceau envoyé au LLM (défaut 60000) |
| `concurrency` | appels simultanés (défaut 4) |
| `timeout_secs` | durée maximale d'un appel LLM HTTP (extraction, `ask`, vérification de clé), entier > 0 (défaut 300) ; le délai de connexion reste fixe (10 s) ; ignoré par `mock` |

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

## Ce que l'analyse extrait

Chaque fichier est analysé localement (tree-sitter pour les langages JVM, lecteur XML, lecteur lexical Gradle, CommonMark), sans exécuter Maven, Gradle ni compilateur.

| Fichier | Nœuds | Références (liens) |
|---|---|---|
| `.java`, `.kt`, `.kts` (hors Gradle), `.groovy`, `.scala` | package, types (class, interface, enum, annotation, object, trait Scala ; imbriqués `Externe.Interne`), méthodes (constructeurs `<init>`) ; fonctions de premier niveau rattachées au type de fichier conventionnel (Kotlin `NomFichierKt`, script Groovy/Kotlin `NomFichier`, Scala 3 `NomFichier$package`) | `imports`, `extends`, `implements`, `calls` |
| `pom.xml`, `ivy.xml` | module `groupId:artifactId` / `organisation:module` | `depends_on` (dépendances ; `dependencyManagement`, plugins et `parent` exclus) |
| `build.gradle`, `build.gradle.kts`, `settings.gradle(.kts)` | module = chemin de projet (`:services:api`) ; `settings` suivi mais sans nœud | `depends_on` (`project(':core')`, chaînes `g:a[:v]`, notation map) ; seul le bloc `dependencies { }` de premier niveau compte ; catalogues `libs.…` et `files(…)` ignorés |
| autre `.gradle` (script appliqué) | analysé comme un script Groovy | idem Groovy |
| `.md`, `.markdown` | document (chemin), sections (titres, ancre à la GitHub) | `links_to` (liens relatifs vers un autre `.md`), `mentions` (spans de code en ligne au format de nom JVM : `Order`, `shop.Order`, `place()`, `Order#place`) |

Les références sont des noms ; elles sont résolues en liens à la fin du traitement, celles qui ne le sont pas sont comptées dans « références non résolues » (les mentions Markdown non résolues ne sont pas listées).

Heuristiques d'appels (DEC-021) : le type du receveur n'est cherché que dans le fichier (paramètre, variable locale, champ de type déclaré ou initialisé par un constructeur) ; un receveur non déclaré qui commence par une majuscule est un appel statique `T.m` ; `this.m()` ou `m()` visent le type englobant s'il déclare `m`. Receveur de type inconnu (chaîne d'appels, `var`/`val`/`def` sans initialiseur reconnaissable) : aucun lien, car un lien manquant coûte moins qu'un lien faux. Les mots du texte courant Markdown ne sont jamais des mentions.

Un fichier mal formé est exploité au mieux (ce qui précède l'erreur), sans échec.

## Fichiers « en attente » et « à enrichir »

- **en attente** : l'analyse (ou, pour un fichier de build, la lecture) du fichier n'a pas abouti ou n'a pas été faite ; la commande sort avec le code 1 ; `rkg update` le retraite.
- **à enrichir** : la structure du fichier est dans le graphe, mais son contenu actuel n'a pas été enrichi avec succès par le LLM (réponse non conforme, erreur, `--no-llm`, ou fichier modifié depuis). Réanalyser un fichier modifié efface ses résumés et concepts. Avec le LLM actif, un fichier à enrichir fait sortir `build`/`update` en code 1 ; `rkg update` ne renvoie au LLM que ces fichiers. Avec `--no-llm` ou sans configuration, ce n'est pas une erreur (code 0, `enrichissement : désactivé`). `rkg status` indique leur nombre.

Le bilan de `build`/`update` donne les deux compteurs : `... 0 fichiers en attente, 2 fichiers à enrichir`.

## Le dossier `.rkg/`

Le graphe est stocké dans `<dépôt>/.rkg/` (`graph.db`, fichiers WAL, `lock`). Ajoutez `.rkg/` au `.gitignore` du dépôt analysé. Le dossier `.rkg/`, `target/`, `build/`, `.gradle/` et `.git/` ne sont jamais analysés ; le `.gitignore` du dépôt est respecté.

## Commandes

Option globale : `-C/--repo <dossier>` (racine du dépôt, défaut : dossier courant). Les exemples ci-dessous viennent d'un petit dépôt avec le fournisseur `mock`.

### `rkg build`
Reconstruit entièrement le graphe : analyse locale de chaque fichier, puis, si le LLM est actif, un appel d'enrichissement par fichier (ou morceau). Option `--no-llm` : structure seule, sans lire la configuration.
```
$ rkg -C repo build
build terminé : 2 fichiers traités, 4 nœuds, 3 liens, 0 références non résolues, 0 fichiers en attente, 0 fichiers à enrichir
```

### `rkg update`
Ne retraite que les fichiers ajoutés, modifiés ou supprimés, plus les fichiers en attente ou à enrichir ; même format de sortie et même option `--no-llm` que `build`.
```
$ rkg -C repo update
update terminé : 2 fichiers traités, 2 nœuds, 1 liens, 0 références non résolues, 0 fichiers en attente, 0 fichiers à enrichir
```
Échecs (`build` comme `update`) :
- un fichier en échec (lecture impossible ; pour l'enrichissement : réponse non conforme, délai dépassé, erreur réseau isolée) reste « en attente » ou « à enrichir » et le traitement des autres fichiers continue ; sa structure, elle, est déjà dans le graphe ; chaque fichier en échec est nommé sur stderr (`avertissement:`) avec sa cause ;
- l'envoi s'arrête seulement sur une clé refusée (HTTP 401/403, dès le premier refus) ou sur 3 erreurs réseau consécutives (une réussite remet le compte à zéro ; seuil fixe) ;
- les appels déjà en cours se terminent et sont enregistrés ; la ligne de bilan est toujours affichée ;
- s'il reste au moins un fichier en attente, ou à enrichir avec le LLM actif, la commande sort avec le code 1 : relancez `rkg update` ;
- une référence dont le genre de lien est inconnu ou non autorisé depuis le genre de son nœud source est écartée avec un `avertissement:` (fichier et nombre de références écartées) ; le fichier est indexé normalement.

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
| 1 | erreur d'exécution (configuration, clé refusée, réseau, graphe absent, pas de résultat, fichiers en attente ou à enrichir avec le LLM actif...) |
| 2 | erreur d'usage (commande ou option inconnue) |

Les messages sont préfixés `erreur:` ou `avertissement:` et écrits sur stderr ; stdout ne contient que le résultat.

## Limites à connaître

- **Graphe d'une ancienne version** : un graphe de schéma incompatible, en particulier un graphe créé par la version 0.2, est refusé (`... lancez rkg build`) ; reconstruisez-le avec `rkg build`.
- **`trait` Groovy non géré** (DEC-016) : la grammaire Groovy utilisée ne reconnaît pas `trait`. Un trait ne produit ni nœud ni méthode, sans avertissement ; le reste du fichier est exploité au mieux. Une classe qui l'implémente (`class A implements Greeter`) garde une référence `implements` non résolue tant que rien d'autre ne déclare ce nom.
- **Premier niveau d'un script Groovy** (DEC-023) : seuls les déclarations de variables et les appels avec parenthèses (`helper(42)`, `new T()`, affectations) sont des instructions de script. Un appel de commande sans parenthèses (`helper 42`, `println "hello"`, `apply plugin: 'java'`) et une fermeture isolée `{ ... }` sont ignorés : pas de lien, et un script qui n'en contient pas d'autres ne produit aucun nœud. Contournement : écrire `helper(42)` pour obtenir le lien `calls`. Dans le corps d'une méthode ou d'une fonction, l'appel de commande reste reconnu (sauf directement dans le bloc d'une méthode de classe, où la grammaire le lit comme une erreur).
- **`println x` lu comme une déclaration** : un appel de commande dont l'argument est un identifiant seul (`println x`, `helper x`) est lu par la grammaire comme une déclaration de variable (type `println`, variable `x`) : instruction de script sans lien `calls`.
- **Chemins relatifs Gradle** : `project("core")` (sans `:` initial, chemin relatif) n'est pas pris en compte ; écrire `project(':core')`.
- **Un seul `build`/`update` à la fois** : un verrou (`.rkg/lock`) protège le dépôt ; une seconde commande échoue aussitôt avec `erreur: indexation déjà en cours sur ce dépôt`. Le verrou disparaît avec le processus, même après un arrêt brutal.
- **Fenêtre de 2 s après écriture** : un fichier modifié moins de 2 secondes avant son enregistrement dans le graphe est considéré comme « douteux » (même taille et même date possibles). Son empreinte est recalculée à chaque passage jusqu'à ce que cette fenêtre soit passée ; cela coûte une lecture du fichier, pas un appel LLM, et évite de manquer une modification.

## Usage par un agent

Les sorties `--json` sont stables et sans décoration. Séquence type :

1. `rkg status --json` : `a_jour` vaut `false` ? lancer `rkg update`.
2. `rkg query search --json <terme>` pour localiser, `rkg query neighbors --json <nœud> --depth 2` pour explorer les dépendances, `rkg query path` pour relier deux éléments.
3. `rkg summary --json` pour une vue d'ensemble, `rkg export` pour tout charger.

Tester le code retour (0, 1, 2) et lire stderr pour les messages.

## Mise à jour depuis la version 0.2

Le schéma du graphe a changé : lancez `rkg build` pour reconstruire le graphe (l'ancien est refusé avec ce message). Les fixtures du fournisseur `mock` changent aussi (schéma version 2, ci-dessous) : une fixture `"version":1` est une réponse non conforme.

## Tests et LLM simulé

```sh
cargo test -q --color never
cargo fmt --all -- --check && cargo clippy -q --all-targets --color never -- -D warnings
cargo test --release -q --color never -- --ignored perf   # performances, 10 000 fichiers
```

Avec `provider = "mock"`, le LLM est simulé et piloté par l'environnement :

| Variable | Effet |
|---|---|
| `RKG_MOCK_DIR` | dossier de fixtures : `<dir>/<chemin relatif>.json` (ou `<chemin>.<n>.json` pour le morceau n, à partir de 1) donne la réponse d'enrichissement ; `_ask.json` donne la réponse de `query ask` ; sans fixture, réponse vide valide |
| `RKG_MOCK_LOG` | journal des appels, une ligne `chemin<TAB>n°morceau` par appel |
| `RKG_MOCK_FAIL_AFTER` | nombre d'appels réussis avant des erreurs réseau simulées |
| `RKG_MOCK_INVALID_KEY` | si non vide et différent de `0`, la vérification de clé échoue |
| `RKG_MOCK_DELAY_MS` | délai ajouté à chaque appel |
| `RKG_MOCK_TIMEOUT_FILES` | chemins relatifs séparés par des virgules : l'enrichissement de ces fichiers échoue par « délai dépassé » |
| `RKG_MOCK_KEY_REVOKED_AFTER` | nombre d'appels d'enrichissement (réussis ou non, un par morceau) avant des refus de clé simulés (arrêt de l'envoi) |

Format d'une fixture d'enrichissement (schéma version 2) : `version`, `summaries` et `concepts` sont tous obligatoires (listes vides au besoin). Les résumés s'appliquent aux nœuds du fichier (identifiants `genre:nom`), les concepts ne sont acceptés que pour un fichier Markdown et citent des sections du fichier par leur identifiant complet, préfixe `section:` compris. Un résumé pour un nœud inconnu, un concept d'un autre type de fichier ou une section hors fichier sont écartés avec un `avertissement:`. Une fixture ne crée aucun nœud de structure.

Une fixture par fichier, dans `RKG_MOCK_DIR` : pour `src/shop/Order.java`, le fichier `<dir>/src/shop/Order.java.json` (résumé, pas de concept) :

```json
{"version":2,
 "summaries":[{"node":"type:shop.Order","summary":"Une commande."}],
 "concepts":[]}
```

et pour `docs/guide.md`, le fichier `<dir>/docs/guide.md.json` (un concept, pas de résumé) :

```json
{"version":2,
 "summaries":[],
 "concepts":[{"name":"Facturation","summary":"Paiement des commandes.","sections":["section:docs/guide.md#facturation"]}]}
```

Format de `_ask.json` : `{"found":true,"answer":"...","nodes":["method:shop.Order.place"]}` ou `{"found":false}`.
