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
use schema6::schema7::schema8;

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
        let mut expected = text(&reference.ok(args).stdout);
        if args[0] == "status" {
            // REQ-088 : le graphe de schéma 5 n'a pas de marqueur, donc il est à résoudre.
            expected = expected.replacen(
                "graphe à jour",
                "graphe à résoudre : lancez nephila update",
                1,
            );
        }
        assert_eq!(text(&env.ok(args).stdout), expected, "{args:?}");
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
            // REQ-088 : un graphe de schéma 5 à 7 n'a pas de marqueur, donc il est à résoudre.
            let expected = if args[0] == "status" {
                assert!(expected.starts_with("graphe à jour\n"), "{expected}");
                expected.replacen(
                    "graphe à jour",
                    "graphe à résoudre : lancez nephila update",
                    1,
                )
            } else if args[0] == "summary" {
                // T-232 : un graphe lu sans migration n'a pas de communautés calculées.
                let at = expected.find("Communautés (").unwrap();
                format!(
                    "{}{}",
                    &expected[..at],
                    "Communautés\n  communautés non calculées (lancez nephila update)\n"
                )
            } else if args[0] == "export" {
                // REQ-149 : un graphe lu sans migration n'a ni numéro ni liste de communautés.
                let mut doc: serde_json::Value = serde_json::from_str(expected).unwrap();
                doc["communities"] = serde_json::json!([]);
                for n in doc["nodes"].as_array_mut().unwrap() {
                    n["community"] = serde_json::Value::Null;
                }
                serde_json::to_string(&doc).unwrap() + "\n"
            } else {
                expected.clone()
            };
            assert_eq!(text(&o.stdout), expected, "schéma {version} : {args:?}");
            assert_eq!(env.user_version(), version as i32, "{args:?}");
            assert_eq!(graph_fingerprint(&env), before, "{args:?}");
        }
        let enrichments = env.enrichments();
        assert!(!enrichments.is_empty());

        // Premier `update` : bilan d'une passe complète (graphe à résoudre), aucun appel LLM.
        let o = env.ok(&["update"]);
        assert!(
            text(&o.stdout).starts_with("update terminé : 0 fichiers traités,"),
            "schéma {version} : {}",
            text(&o.stdout)
        );
        assert!(env.logged().is_empty(), "{:?}", env.logged());
        assert_eq!(env.user_version(), SCHEMA_VERSION);
        assert_eq!(text(&env.ok(&["status"]).stdout), outputs[2]);
        let o = env.ok(&["update"]);
        assert_eq!(text(&o.stdout).trim(), "graphe à jour", "schéma {version}");
        assert!(env.logged().is_empty(), "{:?}", env.logged());
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

// ---- REQ-115 : trace de la migration du schéma ----

/// Dépôt exemple construit puis ramené au schéma `version` (5 à 8).
fn built_at(version: i32) -> Env {
    let env = Env::new();
    env.ok(&["build", "--no-llm"]);
    match version {
        8 => schema8::downgrade_to_8(&env.db()),
        7 => schema6::schema7::downgrade_to_7(&env.db()),
        6 => schema6::downgrade_to_6(&env.db()),
        5 => schema6::downgrade_to_5(&env.db()),
        v => panic!("schéma {v}"),
    }
    assert_eq!(env.user_version(), version);
    env
}

/// Lignes `trace:` de stderr sans leur préfixe `trace: [..] `.
fn trace_lines(stderr: &[u8]) -> Vec<String> {
    text(stderr)
        .lines()
        .filter(|l| l.starts_with("trace: ["))
        .map(|l| l.split_once("] ").unwrap().1.to_string())
        .collect()
}

fn migration_line(version: i32) -> String {
    format!("schéma: migration {version} vers {SCHEMA_VERSION} en ")
}

/// Une seule ligne `schéma:`, de la forme du design, avant la ligne `parcours:`.
fn assert_migration_trace(stderr: &[u8], version: i32) {
    let lines = trace_lines(stderr);
    let schema: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("schéma:"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(schema.len(), 1, "{lines:?}");
    let line = &lines[schema[0]];
    let ms = line
        .strip_prefix(&migration_line(version))
        .and_then(|r| r.strip_suffix(" ms"))
        .unwrap_or_else(|| panic!("ligne inattendue : {line}"));
    assert!(
        !ms.is_empty() && ms.chars().all(|c| c.is_ascii_digit()),
        "{line}"
    );
    let config = lines
        .iter()
        .position(|l| l.contains("config: llm inactif (--no-llm)"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    let walk = lines.iter().position(|l| l.contains("parcours:")).unwrap();
    assert!(config < schema[0] && schema[0] < walk, "{lines:?}");
}

#[test]
fn req115_ac1_update_trace_la_migration_depuis_chaque_schema() {
    for version in [8, 7, 6, 5] {
        let env = built_at(version);
        let o = env.ok(&["update", "--no-llm", "--verbose"]);
        assert_migration_trace(&o.stderr, version);
        assert_eq!(env.user_version(), SCHEMA_VERSION);
    }
}

#[test]
fn req115_ac1_build_trace_la_migration() {
    let env = built_at(7);
    let o = env.ok(&["build", "--no-llm", "--verbose"]);
    assert_migration_trace(&o.stderr, 7);
    assert_eq!(env.user_version(), SCHEMA_VERSION);
}

#[test]
fn req115_ac2_aucune_trace_sans_migration() {
    let env = Env::new();
    // Base neuve.
    let o = env.ok(&["build", "--no-llm", "--verbose"]);
    assert!(!text(&o.stderr).contains("schéma:"), "{}", text(&o.stderr));
    // Schéma courant.
    for args in [
        ["update", "--no-llm", "--verbose"],
        ["build", "--no-llm", "--verbose"],
    ] {
        let o = env.ok(&args);
        assert!(!text(&o.stderr).contains("schéma:"), "{}", text(&o.stderr));
    }
    // Sans --verbose, aucune trace.
    let env = built_at(7);
    let o = env.ok(&["update", "--no-llm"]);
    assert!(!text(&o.stderr).contains("trace:"), "{}", text(&o.stderr));
    assert_eq!(env.user_version(), SCHEMA_VERSION);
}

#[test]
fn req115_ac2_aucune_trace_si_la_migration_echoue() {
    let env = built_at(6);
    env.db()
        .execute_batch("CREATE TABLE file_contexts (x TEXT)")
        .unwrap();
    let o = env.run(&["update", "--no-llm", "--verbose"]);
    assert!(!o.status.success());
    assert!(!text(&o.stderr).contains("schéma:"), "{}", text(&o.stderr));
}

#[test]
fn req115_ac3_texte_exact_et_place() {
    let env = built_at(7);
    let o = env.ok(&["update", "--no-llm", "--verbose"]);
    let err = text(&o.stderr);
    let re = is_migration_trace_7;
    let matching: Vec<&str> = err.lines().filter(|l| re(l)).collect();
    assert_eq!(matching.len(), 1, "{err}");
    assert!(!err.contains('→') && !err.contains("->"), "{err}");
    assert_migration_trace(&o.stderr, 7);
}

/// Vérifie le motif de AC-115-3 (pas de dépendance de test nouvelle) :
/// `^trace: \[[0-9]+\.[0-9]{3}s\] schéma: migration 7 vers 9 en [0-9]+ ms$`.
fn is_migration_trace_7(line: &str) -> bool {
    {
        let Some(rest) = line.strip_prefix("trace: [") else {
            return false;
        };
        let Some((time, rest)) = rest.split_once("s] ") else {
            return false;
        };
        let Some((secs, millis)) = time.split_once('.') else {
            return false;
        };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !(digits(secs) && millis.len() == 3 && digits(millis)) {
            return false;
        }
        let head = format!("schéma: migration 7 vers {SCHEMA_VERSION} en ");
        rest.strip_prefix(&head)
            .and_then(|r| r.strip_suffix(" ms"))
            .is_some_and(digits)
    }
}

// ---- REQ-147 : schéma 9, tables des communautés (DEC-120) ----

type Communities = (Vec<(i64, String, String, i64, u64)>, Vec<(String, i64)>);

/// Contenu des tables des communautés (cohésion en bits, pour une égalité exacte).
fn community_tables(env: &Env) -> Communities {
    let db = env.db();
    let rows = db
        .prepare("SELECT id, label, pivot, size, cohesion FROM communities ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get::<_, f64>(4)?.to_bits(),
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let members = db
        .prepare("SELECT node, community FROM node_communities ORDER BY node")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    (rows, members)
}

/// Communautés d'un `build --no-llm` du même dépôt, sans passage par un ancien schéma.
fn communities_of_a_build() -> Communities {
    let fresh = Env::new();
    fresh.ok(&["build", "--no-llm"]);
    let tables = community_tables(&fresh);
    assert!(!tables.0.is_empty());
    tables
}

#[test]
fn req147_ac4_schema8_is_read_unchanged_by_status_summary_and_export() {
    let env = built_at(8);
    let before = graph_fingerprint(&env);
    for args in [
        &["status"][..],
        &["summary"][..],
        &["export", "--output", "-"][..],
    ] {
        let o = env.ok(args);
        assert!(!text(&o.stdout).is_empty(), "{args:?}");
        assert_eq!(env.user_version(), 8, "{args:?}");
        assert_eq!(graph_fingerprint(&env), before, "{args:?}");
    }
    assert!(env.db().prepare("SELECT 1 FROM communities").is_err());
}

#[test]
fn req147_ac4_update_migrates_schema8_to_9_and_traces_it() {
    let env = built_at(8);
    let o = env.ok(&["update", "--no-llm", "--verbose"]);
    assert!(o.status.success());
    assert_eq!(env.user_version(), 9);
    assert_migration_trace(&o.stderr, 8);
    assert!(
        trace_lines(&o.stderr)
            .iter()
            .any(|l| l.starts_with("schéma: migration 8 vers 9 en ")),
        "{}",
        text(&o.stderr)
    );
    // La passe complète qui suit la migration remplit les deux tables : celles d'un `build`.
    assert_eq!(community_tables(&env), communities_of_a_build());
    let v: String = env
        .db()
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, "9");
}

#[test]
fn req147_ac4_schema7_reaches_the_same_result_as_schema8() {
    let from8 = built_at(8);
    from8.ok(&["update", "--no-llm"]);
    let from7 = built_at(7);
    let o = from7.ok(&["update", "--no-llm", "--verbose"]);
    assert_eq!(from7.user_version(), 9);
    assert!(
        trace_lines(&o.stderr)
            .iter()
            .any(|l| l.starts_with("schéma: migration 7 vers 9 en ")),
        "{}",
        text(&o.stderr)
    );
    let names = |env: &Env| -> Vec<(String, String, String)> {
        master(env)
            .into_iter()
            .map(|(t, n, tbl, _)| (t, n, tbl))
            .collect()
    };
    assert_eq!(names(&from7), names(&from8));
    assert_eq!(community_tables(&from7), communities_of_a_build());
    assert_eq!(community_tables(&from8), communities_of_a_build());
    assert_eq!(
        text(&from7.ok(&["status"]).stdout),
        text(&from8.ok(&["status"]).stdout)
    );
}
