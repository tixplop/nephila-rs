#![allow(non_snake_case)]
//! `nephila cache export|import` (REQ-295, DEC-226) : AC-295-1 et lignes de résultat d'AC-295-5.

use assert_cmd::Command;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "support/mod.rs"]
mod support;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    mock: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let root = tmp.path().to_path_buf();
        support::copy_dir(&fx.join("maven2"), &root.join("repo"));
        support::copy_dir(&fx.join("mock"), &root.join("mock"));
        let config = root.join("config.toml");
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\n",
        )
        .unwrap();
        Env {
            _tmp: tmp,
            mock: root.join("mock"),
            config,
            root,
        }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// Copie des fichiers du dépôt, sans `.nephila/`.
    fn copy(&self, name: &str) -> PathBuf {
        let to = self.root.join(name);
        support::copy_dir(&self.repo(), &to);
        let _ = fs::remove_dir_all(to.join(".nephila"));
        to
    }

    fn log(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.log"))
    }

    fn run(&self, repo: &Path, log: &str, args: &[&str]) -> Output {
        Command::cargo_bin("nephila")
            .unwrap()
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", self.log(log))
            .env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn log_is_empty(&self, name: &str) -> bool {
        fs::read_to_string(self.log(name)).map_or(true, |s| s.trim().is_empty())
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "échec : {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.stderr.is_empty(), "stderr : {:?}", o.stderr);
    stdout(o)
}

fn db(repo: &Path) -> Connection {
    Connection::open(repo.join(".nephila/graph.db")).unwrap()
}

fn count(repo: &Path) -> usize {
    db(repo)
        .query_row("SELECT count(*) FROM enrichments", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap() as usize
}

fn rows(repo: &Path, sql: &str) -> Vec<Vec<Option<String>>> {
    let c = db(repo);
    let mut q = c.prepare(sql).unwrap();
    let n = q.column_count();
    q.query_map([], |r| {
        (0..n).map(|i| r.get::<_, Option<String>>(i)).collect()
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

const NODES: &str = "SELECT id, kind, qualified, summary FROM nodes ORDER BY id";
const MENTIONS: &str = "SELECT source, target FROM edges WHERE kind = 'mentions' ORDER BY 1, 2";

#[test]
fn REQ295_AC1_build_export_import_build_sans_appel() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build"]));
    assert!(!e.log_is_empty("origin"));
    let n = count(&repo);
    assert!(n > 0);
    let file = e.root.join("enrichissements.db");
    let file_s = file.to_str().unwrap();

    let out = ok(&e.run(&repo, "export", &["cache", "export", file_s]));
    assert_eq!(
        out,
        format!("cache export terminé : {n} enrichissements écrits\n")
    );

    let copy = e.copy("copie");
    let out = ok(&e.run(&copy, "import", &["cache", "import", file_s]));
    assert_eq!(
        out,
        format!("cache import terminé : {n} enrichissements ajoutés, 0 déjà présents ; lancez nephila build pour les appliquer\n")
    );
    assert!(e.log_is_empty("import"));

    ok(&e.run(&copy, "build", &["build"]));
    assert!(e.log_is_empty("build"), "le build a appelé le LLM");
    let status = ok(&e.run(&copy, "status", &["status", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(v["to_enrich"], 0);
    assert_eq!(rows(&copy, NODES), rows(&repo, NODES));
    assert_eq!(rows(&copy, MENTIONS), rows(&repo, MENTIONS));
    assert!(!rows(&repo, MENTIONS).is_empty());

    // Second import : tout est déjà présent.
    let out = ok(&e.run(&copy, "import2", &["cache", "import", file_s]));
    assert_eq!(
        out,
        format!("cache import terminé : 0 enrichissements ajoutés, {n} déjà présents ; lancez nephila build pour les appliquer\n")
    );
}

#[test]
fn REQ295_AC1_import_puis_build_no_llm() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build"]));
    let file = e.root.join("enrichissements.db");
    ok(&e.run(
        &repo,
        "export",
        &["cache", "export", file.to_str().unwrap()],
    ));

    let copy = e.copy("copie");
    ok(&e.run(
        &copy,
        "import",
        &["cache", "import", file.to_str().unwrap()],
    ));
    ok(&e.run(&copy, "build", &["build", "--no-llm"]));
    assert!(e.log_is_empty("build"));
    assert_eq!(rows(&copy, NODES), rows(&repo, NODES));
}

#[test]
fn REQ295_AC1_AC5_export_sans_enrichissement() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build", "--no-llm"]));
    let file = e.root.join("vide.db");
    let out = ok(&e.run(
        &repo,
        "export",
        &["cache", "export", file.to_str().unwrap()],
    ));
    assert_eq!(out, "cache export terminé : 0 enrichissements écrits\n");
    assert!(file.is_file());
}
