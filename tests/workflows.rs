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
    for (file, n) in [("ci.yml", 9), ("release.yml", 7)] {
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
