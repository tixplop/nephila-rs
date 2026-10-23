//! Performances sur 10 000 fichiers (REQ-021) et sur un dépôt JS/TS de 3 000 fichiers. Tests `#[ignore]` à lancer en release :
//! `cargo test --release -q --color never -- --ignored perf`.

#[path = "support/gen_js.rs"]
mod gen_js;
#[path = "support/gen_repo.rs"]
mod gen_repo;

use assert_cmd::cargo::cargo_bin;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

struct Fixture {
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
}

fn cmd(f: &Fixture, args: &[&str]) -> Command {
    let mut c = Command::new(cargo_bin("nephila"));
    c.arg("-C")
        .arg(&f.repo)
        .args(args)
        .env("NEPHILA_CONFIG", &f.config)
        .env("NEPHILA_MOCK_DIR", &f.mock)
        .env_remove("NEPHILA_MOCK_LOG")
        .env_remove("NEPHILA_MOCK_FAIL_AFTER")
        .env_remove("NEPHILA_MOCK_DELAY_MS")
        .env_remove("NEPHILA_MOCK_INVALID_KEY");
    c
}

fn timed(f: &Fixture, args: &[&str]) -> (Duration, Output) {
    let t = Instant::now();
    let out = cmd(f, args).output().unwrap();
    let d = t.elapsed();
    assert!(
        out.status.success(),
        "{args:?} : {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (d, out)
}

/// Sérialise les tests : `update` (AC3) modifie la fixture partagée, et les mesures
/// de temps ne doivent pas se perturber entre elles.
fn serial() -> MutexGuard<'static, ()> {
    static M: Mutex<()> = Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

/// Dépôt indexé une seule fois pour tous les tests. Il vit sous `target/` (jamais dans
/// `/tmp`) et est supprimé puis régénéré à chaque lancement.
fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-fixture");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let repo = root.join("repo");
        let mock = root.join("mock");
        gen_repo::generate(&repo, &mock);
        let config = root.join("config.toml");
        std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let f = Fixture { repo, config, mock };
        let (d, out) = timed(&f, &["build"]);
        eprintln!(
            "build : {d:?} ; {}",
            String::from_utf8_lossy(&out.stdout).trim()
        );
        // Laisse passer la fenêtre « racy » pour que les empreintes soient réutilisables.
        std::thread::sleep(Duration::from_millis(2_100));
        timed(&f, &["update"]);
        f
    })
}

/// Meilleure de trois mesures (écarte le bruit de démarrage de la machine).
fn best(f: &Fixture, args: &[&str]) -> (Duration, Output) {
    (0..3)
        .map(|_| timed(f, args))
        .min_by_key(|(d, _)| *d)
        .unwrap()
}

fn assert_under(label: &str, d: Duration, max_ms: u64) {
    eprintln!("{label} : {d:?} (seuil {max_ms} ms)");
    assert!(d < Duration::from_millis(max_ms), "{label} : {d:?}");
}

#[test]
#[ignore]
fn perf_req021_ac1_status_unchanged() {
    let _g = serial();
    let f = fixture();
    let (d, out) = best(f, &["status"]);
    assert_under("status", d, 200);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "graphe à jour");
}

#[test]
#[ignore]
fn perf_req021_ac2_queries() {
    let _g = serial();
    let f = fixture();
    let a = gen_repo::class_name(4242);
    let b = gen_repo::class_name(77);
    let id_a = format!("type:{}.{a}", gen_repo::package_of(4242));
    let id_b = format!("type:{}.{b}", gen_repo::package_of(77));
    // AC-021-8 : moins de 100 ms sans `--check-stale`, moins de 200 ms avec.
    for (flag, max_ms) in [(None, 100), (Some("--check-stale"), 200)] {
        let with = |mut v: Vec<&'static str>, tail: &[&str]| -> Vec<String> {
            v.extend(flag);
            v.into_iter()
                .map(str::to_string)
                .chain(tail.iter().map(|t| t.to_string()))
                .collect()
        };
        let label = flag.unwrap_or("sans --check-stale");
        let run = |head: Vec<&'static str>, tail: &[&str]| {
            let args = with(head, tail);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            best(f, &refs)
        };
        let (d, out) = run(vec!["query", "search"], &[&a]);
        assert_under(&format!("query search {label}"), d, max_ms);
        assert!(String::from_utf8_lossy(&out.stdout).contains(&a));
        let (d, out) = run(vec!["query", "neighbors"], &[&id_a, "--depth", "1"]);
        assert_under(&format!("query neighbors {label}"), d, max_ms);
        assert!(String::from_utf8_lossy(&out.stdout).contains("utilisé par"));
        let (d, out) = run(vec!["query", "path"], &[&id_a, &id_b]);
        assert_under(&format!("query path {label}"), d, max_ms);
        assert!(String::from_utf8_lossy(&out.stdout).contains(&b));
    }
}

/// Graphe écrit par l'API `Store` : un type `Hub` (avec sa méthode `run`) qui reçoit `n` liens
/// `imports` de `n` types feuilles et émet `n` liens `calls` vers `n` méthodes.
fn hub_fixture(n: usize) -> Fixture {
    use nephila::model::{Edge, EdgeKind, FileRecord, FileState, Node, NodeKind, SubKind};
    use nephila::store::Store;
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("target/perf-hub-{n}"));
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let config = root.join("config.toml");
    std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let path = "src/Hub.java";
    let ty = |q: &str| {
        Node::new(
            NodeKind::Type,
            q,
            q.rsplit('.').next().unwrap(),
            Some(path),
            Some(SubKind::Class),
            None,
        )
    };
    let method = |q: &str| {
        Node::new(
            NodeKind::Method,
            q,
            q.rsplit('.').next().unwrap(),
            Some(path),
            None,
            None,
        )
    };
    let mut nodes = vec![ty("hub.Hub"), method("hub.Hub.run")];
    let mut edges = vec![Edge {
        source: "type:hub.Hub".into(),
        target: "method:hub.Hub.run".into(),
        kind: EdgeKind::Contains,
    }];
    for i in 0..n {
        let leaf = ty(&format!("hub.Leaf{i:06}"));
        let callee = method(&format!("hub.Callee{i:06}.m"));
        edges.push(Edge {
            source: leaf.id.clone(),
            target: "type:hub.Hub".into(),
            kind: EdgeKind::Imports,
        });
        edges.push(Edge {
            source: "method:hub.Hub.run".into(),
            target: callee.id.clone(),
            kind: EdgeKind::Calls,
        });
        nodes.push(leaf);
        nodes.push(callee);
    }
    let mut store = Store::open_or_create(&repo).unwrap();
    store
        .insert_pending_files(&[FileRecord {
            path: path.into(),
            hash: "h".into(),
            size: 1,
            mtime: 1,
            recorded_at: 0,
            state: FileState::Pending,
        }])
        .unwrap();
    store.replace_structure(path, &nodes, &[]).unwrap();
    store.replace_edges(&edges).unwrap();
    drop(store);
    Fixture {
        repo,
        config,
        mock: root.join("mock"),
    }
}

/// AC-056-2 : `neighbors` et `path` autour d'un nœud très relié, en temps linéaire.
#[test]
#[ignore]
fn perf_req056_ac2_hub() {
    let _g = serial();
    let mut times: Vec<[Duration; 3]> = Vec::new();
    for n in [10_000usize, 40_000] {
        let f = hub_fixture(n);
        let leaf_a = "type:hub.Leaf000007";
        let leaf_b = "type:hub.Leaf000042";
        let (d1, out) = best(&f, &["query", "neighbors", "Hub"]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(text.lines().filter(|l| l.starts_with("  ")).count(), 50);
        assert_eq!(
            text.lines().last().unwrap(),
            format!("{} résultats supplémentaires", 2 * n - 50)
        );
        let (d2, out) = best(&f, &["query", "neighbors", leaf_a, "--depth", "2"]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(text.lines().filter(|l| l.starts_with("  ")).count(), 50);
        assert_eq!(
            text.lines().last().unwrap(),
            format!("{} résultats supplémentaires", n + 1 - 50)
        );
        let (d3, out) = best(&f, &["query", "path", leaf_a, leaf_b]);
        assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 2);
        eprintln!(
            "N = {n} : neighbors Hub {d1:?}, neighbors feuille --depth 2 {d2:?}, path {d3:?}"
        );
        times.push([d1, d2, d3]);
    }
    for (i, label) in ["neighbors Hub", "neighbors feuille --depth 2", "path"]
        .iter()
        .enumerate()
    {
        let (small, big) = (times[0][i], times[1][i]);
        assert!(
            big < Duration::from_millis(1_000),
            "{label} : {big:?} (40 000)"
        );
        assert!(
            big < small * 6,
            "{label} : {big:?} (40 000) pour {small:?} (10 000)"
        );
    }
}

/// AC-055-3 : le temps de `query ask` reste sous 1,5 fois le temps d'un mot-clé pour 32 mots-clés.
#[test]
#[ignore]
fn perf_req055_ac3_ask_keywords() {
    let _g = serial();
    let shared = fixture();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-ask");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let config = root.join("config.toml");
    std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let mock = root.join("mock");
    std::fs::create_dir_all(&mock).unwrap();
    let cited = format!(
        "{}.{}",
        gen_repo::package_of(4242),
        gen_repo::class_name(4242)
    );
    std::fs::write(
        mock.join("_ask.json"),
        serde_json::json!({"found": true, "answer": "réponse", "nodes": [cited]}).to_string(),
    )
    .unwrap();
    let f = Fixture {
        repo: shared.repo.clone(),
        config,
        mock,
    };
    let names: Vec<String> = (0..32)
        .map(|i| {
            assert!(100 + i * 250 < gen_repo::CODE_FILES);
            gen_repo::class_name(100 + i * 250)
        })
        .collect();
    let q1 = format!("Quel rôle joue {} ?", names[0]);
    let q32 = format!("Quel rôle jouent {} ?", names.join(" "));
    let (t1, o1) = best(&f, &["query", "ask", &q1]);
    let (t32, o32) = best(&f, &["query", "ask", &q32]);
    let ratio = t32.as_secs_f64() / t1.as_secs_f64();
    eprintln!("ask 1 mot-clé : {t1:?} ; 32 mots-clés : {t32:?} ; rapport {ratio:.2}");
    assert!(String::from_utf8_lossy(&o1.stdout).contains("réponse"));
    assert!(String::from_utf8_lossy(&o32.stdout).contains("réponse"));
    assert!(
        t32 * 2 < t1 * 3,
        "1 mot-clé : {t1:?} ; 32 mots-clés : {t32:?} ; rapport {ratio:.2} (seuil 1,5)"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore]
fn perf_req021_ac3_update_one_modified_file() {
    let _g = serial();
    let f = fixture();
    let path = f.repo.join(gen_repo::java_path(1234));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}// modifié\n")).unwrap();
    let log = f.repo.parent().unwrap().join("mock.log");
    let _ = std::fs::remove_file(&log);
    let (d, out) = {
        let t = Instant::now();
        let out = cmd(f, &["update"])
            .env("NEPHILA_MOCK_LOG", &log)
            .output()
            .unwrap();
        let d = t.elapsed();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        (d, out)
    };
    let logged = std::fs::read_to_string(&log).unwrap();
    let called: Vec<&str> = logged
        .lines()
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    assert_eq!(called, vec![gen_repo::java_path(1234)], "{logged}");
    let (_, found) = timed(f, &["query", "search", gen_repo::SUMMARY_WORD]);
    assert!(
        String::from_utf8_lossy(&found.stdout).contains(&gen_repo::class_name(1234)),
        "{}",
        String::from_utf8_lossy(&found.stdout)
    );
    assert_under("update (1 fichier modifié)", d, 3_000);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("terminé : 1 fichiers traités,"), "{stdout}");
    // Seconde modification juste après l'update : la fenêtre « racy » force un nouveau calcul d'empreintes.
    std::fs::write(&path, format!("{text}// encore\n")).unwrap();
    let (d, _) = timed(f, &["update"]);
    assert_under("update juste après update", d, 3_000);
}

#[test]
#[ignore]
fn perf_req021_ac4_single_native_binary_help() {
    let _g = serial();
    let f = fixture();
    let bin = cargo_bin("nephila");
    let md = std::fs::metadata(&bin).unwrap();
    assert!(md.is_file());
    let (d, _) = best(f, &["--help"]);
    assert_under("--help", d, 50);
}

#[test]
#[ignore]
fn perf_req021_ac5_build_no_llm() {
    let _g = serial();
    let shared = fixture();
    // Répertoire à part : la fixture partagée garde son graphe enrichi (indépendance à l'ordre).
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-nollm");
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let mock = root.join("mock");
    gen_repo::generate(&repo, &mock);
    let f = Fixture {
        repo,
        config: shared.config.clone(),
        mock,
    };
    let result = std::panic::catch_unwind(|| {
        let (d, out) = timed(&f, &["build", "--no-llm"]);
        assert_under("build --no-llm", d, 30_000);
        eprintln!("{}", String::from_utf8_lossy(&out.stdout).trim());
        // Le graphe reconstruit reste exploitable.
        let name = gen_repo::class_name(4242);
        let (_, out) = timed(&f, &["query", "search", &name]);
        assert!(String::from_utf8_lossy(&out.stdout).contains(&name));
    });
    let _ = std::fs::remove_dir_all(&root);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

#[test]
#[ignore]
fn perf_req021_ac6_export() {
    let _g = serial();
    let f = fixture();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-export");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (format, name) in [("json", "out.json"), ("html", "out.html")] {
        let out = dir.join(name);
        let (d, _) = timed(
            f,
            &[
                "export",
                "--format",
                format,
                "--output",
                out.to_str().unwrap(),
            ],
        );
        assert_under(&format!("export {format}"), d, 5_000);
        assert!(out.metadata().unwrap().len() > 0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dépôt JavaScript/TypeScript de 3 000 fichiers sous `target/perf-fixture-js` (AC-021-7).
fn js_fixture() -> Fixture {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-fixture-js");
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let mock = root.join("mock");
    gen_js::generate(&repo, &mock);
    let config = root.join("config.toml");
    std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    Fixture { repo, config, mock }
}

#[test]
#[ignore]
fn perf_req021_ac7_js_ts() {
    let _g = serial();
    let f = js_fixture();
    let (d, out) = timed(&f, &["build", "--no-llm"]);
    assert_under("build --no-llm (JS/TS)", d, 15_000);
    let summary = String::from_utf8_lossy(&out.stdout).to_string();
    eprintln!("{}", summary.trim());
    assert!(
        summary.contains(", 0 références non résolues,"),
        "références non résolues : {summary}"
    );
    let name = gen_js::class_name(gen_js::EDITED);
    let (_, found) = timed(&f, &["query", "search", &name]);
    assert!(String::from_utf8_lossy(&found.stdout).contains(&name));
    // Enrichissement initial (LLM simulé), puis fenêtre « racy ».
    timed(&f, &["build"]);
    std::thread::sleep(Duration::from_millis(2_100));
    timed(&f, &["update"]);
    let path = f.repo.join(gen_js::path(gen_js::EDITED));
    assert!(path.to_string_lossy().ends_with(".ts"));
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}// modifié\n")).unwrap();
    let log = f.repo.parent().unwrap().join("mock.log");
    let _ = std::fs::remove_file(&log);
    let t = Instant::now();
    let out = cmd(&f, &["update"])
        .env("NEPHILA_MOCK_LOG", &log)
        .output()
        .unwrap();
    let d = t.elapsed();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_under("update (1 .ts modifié)", d, 3_000);
    let logged = std::fs::read_to_string(&log).unwrap();
    let called: Vec<&str> = logged
        .lines()
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    assert_eq!(called, vec![gen_js::path(gen_js::EDITED)], "{logged}");
    let (_, found) = timed(&f, &["query", "search", gen_js::SUMMARY_WORD]);
    assert!(String::from_utf8_lossy(&found.stdout).contains(&name));
}

#[test]
#[ignore]
fn perf_req021_ac7_help_with_grammars() {
    let _g = serial();
    let f = js_fixture();
    let (d, _) = best(&f, &["--help"]);
    assert_under("--help (grammaires JS/TS)", d, 50);
}

/// Empreintes de l'export JSON des gros dépôts, sur le code de la 0.20.0 (AC-065-2) :
/// `(dépôt, étape, empreinte BLAKE3)`. Relever de nouvelles valeurs avec `NEPHILA_ORACLE_CAPTURE=1`.
const ORACLE_GROS_DEPOTS: &[(&str, &str, &str)] = &[
    (
        "gen_repo",
        "build",
        "697d76e547c3511c98c2dac7fd4bc241e301ad8a35d8ebfe092952b3cf870328",
    ),
    (
        "gen_repo",
        "update",
        "df7bf978c1a5c62ad88a41501dafb0c0cd5d6d5725e6e0166b8bd91ccef1680e",
    ),
    (
        "gen_js",
        "build",
        "39b2d29cbbf97edf2814c6f2a4ec86a0d21dd624d3608f5710864f3115b6977e",
    ),
    (
        "gen_js",
        "update",
        "1b24971293c2e0aa4dbdaedec81518686003b0e7179b172f4deb89f0e383bd21",
    ),
];

fn export_hash(f: &Fixture) -> String {
    let (_, out) = timed(f, &["export", "--format", "json", "--output", "-"]);
    blake3::hash(&out.stdout).to_hex().to_string()
}

/// AC-065-2 : l'export des gros dépôts, après `build --no-llm` puis après `update`, est celui de la 0.20.0.
#[test]
#[ignore]
fn perf_req065_ac2_oracle_gros_depots() {
    let _g = serial();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found: Vec<(&str, &str, String)> = Vec::new();
    for name in ["gen_repo", "gen_js"] {
        let root = base.join(format!("target/perf-oracle-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let mock = root.join("mock");
        let edited = if name == "gen_repo" {
            gen_repo::generate(&repo, &mock);
            gen_repo::java_path(1234)
        } else {
            gen_js::generate(&repo, &mock);
            gen_js::path(gen_js::EDITED)
        };
        let config = root.join("config.toml");
        std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let f = Fixture { repo, config, mock };
        timed(&f, &["build", "--no-llm"]);
        found.push((name, "build", export_hash(&f)));
        let path = f.repo.join(edited);
        let text = std::fs::read_to_string(&path).unwrap();
        let extra = if name == "gen_repo" {
            "\nclass OracleExtra {}\n"
        } else {
            "\nexport function oracleExtra(): number {\n  return 1;\n}\n"
        };
        std::fs::write(&path, format!("{text}{extra}")).unwrap();
        timed(&f, &["update"]);
        found.push((name, "update", export_hash(&f)));
        let _ = std::fs::remove_dir_all(&root);
    }
    if std::env::var_os("NEPHILA_ORACLE_CAPTURE").is_some_and(|v| v == "1") {
        for (n, s, h) in &found {
            eprintln!("ORACLE (\"{n}\", \"{s}\", \"{h}\"),");
        }
        return;
    }
    assert_eq!(found.len(), ORACLE_GROS_DEPOTS.len(), "oracle non figé");
    for ((n, s, h), (en, es, eh)) in found.iter().zip(ORACLE_GROS_DEPOTS) {
        assert_eq!((*n, *s, h.as_str()), (*en, *es, *eh), "écart d'oracle");
    }
}

/// Mesures de la 0.20.0 (AC-063-5), figées sur HEAD : place utilisée U₀ et Σ `length(context)` C₀.
/// U varie de ~0,3 % d'un lancement à l'autre (ordre d'écriture des fils) : on fige le minimum des mesures relevées sur HEAD (six lancements).
const U0_JS: i64 = 162_426_880;
const C0_JS: i64 = 84_904_838;
const U0_JVM: i64 = 153_509_888;
const C0_JVM: i64 = 3_199_600;

/// Place utilisée `(page_count − freelist_count) × page_size` et Σ `length(context)` de `refs`.
fn graph_size(f: &Fixture) -> (i64, i64) {
    let conn = rusqlite::Connection::open(f.repo.join(".nephila/graph.db")).unwrap();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    let pragma = |p: &str| -> i64 {
        conn.query_row(&format!("PRAGMA {p}"), [], |r| r.get(0))
            .unwrap()
    };
    let used = (pragma("page_count") - pragma("freelist_count")) * pragma("page_size");
    let ctx: i64 = conn
        .query_row(
            "SELECT coalesce(sum(length(context)), 0) FROM refs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    (used, ctx)
}

/// AC-063-5 (mesures U₀ et C₀ ; T-140 ajoute la condition U ≤ U₀ − C₀ / 2 et U < U₀).
#[test]
#[ignore]
fn perf_req063_ac5_taille_du_graphe() {
    let _g = serial();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for (name, u0, c0) in [("gen_js", U0_JS, C0_JS), ("gen_repo", U0_JVM, C0_JVM)] {
        let root = base.join(format!("target/perf-size-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let mock = root.join("mock");
        if name == "gen_js" {
            gen_js::generate(&repo, &mock);
        } else {
            gen_repo::generate(&repo, &mock);
        }
        let config = root.join("config.toml");
        std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let f = Fixture { repo, config, mock };
        timed(&f, &["build", "--no-llm"]);
        let (u, c) = graph_size(&f);
        eprintln!("MESURE {name} : U = {u} octets, C = {c} octets (U0 = {u0}, C0 = {c0})");
        assert!(c > 0, "{name} : aucun contexte de référence");
        let _ = std::fs::remove_dir_all(&root);
    }
}
