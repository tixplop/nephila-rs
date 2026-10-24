//! Mise à niveau du graphe du schéma 5 au schéma 6 (REQ-057, DEC-066) : AC-057-3 ; vers le
//! schéma 7 (REQ-062, DEC-073) : AC-062-5, AC-062-6 ; vers le schéma courant (REQ-069, DEC-076).

use assert_cmd::Command;
use nephila::store::SCHEMA_VERSION;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "support/schema6.rs"]
mod schema6;

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

    /// Construit le graphe puis le ramène au schéma 5 (l'assistant du schéma 6 d'abord).
    fn built_at_schema5() -> Env {
        let env = Env::new();
        env.ok(&["build"]);
        assert_eq!(env.user_version(), SCHEMA_VERSION);
        schema6::downgrade_to_5(&env.db());
        assert_eq!(env.user_version(), 5);
        env
    }

    /// Construit le graphe puis le ramène au schéma 6 de la 0.20.0.
    fn built_at_schema6() -> Env {
        let env = Env::new();
        env.ok(&["build"]);
        assert_eq!(env.user_version(), SCHEMA_VERSION);
        schema6::downgrade_to_6(&env.db());
        assert_eq!(env.user_version(), 6);
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
    assert_eq!(env.user_version(), SCHEMA_VERSION);
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
    assert_eq!(env.user_version(), SCHEMA_VERSION);
    assert_eq!(env.enrichments(), enrichments);
}

const READS: [&[&str]; 5] = [
    &["query", "neighbors", "OrderRepository.save"],
    &["query", "search", "Order"],
    &["status"],
    &["summary"],
    &["export", "--output", "-"],
];

fn graph_fingerprint(env: &Env) -> (u64, String) {
    let bytes = fs::read(env.repo.join(".nephila/graph.db")).unwrap();
    (
        bytes.len() as u64,
        blake3::hash(&bytes).to_hex().to_string(),
    )
}

/// Dépôt exemple au schéma `version` (5 ou 6) et les sorties de lecture d'avant la descente.
fn downgraded(version: u32) -> (Env, Vec<String>) {
    let env = Env::new();
    env.ok(&["build"]);
    assert_eq!(env.user_version(), SCHEMA_VERSION);
    let outputs: Vec<String> = READS.iter().map(|a| text(&env.ok(a).stdout)).collect();
    if version == 5 {
        schema6::downgrade_to_5(&env.db());
    } else {
        schema6::downgrade_to_6(&env.db());
    }
    assert_eq!(env.user_version(), version as i32);
    (env, outputs)
}

#[test]
fn req062_ac5_old_schemas_are_read_unchanged_then_migrated_by_update_or_build() {
    for version in [6, 5] {
        let (env, outputs) = downgraded(version);
        let before = graph_fingerprint(&env);
        for (args, expected) in READS.iter().zip(&outputs) {
            let o = env.ok(args);
            assert_eq!(&text(&o.stdout), expected, "schéma {version} : {args:?}");
            assert_eq!(env.user_version(), version as i32, "{args:?}");
            assert_eq!(graph_fingerprint(&env), before, "{args:?}");
        }
        let enrichments = env.enrichments();
        assert!(!enrichments.is_empty());

        let o = env.ok(&["update"]);
        assert_eq!(text(&o.stdout).trim(), "graphe à jour", "schéma {version}");
        assert!(env.logged().is_empty(), "{:?}", env.logged());
        assert_eq!(env.user_version(), SCHEMA_VERSION);
        assert_eq!(text(&env.ok(READS[4]).stdout), outputs[4]);
        assert_eq!(env.enrichments(), enrichments);

        // Autre copie du même graphe : `build` migre sans appel LLM.
        let (other, _) = downgraded(version);
        other.ok(&["build"]);
        assert!(other.logged().is_empty(), "{:?}", other.logged());
        assert_eq!(other.user_version(), SCHEMA_VERSION);
        assert_eq!(text(&other.ok(READS[4]).stdout), outputs[4]);
        assert_eq!(other.enrichments(), enrichments);
    }
}

fn master(env: &Env) -> Vec<(String, String, String, Option<String>)> {
    let db = env.db();
    let mut q = db
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY name")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn table_rows(env: &Env, sql: &str, cols: usize) -> Vec<Vec<String>> {
    let db = env.db();
    let mut q = db.prepare(sql).unwrap();
    q.query_map([], |r| {
        (0..cols)
            .map(|i| Ok(format!("{:?}", r.get_ref(i)?)))
            .collect()
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

#[test]
fn req062_ac6_failed_migration_leaves_the_schema_6_graph_untouched() {
    let env = Env::built_at_schema6();
    // Objet en conflit avec celui que la migration crée (sans `IF NOT EXISTS`).
    env.db()
        .execute_batch("CREATE TABLE file_contexts (x TEXT)")
        .unwrap();
    let master_before = master(&env);
    let queries = [
        ("SELECT * FROM refs ORDER BY id", 7),
        ("SELECT * FROM files ORDER BY path", 6),
        ("SELECT * FROM nodes ORDER BY id", 9),
        ("SELECT * FROM edges ORDER BY source, target, kind", 3),
        ("SELECT * FROM enrichments ORDER BY path", 5),
    ];
    let rows_before: Vec<_> = queries
        .iter()
        .map(|(q, n)| table_rows(&env, q, *n))
        .collect();

    let o = env.run(&["update"]);
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).lines().any(|l| l.starts_with("erreur:")),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(env.user_version(), 6);
    assert_eq!(master(&env), master_before);
    let rows_after: Vec<_> = queries
        .iter()
        .map(|(q, n)| table_rows(&env, q, *n))
        .collect();
    assert_eq!(rows_after, rows_before);
}
