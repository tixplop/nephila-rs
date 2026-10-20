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
- [Filtrer l'analyse](#filtrer-lanalyse)
- [Export et vue HTML](#export-et-vue-html)
- [Configuration du LLM](#configuration-du-llm)
- [Proxy d'entreprise et certificats auto-signés](#proxy-dentreprise-et-certificats-auto-signés)
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
V=v0.16.1                                      # version voulue
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
nephila build  [--no-llm] [-v|--verbose | --show-progress] [--batch-files <N>] [--format-retries <N>] [--exclude <MOTIF>] [--exclude-type <TYPES>]
nephila update [--no-llm] [-v|--verbose | --show-progress] [--batch-files <N>] [--format-retries <N>] [--exclude <MOTIF>] [--exclude-type <TYPES>]
```

`build` analyse chaque fichier puis, si le LLM est actif, enrichit le graphe. `update` ne retraite que les fichiers ajoutés, modifiés ou supprimés, plus ceux restés en attente ou à enrichir.

| Option | Effet |
|---|---|
| `--no-llm` | Structure seule : la configuration n'est pas lue (même invalide) et aucun appel n'est fait |
| `-v, --verbose` | Traces sur stderr (voir [Traces](#traces---verbose)) |
| `--show-progress` | Avancement des phases sur stderr, voir [Progression](#progression---show-progress) |
| `--batch-files <N>` | Taille de lot pour cette commande seulement (voir [Enrichissement en lots](#enrichissement-en-lots)) |
| `--format-retries <N>` | Nombre de nouveaux essais d'un appel dont la réponse est non conforme, pour cette commande seulement (voir [Réponses non conformes et nouveaux essais](#réponses-non-conformes-et-nouveaux-essais)) |
| `--exclude <MOTIF>` | Exclut de l'analyse les fichiers qui correspondent au motif ; répétable (voir [Filtrer l'analyse](#filtrer-lanalyse)) |
| `--exclude-type <TYPES>` | Exclut un ou plusieurs types de fichiers, séparés par des virgules ; répétable |

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

`query ask` accepte `--format-retries <N>` : une réponse non conforme est renvoyée aussitôt, jusqu'à `N` fois (voir [Réponses non conformes et nouveaux essais](#réponses-non-conformes-et-nouveaux-essais)). Si la dernière réponse est encore non conforme, stdout reste vide et stderr porte une ligne `erreur: … (<k> essais sur <t>)`.

### `nephila summary`

```
$ nephila summary
Modules (0)
Nœuds les plus reliés
  method:shop.Order.place (degré 2)
  ...
```

### Traces (`--verbose`)

Avec `-v`, `build` et `update` écrivent sur stderr une ligne par événement, préfixée `trace:` et horodatée depuis le lancement. stdout est inchangé : `nephila build -v 2>trace.log` garde le bilan lisible. `--verbose` est incompatible avec `--show-progress` (erreur d'usage) : voir [Progression](#progression---show-progress).

Événements tracés :

- la configuration (fournisseur, modèle, `concurrency`, `timeout_secs`, `max_chunk_chars`, `batch_files`, `format_retries`, ou `llm inactif`), complétée de ` tls_ca_cert=<chemin résolu>` si cette clé est donnée et de ` tls_insecure` si elle vaut `true` ; quand un réglage vient de `.nephila.toml`, la ligne se termine par ` (.nephila.toml : <réglages>)` ;
- la vérification de la clé ;
- le parcours des fichiers ;
- le début et la fin de chaque phase (structure, résolution, enrichissement) ;
- le départ et la fin de chaque appel LLM, avec son résultat et sa durée ; le départ d'un nouvel essai porte la marque ` (essai <e>/<t>)`, où `<t>` vaut `format_retries` + 1 (le premier essai n'a pas de marque) ;
- les décisions d'arrêt.

```
trace: [0.000s] config: llm=mock model=m-projet concurrency=4 timeout_secs=300 max_chunk_chars=60000 batch_files=1 format_retries=1 (.nephila.toml : model,format_retries)
trace: [0.012s] llm enrich départ src/shop/Order.java [1/1] 812 car.
trace: [0.031s] llm enrich fin src/shop/Order.java [1/1] ok (1 résumés, 0 concepts) 19 ms
trace: [0.003s] llm enrich fin src/A.java [1/1] erreur 0 ms : réponse LLM non conforme au schéma
trace: [0.003s] llm enrich départ src/A.java [1/1] 24 car. (essai 2/3)
trace: [0.003s] llm enrich fin src/A.java [1/1] erreur 0 ms : réponse LLM non conforme au schéma
trace: [0.003s] llm enrich départ src/A.java [1/1] 24 car. (essai 3/3)
trace: [0.003s] llm enrich fin src/A.java [1/1] ok (0 résumés, 0 concepts) 0 ms
trace: [1.040s] llm enrich fin docs/guide.md [1/1] délai dépassé 1003 ms : délai dépassé avec openai sur https://api.openai.com/v1/chat/completions ; augmentez timeout_secs
trace: [1.053s] politique: arrêt de l'envoi (clé d'API refusée par le fournisseur), 3 fichiers non envoyés
```

Les traces ne contiennent jamais de contenu de fichier, de prompt, de réponse du LLM ni de clé d'API.

### Progression (`--show-progress`)

Avec `--show-progress`, `build` et `update` écrivent sur stderr l'avancement de leurs deux phases, `extraction` puis `enrichissement`, et elles seules. Chaque ligne suit le gabarit `progression: <phase> <p> % (<k>/<n> fichiers)`. stdout est inchangé.

- L'unité est le fichier : `k` fichiers traités sur `n`. Un lot avance d'un bloc ; les morceaux et les nouveaux essais ne sont pas comptés ; un fichier en échec est compté comme traité.
- Une ligne au départ de la phase (`0 %`), puis une par palier de 10 % franchi : au plus 11 lignes par phase, quelle que soit la taille du dépôt.
- `update` ne compte que les fichiers à traiter.
- Si l'envoi au LLM s'arrête (clé refusée, panne répétée), la dernière ligne d'enrichissement reste sous 100 % : l'écart `n` − `k` est le nombre de fichiers non envoyés.
- Aucune ligne pour une phase sans fichier, ni pour l'enrichissement avec `--no-llm`.
- Les lignes sont entières et jamais réécrites : elles restent lisibles redirigées dans un fichier et se filtrent par `grep '^progression:'`.
- `--show-progress` est incompatible avec `--verbose` (erreur d'usage).

Exemple, `build --show-progress --batch-files 4` sur 10 fichiers Java avec le LLM simulé :

```
$ nephila build --show-progress --batch-files 4
progression: extraction 0 % (0/10 fichiers)
progression: extraction 10 % (1/10 fichiers)
progression: extraction 20 % (2/10 fichiers)
progression: extraction 30 % (3/10 fichiers)
progression: extraction 40 % (4/10 fichiers)
progression: extraction 50 % (5/10 fichiers)
progression: extraction 60 % (6/10 fichiers)
progression: extraction 70 % (7/10 fichiers)
progression: extraction 80 % (8/10 fichiers)
progression: extraction 90 % (9/10 fichiers)
progression: extraction 100 % (10/10 fichiers)
progression: enrichissement 0 % (0/10 fichiers)
progression: enrichissement 40 % (4/10 fichiers)
progression: enrichissement 80 % (8/10 fichiers)
progression: enrichissement 100 % (10/10 fichiers)
build terminé : 10 fichiers traités, 11 nœuds, 10 liens, 0 références non résolues, 0 fichiers en attente, 0 fichiers à enrichir
```

## Filtrer l'analyse

Par défaut, `nephila` analyse tout ce qu'il prend en charge. Les filtres retirent de l'analyse des fichiers sans intérêt (code généré, fichiers minifiés, données). Ils ne font qu'exclure : aucun filtre ne fait indexer un fichier que `.gitignore` ou les exclusions fixes (dossiers `.git/`, `target/`, `node_modules/`, fichiers de verrouillage…) écartent déjà.

### Motifs

Un motif suit la syntaxe `.gitignore`, relativement à la racine du dépôt, et est sensible à la casse :

- sans `/`, il vaut à tout niveau : `*.min.js` ;
- un `/` en tête ou au milieu l'ancre à la racine : `/src/gen/`, `docs/legacy/` ;
- un `/` final désigne un dossier : tout son contenu est exclu ;
- `**` traverse les dossiers : `**/generated/**` ;
- `!motif` réinclut un fichier exclu par un motif précédent de la liste : `!docs/garde.md`.

### Types

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

Il n'y a pas de groupe : `--exclude-type sql,xml,json,yaml` exclut les fichiers de données.

### En ligne de commande

```
nephila build --exclude '*.min.js' --exclude-type json,yaml
```

`--exclude` et `--exclude-type` (de `build` et `update`) valent pour la commande seulement et ne sont enregistrées nulle part ; la commande écrit un avertissement sur stderr. `status` ne les accepte pas : il compare le graphe au parcours filtré par le seul fichier de projet.

### Fichier de projet `.nephila.toml`

Pour un filtre durable, créez `.nephila.toml` à la racine du dépôt (le dossier de `-C`) et versionnez-le avec le dépôt :

```toml
# Filtres d'analyse de nephila (exclusions seulement)
exclude = ["docs/legacy/", "*.min.js"]   # motifs .gitignore, relatifs à la racine
exclude_types = ["json"]                 # types : voir nephila build --help
```

Les filtres (`exclude`, `exclude_types`) et les six réglages sont facultatifs ; toute autre clé est une erreur. Le fichier est lu par `build`, `update`, `status` et `query` (pour l'avertissement « graphe pas à jour » et, avec `query ask`, pour les réglages ci-dessous), pas par `export` ni `summary`. Il n'active pas le LLM et ne remplace pas la [configuration du LLM](#configuration-du-llm), qui reste personnelle. `.nephila.toml` n'est jamais indexé.

Le même fichier porte aussi les réglages LLM du dépôt, au premier niveau, avec le même nom et le même type que dans la configuration personnelle : on copie une ligne d'un fichier à l'autre.

```toml
# Réglages LLM du dépôt (prioritaires sur la configuration personnelle)
model = "qwen2.5-coder:14b"
format_retries = 4
```

Les six réglages sont `model` (chaîne non vide), `max_chunk_chars`, `concurrency`, `timeout_secs`, `batch_files` (entiers ≥ 1) et `format_retries` (entier ≥ 0). Un réglage du fichier de projet remplace celui de la configuration personnelle ; un réglage absent garde la valeur personnelle, ou le défaut. Priorité : option > fichier de projet > configuration personnelle > défaut (les options sont `--batch-files` et `--format-retries`).

- **Clés de connexion refusées** : `provider`, `base_url`, `api_key_env`, `tls_ca_cert` et `tls_insecure` ne sont permises que dans la configuration personnelle. Le fichier de projet est versionné et vient souvent d'un dépôt cloné : s'il pouvait changer l'adresse du serveur ou la variable qui contient la clé, cloner un dépôt suffirait à envoyer votre code et une clé d'API (ou un autre secret de l'environnement) à un serveur tiers, ou à désactiver le contrôle des certificats.
- **Sans LLM actif** (aucune configuration personnelle, ou `--no-llm`), les réglages n'ont aucun effet et aucun message : un collègue sans configuration ne voit rien. Le fichier reste validé en entier.
- **Changer de modèle ne réenrichit rien** : `status` et `update` disent `graphe à jour`. Pour réenrichir avec le nouveau modèle, lancez `nephila build`.
- **Dépôt tiers** : un dépôt cloné peut choisir un modèle plus coûteux ou une `concurrency` plus forte que les vôtres. Vérifiez avec `nephila build --verbose` : la ligne `config` donne les valeurs effectives et nomme les réglages venus de `.nephila.toml` (voir [Traces](#traces---verbose)).

Un exemple commenté est fourni dans [`examples/nephila.toml`](examples/nephila.toml).

### Fusion des deux sources

Les types sont réunis. Les motifs forment une seule liste, ceux du fichier puis ceux de l'option, et le dernier motif qui s'applique l'emporte : `--exclude '!docs/garde.md'` réinclut un fichier exclu par le fichier de projet.

### Effet incrémental

Un fichier indexé puis exclu est traité par `update` comme supprimé (nœuds, références et liens retirés) ; un fichier qui cesse d'être exclu est traité comme ajouté. `update` suffit : pas besoin de `build`. Un fichier de build exclu (`pom.xml`, `package.json`, `build.gradle`…) ne déclare pas de module.

```
$ nephila build --exclude '*.min.js' --exclude-type json --verbose
trace: [0.001s] config: llm inactif (aucune configuration)
trace: [0.018s] parcours: 1 fichiers (filtres : 1 motifs, types json, 2 fichiers exclus)
build terminé : 1 fichiers traités, 1 nœuds, 0 liens, 0 références non résolues, 0 fichiers en attente, 1 fichiers à enrichir, enrichissement : désactivé, 2 fichiers exclus par les filtres
avertissement: --exclude et --exclude-type valent pour cette commande seulement : status et update sans option ne les appliquent pas (filtre durable : .nephila.toml)
```

Le segment `fichiers exclus par les filtres` n'apparaît que si un filtre est actif ; un compte à `0` révèle un motif qui ne correspond à rien. Avec `--verbose` (exemple ci-dessus), la ligne `parcours` se termine par le détail des filtres (`filtres : 1 motifs, types json, 2 fichiers exclus`).

### Erreurs

Un fichier de projet invalide arrête toute commande qui parcourt le dépôt, avant toute analyse :

```
erreur: ./.nephila.toml : clé `exclude_types` : type inconnu `python` (types valides : markdown, java, kotlin, groovy, scala, javascript, typescript, sql, xml, json, yaml, maven, ivy, gradle, npm)
```

Une clé de connexion et un réglage invalide donnent chacun une ligne :

```
erreur: ./.nephila.toml : clé `base_url` permise seulement dans la configuration personnelle (/home/vous/.config/nephila/config.toml)
erreur: ./.nephila.toml : clé `concurrency` : valeur invalide `-1` (entier strictement positif attendu)
```

La première ne reprend pas la valeur écrite (une `base_url` peut contenir un jeton).

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
- **Extension** : un double-clic ou le bouton « Étendre » ajoute les voisins directs. Hors « Tout étendre », la vue est plafonnée à 300 nœuds : les voisins les plus reliés sont retenus, et la page indique « N voisins non affichés ». Si la vue est pleine, naviguer vers un nouveau nœud remplace la vue par ce nœud et ses voisins.
- **Filtres** : une case par genre de nœud et par genre de lien. Le bouton « Vue initiale » ramène à la vue de départ.
- **Tout étendre** : affiche tout le graphe jusqu'à 5000 nœuds. Au-delà, les 5000 nœuds les plus reliés sont retenus et la page indique « N nœuds non affichés ». « Étendre » et les liens du panneau travaillent ensuite avec ce plafond de 5000 ; « Vue initiale » ou un résultat de recherche ramène au plafond de 300.
- Déplacement, zoom et glisser d'un nœud à la souris. Les liens ne sont pas dessinés pendant la molette et le déplacement du fond ; ils reviennent 150 ms après le dernier cran ou au relâchement. Glisser un nœud garde ses liens.

## Configuration du LLM

La configuration est facultative. Le LLM est actif quand un fichier de configuration existe et que `--no-llm` est absent. `nephila` cherche ce fichier TOML dans cet ordre :

1. le chemin donné par la variable `NEPHILA_CONFIG` ;
2. `$XDG_CONFIG_HOME/nephila/config.toml` ;
3. `~/.config/nephila/config.toml`.

Six de ces réglages (`model`, `max_chunk_chars`, `concurrency`, `timeout_secs`, `batch_files`, `format_retries`) peuvent être remplacés par dépôt dans `.nephila.toml` (voir [Fichier de projet](#fichier-de-projet-nephilatoml)) ; les autres clés restent personnelles.

[`examples/config.toml`](examples/config.toml) documente toutes les clés et toutes les variables (fournisseurs, options communes, TLS, LLM simulé).

| Clé | Rôle | Défaut |
|---|---|---|
| `provider` | `anthropic`, `openai` (protocole compatible OpenAI) ou `mock` | obligatoire |
| `model` | Nom du modèle | obligatoire, sauf `mock` |
| `api_key_env` | **Nom** de la variable d'environnement qui contient la clé | obligatoire pour `anthropic` |
| `base_url` | URL du serveur, **sans** `/v1` final | celle du fournisseur |
| `max_chunk_chars` | Taille maximale d'un morceau envoyé au LLM | 60000 |
| `concurrency` | Appels simultanés | 4 |
| `batch_files` | Fichiers par appel d'enrichissement, entier > 0 | 1 |
| `format_retries` | Nouveaux essais d'un appel dont la réponse est non conforme, entier ≥ 0 (2 = au plus 3 appels pour une même requête ; 0 = aucun nouvel essai, comme en 0.12.0) | 2 |
| `timeout_secs` | Durée maximale d'un appel HTTP, entier > 0 (la connexion reste limitée à 10 s) | 300 |
| `tls_ca_cert` | Chemin d'un fichier PEM d'autorités de certification, **ajoutées** aux autorités publiques embarquées. Absolu, `~/…`, ou relatif au dossier du fichier de configuration. Le magasin du système n'est pas lu (pour s'y fier : `/etc/ssl/certs/ca-certificates.crt`) | aucun |
| `tls_insecure` | Booléen. `true` désactive tout contrôle du certificat du serveur, comme `curl -k` ; un avertissement est écrit à chaque commande | `false` |

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

## Proxy d'entreprise et certificats auto-signés

Si le serveur (proxy d'entreprise, serveur local) présente un certificat signé par une autorité privée, donnez le PEM de cette autorité :

```toml
provider = "openai"
model = "llama3.1"
base_url = "https://127.0.0.1:8443"
tls_ca_cert = "certs/ca.pem"   # relatif au fichier de configuration
```

Les autorités publiques restent reconnues. En dernier recours, `tls_insecure = true` désactive le contrôle (chaîne, dates, nom d'hôte) ; la connexion reste chiffrée. À éviter hors d'un poste de développement.

Un certificat refusé produit une erreur contenant « certificat » et nommant `tls_ca_cert` et `tls_insecure` : ajoutez l'autorité avec `tls_ca_cert`, ou, faute de mieux, activez `tls_insecure`. Ces clés n'ont aucun effet avec `provider = "mock"` ni avec une `base_url` en `http://`. Le TLS mutuel (certificat client) n'est pas pris en charge.

## Enrichissement en lots

Par défaut (`batch_files = 1`), un appel LLM porte sur un seul fichier. Pour un dépôt fait de nombreux petits fichiers, `batch_files = 4` à `8` regroupe plusieurs fichiers par appel et réduit le nombre d'appels.

- Les fichiers sont pris dans l'ordre des chemins. Un lot ne dépasse ni `batch_files` fichiers ni `max_chunk_chars` octets au total. Un fichier plus gros que `max_chunk_chars` part seul, en morceaux successifs.
- `--batch-files <N>` remplace la clé pour la commande en cours, sans rien écrire. Priorité : option, puis clé `batch_files`, puis défaut 1. Sans LLM actif, l'option est acceptée sans effet.
- Jusqu'à `concurrency` lots sont en vol en même temps, soit `concurrency` × `batch_files` fichiers.
- Si un lot échoue, tous ses fichiers restent « à enrichir » et sont repris par le prochain `nephila update`. L'échec compte pour un seul appel dans la politique d'arrêt. Un fichier absent de la réponse échoue seul.
- Si la réponse du modèle est tronquée par sa limite de jetons de sortie, tout le lot échoue. Réduisez alors `batch_files`.

## Réponses non conformes et nouveaux essais

Un modèle, surtout local, rate parfois son JSON. Quand un appel aboutit mais que la réponse est inexploitable, `nephila` renvoie aussitôt la même requête, à l'identique, jusqu'à `format_retries` fois (défaut 2, soit au plus 3 appels).

- **Retenté** : réponse reçue mais inexploitable (JSON illisible, aucun objet JSON dans le texte, schéma non respecté, réponse tronquée par la limite de jetons de sortie).
- **Jamais retenté** : délai dépassé, erreur réseau, clé refusée (ils relèvent de la politique d'arrêt), fichier illisible, éléments écartés d'une réponse conforme, fichier absent d'une réponse de lot conforme (il reste « à enrichir » pour le prochain `update`).
- **Unité d'essai** : l'appel. Un morceau d'un gros fichier est retenté seul ; un lot est retenté en entier.
- **Un essai réussi ne laisse aucune trace** : ni avertissement, ni changement du bilan ; seul `--verbose` montre les essais.
- **Échec final** : le fichier reste « à enrichir » et une ligne le nomme, avec le nombre d'essais faits :

```
avertissement: src/A.java : réponse LLM non conforme au schéma : JSON illisible (NEPHILA_MOCK_MALFORMED) (3 essais sur 3)
```

- Toute la suite d'essais d'un appel compte pour **un seul appel** dans la politique d'arrêt.
- `--format-retries <N>` de `build`, `update` et `query ask` remplace la clé pour la commande seulement, sans rien écrire. Priorité : option, puis clé `format_retries`, puis défaut 2. Sans LLM actif, l'option est acceptée sans effet.

Un modèle qui échoue souvent au format se règle mieux en changeant de modèle ou en réduisant `batch_files` qu'en augmentant les essais.

## Ce que l'analyse extrait

Chaque fichier est analysé localement : tree-sitter pour les langages JVM, JavaScript et TypeScript, un lecteur XML pour Maven et Ivy, un lecteur lexical pour Gradle et pour SQL, `serde_json` pour `package.json` et pour JSON, un analyseur à événements pour YAML, et CommonMark pour Markdown.

| Fichier | Nœuds | Liens |
|---|---|---|
| `.java`, `.kt`, `.kts` (hors Gradle), `.groovy`, `.scala` | package, types (class, interface, enum, annotation, object, trait Scala ; imbriqués `Externe.Interne`), méthodes (constructeurs `<init>`) ; fonctions de premier niveau rattachées au type de fichier conventionnel (`NomFichierKt`, `NomFichier`, `NomFichier$package`) | `imports`, `extends`, `implements`, `calls` |
| `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.mts`, `.cts`, `.tsx` | package (un fichier = un module ES, nommé par son chemin sans extension : `src/app/order.service`), types (class, interface, enum ; sous-genres `alias` pour `type X = …` et `object` pour `namespace`), méthodes (constructeurs `<init>`) ; fonctions de premier niveau contenues par le package | `imports`, `extends`, `implements`, `calls` |
| `package.json` | module = champ `name` (sinon nom du dossier) | `depends_on` : clés de `dependencies`, `devDependencies`, `peerDependencies`, `optionalDependencies` |
| `pom.xml`, `ivy.xml` | module `groupId:artifactId` ou `organisation:module` | `depends_on` (hors `dependencyManagement`, plugins et `parent`) |
| `build.gradle(.kts)`, `settings.gradle(.kts)` | module = chemin de projet (`:services:api`) ; `settings` est suivi mais ne crée pas de nœud | `depends_on` : `project(':core')`, chaînes `g:a[:v]`, notation map ; seul le bloc `dependencies { }` de premier niveau compte |
| autre `.gradle` | analysé comme un script Groovy | comme Groovy |
| `.md`, `.markdown` | document, sections (ancres à la GitHub) | `links_to` (liens relatifs vers un `.md`), `mentions` (code en ligne au format de nom JVM : `Order`, `shop.Order`, `place()`, `Order#place`) |
| `.sql` | document (sous-genre `sql`), `table` (sous-genre `table` ou `view`) | `references` (clé étrangère, ou table lue par une vue) |
| `.xml` autre que `pom.xml` et `ivy.xml` | document (sous-genre `xml`), sans nœud par élément | `mentions` (noms JVM cités) |
| `.json` | document (sous-genre `json`), `key` par clé feuille | `mentions` (noms JVM cités) |
| `.yaml`, `.yml` | document (sous-genre `yaml`), `key` par clé feuille | `mentions` (noms JVM cités) |

**JavaScript et TypeScript.** Grammaires : `.ts`, `.mts`, `.cts` en TypeScript, `.tsx` en TSX, `.js`, `.mjs`, `.cjs`, `.jsx` en JavaScript (JSX compris). Un fichier est un module ES, nœud `package` nommé par son chemin sans extension ; `src/a.ts` et `src/a.d.ts` partagent le même nœud. Les fonctions de premier niveau (`function f`, `const f = () => …`) sont des méthodes contenues par le package. Formes d'import reconnues : `import { a, b as c } from`, `import type`, `import X from`, `import * as ns from`, `const ns = require(…)` et `const { a } = require(…)` ; seuls les spécifieurs relatifs (`./`, `../`) et les alias `@/`, `~/` sont suivis (ces derniers par nom simple unique), un paquet npm est externe. Appels : `f()` vise la fonction du fichier ou le nom importé, `this.m()` la classe englobante, `x.m()` le type annoté ou construit par `new T()` de `x` (paramètres-propriétés de constructeur compris), `T.m()` et `ns.f()` la classe ou le module visé ; un global (`fetch`, `console`), un appel `f()` sur un paramètre ou un receveur inconnu ne crée aucun lien. En JSX, `<Button/>` (majuscule) est un appel de `Button`, `<div>` n'en est pas un. `node_modules/` et les fichiers de verrouillage (`package-lock.json`, `npm-shrinkwrap.json`, `pnpm-lock.yaml`) ne sont jamais indexés.

**Fichiers `package.json`.** Fichier de build de l'écosystème JS/TS : le module porte le champ `name` (`@shop/web`, nom court `web`). Les paquets des espaces de travail sont reliés aux modules du dépôt de même nom, les autres dépendances à un nœud `dependency`. Un module ES se rattache au `package.json` ancêtre le plus proche, un nœud de code JVM au fichier de build JVM le plus proche. `scripts`, `workspaces` et `engines` ne donnent rien.

Dans un dépôt où le module `@shop/web` (un `package.json`) dépend de `lodash` et du module `shop` du même dépôt :

```
$ nephila query search formatPrice
src/utils/format.formatPrice (method, src/utils/format.ts)

$ nephila query neighbors @shop/web
utilise :
  depends_on lodash (dependency)
  depends_on shop (module, package.json)
```

**Fichiers SQL.** Le lecteur est indépendant du dialecte : il n'exécute rien et ne se connecte à aucune base. Il reconnaît `CREATE [modificateurs] TABLE`, `CREATE [modificateurs] VIEW` (dont `MATERIALIZED`) et `ALTER TABLE`, avec identifiants entre `"…"`, `` `…` `` ou `[…]` ; les noms sont mis en minuscules (`"Orders"` donne `orders`, `shop.products` reste qualifié). Une table déclarée par plusieurs fichiers (création, puis `ALTER TABLE` d'une migration) est un seul nœud. Les liens `references` viennent de `REFERENCES <table>` dans un `CREATE TABLE` ou un `ALTER TABLE`, et de chaque table qui suit `FROM` ou `JOIN` dans une vue (ni fonctions, ni sous-requêtes, ni noms de CTE). Hors périmètre : colonnes, index, séquences, fonctions, procédures, triggers, `INSERT`/`UPDATE`/`DELETE`, `DROP` (le graphe décrit les déclarations des fichiers, pas l'état de la base après migrations), `CREATE TABLE … AS SELECT` (table sans référence) et changelogs Liquibase.

**Fichiers XML.** Hors `pom.xml` et `ivy.xml`, un fichier XML donne son document seul, sans nœud par élément. Les noms JVM cités dans les valeurs d'attributs et dans les textes ou CDATA d'éléments donnent des `mentions` depuis le document.

**Fichiers JSON et YAML.** Chaque clé feuille (valeur scalaire, `null`, tableau ou objet vide) donne un nœud `key` d'identifiant `key:<fichier>#a.b.c` (nom court `a.b.c`), contenu par le document ; les objets non vides sont parcourus, pas les tableaux. Les noms JVM cités dans les valeurs, y compris dans les tableaux, donnent des `mentions` depuis la clé. Un fichier est limité à 1 000 clés (les premières par ordre lexicographique de chemin ; `avertissement:` indiquant le nombre non retenu). Un YAML de plusieurs documents (`---`) réunit leurs clés (un chemin = un nœud) ; la clé de fusion `<<` est ignorée et un alias `*a` n'est pas développé (clé feuille, sans candidat).

**Noms JVM cités.** Dans les fichiers XML, JSON et YAML, chaque valeur est découpée sur les blancs, `,` et `;`. Un morceau est un candidat s'il a au moins deux segments séparés par `.` ou `$` (chaque segment : lettre ASCII ou `_`, puis lettres, chiffres ou `_`), éventuellement suivi de `()` ou de `#segment`. Exemples : `com.acme.Outer$Inner` vise `com.acme.Outer.Inner`, `com.acme.Repo#save` vise `com.acme.Repo.save`, `com.acme.A, com.acme.B;com.acme.C` donne trois candidats ; `orderService`, `1.0`, `v1.2`, `${db.url}`, `http://x.org/a.b`, `classpath:com.acme` et `a..b` n'en sont pas. Les candidats sont des `mentions` non résolues tant que le nom est absent du graphe, sans figurer dans les références non résolues ; au plus 1 000 par fichier (`avertissement:` indiquant le nombre ignoré).

Exemples sur un dépôt contenant `application.yml` et `db/V1__init.sql` :

```
$ nephila query search spring.datasource.url
src/main/resources/application.yml#spring.datasource.url (key, src/main/resources/application.yml)

$ nephila query neighbors orders
utilise :
  references customers (table, db/V1__init.sql)
utilisé par :
  contains db/V1__init.sql (sql, db/V1__init.sql)
```

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
- **JSONC et JSON5** : non pris en charge. Un JSON à commentaires, à virgule finale ou tronqué donne le document seul, sans clé ni candidat, avec un `avertissement:`.
- **Nom SQL qualifié** : `public.orders` ne trouve pas une table déclarée `orders` (l'inverse fonctionne si `public.orders` est seul).
- **Ré-exportations** : `export … from` et `export *` ne sont pas suivis. Un nom importé depuis un barrel (`index.ts`) se résout par nom simple unique, ou reste non résolu s'il est ambigu.
- **`tsconfig.json`** : ses `paths` ne sont pas lus. Les imports `@/…` et `~/…` visent le nom importé par nom simple unique. Un `tsconfig.json` à commentaires est un JSON mal formé : document seul, avec avertissement.
- **Imports dynamiques** : `import()` n'est pas suivi.
- **`.vue`, `.svelte`, `.astro`** : non pris en charge.
- **Syntaxe TypeScript récente** : une syntaxe postérieure à la grammaire embarquée est exploitée au mieux.
- **Pas de lecture propre à Spring ou Kubernetes** : `application.yml`, manifestes et workflows CI sont des YAML comme les autres.
- **Gros JSON de données** (fixtures, exports de données) : indexés dans la limite de 1 000 clés ; excluez-les par `.gitignore`.
- **Export dans le dépôt** : un export écrit dans le dépôt sous un autre nom que `<slug>.json` est indexé comme n'importe quel JSON ; excluez-le par `.gitignore`.
- **`query path`** : renvoie le plus court chemin sans tenir compte du sens des liens. Il peut donc passer par un `import` ou un package plutôt que par des appels.

## Mise à niveau

- **Renommage `rkg` → `nephila`** : le binaire s'appelle `nephila`, le graphe est stocké dans `.nephila/` (au lieu de `.rkg/`), la configuration dans `~/.config/nephila/config.toml` et les variables d'environnement sont préfixées `NEPHILA_` (`NEPHILA_CONFIG`, `NEPHILA_MOCK_DIR`…). Déplacez votre configuration (`mv ~/.config/rkg ~/.config/nephila`), puis reconstruisez le graphe avec `nephila build` et supprimez l'ancien dossier `.rkg/`.
- **Vers 0.10** : un graphe 0.9 est complété par `nephila update`, sans `build` : les fichiers JS/TS sont ajoutés, les `package.json` sont réanalysés en fichiers de build (leur document et leurs clés disparaissent) et les fichiers de verrouillage (`package-lock.json`…) sont retirés.
- **Vers 0.9** : un graphe 0.8 est complété par `nephila update` : les fichiers `.sql`, `.xml`, `.json`, `.yaml` et `.yml`, nouvellement pris en charge, sont ajoutés sans reconstruction.
- **Vers 0.4** : `nephila export` sans option écrit dans un fichier au lieu de stdout. Pour l'ancien comportement, utilisez `nephila export --output -`.
- **Depuis 0.2** : le schéma du graphe a changé. Un graphe ancien est refusé (`… lancez nephila build`) : reconstruisez-le avec `nephila build`. Les fixtures du fournisseur `mock` passent au schéma version 2.

| Version | Nouveautés |
|---|---|
| 0.16.1 | Analyse des expressions très imbriquées (Java, Kotlin, Scala, Groovy) sans débordement de pile |
| 0.16 | Option `--show-progress` de `build` et `update` |
| 0.15 | Réglages LLM du dépôt dans `.nephila.toml` |
| 0.14.1 | Exemples de configuration complets (`examples/config.toml`, `examples/nephila.toml`), vérifiés par les tests |
| 0.14 | Vue HTML : bouton « Tout étendre », liens masqués pendant le zoom et le déplacement |
| 0.13 | Nouveaux essais des réponses LLM non conformes (`format_retries`, `--format-retries`) |
| 0.12.0 | Filtres d'analyse (`--exclude`, `--exclude-type`, `.nephila.toml`) |
| 0.11.0 | Certificats d'autorité et désactivation du contrôle TLS (clés `tls_ca_cert`, `tls_insecure`) |
| 0.10.0 | JavaScript et TypeScript (modules ES, imports, appels, JSX), `package.json` comme fichier de build ; sous-genres `alias` et `object` ; modules désignés par leur nom qualifié |
| 0.9.0 | Fichiers de données : SQL (tables, vues, `references`), XML, JSON et YAML (clés, noms JVM cités) ; genres `table` et `key` |
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
git tag v0.16.1 && git push origin v0.16.1
```

Les tests s'exécutent avec [cargo-nextest](https://nexte.st) (`cargo install cargo-nextest --locked`), en série, avec une limite de temps par test (`.config/nextest.toml` : 10 s pour les tests unitaires, 60 s pour les tests d'intégration, 300 s pour les tests de performance) ; un test trop long est tué et nommé. Préfixer une commande par `timeout -k 10 540` sert de filet global.

```sh
cargo nextest run --cargo-quiet --color never --test-threads 1 --no-fail-fast --status-level fail --final-status-level fail   # suite complète
cargo fmt --all -- --check
cargo clippy -q --all-targets --color never -- -D warnings      # lint
cargo nextest run --release --cargo-quiet --color never --test-threads 1 --run-ignored only --success-output final perf       # performances (dépôt de 10 000 fichiers)
node --test --test-timeout=10000 --test-concurrency=1 --test-reporter=dot 'tests/view/*.test.js'   # logique de la vue HTML
```

Les certificats de test TLS (`tests/fixtures/tls/`) se régénèrent par `tests/fixtures/tls/gen.sh` (nécessite `openssl`).

La logique de la vue HTML est un module JS pur, testé par `node --test`. Node 20 ou plus est requis pour ces tests seulement, sans npm ni dépendance : le produit n'en dépend pas. Un test Rust compare les données de la vue à la fixture `tests/view/fixtures/maven3.json`. Après un changement volontaire de l'export, régénérez-la avec `NEPHILA_UPDATE_VIEW_FIXTURE=1 cargo nextest run view_fixture`. Le rendu et les interactions souris se vérifient à la main ; `node tests/view/synthetic_page.js <page exportée> <n> <sortie>` remplace les données d'une page exportée par un graphe synthétique de n nœuds (sortie sous `target/view-check/`) pour ces contrôles.

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
| `NEPHILA_MOCK_MALFORMED` | `<chemin>:<k>[,<chemin>:<k>…]` (`_ask` pour `query ask`, k entier > 0) : les k premiers appels qui portent ce fichier (un morceau, ou un lot qui le contient) reçoivent une réponse non conforme, les suivants la réponse habituelle |

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
