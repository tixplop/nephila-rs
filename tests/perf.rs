//! Performances sur 10 000 fichiers (REQ-021) et sur un dépôt JS/TS de 3 000 fichiers. Tests `#[ignore]` à lancer en release :
//! `cargo nextest run --release --cargo-quiet --color never --test-threads 1 --run-ignored only --success-output final perf`.

#[path = "support/gen_js.rs"]
mod gen_js;
#[path = "support/gen_repo.rs"]
mod gen_repo;
#[path = "support/mod.rs"]
mod support;

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

/// Antidate de 10 s tous les fichiers de `dir` : hors de la fenêtre « racy », leurs empreintes
/// sont réutilisables dès le premier `build`, sans attente réelle.
fn backdate(dir: &std::path::Path) {
    let t = std::time::SystemTime::now() - Duration::from_secs(10);
    fn walk(dir: &std::path::Path, times: std::fs::FileTimes) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            let ty = std::fs::symlink_metadata(&p).unwrap().file_type();
            if ty.is_dir() {
                walk(&p, times);
            } else if ty.is_file() {
                std::fs::set_times(&p, times).unwrap();
            }
        }
    }
    walk(
        dir,
        std::fs::FileTimes::new().set_modified(t).set_accessed(t),
    );
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
        backdate(&repo);
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\n",
        )
        .unwrap();
        let f = Fixture { repo, config, mock };
        let (d, out) = timed(&f, &["build"]);
        eprintln!(
            "build : {d:?} ; {}",
            String::from_utf8_lossy(&out.stdout).trim()
        );
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
    js_fixture_with(Some(
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\n",
    ))
}

/// Génère et antidate la fixture JS ; `config` = contenu du fichier de configuration, ou `None`
/// pour un chemin de configuration absent (aucun LLM).
fn js_fixture_with(config_text: Option<&str>) -> Fixture {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-fixture-js");
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let mock = root.join("mock");
    gen_js::generate(&repo, &mock);
    backdate(&repo);
    let config = root.join("config.toml");
    if let Some(text) = config_text {
        std::fs::write(&config, text).unwrap();
    }
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
    // Enrichissement initial (LLM simulé).
    timed(&f, &["build"]);
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

/// Empreintes de l'export JSON normalisé des gros dépôts (l'ordre de `unresolved` n'est pas garanti :
/// DEC-081), sur le code de la 0.20.0 (AC-065-2) :
/// `(dépôt, étape, empreinte BLAKE3)`. Relever de nouvelles valeurs avec `NEPHILA_ORACLE_CAPTURE=1`.
const ORACLE_GROS_DEPOTS: &[(&str, &str, &str)] = &[
    (
        "gen_repo",
        "build",
        "acc20901424008f094fc1e5bdf620270fec208d2d0aa7dd6e7b468d2b553d31e",
    ),
    (
        "gen_repo",
        "update",
        "c55d9fc03eff50a2fecc1a558731bdfd6222ce6a2ee2416ca712f711864e8ece",
    ),
    (
        "gen_js",
        "build",
        "8fb0d18e27eb481f8409862193c22b260f123ce23b644e8fbc01db0447dced68",
    ),
    (
        "gen_js",
        "update",
        "66a74b71c922190eb21a03358399619f75520e2e09de116c969c26b52fa1bdce",
    ),
];

fn export_hash(f: &Fixture) -> String {
    let (_, out) = timed(f, &["export", "--format", "json", "--output", "-"]);
    support::export_fingerprint(&out.stdout)
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

/// AC-074-4 : `build --no-llm --verbose` avec `--jobs 1` puis sans `--jobs` : durées des lignes
/// `parcours:` et `structure: fin` affichées, exports égaux entre eux et à l'oracle de la 0.20.0.
#[test]
#[ignore]
fn perf_req074_ac4_parcours_et_structure() {
    let _g = serial();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["gen_repo", "gen_js"] {
        let root = base.join(format!("target/perf-req074-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let mock = root.join("mock");
        if name == "gen_repo" {
            gen_repo::generate(&repo, &mock);
        } else {
            gen_js::generate(&repo, &mock);
        }
        let config = root.join("config.toml");
        std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let f = Fixture { repo, config, mock };
        let result = std::panic::catch_unwind(|| {
            let mut hashes = Vec::new();
            for args in [
                &["build", "--no-llm", "--verbose", "--jobs", "1"][..],
                &["build", "--no-llm", "--verbose"][..],
            ] {
                let _ = std::fs::remove_dir_all(f.repo.join(".nephila"));
                let (d, out) = timed(&f, args);
                let err = String::from_utf8_lossy(&out.stderr);
                let lines: Vec<&str> = err
                    .lines()
                    .filter(|l| l.contains("parcours:") || l.contains("structure: fin"))
                    .collect();
                assert_eq!(lines.len(), 2, "{name} {args:?} : {err}");
                eprintln!("{name} {args:?} : total {d:?}");
                for l in lines {
                    eprintln!("  {l}");
                }
                hashes.push(export_hash(&f));
            }
            assert_eq!(hashes[0], hashes[1], "{name} : exports différents");
            let oracle = ORACLE_GROS_DEPOTS
                .iter()
                .find(|(n, s, _)| *n == name && *s == "build")
                .unwrap();
            assert_eq!(hashes[0], oracle.2, "{name} : écart d'oracle");
        });
        let _ = std::fs::remove_dir_all(&root);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}

/// Mesures de la 0.20.0 (AC-063-5), figées sur HEAD : place utilisée U₀ et Σ `length(context)` C₀.
/// U varie de ~0,3 % d'un lancement à l'autre (ordre d'écriture des fils) : on fige le minimum des mesures relevées sur HEAD (six lancements).
const U0_JS: i64 = 162_426_880;
const C0_JS: i64 = 84_904_838;
const U0_JVM: i64 = 153_509_888;
const C0_JVM: i64 = 3_199_600;

/// Place utilisée `(page_count − freelist_count) × page_size` et Σ `length(context)` de `file_contexts` (schéma 7).
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
            "SELECT coalesce(sum(length(context)), 0) FROM file_contexts",
            [],
            |r| r.get(0),
        )
        .unwrap();
    (used, ctx)
}

/// AC-063-5 : U ≤ U₀ − C₀ / 2 et U < U₀ (U₀ et C₀ mesurés sur la 0.20.0).
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
        assert!(c > 0, "{name} : aucun contexte de fichier");
        assert!(u < u0, "{name} : U = {u} n'est pas < U0 = {u0}");
        assert!(
            u <= u0 - c0 / 2,
            "{name} : U = {u} > U0 - C0 / 2 = {}",
            u0 - c0 / 2
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Empreinte de l'export normalisé (DEC-080 : l'ordre des non résolues n'est pas comparé).
fn normalized_hash(f: &Fixture) -> String {
    let (_, out) = timed(f, &["export", "--format", "json", "--output", "-"]);
    support::export_fingerprint(&out.stdout)
}

/// Durées (ms) des lignes `résolution:` de la sortie d'erreur d'une commande `--verbose`.
fn resolution_ms(out: &Output) -> Vec<u64> {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|l| l.split("résolution: ").nth(1))
        .filter_map(|r| r.split(" ms").next()?.trim().parse().ok())
        .collect()
}

/// AC-066-6 : résolution après un fichier modifié (`build` puis `update`) : export identique à
/// celui d'un `build` neuf, durée de la résolution de l'`update` au plus le quart de celle du `build`.
#[test]
#[ignore]
fn perf_req066_ac6_resolution_incrementale() {
    let _g = serial();
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["gen_js", "gen_repo"] {
        let root = base.join(format!("target/perf-incremental-{name}"));
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
        backdate(&repo);
        let config = root.join("config.toml");
        std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let f = Fixture { repo, config, mock };
        let (_, built) = timed(&f, &["build", "--no-llm", "--verbose"]);
        let build_ms = resolution_ms(&built);
        assert_eq!(build_ms.len(), 1, "{name} : lignes résolution du build");
        let path = f.repo.join(edited);
        let text = std::fs::read_to_string(&path).unwrap();
        let extra = if name == "gen_repo" {
            "\n    // méthode ajoutée\n"
        } else {
            "\nexport function incrementalExtra(): number {\n  return 1;\n}\n"
        };
        let text = if name == "gen_repo" {
            let i = text.rfind('}').expect("accolade finale");
            format!(
                "{}    public int incrementalExtra() {{\n        return 1;\n    }}\n{}",
                &text[..i],
                &text[i..]
            )
        } else {
            format!("{text}{extra}")
        };
        std::fs::write(&path, text).unwrap();
        let (_, updated) = timed(&f, &["update", "--no-llm", "--verbose"]);
        let update_ms = resolution_ms(&updated);
        assert_eq!(update_ms.len(), 1, "{name} : lignes résolution de l'update");
        eprintln!(
            "MESURE {name} : résolution build = {} ms, update = {} ms",
            build_ms[0], update_ms[0]
        );
        assert!(
            update_ms[0] * 4 <= build_ms[0],
            "{name} : résolution de l'update ({} ms) > le quart de celle du build ({} ms)",
            update_ms[0],
            build_ms[0]
        );
        let inc = normalized_hash(&f);
        let copy = root.join("fresh");
        copy_tree(&f.repo, &copy);
        let g = Fixture {
            repo: copy,
            config: f.config.clone(),
            mock: f.mock.clone(),
        };
        timed(&g, &["build", "--no-llm"]);
        assert_eq!(
            inc,
            normalized_hash(&g),
            "{name} : export normalisé de l'update != build"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Copie de l'arbre du dépôt sans `.nephila/`.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.file_name() == ".nephila" {
            continue;
        }
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Empreinte BLAKE3 de l'arbre : chemins triés (relatifs, `/`) et contenus, avec longueurs.
fn tree_fingerprint(dir: &std::path::Path) -> String {
    fn files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if std::fs::symlink_metadata(&p).unwrap().file_type().is_dir() {
                files(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut all = Vec::new();
    files(dir, &mut all);
    let mut rel: Vec<String> = all
        .iter()
        .map(|p| {
            p.strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    rel.sort();
    let mut h = blake3::Hasher::new();
    for r in &rel {
        let body = std::fs::read(dir.join(r)).unwrap();
        h.update(&(r.len() as u64).to_le_bytes());
        h.update(r.as_bytes());
        h.update(&(body.len() as u64).to_le_bytes());
        h.update(&body);
    }
    h.finalize().to_hex().to_string()
}

/// Mesures d'un lancement du banc, en millisecondes.
struct Sample {
    parcours: u64,
    structure: u64,
    resolution: u64,
    autres: u64,
    total: u64,
    files: u64,
    jobs: u64,
}

/// Premier nombre entier qui suit `marker` dans la première ligne de `err` qui le contient.
fn number_after(err: &str, line_key: &str, marker: &str) -> Option<u64> {
    let line = err.lines().find(|l| l.contains(line_key))?;
    let rest = line.split(marker).nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// `[<s>.<mmm>s]` de la ligne `parcours:` en millisecondes.
fn parcours_ms(err: &str) -> Option<u64> {
    let line = err.lines().find(|l| l.contains("parcours:"))?;
    let t = line.split_once('[')?.1.split_once("s]")?.0;
    let (s, ms) = t.split_once('.')?;
    Some(s.parse::<u64>().ok()? * 1000 + ms.parse::<u64>().ok()?)
}

fn bench_run(f: &Fixture, cmd_name: &str) -> Sample {
    let t = Instant::now();
    let out = cmd(f, &[cmd_name, "--no-llm", "--verbose"])
        .output()
        .unwrap();
    let total = t.elapsed().as_millis() as u64;
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{cmd_name} : {err}");
    let need = |v: Option<u64>, what: &str| {
        v.unwrap_or_else(|| {
            panic!("{cmd_name} : ligne `{what}` absente ou illisible, stderr :\n{err}")
        })
    };
    let parcours = need(parcours_ms(&err), "parcours:");
    let structure = need(
        number_after(&err, "structure: fin", "structure: fin "),
        "structure: fin",
    );
    let resolution = need(
        number_after(&err, "résolution:", "résolution: "),
        "résolution:",
    );
    let files = need(
        number_after(&err, "structure: début", "structure: début "),
        "structure: début",
    );
    let jobs = need(
        number_after(&err, "structure: début", " fichiers, "),
        "structure: début",
    );
    let autres = total.saturating_sub(parcours + structure + resolution);
    Sample {
        parcours,
        structure,
        resolution,
        autres,
        total,
        files,
        jobs,
    }
}

fn median(v: &mut [u64]) -> u64 {
    v.sort_unstable();
    v[v.len() / 2]
}

fn bench_line(label: &str, runs: &[Sample]) -> String {
    let m = |g: fn(&Sample) -> u64| median(&mut runs.iter().map(g).collect::<Vec<_>>());
    format!(
        "banc: {label} --no-llm : parcours {} ms, structure {} ms, résolution {} ms, autres {} ms, total {} ms (médiane de 3, {} fichiers, {} fils)",
        m(|s| s.parcours),
        m(|s| s.structure),
        m(|s| s.resolution),
        m(|s| s.autres),
        m(|s| s.total),
        runs[0].files,
        runs[0].jobs
    )
}

/// REQ-082 : banc de `build --no-llm` et `update --no-llm` sur `target/perf-fixture-js`, par phase.
/// Commande `banc` du plan (AC-082-1) ; aucun seuil, sauf l'exactitude du graphe (AC-082-2).
#[test]
#[ignore]
fn perf_banc_js() {
    let _g = serial();
    let f = js_fixture_with(None);
    assert!(!f.config.exists());
    eprintln!(
        "banc: fixture {}",
        tree_fingerprint(f.repo.parent().unwrap())
    );
    let nephila = f.repo.join(".nephila");
    let mut builds = Vec::new();
    for _ in 0..3 {
        let _ = std::fs::remove_dir_all(&nephila);
        builds.push(bench_run(&f, "build"));
    }
    let oracle = ORACLE_GROS_DEPOTS
        .iter()
        .find(|(n, s, _)| *n == "gen_js" && *s == "build")
        .unwrap();
    assert_eq!(
        export_hash(&f),
        oracle.2,
        "AC-082-2 : écart d'oracle après build"
    );
    let edited = f.repo.join(gen_js::path(gen_js::EDITED));
    let original = std::fs::read_to_string(&edited).unwrap();
    let mut updates = Vec::new();
    for k in 0..3u64 {
        std::fs::write(&edited, format!("{original}\n// banc {k}\n")).unwrap();
        let t = std::time::SystemTime::now() - Duration::from_secs(8) + Duration::from_secs(k);
        std::fs::File::options()
            .write(true)
            .open(&edited)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
            .unwrap();
        updates.push(bench_run(&f, "update"));
    }
    eprintln!("{}", bench_line("build", &builds));
    eprintln!("{}", bench_line("update", &updates));
    let after_update = export_hash(&f);
    let _ = std::fs::remove_dir_all(&nephila);
    timed(&f, &["build", "--no-llm"]);
    assert_eq!(
        after_update,
        export_hash(&f),
        "AC-082-2 : update différent d'un build neuf"
    );
}

/// Pic de mémoire résidente (Kio) du seul processus `export --format json` de la 0.23.2, mesuré
/// sur HEAD avant le flux (plus petite de 3 mesures) : AC-080-3.
const EXPORT_RSS0_KIB: i64 = 665_432;

/// Lance `cmd` et rend son statut et le `ru_maxrss` (Kio, Linux) du seul processus fils.
#[cfg(target_os = "linux")]
#[allow(clippy::zombie_processes)] // attendu par `libc::wait4` ci-dessous (ru_maxrss)
fn run_maxrss(mut cmd: Command) -> (bool, i64) {
    use std::process::Stdio;
    let child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id() as libc::pid_t;
    let mut status: libc::c_int = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
    assert_eq!(r, pid, "wait4 a échoué");
    (
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        usage.ru_maxrss,
    )
}

/// AC-080-3 : pic de mémoire de `export --format json --output <fichier>` ≤ 50 % de celui de la
/// 0.23.2, export normalisé égal à l'oracle des gros dépôts.
#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn perf_req080_ac3_export_memoire() {
    let _g = serial();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-export-mem");
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    let mock = root.join("mock");
    gen_repo::generate(&repo, &mock);
    let config = root.join("config.toml");
    std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let f = Fixture { repo, config, mock };
    timed(&f, &["build", "--no-llm"]);
    let out = root.join("export-mem.json");
    let mut rss = Vec::new();
    for _ in 0..3 {
        let _ = std::fs::remove_file(&out);
        let (ok, kib) = run_maxrss(cmd(
            &f,
            &[
                "export",
                "--format",
                "json",
                "--output",
                out.to_str().unwrap(),
            ],
        ));
        assert!(ok, "export en échec");
        rss.push(kib);
    }
    let best = *rss.iter().min().unwrap();
    eprintln!("export RSS (Kio) : {rss:?}, plus petite {best}");
    let raw = std::fs::read(&out).unwrap();
    let oracle = ORACLE_GROS_DEPOTS
        .iter()
        .find(|(n, s, _)| *n == "gen_repo" && *s == "build")
        .unwrap();
    assert_eq!(
        support::export_fingerprint(&raw),
        oracle.2,
        "écart d'oracle"
    );
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        best * 2 <= EXPORT_RSS0_KIB,
        "RSS {best} Kio > 50 % de {EXPORT_RSS0_KIB} Kio"
    );
}

/// AC-109-2 (`big.md` : `.txt` n'est pas un type analysé) : `query search X --check-stale`, dépôt qui gagne un fichier valide de 256 Mio :
/// pic de mémoire du seul processus < 64 Mio, fichier compté comme ajouté ; avec un octet invalide
/// final, fichier non compté.
#[test]
#[ignore]
#[cfg(target_os = "linux")]
fn perf_req109_ac2_check_stale_memoire() {
    use std::io::Write;
    let _g = serial();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/perf-check-stale-mem");
    let _ = std::fs::remove_dir_all(&root);
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("A.java"), "class A {}\n").unwrap();
    let config = root.join("config.toml");
    std::fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let f = Fixture {
        repo: repo.clone(),
        config,
        mock: root.join("mock"),
    };
    std::fs::create_dir_all(&f.mock).unwrap();
    backdate(&repo);
    assert!(timed(&f, &["build", "--no-llm"]).1.status.success());
    let big = repo.join("big.md");
    // Supprime le gros fichier et le dossier de travail même si un assert échoue.
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let write_big = |tail: &[u8]| {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&big).unwrap());
        let line = [b'a'; 1023];
        for _ in 0..(256 * 1024 - 1) {
            w.write_all(&line).unwrap();
            w.write_all(b"\n").unwrap();
        }
        w.write_all(&line[..1023 - tail.len()]).unwrap();
        w.write_all(tail).unwrap();
        w.flush().unwrap();
    };
    let measure = |label: &str| -> (String, i64) {
        let out = root.join("stderr.txt");
        let mut c = cmd(&f, &["query", "search", "A", "--check-stale"]);
        c.stderr(std::fs::File::create(&out).unwrap());
        use std::process::Stdio;
        #[allow(clippy::zombie_processes)] // attendu par `libc::wait4` ci-dessous
        let child = c.stdout(Stdio::null()).spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        let mut status: libc::c_int = 0;
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
        assert_eq!(r, pid, "wait4 a échoué");
        let err = std::fs::read_to_string(&out).unwrap();
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "{label} : sortie inattendue (statut {status}) ; stderr : {err}"
        );
        eprintln!("{label} : RSS {} Kio", usage.ru_maxrss);
        (err, usage.ru_maxrss)
    };
    write_big(b"a");
    let (err, kib) = measure("valide");
    let _ = std::fs::remove_file(&big);
    write_big(&[0xFF]);
    let (err_bad, kib_bad) = measure("octet invalide final");
    assert!(
        err.contains("pas à jour"),
        "fichier ajouté non compté : {err}"
    );
    assert!(
        !err_bad.contains("pas à jour"),
        "fichier invalide compté : {err_bad}"
    );
    assert!(kib < 64 * 1024, "RSS {kib} Kio >= 64 Mio (valide)");
    assert!(
        kib_bad < 64 * 1024,
        "RSS {kib_bad} Kio >= 64 Mio (invalide)"
    );
}
