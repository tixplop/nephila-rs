//! Fichiers de données SQL, XML, JSON et YAML de bout en bout (REQ-029, REQ-004, DEC-038).

use assert_cmd::cargo::cargo_bin;
use nephila::model::{NodeKind, SubKind};
use nephila::store::Store;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("monrepo");
        fs::create_dir_all(&repo).unwrap();
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&mock).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            mock,
            log,
        }
    }

    fn put(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// `llm` : configuration `mock` active ; sinon aucune configuration.
    fn run(&self, llm: bool, args: &[&str]) -> Output {
        let config = if llm {
            self.config.clone()
        } else {
            self.config.with_file_name("absent.toml")
        };
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .output()
            .unwrap()
    }

    fn ok(&self, llm: bool, args: &[&str]) -> Output {
        let out = self.run(llm, args);
        assert!(out.status.success(), "{}", text(&out.stderr));
        out
    }

    fn log_lines(&self) -> usize {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// Nœuds (id, nom court, sous-genre) et liens, triés.
    fn structure(&self) -> (Vec<String>, Vec<String>) {
        let st = Store::open_readonly(&self.repo).unwrap();
        let mut nodes: Vec<String> = st
            .nodes()
            .unwrap()
            .iter()
            .map(|n| {
                format!(
                    "{}|{}|{}",
                    n.id,
                    n.short,
                    n.subkind.map_or("", |s| s.as_str())
                )
            })
            .collect();
        nodes.sort();
        let mut edges: Vec<String> = st
            .edges()
            .unwrap()
            .iter()
            .map(|e| format!("{} {} {}", e.source, e.kind, e.target))
            .collect();
        edges.sort();
        (nodes, edges)
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn one_of_each(env: &Env) {
    env.put("db/V1__init.sql", "CREATE TABLE a (id INT);\n");
    env.put(
        "src/main/resources/beans.xml",
        "<beans><bean class=\"com.acme.A\"/></beans>\n",
    );
    env.put("config/app.json", "{\"server\": {\"port\": 8080}}\n");
    env.put("application.yml", "spring:\n  name: demo\n");
}

/// AC-029-1
#[test]
fn req029_ac1_un_fichier_de_chaque_format_sans_configuration() {
    let env = Env::new();
    one_of_each(&env);
    env.ok(false, &["build"]);
    assert_eq!(env.log_lines(), 0);
    let st = Store::open_readonly(&env.repo).unwrap();
    let nodes = st.nodes().unwrap();
    for (path, sub, short) in [
        ("db/V1__init.sql", SubKind::Sql, "V1__init.sql"),
        ("src/main/resources/beans.xml", SubKind::Xml, "beans.xml"),
        ("config/app.json", SubKind::Json, "app.json"),
        ("application.yml", SubKind::Yaml, "application.yml"),
    ] {
        let n = nodes
            .iter()
            .find(|n| n.id == format!("document:{path}"))
            .unwrap_or_else(|| panic!("document absent : {path}"));
        assert_eq!(n.kind, NodeKind::Document);
        assert_eq!(n.subkind, Some(sub), "{path}");
        assert_eq!(n.short, short);
        assert_eq!(n.file.as_deref(), Some(path));
    }
}

/// AC-029-1 : même graphe structurel avec `mock` et `--no-llm`, journal vide avec `--no-llm`.
#[test]
fn req029_ac1_meme_graphe_avec_mock_et_no_llm() {
    let env = Env::new();
    one_of_each(&env);
    env.ok(false, &["build"]);
    let sans = env.structure();

    env.ok(true, &["build", "--no-llm"]);
    assert_eq!(env.log_lines(), 0, "--no-llm ne doit appeler aucun LLM");
    assert_eq!(env.structure(), sans);

    env.ok(true, &["build"]);
    assert!(env.log_lines() > 0, "le mock doit avoir été appelé");
    assert_eq!(env.structure(), sans);
}

/// AC-004-5
#[test]
fn req004_ac5_export_du_depot_jamais_indexe() {
    let env = Env::new();
    env.put("monrepo.json", "{\"nodes\": []}");
    env.put("sub/monrepo.json", "{\"a\": 1}");
    env.put("config.json", "{}");
    env.ok(false, &["build"]);
    let (nodes, _) = env.structure();
    let ids: Vec<&str> = nodes.iter().map(|n| n.split('|').next().unwrap()).collect();
    assert_eq!(ids, ["document:config.json"], "{ids:?}");
    let status = text(&env.ok(false, &["status"]).stdout);
    assert!(!status.contains("monrepo.json"), "{status}");

    // `export` réécrit `monrepo.json` à la racine du dépôt.
    let out = Command::new(cargo_bin("nephila"))
        .current_dir(&env.repo)
        .arg("export")
        .env("NEPHILA_CONFIG", env.config.with_file_name("absent.toml"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(Path::new(&env.repo.join("monrepo.json")).exists());
    let status = text(&env.ok(false, &["status"]).stdout);
    assert!(status.contains("graphe à jour"), "{status}");
    let (nodes, _) = env.structure();
    assert_eq!(nodes.len(), 1);
}

// ---------------------------------------------------------------------------
// YAML (REQ-033)
// ---------------------------------------------------------------------------

const APP_YML: &str = "src/main/resources/application.yml";

const APP_YML_TEXT: &str = "spring:\n  datasource:\n    url: jdbc:postgresql://db/app\n    driver-class-name: org.postgresql.Driver\nnephila:\n  listener: com.acme.core.OrderService\n  handlers:\n    - com.acme.core.OrderRepository\n    - name: x\n      impl: com.acme.core.OrderService\n";

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

impl Env {
    /// Dépôt Maven exemple.
    fn maven() -> Env {
        let env = Env::new();
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven3/repo"),
            &env.repo,
        );
        env
    }

    /// Dépôt Ivy exemple.
    fn ivy() -> Env {
        let env = Env::new();
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ivy"),
            &env.repo,
        );
        env
    }

    fn has_edge(&self, s: &str, k: &str, t: &str) -> bool {
        self.structure().1.contains(&format!("{s} {k} {t}"))
    }

    fn key_ids(&self, file: &str) -> Vec<String> {
        let prefix = format!("key:{file}#");
        let mut v: Vec<String> = self
            .structure()
            .0
            .iter()
            .map(|n| n.split('|').next().unwrap().to_string())
            .filter(|id| id.starts_with(&prefix))
            .map(|id| id[prefix.len()..].to_string())
            .collect();
        v.sort();
        v
    }

    fn parse_ref_ids(&self, file: &str) -> Vec<i64> {
        let conn = rusqlite::Connection::open(self.repo.join(".nephila/graph.db")).unwrap();
        let mut st = conn
            .prepare("SELECT id FROM refs WHERE file = ?1 AND origin = 'parse' ORDER BY id")
            .unwrap();
        let rows = st.query_map([file], |r| r.get(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }
}

/// AC-033-1 de bout en bout
#[test]
fn req033_ac1_application_yml_de_bout_en_bout() {
    let env = Env::maven();
    env.put(APP_YML, APP_YML_TEXT);
    env.ok(false, &["build"]);
    assert_eq!(
        env.key_ids(APP_YML),
        [
            "nephila.handlers",
            "nephila.listener",
            "spring.datasource.driver-class-name",
            "spring.datasource.url"
        ]
    );
    let k = |p: &str| format!("key:{APP_YML}#{p}");
    assert!(env.has_edge(
        &k("nephila.listener"),
        "mentions",
        "type:com.acme.core.OrderService"
    ));
    assert!(env.has_edge(
        &k("nephila.handlers"),
        "mentions",
        "type:com.acme.core.OrderRepository"
    ));
    assert!(env.has_edge(
        &k("nephila.handlers"),
        "mentions",
        "type:com.acme.core.OrderService"
    ));
    let doc = format!("document:{APP_YML}");
    assert!(env.has_edge(&doc, "contains", &k("nephila.listener")));

    let out = env.ok(false, &["export", "--output", "-"]);
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    let unresolved = v["unresolved"].as_array().unwrap();
    assert!(
        !unresolved
            .iter()
            .any(|u| u.to_string().contains("org.postgresql.Driver")),
        "{unresolved:?}"
    );
}

/// AC-029-4
#[test]
fn req029_ac4_application_yml_modifie_puis_update() {
    let env = Env::maven();
    env.put(APP_YML, APP_YML_TEXT);
    env.ok(false, &["build"]);
    let other = "core/src/main/java/com/acme/core/OrderService.java";
    let other_before = env.parse_ref_ids(other);
    let yml_before = env.parse_ref_ids(APP_YML);
    assert!(!other_before.is_empty() && !yml_before.is_empty());

    env.put(
        APP_YML,
        "spring:\n  datasource:\n    url: jdbc:postgresql://db/app\nnephila:\n  listener: com.acme.core.OrderService\n  nouvelle: 1\n",
    );
    let status = text(&env.ok(false, &["status"]).stdout);
    assert!(
        status.lines().any(|l| l == format!("modifié\t{APP_YML}")),
        "{status}"
    );
    env.ok(false, &["update"]);
    assert_eq!(env.parse_ref_ids(other), other_before);
    assert_ne!(env.parse_ref_ids(APP_YML), yml_before);
    let keys = env.key_ids(APP_YML);
    assert!(keys.contains(&"nephila.nouvelle".to_string()), "{keys:?}");
    assert!(!keys.contains(&"nephila.handlers".to_string()), "{keys:?}");
    assert!(
        !keys.contains(&"spring.datasource.driver-class-name".to_string()),
        "{keys:?}"
    );
}

/// AC-029-5 (partie `t.yaml`) : vide puis mal formé
#[test]
fn req029_ac5_yaml_vide_puis_mal_forme() {
    let env = Env::new();
    env.put("t.yaml", "");
    let out = env.ok(false, &["build"]);
    assert!(
        !text(&out.stderr).contains("avertissement:"),
        "{}",
        text(&out.stderr)
    );
    let (nodes, _) = env.structure();
    assert_eq!(nodes, ["document:t.yaml|t.yaml|yaml"]);

    env.put("t.yaml", "a: [1");
    let out = env.ok(false, &["update"]);
    let err = text(&out.stderr);
    assert_eq!(err.matches("avertissement:").count(), 1, "{err}");
    assert!(err.contains("t.yaml"), "{err}");
    let status = text(&env.ok(false, &["status"]).stdout);
    assert!(status.contains("graphe à jour"), "{status}");
    assert!(env.key_ids("t.yaml").contains(&"a".to_string()));
}

// ---------------------------------------------------------------------------
// SQL (REQ-030)
// ---------------------------------------------------------------------------

const V1: &str = "CREATE TABLE customers (id INT PRIMARY KEY);\nCREATE TABLE IF NOT EXISTS \"Orders\" (id INT, customer_id INT REFERENCES customers(id))";
const V2: &str = "ALTER TABLE orders ADD CONSTRAINT fk_p FOREIGN KEY (product_id) REFERENCES shop.products (id);\n";
const V3: &str = "CREATE TABLE shop.products (id INT);\n";

/// AC-030-1 de bout en bout
#[test]
fn req030_ac1_tables_et_lien_references() {
    let env = Env::new();
    env.put("db/V1__init.sql", V1);
    let out = env.ok(false, &["build"]);
    assert!(
        !text(&out.stderr).contains("avertissement:"),
        "{}",
        text(&out.stderr)
    );
    let (nodes, _) = env.structure();
    assert!(nodes.contains(&"table:customers|customers|table".to_string()));
    assert!(nodes.contains(&"table:orders|orders|table".to_string()));
    assert!(env.has_edge("table:orders", "references", "table:customers"));
    assert!(env.has_edge("document:db/V1__init.sql", "contains", "table:orders"));
}

/// AC-030-2 : nœud partagé, puis suppression de V2 et `update`
#[test]
fn req030_ac2_noeud_partage_et_suppression() {
    let env = Env::new();
    env.put("db/V1__init.sql", V1);
    env.put("db/V2__fk.sql", V2);
    env.put("db/V3__products.sql", V3);
    env.ok(false, &["build"]);
    let (nodes, _) = env.structure();
    assert_eq!(
        nodes
            .iter()
            .filter(|n| n.starts_with("table:orders|"))
            .count(),
        1
    );
    let st = Store::open_readonly(&env.repo).unwrap();
    let orders = st
        .nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.id == "table:orders")
        .unwrap();
    assert_eq!(orders.file.as_deref(), Some("db/V1__init.sql"));
    drop(st);
    assert!(env.has_edge("table:orders", "references", "table:shop.products"));

    fs::remove_file(env.repo.join("db/V2__fk.sql")).unwrap();
    env.ok(false, &["update"]);
    assert!(!env.has_edge("table:orders", "references", "table:shop.products"));
    assert!(env
        .structure()
        .0
        .contains(&"table:orders|orders|table".to_string()));

    let fresh = Env::new();
    fresh.put("db/V1__init.sql", V1);
    fresh.put("db/V3__products.sql", V3);
    fresh.ok(false, &["build"]);
    assert_eq!(fresh.structure(), env.structure());
}

/// AC-008-9
#[test]
fn req008_ac9_reference_sql_non_resolue_puis_reparee() {
    let env = Env::new();
    env.put(
        "db/a.sql",
        "CREATE TABLE orders (c INT REFERENCES absente(id));\n",
    );
    let out = env.ok(false, &["build"]);
    assert!(
        text(&out.stdout).contains("1 références non résolues"),
        "{}",
        text(&out.stdout)
    );
    let v: serde_json::Value =
        serde_json::from_str(&text(&env.ok(false, &["export", "--output", "-"]).stdout)).unwrap();
    let un = v["unresolved"].as_array().unwrap();
    assert!(
        un.iter().any(|u| u["source"] == "table:orders"
            && u["kind"] == "references"
            && u["target"] == "absente"),
        "{un:?}"
    );

    let before = env.parse_ref_ids("db/a.sql");
    env.put("db/b.sql", "CREATE TABLE absente (id INT);\n");
    env.ok(false, &["update"]);
    assert!(env.has_edge("table:orders", "references", "table:absente"));
    assert_eq!(env.parse_ref_ids("db/a.sql"), before);
}

/// AC-030-5 et AC-029-5 (partie `t.sql`) : tronqué
#[test]
fn req030_ac5_sql_tronque() {
    let env = Env::new();
    env.put("t.sql", "CREATE TABLE a (id INT, b_id INT REFERENCES b(id)");
    let out = env.ok(false, &["build"]);
    let err = text(&out.stderr);
    assert_eq!(err.matches("avertissement:").count(), 1, "{err}");
    assert!(err.contains("t.sql"), "{err}");
    assert!(env.structure().0.contains(&"table:a|a|table".to_string()));
    let status = text(&env.ok(false, &["status"]).stdout);
    assert!(status.contains("graphe à jour"), "{status}");
}

// ---------------------------------------------------------------------------
// XML et JSON (REQ-031, REQ-032)
// ---------------------------------------------------------------------------

/// AC-029-5 : quatre fichiers vides, aucun avertissement
#[test]
fn req029_ac5_quatre_fichiers_vides() {
    let env = Env::new();
    for f in ["t.sql", "t.xml", "t.json", "t.yaml"] {
        env.put(f, "");
    }
    let out = env.ok(false, &["build"]);
    assert!(
        !text(&out.stderr).contains("avertissement:"),
        "{}",
        text(&out.stderr)
    );
    let (nodes, edges) = env.structure();
    assert_eq!(nodes.len(), 4, "{nodes:?}");
    assert!(edges.is_empty());
}

/// AC-029-5 : quatre fichiers mal formés
#[test]
fn req029_ac5_quatre_fichiers_mal_formes() {
    let env = Env::new();
    env.put("t.sql", "CREATE TABLE a (");
    env.put("t.xml", "<a><b>");
    env.put("t.json", "{\"a\":");
    env.put("t.yaml", "a: [1");
    let out = env.ok(false, &["build"]);
    let err = text(&out.stderr);
    for f in ["t.sql", "t.xml", "t.json", "t.yaml"] {
        let n = err
            .lines()
            .filter(|l| l.starts_with("avertissement:") && l.contains(f))
            .count();
        assert_eq!(n, 1, "{f} : {err}");
    }
    assert_eq!(err.matches("avertissement:").count(), 4, "{err}");
    let status = text(&env.ok(false, &["status"]).stdout);
    assert!(status.contains("graphe à jour"), "{status}");
}

/// AC-032-3 de bout en bout
#[test]
fn req032_ac3_json_corrige_puis_update() {
    let env = Env::new();
    env.put("c.json", "{\"a\": 1,");
    let out = env.ok(false, &["build"]);
    let err = text(&out.stderr);
    assert_eq!(err.matches("avertissement:").count(), 1, "{err}");
    assert!(err.contains("c.json"), "{err}");
    assert!(env.key_ids("c.json").is_empty());
    let status = text(&env.ok(false, &["status"]).stdout);
    assert!(status.contains("graphe à jour"), "{status}");

    env.put("c.json", "{\"a\": 1}");
    env.ok(false, &["update"]);
    assert_eq!(env.key_ids("c.json"), ["a"]);
}

/// AC-031-3 : `pom.xml` et `ivy.xml` ne donnent pas de nœud document
#[test]
fn req031_ac3_pom_et_ivy_sans_noeud_document() {
    let env = Env::maven();
    env.ok(false, &["build"]);
    let (nodes, _) = env.structure();
    assert!(
        !nodes
            .iter()
            .any(|n| n.starts_with("document:") && (n.contains("pom.xml") || n.contains("ivy.xml"))),
        "{nodes:?}"
    );
    assert!(nodes.iter().any(|n| n.starts_with("module:")));
}

/// AC-031-3, côté Ivy : `ivy.xml` présent, aiguillé vers `ivy`
#[test]
fn req031_ac3_ivy_sans_noeud_document() {
    let env = Env::ivy();
    assert!(env.repo.join("ivy.xml").is_file());
    env.ok(false, &["build"]);
    let (nodes, _) = env.structure();
    assert!(
        !nodes
            .iter()
            .any(|n| n.starts_with("document:") && n.contains("ivy.xml")),
        "{nodes:?}"
    );
    assert!(nodes.iter().any(|n| n.starts_with("module:")), "{nodes:?}");
}

/// AC-008-8
#[test]
fn req008_ac8_mention_xml_non_resolue_puis_reparee() {
    let env = Env::new();
    env.put(
        "beans.xml",
        "<beans><bean class=\"com.acme.Later\"/></beans>",
    );
    let out = env.ok(true, &["build"]);
    assert!(
        text(&out.stdout).contains(" 0 références non résolues"),
        "{}",
        text(&out.stdout)
    );
    let v: serde_json::Value =
        serde_json::from_str(&text(&env.ok(true, &["export", "--output", "-"]).stdout)).unwrap();
    assert!(
        !v["unresolved"].to_string().contains("com.acme.Later"),
        "{}",
        v["unresolved"]
    );

    env.put(
        "com/acme/Later.java",
        "package com.acme;\npublic class Later {}\n",
    );
    let before = env.log_lines();
    env.ok(true, &["update"]);
    assert!(env.has_edge("document:beans.xml", "mentions", "type:com.acme.Later"));
    let log = fs::read_to_string(&env.log).unwrap_or_default();
    let new_log: Vec<&str> = log.lines().skip(before).collect();
    assert!(
        new_log.iter().all(|l| !l.contains("beans.xml")),
        "beans.xml renvoyé au LLM : {new_log:?}"
    );
}

// ---------------------------------------------------------------------------
// T-061 : un seul graphe, requêtes, export, vue et enrichissement
// ---------------------------------------------------------------------------

const DATA_APP_YML: &str = "src/main/resources/application.yml";

impl Env {
    /// Dépôt exemple complété des fichiers de données ; fixtures du mock copiées.
    fn data() -> Env {
        let env = Env::new();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/data");
        copy_dir(&fx.join("repo"), &env.repo);
        copy_dir(&fx.join("mock"), &env.mock);
        env
    }

    fn query(&self, args: &[&str]) -> String {
        let mut a = vec!["query"];
        a.extend_from_slice(args);
        text(&self.ok(false, &a).stdout)
    }
}

fn html_parts(html: &str) -> (String, serde_json::Value, String) {
    let open = "<script type=\"application/json\" id=\"nephila-data\">";
    let start = html.find(open).expect("bloc nephila-data");
    let body = start + open.len();
    let end = body + html[body..].find("</script>").expect("fin du bloc");
    (
        html[..start].to_string(),
        serde_json::from_str(&html[body..end]).expect("bloc JSON"),
        html[end..].to_string(),
    )
}

/// AC-005-6
#[test]
fn req005_ac6_un_seul_graphe_pour_les_cinq_familles() {
    let env = Env::data();
    env.ok(false, &["build"]);
    assert!(env.has_edge(
        "section:docs/schema.md#commandes",
        "mentions",
        "table:orders"
    ));
    assert!(env.has_edge(
        "document:src/main/resources/beans.xml",
        "mentions",
        "type:com.acme.core.OrderService"
    ));
    assert!(env.has_edge(
        &format!("key:{DATA_APP_YML}#nephila.listener"),
        "mentions",
        "type:com.acme.core.OrderService"
    ));

    let out = env.ok(false, &["export", "--output", "-"]);
    let v: serde_json::Value = serde_json::from_str(&text(&out.stdout)).unwrap();
    let kinds: Vec<&str> = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"table") && kinds.contains(&"key"));
    assert!(v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "references"));
}

/// AC-005-6 : la vue HTML
#[test]
fn req005_ac6_vue_html_identique_hors_donnees() {
    let with = Env::data();
    with.ok(false, &["build"]);
    let without = Env::maven();
    without.ok(false, &["build"]);
    let page = |e: &Env| {
        let out = e.ok(false, &["export", "--format", "html", "--output", "-"]);
        html_parts(&text(&out.stdout))
    };
    let (before_w, data_w, after_w) = page(&with);
    let (before_o, data_o, after_o) = page(&without);
    assert_eq!(before_w, before_o);
    assert_eq!(after_w, after_o);

    let node_kinds = |d: &serde_json::Value| -> Vec<String> {
        d["nodeKinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    };
    let edge_kinds = |d: &serde_json::Value| -> Vec<String> {
        d["edgeKinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_string())
            .collect()
    };
    let nk = node_kinds(&data_w);
    assert!(nk.contains(&"table".to_string()) && nk.contains(&"key".to_string()));
    assert!(edge_kinds(&data_w).contains(&"references".to_string()));
    let nk_o = node_kinds(&data_o);
    assert!(!nk_o.contains(&"table".to_string()) && !nk_o.contains(&"key".to_string()));
    assert!(!edge_kinds(&data_o).contains(&"references".to_string()));
}

/// AC-030-6
#[test]
fn req030_ac6_neighbors_orders_et_search_products() {
    let env = Env::data();
    env.ok(false, &["build"]);
    let out = env.query(&["neighbors", "orders"]);
    let (uses, used_by) = out.split_once("utilisé par :").expect(&out);
    assert!(uses.contains("references customers (table"), "{out}");
    assert!(uses.contains("references shop.products (table"), "{out}");
    assert!(used_by.contains("contains db/V1__init.sql (sql"), "{out}");
    let out = env.query(&["search", "products"]);
    assert!(out.contains("shop.products (table"), "{out}");
}

/// AC-030-6 : chemin entre deux tables
#[test]
fn req030_ac6_path_entre_deux_tables() {
    let env = Env::data();
    env.ok(false, &["build"]);
    let out = env.query(&["path", "customers", "shop.products"]);
    assert!(out.contains("customers référencé par orders"), "{out}");
    assert!(out.contains("orders references shop.products"), "{out}");
}

/// AC-032-5
#[test]
fn req032_ac5_search_et_neighbors_sur_une_cle() {
    let env = Env::data();
    env.ok(false, &["build"]);
    let out = env.query(&["search", "server.port"]);
    assert!(out.contains("config/app.json#server.port (key"), "{out}");
    let out = env.query(&["neighbors", "server.handler"]);
    let (uses, used_by) = out.split_once("utilisé par :").expect(&out);
    assert!(
        uses.contains("mentions com.acme.core.OrderService"),
        "{out}"
    );
    assert!(used_by.contains("contains config/app.json"), "{out}");
}

/// `summary` sur le dépôt complété
#[test]
fn req019_summary_du_depot_avec_fichiers_de_donnees() {
    let env = Env::data();
    env.ok(false, &["build"]);
    let out = env.ok(false, &["summary"]);
    assert!(text(&out.stderr).is_empty(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("table:orders"));
}

/// AC-024-10 avec `mock`
#[test]
fn req024_ac10_enrichissement_mock_des_fichiers_de_donnees() {
    let env = Env::data();
    let out = env.ok(true, &["build"]);
    let err = text(&out.stderr);
    let warn: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("avertissement:") && l.contains("config/app.json"))
        .collect();
    assert_eq!(warn.len(), 1, "{err}");
    assert!(warn[0].contains("key:config/app.json#server.port"), "{err}");

    let out = env.query(&["search", "Configuration JSON"]);
    assert!(out.contains("config/app.json"), "{out}");
    let out = env.query(&["search", "commandes"]);
    assert!(out.contains("orders"), "{out}");
    let st = Store::open_readonly(&env.repo).unwrap();
    let with_summary = |id: &str| {
        st.nodes()
            .unwrap()
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.summary.clone())
    };
    assert_eq!(
        with_summary("document:config/app.json").as_deref(),
        Some("Configuration JSON de l'application.")
    );
    assert_eq!(
        with_summary("table:orders").as_deref(),
        Some("Table des commandes.")
    );
    assert_eq!(with_summary("key:config/app.json#server.port"), None);
}
