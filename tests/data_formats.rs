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
