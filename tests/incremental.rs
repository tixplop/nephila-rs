//! Résolution incrémentale (REQ-066, AC-066-5) et oracle de la 0.21.0 (REQ-070, AC-070-2).
//!
//! Chaque scénario part d'un dépôt de `tests/fixtures`, construit le graphe puis enchaîne des
//! `update` après des modifications écrites en dur. Après chaque `update`, le graphe doit être
//! celui d'un `build` du même arbre : export JSON, `status`, `summary` et les trois comptes de la
//! ligne de bilan (nœuds, liens, références non résolues).
//!
//! `concurrency = 1` : avec des fils d'enrichissement concurrents, l'ordre des références `llm` (donc de
//! `unresolved` dans l'export brut) dépend de l'ordre d'arrivée des réponses.
//!
//! `NEPHILA_ORACLE_CAPTURE=1` réécrit `tests/fixtures/oracle_0_21_0.txt` (une ligne
//! `dépôt<TAB>étape<TAB>sortie<TAB>code<TAB>empreinte`, triée) ; sans, le test compare.

#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use support::{apply_case, copy_dir, normalized_export, run, CASES};

enum Op {
    Write(&'static str, &'static str),
    Append(&'static str, &'static str),
    Remove(&'static str),
    Rename(&'static str, &'static str),
}

struct Step {
    name: &'static str,
    ops: &'static [Op],
}

struct Scenario {
    name: &'static str,
    repo: &'static str,
    mock: Option<&'static str>,
    steps: &'static [Step],
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "inc-maven3",
        repo: "maven3/repo",
        mock: Some("maven3/mock"),
        steps: &[
            // Un second `OrderService` rend ambigu le nom simple cité par les documents.
            Step {
                name: "01-type-homonyme",
                ops: &[Op::Write(
                    "api/src/main/java/com/acme/api/OrderService.java",
                    "package com.acme.api;\n\npublic class OrderService {\n    public void place() {\n    }\n}\n",
                )],
            },
            // Suppression du type visé par `implements` et par un document.
            Step {
                name: "02-suppression-du-type-vise",
                ops: &[Op::Remove("core/src/main/java/com/acme/core/OrderRepository.java")],
            },
            // `cache.warm()` : appel sans cible.
            Step {
                name: "03-appel-sans-cible",
                ops: &[Op::Write(
                    "api/src/main/java/com/acme/api/Caller.java",
                    "package com.acme.api;\n\npublic class Caller {\n    private Cache cache;\n\n    public void go() {\n        cache.warm();\n    }\n}\n",
                )],
            },
            Step {
                name: "04-cible-de-l-appel",
                ops: &[Op::Write(
                    "api/src/main/java/com/acme/api/Cache.java",
                    "package com.acme.api;\n\npublic class Cache {\n    public void warm() {\n    }\n}\n",
                )],
            },
            // Mention d'un document sans cible, puis la cible.
            Step {
                name: "05-mention-de-document-sans-cible",
                ops: &[Op::Append(
                    "docs/guide.md",
                    "\nLe composant `Ghost` est décrit plus loin, avec `Ghost.spin`.\n",
                )],
            },
            Step {
                name: "06-cible-de-la-mention",
                ops: &[Op::Write(
                    "core/src/main/java/com/acme/core/Ghost.java",
                    "package com.acme.core;\n\npublic class Ghost {\n    public void spin() {\n    }\n}\n",
                )],
            },
            Step {
                name: "07-renommage-de-fichier",
                ops: &[Op::Rename(
                    "store/src/main/java/com/acme/store/JdbcOrderRepository.java",
                    "store/src/main/java/com/acme/store/JdbcRepo.java",
                )],
            },
            Step {
                name: "08-renommage-de-type",
                ops: &[Op::Write(
                    "api/src/main/java/com/acme/api/Cache.java",
                    "package com.acme.api;\n\npublic class Store {\n    public void warm() {\n    }\n}\n",
                )],
            },
            Step {
                name: "09-dependance-externe-ajoutee",
                ops: &[Op::Write(
                    "core/pom.xml",
                    "<project>\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>com.acme</groupId>\n  <artifactId>core</artifactId>\n  <version>1.0</version>\n  <dependencies>\n    <dependency>\n      <groupId>org.slf4j</groupId>\n      <artifactId>slf4j-api</artifactId>\n      <version>2.0.9</version>\n    </dependency>\n    <dependency>\n      <groupId>com.google.guava</groupId>\n      <artifactId>guava</artifactId>\n      <version>33.0.0</version>\n    </dependency>\n  </dependencies>\n</project>\n",
                )],
            },
            Step {
                name: "10-dependance-externe-retiree",
                ops: &[Op::Write(
                    "core/pom.xml",
                    "<project>\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>com.acme</groupId>\n  <artifactId>core</artifactId>\n  <version>1.0</version>\n</project>\n",
                )],
            },
            Step {
                name: "11-module-ajoute",
                ops: &[
                    Op::Write(
                        "cache/pom.xml",
                        "<project>\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>com.acme</groupId>\n  <artifactId>cache</artifactId>\n  <version>1.0</version>\n  <dependencies>\n    <dependency>\n      <groupId>com.acme</groupId>\n      <artifactId>core</artifactId>\n      <version>1.0</version>\n    </dependency>\n  </dependencies>\n</project>\n",
                    ),
                    Op::Write(
                        "cache/src/main/java/com/acme/cache/Warm.java",
                        "package com.acme.cache;\n\nimport com.acme.core.OrderService;\n\npublic class Warm {\n    private OrderService service;\n}\n",
                    ),
                ],
            },
        ],
    },
    Scenario {
        name: "inc-gradle",
        repo: "gradle/edge",
        mock: None,
        steps: &[
            Step {
                name: "01-appel-sans-cible",
                ops: &[Op::Write(
                    "app/src/main/java/com/acme/app/Main.java",
                    "package com.acme.app;\n\npublic class Main {\n    private Engine engine;\n\n    public void run() {\n        engine.start();\n    }\n}\n",
                )],
            },
            Step {
                name: "02-cible-de-l-appel",
                ops: &[Op::Write(
                    "core/src/main/java/com/acme/core/Engine.java",
                    "package com.acme.core;\n\npublic class Engine {\n    public void start() {\n    }\n}\n",
                )],
            },
            Step {
                name: "03-type-homonyme",
                ops: &[Op::Write(
                    "app/src/main/java/com/acme/app/Engine.java",
                    "package com.acme.app;\n\npublic class Engine {\n    public void start() {\n    }\n}\n",
                )],
            },
            Step {
                name: "04-suppression-du-type-vise",
                ops: &[Op::Remove("app/src/main/java/com/acme/app/Engine.java")],
            },
            Step {
                name: "05-renommage-de-fichier",
                ops: &[Op::Rename(
                    "core/src/main/java/com/acme/core/Engine.java",
                    "core/src/main/java/com/acme/core/Motor.java",
                )],
            },
            Step {
                name: "06-dependance-externe-ajoutee",
                ops: &[Op::Write(
                    "app/build.gradle",
                    "dependencies {\n    implementation project(path: ':core')\n    implementation platform('org.springframework.boot:spring-boot-dependencies:3.3.0')\n    implementation libs.foo\n    implementation files('x.jar')\n    implementation \"org.slf4j:slf4j-api:$slf4jVersion\"\n    implementation 'com.google.guava:guava:33.0.0'\n}\n",
                )],
            },
            Step {
                name: "07-dependance-externe-retiree",
                ops: &[Op::Write(
                    "app/build.gradle",
                    "dependencies {\n    implementation project(path: ':core')\n    implementation platform('org.springframework.boot:spring-boot-dependencies:3.3.0')\n    implementation libs.foo\n    implementation files('x.jar')\n    implementation \"org.slf4j:slf4j-api:$slf4jVersion\"\n}\n",
                )],
            },
            Step {
                name: "08-module-ajoute",
                ops: &[
                    Op::Write(
                        "settings.gradle",
                        "rootProject.name = 'edge'\ninclude 'core', 'app', 'extra'\n",
                    ),
                    Op::Write(
                        "extra/build.gradle",
                        "dependencies {\n    implementation project(path: ':core')\n}\n",
                    ),
                    Op::Write(
                        "extra/src/main/java/com/acme/extra/Extra.java",
                        "package com.acme.extra;\n\npublic class Extra {\n    private Motor motor;\n}\n",
                    ),
                ],
            },
        ],
    },
    Scenario {
        name: "inc-js-mixed",
        repo: "js/mixed",
        mock: Some("js/mock"),
        steps: &[
            Step {
                name: "01-appel-sans-cible",
                ops: &[Op::Write(
                    "web/src/api/client.ts",
                    "import { Id, toId } from './types';\n\nexport class ApiClient {\n  fetchOrder(raw: string): Id {\n    return toId(raw);\n  }\n\n  retry(raw: string): Id {\n    return backoff(raw);\n  }\n}\n",
                )],
            },
            Step {
                name: "02-cible-de-l-appel",
                ops: &[Op::Write(
                    "web/src/api/backoff.ts",
                    "export function backoff(raw: string): string {\n  return raw;\n}\n",
                )],
            },
            Step {
                name: "03-type-homonyme",
                ops: &[Op::Write(
                    "web/src/util/types.ts",
                    "export function toId(raw: string): string {\n  return raw.trim();\n}\n",
                )],
            },
            Step {
                name: "04-suppression-de-la-cible",
                ops: &[Op::Remove("web/src/api/types.ts")],
            },
            Step {
                name: "05-mention-de-document-sans-cible",
                ops: &[Op::Append(
                    "docs/web.md",
                    "\nLe widget `Gadget` est rendu par `Gadget.draw`.\n",
                )],
            },
            Step {
                name: "06-cible-de-la-mention",
                ops: &[Op::Write(
                    "web/src/ui/Gadget.ts",
                    "export class Gadget {\n  draw(): void {}\n}\n",
                )],
            },
            Step {
                name: "07-renommage-de-fichier",
                ops: &[Op::Rename("web/src/api/client.ts", "web/src/api/http.ts")],
            },
            Step {
                name: "08-dependance-externe-ajoutee",
                ops: &[Op::Write(
                    "web/package.json",
                    "{\"name\": \"@acme/web\", \"dependencies\": {\"react\": \"^18.3.0\", \"lodash\": \"^4.17.21\"}}\n",
                )],
            },
            Step {
                name: "09-dependance-externe-retiree",
                ops: &[Op::Write(
                    "web/package.json",
                    "{\"name\": \"@acme/web\", \"dependencies\": {\"react\": \"^18\"}}\n",
                )],
            },
            Step {
                name: "10-module-ajoute",
                ops: &[
                    Op::Write(
                        "admin/package.json",
                        "{\"name\": \"@acme/admin\", \"dependencies\": {\"react\": \"^18.3.0\"}}\n",
                    ),
                    Op::Write(
                        "admin/src/main.ts",
                        "import { Gadget } from '../../web/src/ui/Gadget';\n\nexport const g = new Gadget();\n",
                    ),
                ],
            },
        ],
    },
    Scenario {
        name: "inc-data",
        repo: "data/repo",
        mock: Some("data/mock"),
        steps: &[
            // `REFERENCES ghosts` : référence SQL sans cible, puis sa cible.
            Step {
                name: "01-reference-sql-sans-cible",
                ops: &[Op::Write(
                    "db/V4__ghost.sql",
                    "ALTER TABLE orders ADD COLUMN ghost_id INT REFERENCES ghosts (id);\n",
                )],
            },
            Step {
                name: "02-cible-de-la-reference-sql",
                ops: &[Op::Write(
                    "db/V5__ghosts.sql",
                    "CREATE TABLE ghosts (id INT PRIMARY KEY, label TEXT);\n",
                )],
            },
            // Mention d'un fichier de données sans cible, puis la cible.
            Step {
                name: "03-mention-de-donnees-sans-cible",
                ops: &[Op::Write(
                    "config/app.json",
                    "{\"server\": {\"port\": 8080, \"handler\": \"com.acme.core.OrderService\", \"fallback\": \"com.acme.core.Phantom\"}}\n",
                )],
            },
            Step {
                name: "04-cible-de-la-mention-de-donnees",
                ops: &[Op::Write(
                    "core/src/main/java/com/acme/core/Phantom.java",
                    "package com.acme.core;\n\npublic class Phantom {\n}\n",
                )],
            },
            Step {
                name: "05-fichier-de-donnees-modifie-yaml",
                ops: &[Op::Write(
                    "src/main/resources/application.yml",
                    "nephila:\n  listener: com.acme.core.OrderService\n  fallback: com.acme.core.Phantom\n  retries: 3\n",
                )],
            },
            Step {
                name: "06-fichier-de-donnees-modifie-sql",
                ops: &[Op::Write(
                    "db/V3__products.sql",
                    "CREATE TABLE shop.products (id INT PRIMARY KEY, label TEXT, price INT);\nCREATE TABLE shop.tags (id INT PRIMARY KEY, product_id INT REFERENCES shop.products (id));\n",
                )],
            },
            Step {
                name: "07-document-modifie",
                ops: &[Op::Append(
                    "docs/schema.md",
                    "\n## Fantômes\n\nLa table `ghosts` et la classe `Phantom` sont décrites ici.\n",
                )],
            },
            Step {
                name: "08-type-homonyme",
                ops: &[Op::Write(
                    "api/src/main/java/com/acme/api/OrderService.java",
                    "package com.acme.api;\n\npublic class OrderService {\n}\n",
                )],
            },
            Step {
                name: "09-suppression-du-type-vise",
                ops: &[Op::Remove("core/src/main/java/com/acme/core/OrderService.java")],
            },
            Step {
                name: "10-renommage-de-fichier",
                ops: &[Op::Rename("db/V1__init.sql", "db/V1__initial.sql")],
            },
            Step {
                name: "11-table-homonyme",
                ops: &[Op::Write(
                    "db/V6__shop_orders.sql",
                    "CREATE TABLE shop.orders (id INT PRIMARY KEY);\n",
                )],
            },
        ],
    },
    Scenario {
        name: "inc-langs-mixed",
        repo: "langs/mixed",
        mock: None,
        steps: &[
            Step {
                name: "01-type-homonyme",
                ops: &[Op::Write(
                    "src/main/java/com/acme/core/Pricing.java",
                    "package com.acme.core;\n\npublic interface Pricing {\n    int price();\n}\n",
                )],
            },
            Step {
                name: "02-suppression-du-type-vise",
                ops: &[Op::Remove("src/main/scala/com/acme/sc/Pricing.scala")],
            },
            Step {
                name: "03-appel-sans-cible",
                ops: &[Op::Write(
                    "src/main/kotlin/com/acme/kt/Caller.kt",
                    "package com.acme.kt\n\nclass Caller {\n    fun go(p: Phantom) {\n        p.run()\n    }\n}\n",
                )],
            },
            Step {
                name: "04-cible-de-l-appel",
                ops: &[Op::Write(
                    "src/main/java/com/acme/core/Phantom.java",
                    "package com.acme.core;\n\npublic class Phantom {\n    public void run() {\n    }\n}\n",
                )],
            },
            Step {
                name: "05-mention-de-document-sans-cible",
                ops: &[Op::Append(
                    "docs/guide.md",
                    "\nLe type `Specter` est détaillé ailleurs.\n",
                )],
            },
            Step {
                name: "06-cible-de-la-mention",
                ops: &[Op::Write(
                    "src/main/groovy/com/acme/gv/Specter.groovy",
                    "package com.acme.gv\n\nclass Specter {\n}\n",
                )],
            },
            Step {
                name: "07-renommage-de-fichier",
                ops: &[Op::Rename(
                    "src/main/kotlin/com/acme/kt/Util.kt",
                    "src/main/kotlin/com/acme/kt/Helpers.kt",
                )],
            },
            Step {
                name: "08-dependance-externe-ajoutee",
                ops: &[Op::Write(
                    "pom.xml",
                    "<project>\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>com.acme</groupId>\n  <artifactId>mixed</artifactId>\n  <version>1.0</version>\n  <dependencies>\n    <dependency>\n      <groupId>junit</groupId>\n      <artifactId>junit</artifactId>\n      <version>4.13.2</version>\n    </dependency>\n  </dependencies>\n</project>\n",
                )],
            },
            Step {
                name: "09-dependance-externe-retiree",
                ops: &[Op::Write(
                    "pom.xml",
                    "<project>\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>com.acme</groupId>\n  <artifactId>mixed</artifactId>\n  <version>1.1</version>\n</project>\n",
                )],
            },
            Step {
                name: "10-module-ajoute",
                ops: &[
                    Op::Write(
                        "extra/pom.xml",
                        "<project>\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>com.acme</groupId>\n  <artifactId>extra</artifactId>\n  <version>1.0</version>\n</project>\n",
                    ),
                    Op::Write(
                        "extra/src/main/java/com/acme/extra/Extra.java",
                        "package com.acme.extra;\n\npublic class Extra {\n}\n",
                    ),
                ],
            },
        ],
    },
];

fn apply(repo: &Path, ops: &[Op]) {
    for op in ops {
        match op {
            Op::Write(p, c) => {
                let f = repo.join(p);
                fs::create_dir_all(f.parent().unwrap()).unwrap();
                fs::write(f, c).unwrap();
            }
            Op::Append(p, c) => {
                let f = repo.join(p);
                let mut t = fs::read_to_string(&f).unwrap();
                t.push_str(c);
                fs::write(f, t).unwrap();
            }
            Op::Remove(p) => fs::remove_file(repo.join(p)).unwrap(),
            Op::Rename(from, to) => {
                let t = repo.join(to);
                fs::create_dir_all(t.parent().unwrap()).unwrap();
                fs::rename(repo.join(from), t).unwrap();
            }
        }
    }
}

/// Copie récursive de l'arbre, avec ou sans le dossier `.nephila/`.
fn copy_tree(from: &Path, to: &Path, with_graph: bool) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            if e.file_name() == ".nephila" && !with_graph {
                continue;
            }
            copy_tree(&e.path(), &dest, with_graph);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    mock: Option<PathBuf>,
}

impl Env {
    fn new(repo: &str, mock: Option<&str>) -> Env {
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        copy_dir(&fx.join(repo), &root.join("repo"));
        let mock = mock.map(|m| {
            let d = root.join("mock");
            copy_dir(&fx.join(m), &d);
            d
        });
        let config = root.join("config.toml");
        fs::write(
            &config,
            "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\nconcurrency = 1\n",
        )
        .unwrap();
        Env {
            _tmp: tmp,
            root,
            config,
            mock,
        }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn run(&self, repo: &Path, args: &[&str]) -> std::process::Output {
        run(repo, &self.config, self.mock.as_deref(), args)
    }
}

/// Sortie standard d'une commande qui doit réussir.
fn ok(env: &Env, repo: &Path, ctx: &str, args: &[&str]) -> String {
    let out = env.run(repo, args);
    assert!(
        out.status.success(),
        "{ctx} {args:?} : {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Nombres précédant `nœuds`, `liens` et `références non résolues` dans la ligne de bilan.
fn counts(stdout: &str, ctx: &str) -> (u64, u64, u64) {
    let line = stdout
        .lines()
        .find(|l| l.contains(" nœuds, "))
        .unwrap_or_else(|| panic!("{ctx} : pas de ligne de bilan dans {stdout:?}"));
    let before = |label: &str| -> u64 {
        let i = line
            .find(label)
            .unwrap_or_else(|| panic!("{ctx} : {label:?} absent de {line:?}"));
        line[..i]
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap_or_else(|_| panic!("{ctx} : compte illisible dans {line:?}"))
    };
    (
        before(" nœuds"),
        before(" liens"),
        before(" références non résolues"),
    )
}

/// Export brut, puis `[export normalisé, status, summary]`.
fn snapshot(env: &Env, repo: &Path, ctx: &str) -> (String, [String; 3]) {
    let raw = ok(
        env,
        repo,
        ctx,
        &["export", "--format", "json", "--output", "-"],
    );
    let snap = [
        normalized_export(raw.as_bytes()),
        ok(env, repo, ctx, &["status"]),
        ok(env, repo, ctx, &["summary"]),
    ];
    (raw, snap)
}

/// Première section (`nodes`, `edges`, `unresolved`) et première entrée qui diffèrent.
fn export_gap(a: &str, b: &str) -> String {
    let (a, b): (serde_json::Value, serde_json::Value) = (
        serde_json::from_str(a).unwrap(),
        serde_json::from_str(b).unwrap(),
    );
    for section in ["nodes", "edges", "unresolved"] {
        let (x, y) = (&a[section], &b[section]);
        if x == y {
            continue;
        }
        let (xs, ys) = (
            x.as_array().cloned().unwrap_or_default(),
            y.as_array().cloned().unwrap_or_default(),
        );
        for i in 0..xs.len().max(ys.len()) {
            if xs.get(i) != ys.get(i) {
                return format!(
                    "section {section}, entrée {i} : {} au lieu de {}",
                    xs.get(i).map_or("absente".into(), |v| v.to_string()),
                    ys.get(i).map_or("absente".into(), |v| v.to_string()),
                );
            }
        }
    }
    "section inconnue".into()
}

/// Écarts entre le graphe incrémental et le graphe complet (vide si identiques).
fn diff(
    ctx: &str,
    inc: &([String; 3], (u64, u64, u64)),
    full: &([String; 3], (u64, u64, u64)),
) -> Vec<String> {
    let mut out = Vec::new();
    if inc.1 != full.1 {
        out.push(format!(
            "{ctx} : comptes (nœuds, liens, non résolues) {:?} au lieu de {:?}",
            inc.1, full.1
        ));
    }
    for (i, label) in ["export", "status", "summary"].iter().enumerate() {
        if inc.0[i] != full.0[i] {
            let gap = if i == 0 {
                format!(" ({})", export_gap(&inc.0[0], &full.0[0]))
            } else {
                String::new()
            };
            out.push(format!("{ctx} : {label} différent{gap}"));
        }
    }
    out
}

fn hash_line(
    lines: &mut Vec<String>,
    name: &str,
    step: &str,
    label: &str,
    out: &std::process::Output,
) {
    let code = out.status.code().map_or(-1, i64::from);
    let hash = blake3::hash(&out.stdout).to_hex();
    lines.push(format!("{name}\t{step}\t{label}\t{code}\t{hash}"));
}

/// Ligne `export` : empreinte de l'export JSON brut du dépôt mis à jour (AC-070-2, DEC-080).
fn export_line(lines: &mut Vec<String>, name: &str, step: &str, raw: &str) {
    let hash = blake3::hash(raw.as_bytes()).to_hex();
    lines.push(format!("{name}\t{step}\texport\t0\t{hash}"));
}

/// Suite `update --no-llm` : égalité avec un `build --no-llm` de l'arbre copié sans `.nephila/`.
fn suite_no_llm(sc: &Scenario, lines: &mut Vec<String>) -> Vec<String> {
    let mut diffs = Vec::new();
    let env = Env::new(sc.repo, sc.mock);
    let repo = env.repo();
    let out = env.run(&repo, &["build", "--no-llm"]);
    hash_line(lines, sc.name, "nollm-00-build", "build", &out);
    assert!(out.status.success(), "{} : build initial", sc.name);
    for step in sc.steps {
        let ctx = format!("{} / {} (--no-llm)", sc.name, step.name);
        apply(&repo, step.ops);
        let out = env.run(&repo, &["update", "--no-llm"]);
        hash_line(
            lines,
            sc.name,
            &format!("nollm-{}", step.name),
            "update",
            &out,
        );
        assert!(
            out.status.success(),
            "{ctx} : {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let inc_counts = counts(&String::from_utf8_lossy(&out.stdout), &ctx);
        eprintln!("{ctx} : {inc_counts:?}");
        let (raw, snap) = snapshot(&env, &repo, &ctx);
        export_line(lines, sc.name, &format!("nollm-{}", step.name), &raw);
        let inc = (snap, inc_counts);

        let fresh = env.root.join("fresh");
        let _ = fs::remove_dir_all(&fresh);
        copy_tree(&repo, &fresh, false);
        let b = ok(&env, &fresh, &ctx, &["build", "--no-llm"]);
        let full = (snapshot(&env, &fresh, &ctx).1, counts(&b, &ctx));
        diffs.extend(diff(&ctx, &inc, &full));
        fs::remove_dir_all(&fresh).unwrap();
    }
    diffs
}

/// Suite avec le mock : `update`, puis un second `build` dans le même `.nephila/` (copie du dépôt).
fn suite_mock(sc: &Scenario, lines: &mut Vec<String>) -> Vec<String> {
    let mut diffs = Vec::new();
    let env = Env::new(sc.repo, sc.mock);
    let repo = env.repo();
    let out = env.run(&repo, &["build"]);
    hash_line(lines, sc.name, "mock-00-build", "build", &out);
    assert!(out.status.success(), "{} : build initial (mock)", sc.name);
    for step in sc.steps {
        let ctx = format!("{} / {} (mock)", sc.name, step.name);
        apply(&repo, step.ops);
        let out = env.run(&repo, &["update"]);
        hash_line(
            lines,
            sc.name,
            &format!("mock-{}", step.name),
            "update",
            &out,
        );
        assert!(
            out.status.success(),
            "{ctx} : {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let inc_counts = counts(&String::from_utf8_lossy(&out.stdout), &ctx);
        eprintln!("{ctx} : {inc_counts:?}");
        let (raw, snap) = snapshot(&env, &repo, &ctx);
        export_line(lines, sc.name, &format!("mock-{}", step.name), &raw);
        let inc = (snap, inc_counts);

        let second = env.root.join("second");
        let _ = fs::remove_dir_all(&second);
        copy_tree(&repo, &second, true);
        let b = ok(&env, &second, &ctx, &["build"]);
        let full = (snapshot(&env, &second, &ctx).1, counts(&b, &ctx));
        diffs.extend(diff(&ctx, &inc, &full));
        fs::remove_dir_all(&second).unwrap();
    }
    diffs
}

#[test]
fn req066_ac5_update_egale_build_no_llm() {
    let diffs: Vec<String> = SCENARIOS
        .iter()
        .flat_map(|sc| suite_no_llm(sc, &mut Vec::new()))
        .collect();
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}

#[test]
fn req066_ac5_update_egale_second_build_mock() {
    let diffs: Vec<String> = SCENARIOS
        .iter()
        .flat_map(|sc| suite_mock(sc, &mut Vec::new()))
        .collect();
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}

/// Les scénarios de REQ-065 : sorties de `build` et d'`update`.
fn scenario_065(c: &support::Case, lines: &mut Vec<String>) {
    let env = Env::new(c.repo, c.mock);
    let repo = env.repo();
    let name = format!("req065-{}", c.name);
    let step = |label: &str, args: &[&str], lines: &mut Vec<String>| {
        let out = env.run(&repo, args);
        assert!(out.status.success(), "{name} / {label}");
        hash_line(lines, &name, label, args[0], &out);
    };
    step("1-build-no-llm", &["build", "--no-llm"], lines);
    step("2-build-mock", &["build"], lines);
    apply_case(&repo, c);
    step("3-update", &["update"], lines);
    step("4-build-final", &["build"], lines);
}

#[test]
fn req070_ac2_oracle_0_21_0() {
    let mut lines = Vec::new();
    for c in CASES {
        scenario_065(c, &mut lines);
    }
    for sc in SCENARIOS {
        let _ = suite_no_llm(sc, &mut lines);
        let _ = suite_mock(sc, &mut lines);
    }
    lines.sort();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle_0_21_0.txt");
    let actual = lines.join("\n") + "\n";
    if std::env::var_os("NEPHILA_ORACLE_CAPTURE").is_some_and(|v| v == "1") {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).expect("oracle absent : NEPHILA_ORACLE_CAPTURE=1");
    let exp: Vec<&str> = expected.lines().collect();
    for i in 0..lines.len().max(exp.len()) {
        let (a, e) = (lines.get(i).map(String::as_str), exp.get(i).copied());
        assert_eq!(
            a,
            e,
            "première ligne différente de l'oracle (ligne {})",
            i + 1
        );
    }
}

#[test]
fn normalized_export_ignore_l_ordre_des_non_resolues_seulement() {
    let doc = |unres: &str| {
        format!(
            "{{\"edges\":[],\"nodes\":[{{\"id\":\"b\"}},{{\"id\":\"a\"}}],\"unresolved\":{unres},\"version\":1}}"
        )
    };
    let u1 = r#"{"kind":"calls","source":"a","target":"x"}"#;
    let u2 = r#"{"kind":"calls","source":"a","target":"y"}"#;
    let u3 = r#"{"kind":"links_to","source":"b","target":"x"}"#;
    let n = |u: &str| normalized_export(doc(u).as_bytes());
    assert_eq!(
        n(&format!("[{u3},{u1},{u2},{u1}]")),
        n(&format!("[{u1},{u1},{u2},{u3}]"))
    );
    assert_ne!(n(&format!("[{u1},{u2}]")), n(&format!("[{u1},{u2},{u3}]")));
    assert_ne!(n(&format!("[{u1},{u2}]")), n(&format!("[{u1},{u3}]")));
    assert_ne!(n(&format!("[{u1}]")), n(&format!("[{u1},{u1}]")));
    // `nodes` garde son ordre.
    let swapped = doc("[]").replace(r#"[{"id":"b"},{"id":"a"}]"#, r#"[{"id":"a"},{"id":"b"}]"#);
    assert_ne!(n("[]"), normalized_export(swapped.as_bytes()));
}
