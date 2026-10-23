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
