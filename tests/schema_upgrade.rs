//! Mise à niveau du graphe du schéma 5 au schéma 6 (REQ-057, DEC-066) : AC-057-3.

use assert_cmd::Command;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

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

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    /// Dépôt exemple des tests de requête, mock enrichi (`enrich_code`).
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("maven2"), &repo);
        copy_dir(&fx.join("mock"), &mock);
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\n",
        )
        .unwrap();
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            mock,
            log,
        }
    }

    /// Exécute la commande avec un journal du mock vide au départ.
    fn run(&self, args: &[&str]) -> Output {
        let _ = fs::remove_file(&self.log);
        Command::cargo_bin("nephila")
            .unwrap()
            .env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(o.status.success(), "{args:?} : {}", text(&o.stderr));
        o
    }

    fn logged(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn db(&self) -> Connection {
        Connection::open(self.repo.join(".nephila/graph.db")).unwrap()
    }

    fn user_version(&self) -> i32 {
        self.db()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    /// Construit le graphe puis le ramène au schéma 5.
    fn built_at_schema5() -> Env {
        let env = Env::new();
        env.ok(&["build"]);
        assert_eq!(env.user_version(), 6);
        env.db()
            .execute_batch(
                "DROP INDEX idx_nodes_qualified_rev; ALTER TABLE nodes DROP COLUMN qualified_rev;
                 UPDATE meta SET value = '5' WHERE key = 'schema_version'; PRAGMA user_version = 5;",
            )
            .unwrap();
        assert_eq!(env.user_version(), 5);
        env
    }

    fn enrichments(&self) -> Vec<(String, String, String, i64, String)> {
        let db = self.db();
        let mut q = db
            .prepare("SELECT path, content_hash, model, prompt_version, payload FROM enrichments ORDER BY path")
            .unwrap();
        q.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn req057_ac3_schema5_read_only_commands_then_update_migrates_without_llm() {
    let env = Env::built_at_schema5();
    let enrichments = env.enrichments();
    assert!(!enrichments.is_empty());

    for args in [
        &["query", "neighbors", "OrderRepository.save"][..],
        &["status"][..],
    ] {
        let o = env.ok(args);
        assert!(!o.stdout.is_empty(), "{args:?}");
        assert_eq!(env.user_version(), 5, "{args:?}");
    }
    // Mêmes sorties que sur un graphe de schéma 6 construit à l'identique.
    let reference = Env::new();
    reference.ok(&["build"]);
    for args in [
        &["query", "neighbors", "OrderRepository.save"][..],
        &["status"][..],
    ] {
        assert_eq!(
            text(&env.ok(args).stdout),
            text(&reference.ok(args).stdout),
            "{args:?}"
        );
    }
    assert_eq!(env.user_version(), 5);

    env.ok(&["update"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert_eq!(env.user_version(), 6);
    let db = env.db();
    let (total, filled): (i64, i64) = db
        .query_row(
            "SELECT count(*), count(CASE WHEN qualified_rev <> '' THEN 1 END) FROM nodes",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(total > 0);
    assert_eq!(total, filled);
    assert_eq!(env.enrichments(), enrichments);
}

#[test]
fn req057_ac3_schema5_build_migrates_without_llm() {
    let env = Env::built_at_schema5();
    let enrichments = env.enrichments();
    env.ok(&["build"]);
    assert!(env.logged().is_empty(), "{:?}", env.logged());
    assert_eq!(env.user_version(), 6);
    assert_eq!(env.enrichments(), enrichments);
}
