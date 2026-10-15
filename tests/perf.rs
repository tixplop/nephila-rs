//! Performances sur 10 000 fichiers (REQ-021). Tests `#[ignore]` à lancer en release :
//! `cargo test --release -q --color never -- --ignored perf`.

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
    let mut c = Command::new(cargo_bin("rkg"));
    c.arg("-C")
        .arg(&f.repo)
        .args(args)
        .env("RKG_CONFIG", &f.config)
        .env("RKG_MOCK_DIR", &f.mock)
        .env_remove("RKG_MOCK_LOG")
        .env_remove("RKG_MOCK_FAIL_AFTER")
        .env_remove("RKG_MOCK_DELAY_MS")
        .env_remove("RKG_MOCK_INVALID_KEY");
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
    let (d, out) = best(f, &["query", "search", &a]);
    assert_under("query search", d, 200);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&a));
    let (d, out) = best(f, &["query", "neighbors", &id_a, "--depth", "1"]);
    assert_under("query neighbors", d, 200);
    assert!(String::from_utf8_lossy(&out.stdout).contains("utilisé par"));
    let (d, out) = best(f, &["query", "path", &id_a, &id_b]);
    assert_under("query path", d, 200);
    assert!(String::from_utf8_lossy(&out.stdout).contains(&b));
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
            .env("RKG_MOCK_LOG", &log)
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
    let bin = cargo_bin("rkg");
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
