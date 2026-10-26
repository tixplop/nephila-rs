//! REQ-045 : actions des workflows épinglées par SHA de commit.
#![allow(non_snake_case)]

use std::collections::{BTreeMap, BTreeSet};

const FILES: [&str; 2] = ["ci.yml", "release.yml"];

struct Use {
    file: &'static str,
    line: usize,
    text: String,
}

fn read(file: &str) -> String {
    let p = format!("{}/.github/workflows/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("lecture de {p} : {e}"))
}

/// Lignes `uses: ` (hors `./`), nettoyées de l'indentation et du `- ` éventuel.
fn uses(file: &'static str) -> Vec<Use> {
    read(file)
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            let t = l.trim_start();
            let t = t.strip_prefix("- ").unwrap_or(t).trim_start();
            (t.starts_with("uses: ") && !t.starts_with("uses: ./")).then(|| Use {
                file,
                line: i + 1,
                text: t.to_string(),
            })
        })
        .collect()
}

/// Découpe `uses: a/b@sha # c` ; `None` si la forme n'est pas exacte.
fn parse(text: &str) -> Option<(String, String, String)> {
    let rest = text.strip_prefix("uses: ")?;
    let (target, comment) = rest.split_once(" # ")?;
    let (action, sha) = target.split_once('@')?;
    let (owner, repo) = action.split_once('/')?;
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    if !name_ok(owner) || !name_ok(repo) {
        return None;
    }
    if sha.len() != 40 || !sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
        return None;
    }
    let version_ok = comment == "stable" && action == "dtolnay/rust-toolchain"
        || action != "dtolnay/rust-toolchain" && {
            let v = comment.strip_prefix('v').unwrap_or("");
            let parts: Vec<&str> = v.split('.').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        };
    version_ok.then(|| (action.to_string(), sha.to_string(), comment.to_string()))
}

fn all() -> Vec<Use> {
    FILES.iter().flat_map(|f| uses(f)).collect()
}

#[test]
fn REQ045_AC1_uses_epingles_par_sha() {
    let bad: Vec<String> = all()
        .iter()
        .filter(|u| parse(&u.text).is_none())
        .map(|u| format!("{}:{} : {}", u.file, u.line, u.text))
        .collect();
    assert!(bad.is_empty(), "lignes non épinglées :\n{}", bad.join("\n"));
}

#[test]
fn REQ045_AC2_nombre_et_ensemble_des_actions() {
    for (file, n) in [("ci.yml", 18), ("release.yml", 7)] {
        assert_eq!(uses(file).len(), n, "{file} : nombre de lignes uses:");
    }
    let locals = read("release.yml")
        .lines()
        .filter(|l| {
            l.trim_start()
                .trim_start_matches("- ")
                .starts_with("uses: ./")
        })
        .count();
    assert_eq!(locals, 1);
    assert!(read("release.yml")
        .lines()
        .any(|l| l == "    uses: ./.github/workflows/ci.yml"));
    assert!(!read("ci.yml").contains("uses: ./"));
    let set: BTreeSet<String> = all()
        .iter()
        .filter_map(|u| parse(&u.text).map(|p| p.0))
        .collect();
    let want: BTreeSet<String> = [
        "actions/checkout",
        "actions/setup-node",
        "actions/upload-artifact",
        "actions/download-artifact",
        "softprops/action-gh-release",
        "Swatinem/rust-cache",
        "taiki-e/install-action",
        "dtolnay/rust-toolchain",
        "EmbarkStudios/cargo-deny-action",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(set, want);
}

#[test]
fn REQ045_AC3_meme_sha_par_action() {
    let mut seen: BTreeMap<String, (String, String)> = BTreeMap::new();
    for u in all() {
        let (a, sha, c) =
            parse(&u.text).unwrap_or_else(|| panic!("{}:{} illisible", u.file, u.line));
        let e = seen.entry(a.clone()).or_insert((sha.clone(), c.clone()));
        assert_eq!(*e, (sha, c), "{}:{} : {a} incohérente", u.file, u.line);
    }
}

/// Étape d'un job, lue par l'analyse YAML (aide partagée avec les tâches suivantes).
#[allow(dead_code)]
struct Step {
    job: String,
    uses: Option<String>,
    with: BTreeMap<String, String>,
    run: Option<String>,
    save_if_is_bool: bool,
}

fn scalar(y: &yaml_rust2::Yaml) -> String {
    use yaml_rust2::Yaml;
    match y {
        Yaml::String(s) | Yaml::Real(s) => s.clone(),
        Yaml::Integer(i) => i.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        other => panic!("scalaire attendu, trouvé {other:?}"),
    }
}

/// Étapes de chaque job de `file`, dans l'ordre du fichier.
fn steps(file: &str) -> Vec<Step> {
    use yaml_rust2::{Yaml, YamlLoader};
    let docs = YamlLoader::load_from_str(&read(file)).unwrap_or_else(|e| panic!("{file} : {e}"));
    let jobs = docs[0]["jobs"]
        .as_hash()
        .unwrap_or_else(|| panic!("{file} : pas de jobs"));
    let mut out = Vec::new();
    for (name, job) in jobs {
        let Some(list) = job["steps"].as_vec() else {
            continue;
        };
        for s in list {
            let with = match &s["with"] {
                Yaml::Hash(h) => h.iter().map(|(k, v)| (scalar(k), scalar(v))).collect(),
                _ => BTreeMap::new(),
            };
            out.push(Step {
                job: scalar(name),
                uses: s["uses"].as_str().map(str::to_string),
                with,
                run: s["run"].as_str().map(str::to_string),
                save_if_is_bool: matches!(s["with"]["save-if"], Yaml::Boolean(_)),
            });
        }
    }
    out
}

#[test]
fn REQ123_AC2_toolchain_des_workflows_egal_au_channel() {
    let text = std::fs::read_to_string(format!(
        "{}/rust-toolchain.toml",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("rust-toolchain.toml");
    let table: toml::Table = toml::from_str(&text).unwrap();
    let channel = table["toolchain"]["channel"].as_str().expect("channel");
    let mut vues = 0;
    for file in FILES {
        for s in steps(file) {
            let Some(u) = s.uses.as_deref() else { continue };
            if !u.starts_with("dtolnay/rust-toolchain@") {
                continue;
            }
            vues += 1;
            let want = if file == "ci.yml" && s.job == "clippy-beta" {
                "beta"
            } else {
                channel
            };
            assert_eq!(
                s.with.get("toolchain").map(String::as_str),
                Some(want),
                "{file}, job {} : with.toolchain",
                s.job
            );
        }
    }
    assert!(vues >= 3, "étapes dtolnay/rust-toolchain vues : {vues}");
}

#[test]
fn REQ120_AC1_ensemble_des_workflows_egal_a_files() {
    let dir = format!("{}/.github/workflows", env!("CARGO_MANIFEST_DIR"));
    let found: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("lecture de {dir} : {e}"))
        .map(|e| {
            e.expect("entrée")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| n.ends_with(".yml") || n.ends_with(".yaml"))
        .collect();
    let want: BTreeSet<String> = FILES.iter().map(|s| s.to_string()).collect();
    let extra: Vec<&String> = found.difference(&want).collect();
    let missing: Vec<&String> = want.difference(&found).collect();
    assert!(
        extra.is_empty() && missing.is_empty(),
        "workflows en trop : {extra:?} ; manquants : {missing:?}"
    );
}

#[test]
fn REQ121_AC1_build_de_release_ne_sauvegarde_pas_le_cache() {
    let caches: Vec<Step> = steps("release.yml")
        .into_iter()
        .filter(|s| {
            s.uses
                .as_deref()
                .is_some_and(|u| u.starts_with("Swatinem/rust-cache@"))
        })
        .collect();
    assert_eq!(caches.len(), 1, "étapes rust-cache de release.yml");
    let c = &caches[0];
    assert_eq!(c.job, "build");
    assert_eq!(c.with.get("save-if").map(String::as_str), Some("false"));
    assert!(
        c.save_if_is_bool,
        "with.save-if doit être un booléen YAML, pas une chaîne"
    );
    assert_eq!(
        c.with.get("key").map(String::as_str),
        Some("${{ matrix.target }}")
    );
}

fn doc(file: &str) -> yaml_rust2::Yaml {
    let mut d = yaml_rust2::YamlLoader::load_from_str(&read(file))
        .unwrap_or_else(|e| panic!("{file} : {e}"));
    d.remove(0)
}

fn keys(y: &yaml_rust2::Yaml) -> BTreeSet<String> {
    y.as_hash()
        .unwrap_or_else(|| panic!("table attendue : {y:?}"))
        .keys()
        .map(scalar)
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn job_steps(file: &str, job: &str) -> Vec<yaml_rust2::Yaml> {
    doc(file)["jobs"][job]["steps"]
        .as_vec()
        .unwrap_or_else(|| panic!("{file} : job {job} sans étapes"))
        .clone()
}

fn run_of(file: &str, job: &str) -> Vec<String> {
    job_steps(file, job)
        .iter()
        .filter_map(|s| s["run"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn REQ117_AC1_job_deny() {
    let d = doc("ci.yml");
    let job = &d["jobs"]["deny"];
    assert!(job.as_hash().is_some(), "jobs.deny absent");
    assert!(job["if"].is_badvalue(), "jobs.deny ne doit pas avoir de if");
    assert_eq!(job["runs-on"].as_str(), Some("ubuntu-latest"));
    assert!(job["timeout-minutes"].as_i64().is_some());
    let st = job_steps("ci.yml", "deny");
    assert_eq!(st.len(), 2);
    assert!(st[0]["uses"]
        .as_str()
        .unwrap()
        .starts_with("actions/checkout@"));
    assert!(st[1]["uses"]
        .as_str()
        .unwrap()
        .starts_with("EmbarkStudios/cargo-deny-action@"));
    assert_eq!(st[1]["with"]["command"].as_str(), Some("check advisories"));
}

#[test]
fn REQ117_AC2_declencheurs_et_concurrence() {
    let d = doc("ci.yml");
    let on = &d["on"];
    assert_eq!(
        keys(on),
        set(&[
            "push",
            "pull_request",
            "workflow_call",
            "schedule",
            "workflow_dispatch"
        ])
    );
    let br: Vec<&str> = on["push"]["branches"]
        .as_vec()
        .expect("push.branches")
        .iter()
        .filter_map(|b| b.as_str())
        .collect();
    assert_eq!(br, ["master"]);
    let sched = on["schedule"].as_vec().expect("schedule");
    assert_eq!(sched.len(), 1);
    assert_eq!(keys(&sched[0]), set(&["cron"]));
    let g = d["concurrency"]["group"]
        .as_str()
        .expect("concurrency.group");
    assert!(
        g.contains("github.event_name") && g.contains("github.ref"),
        "{g}"
    );

    let r = doc("release.yml");
    assert_eq!(keys(&r["on"]), set(&["push"]));
    let tags: Vec<&str> = r["on"]["push"]["tags"]
        .as_vec()
        .expect("tags")
        .iter()
        .filter_map(|b| b.as_str())
        .collect();
    assert_eq!(tags, ["v*.*.*"]);
}

#[test]
fn REQ118_AC1_job_perf() {
    let d = doc("ci.yml");
    let job = &d["jobs"]["perf"];
    assert_eq!(
        job["if"].as_str(),
        Some("github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'")
    );
    assert!(job["timeout-minutes"].as_i64().is_some());
    let ok = run_of("ci.yml", "perf").iter().any(|r| {
        [
            "cargo nextest run",
            "--release",
            "--locked",
            "--test-threads 1",
            "--run-ignored only",
            "binary_id(nephila-rs::perf)",
        ]
        .iter()
        .all(|f| r.contains(f))
    });
    assert!(ok, "commande perf absente ou incomplète");
    let rel = doc("release.yml");
    assert!(rel["jobs"]
        .as_hash()
        .unwrap()
        .keys()
        .all(|k| !scalar(k).contains("perf")));
    assert!(!read("release.yml").contains("--run-ignored"));
}

#[test]
fn REQ119_AC1_job_clippy_beta() {
    let d = doc("ci.yml");
    assert_eq!(
        d["jobs"]["clippy-beta"]["if"].as_str(),
        Some("github.event_name == 'schedule'")
    );
    let st = job_steps("ci.yml", "clippy-beta");
    let tc = st
        .iter()
        .find(|s| {
            s["uses"]
                .as_str()
                .is_some_and(|u| u.starts_with("dtolnay/rust-toolchain@"))
        })
        .expect("étape dtolnay/rust-toolchain");
    assert_eq!(tc["with"]["toolchain"].as_str(), Some("beta"));
    let ok = run_of("ci.yml", "clippy-beta").iter().any(|r| {
        r.starts_with("cargo +beta clippy")
            && ["--all-targets", "--locked", "-D warnings"]
                .iter()
                .all(|f| r.contains(f))
    });
    assert!(ok, "commande clippy beta absente ou incomplète");
}
