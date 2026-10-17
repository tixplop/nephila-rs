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
    env.put("config.json", "{\"a\": 1}");
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
