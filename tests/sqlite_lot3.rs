//! Oracle de la 0.20.0 pour le lot 3 SQLite (REQ-065, AC-065-1) : pour chaque dépôt de
//! `tests/fixtures` et chaque étape (`build --no-llm`, `build` avec le mock, `update` après
//! modification, ajout et suppression, second `build`), empreinte BLAKE3 de l'export JSON normalisé
//! (l'ordre de `unresolved` n'est pas garanti : DEC-081), de `status` et de `summary` bruts, avec le
//! code de sortie.
//!
//! `NEPHILA_ORACLE_CAPTURE=1` réécrit `tests/fixtures/oracle_0_20_0.txt` (une ligne
//! `dépôt<TAB>étape<TAB>sortie<TAB>code<TAB>empreinte`, triée) ; sans, le test compare.

#[path = "support/mod.rs"]
mod support;

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{copy_dir, run, Case, CASES};

fn fingerprints(
    lines: &mut Vec<String>,
    case: &str,
    step: &str,
    repo: &Path,
    config: &Path,
    mock: Option<&Path>,
) {
    let outputs: [(&str, &[&str]); 3] = [
        ("export", &["export", "--format", "json", "--output", "-"]),
        ("status", &["status"]),
        ("summary", &["summary"]),
    ];
    for (label, args) in outputs {
        let out = run(repo, config, mock, args);
        let code = out.status.code().map_or(-1, i64::from);
        let hash = if label == "export" {
            support::export_fingerprint(&out.stdout)
        } else if label == "summary" {
            // L'oracle de la 0.20.0 précède la section des communautés (T-232, AC-148-4).
            let full = String::from_utf8_lossy(&out.stdout).into_owned();
            let at = full.find("Communautés (").expect("section des communautés");
            blake3::hash(&full.as_bytes()[..at]).to_hex().to_string()
        } else {
            blake3::hash(&out.stdout).to_hex().to_string()
        };
        lines.push(format!("{case}\t{step}\t{label}\t{code}\t{hash}"));
    }
}

fn scenario(c: &Case) -> Vec<String> {
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    copy_dir(&fx.join(c.repo), &repo);
    let mock = c.mock.map(|m| {
        let d = tmp.path().join("mock");
        copy_dir(&fx.join(m), &d);
        d
    });
    let mock = mock.as_deref();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\n",
    )
    .unwrap();
    let mut lines = Vec::new();
    let ok = |step: &str, args: &[&str]| {
        let out = run(&repo, &config, mock, args);
        assert!(
            out.status.success(),
            "{} / {step} : {}",
            c.name,
            String::from_utf8_lossy(&out.stderr)
        );
    };

    ok("build --no-llm", &["build", "--no-llm"]);
    fingerprints(&mut lines, c.name, "1-build-no-llm", &repo, &config, mock);
    ok("build", &["build"]);
    fingerprints(&mut lines, c.name, "2-build-mock", &repo, &config, mock);
    if mock.is_some() {
        let export = |l: &str| l.starts_with(&format!("{}\t", c.name)) && l.contains("\texport\t");
        let hash = |step: &str| {
            lines
                .iter()
                .find(|l| export(l) && l.contains(&format!("\t{step}\t")))
                .and_then(|l| l.rsplit('\t').next())
                .map(str::to_string)
        };
        assert_ne!(
            hash("1-build-no-llm"),
            hash("2-build-mock"),
            "{} : le mock n'enrichit pas le dépôt (enrich_code ?)",
            c.name
        );
    }

    support::apply_case(&repo, c);
    ok("update", &["update"]);
    fingerprints(&mut lines, c.name, "3-update", &repo, &config, mock);
    ok("build", &["build"]);
    fingerprints(&mut lines, c.name, "4-build-final", &repo, &config, mock);
    lines
}

#[test]
fn req065_ac1_oracle_0_20_0() {
    let mut lines: Vec<String> = CASES.iter().flat_map(scenario).collect();
    lines.sort();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle_0_20_0.txt");
    let actual = lines.join("\n") + "\n";
    if std::env::var_os("NEPHILA_ORACLE_CAPTURE").is_some_and(|v| v == "1") {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).expect("oracle absent : NEPHILA_ORACLE_CAPTURE=1");
    let exp: Vec<&str> = expected.lines().collect();
    for i in 0..lines.len().max(exp.len()) {
        let (a, e) = (lines.get(i).map(String::as_str), exp.get(i).copied());
        assert_eq!(
            a,
            e,
            "première ligne différente de l'oracle (ligne {})",
            i + 1
        );
    }
}

/// Sorties observables d'un dépôt : export normalisé (empreinte), `status`, `summary`.
fn observed(repo: &Path, config: &Path, mock: Option<&Path>) -> Vec<(&'static str, String)> {
    let outputs: [(&'static str, &[&str]); 3] = [
        ("export", &["export", "--format", "json", "--output", "-"]),
        ("status", &["status"]),
        ("summary", &["summary"]),
    ];
    outputs
        .iter()
        .map(|(label, args)| {
            let out = run(repo, config, mock, args);
            let text = if *label == "export" {
                support::export_fingerprint(&out.stdout)
            } else {
                String::from_utf8_lossy(&out.stdout).into_owned()
            };
            (*label, text)
        })
        .collect()
}

/// Lance `args` (avec `--jobs n` si demandé) ; renvoie la ligne de bilan (stdout) et les sorties observées.
fn step(
    c: &Case,
    repo: &Path,
    config: &Path,
    mock: Option<&Path>,
    args: &[&str],
    jobs: Option<usize>,
) -> (String, Vec<(&'static str, String)>) {
    let n = jobs.map(|j| j.to_string());
    let mut full: Vec<&str> = args.to_vec();
    if let Some(n) = &n {
        full.extend(["--jobs", n]);
    }
    let out = run(repo, config, mock, &full);
    assert!(
        out.status.success(),
        "{} / {full:?} : {}",
        c.name,
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        observed(repo, config, mock),
    )
}

fn compare(
    c: &Case,
    option: &str,
    expected: &(String, Vec<(&'static str, String)>),
    actual: &(String, Vec<(&'static str, String)>),
) {
    assert_eq!(
        expected.0, actual.0,
        "{} / {option} : ligne de bilan différente",
        c.name
    );
    for (e, a) in expected.1.iter().zip(&actual.1) {
        assert_eq!(
            e.1, a.1,
            "{} / {option} : première section différente : {}",
            c.name, e.0
        );
    }
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    repo: std::path::PathBuf,
    mock: Option<std::path::PathBuf>,
    config: std::path::PathBuf,
}

fn sandbox(c: &Case) -> Sandbox {
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    copy_dir(&fx.join(c.repo), &repo);
    let mock = c.mock.map(|m| {
        let d = tmp.path().join("mock");
        copy_dir(&fx.join(m), &d);
        d
    });
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\nconcurrency = 1\n",
    )
    .unwrap();
    Sandbox {
        _tmp: tmp,
        repo,
        mock,
        config,
    }
}

/// Bilan et sorties d'un `build` de l'arbre (modifié ou non) avec `--jobs jobs`.
fn built(
    c: &Case,
    args: &[&str],
    jobs: Option<usize>,
    modified: bool,
) -> (String, Vec<(&'static str, String)>) {
    let sb = sandbox(c);
    if modified {
        support::apply_case(&sb.repo, c);
    }
    step(c, &sb.repo, &sb.config, sb.mock.as_deref(), args, jobs)
}

/// AC-075-2 : `build --no-llm` donne le même graphe quel que soit `--jobs`.
#[test]
fn req075_ac2_build_no_llm_meme_graphe_quel_que_soit_jobs() {
    for c in CASES {
        let reference = built(c, &["build", "--no-llm"], None, false);
        for j in [1, 2, 8] {
            let actual = built(c, &["build", "--no-llm"], Some(j), false);
            compare(
                c,
                &format!("build --no-llm --jobs {j}"),
                &reference,
                &actual,
            );
        }
    }
}

/// AC-075-2 : `build` avec le mock (`concurrency = 1`) donne le même graphe quel que soit `--jobs`.
#[test]
fn req075_ac2_build_mock_meme_graphe_quel_que_soit_jobs() {
    for c in CASES {
        let reference = built(c, &["build"], None, false);
        for j in [1, 8] {
            let actual = built(c, &["build"], Some(j), false);
            compare(c, &format!("build --jobs {j}"), &reference, &actual);
        }
    }
}

/// AC-075-2 : l'`update` du scénario de REQ-065 avec `--jobs 1` et `--jobs 8` donne les sorties d'un
/// `build` du même arbre ; les deux `update` ont la même ligne de bilan.
#[test]
fn req075_ac2_update_meme_graphe_quel_que_soit_jobs() {
    for c in CASES {
        let reference = built(c, &["build"], None, true);
        let mut bilans = Vec::new();
        for j in [1, 8] {
            let sb = sandbox(c);
            let m = sb.mock.as_deref();
            step(c, &sb.repo, &sb.config, m, &["build"], Some(j));
            support::apply_case(&sb.repo, c);
            let actual = step(c, &sb.repo, &sb.config, m, &["update"], Some(j));
            compare(
                c,
                &format!("update --jobs {j}"),
                &(actual.0.clone(), reference.1.clone()),
                &actual,
            );
            bilans.push(actual.0);
        }
        assert_eq!(
            bilans[0], bilans[1],
            "{} / update : ligne de bilan différente entre --jobs 1 et --jobs 8",
            c.name
        );
    }
}

/// AC-060-5 : un `update` tué pendant l'enrichissement garde les fichiers déjà enregistrés.
#[test]
fn req060_ac5_killed_update_keeps_enriched_files() {
    use nephila::store::Store;
    use std::collections::BTreeSet;

    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let mock = tmp.path().join("mock");
    copy_dir(&fx.join("maven2"), &repo);
    copy_dir(&fx.join("mock"), &mock);
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\nbatch_files = 1\nconcurrency = 1\n",
    )
    .unwrap();
    let log = tmp.path().join("mock.log");
    let out = run(&repo, &config, Some(&mock), &["build", "--no-llm"]);
    assert!(out.status.success());

    let mut child = Command::new(cargo_bin("nephila"))
        .env_remove("NEPHILA_MOCK_FAIL_AFTER")
        .env("NEPHILA_MOCK_DELAY_MS", "300")
        .env("NEPHILA_CONFIG", &config)
        .env("NEPHILA_MOCK_DIR", &mock)
        .env("NEPHILA_MOCK_LOG", &log)
        .arg("-C")
        .arg(&repo)
        .arg("update")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    loop {
        assert!(
            start.elapsed().as_secs() < 30,
            "aucun enrichissement enregistré"
        );
        if let Ok(store) = Store::open_readonly(&repo) {
            let files = store.files().unwrap_or_default();
            if files
                .iter()
                .any(|f| store.enrichment_entry(&f.path).ok().flatten().is_some())
            {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    let st = run(&repo, &config, Some(&mock), &["status"]);
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    let store = Store::open_readonly(&repo).unwrap();
    let files: Vec<String> = store.files().unwrap().into_iter().map(|f| f.path).collect();
    let enriched: BTreeSet<String> = files
        .iter()
        .filter(|f| store.enrichment_entry(f).unwrap().is_some())
        .cloned()
        .collect();
    drop(store);
    assert!(!enriched.is_empty() && enriched.len() < files.len());

    let _ = fs::remove_file(&log);
    let out = Command::new(cargo_bin("nephila"))
        .env_remove("NEPHILA_MOCK_FAIL_AFTER")
        .env_remove("NEPHILA_MOCK_DELAY_MS")
        .env("NEPHILA_CONFIG", &config)
        .env("NEPHILA_MOCK_DIR", &mock)
        .env("NEPHILA_MOCK_LOG", &log)
        .arg("-C")
        .arg(&repo)
        .arg("update")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sent: BTreeSet<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('\t').next().unwrap().to_string())
        .collect();
    assert!(sent.is_disjoint(&enriched), "{sent:?} / {enriched:?}");
    assert_eq!(sent.len() + enriched.len(), files.len());
}
