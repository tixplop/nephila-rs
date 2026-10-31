//! Filtrage des résumés et des noms de concepts de bout en bout, avec le simulé
//! (REQ-178, REQ-179, REQ-180 ; avertissements de design.md V-006).

use assert_cmd::Command;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

const SVC: &str = "core/src/main/java/com/acme/core/OrderService.java";
const TYPE_ID: &str = "type:com.acme.core.OrderService";
const METHOD_ID: &str = "method:com.acme.core.OrderService.place";

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
    log: PathBuf,
    mock: PathBuf,
}

impl Env {
    /// Dépôt Maven d'exemple et fixtures du simulé copiés dans un dossier temporaire.
    fn new(batch_files: usize, max_chunk_chars: Option<usize>) -> Env {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        copy_dir(&fixtures.join("maven2"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fixtures.join("mock"), &mock);
        let config = tmp.path().join("config.toml");
        let mut text = format!(
            "provider = \"mock\"\nenrich_code = true\nbatch_files = {batch_files}\nmodel = \"mock\"\n"
        );
        if let Some(m) = max_chunk_chars {
            text.push_str(&format!("max_chunk_chars = {m}\n"));
        }
        fs::write(&config, text).unwrap();
        let log = tmp.path().join("mock.log");
        Env {
            _tmp: tmp,
            repo,
            config,
            log,
            mock,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        c.arg("-C").arg(&self.repo);
        c
    }

    /// Écrit une fixture du simulé (chemin relatif au dossier des fixtures).
    fn fixture(&self, name: &str, summaries: &[(&str, &str)], concepts: Value) {
        let s: Vec<Value> = summaries
            .iter()
            .map(|(n, t)| json!({"node": n, "summary": t}))
            .collect();
        let body = json!({"version": 2, "summaries": s, "concepts": concepts});
        let path = self.mock.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body.to_string()).unwrap();
    }

    fn write_repo(&self, rel: &str, text: &str) {
        let path = self.repo.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn build_ok(&self) -> String {
        let out = self.cmd().arg("build").output().unwrap();
        assert!(out.status.success(), "stderr: {}", stderr(&out));
        stderr(&out)
    }

    fn search(&self, term: &str) -> String {
        let out = self.cmd().args(["query", "search", term]).output().unwrap();
        assert!(out.status.success(), "stderr: {}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Ligne de `query search` du nœud de qualifié donné (une seule attendue).
    fn search_line(&self, term: &str, qualified: &str) -> String {
        let out = self.search(term);
        let prefix = format!("{qualified} (");
        let lines: Vec<&str> = out.lines().filter(|l| l.starts_with(&prefix)).collect();
        assert_eq!(lines.len(), 1, "{out}");
        lines[0].to_string()
    }

    fn export(&self) -> Value {
        let out = self
            .cmd()
            .args(["export", "--output", "-"])
            .output()
            .unwrap();
        assert!(out.status.success(), "stderr: {}", stderr(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn export_node(&self, id: &str) -> Value {
        self.export()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("nœud {id} absent de l'export"))
    }

    fn status(&self) -> String {
        let out = self.cmd().arg("status").output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn warnings(err: &str) -> Vec<&str> {
    err.lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect()
}

fn warnings_for<'a>(err: &'a str, file: &str) -> Vec<&'a str> {
    warnings(err)
        .into_iter()
        .filter(|l| l.contains(file))
        .collect()
}

fn long_summary() -> String {
    (0..90)
        .map(|i| format!("mot{i:02}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn req178_ac6_multiline_summary_folded_and_empty_summary_dropped() {
    let env = Env::new(1, None);
    env.fixture(
        &format!("{SVC}.json"),
        &[
            (
                TYPE_ID,
                "Place les commandes.\n\nElle les enregistre\taussi.",
            ),
            (METHOD_ID, "N/A"),
        ],
        json!([]),
    );
    let err = env.build_ok();
    let folded = "Place les commandes. Elle les enregistre aussi.";
    let line = env.search_line("OrderService", "com.acme.core.OrderService");
    assert!(line.ends_with(&format!(" - {folded}")), "{line}");
    assert_eq!(env.export_node(TYPE_ID)["summary"], folded);
    // Le nœud écarté n'a pas de résumé, ni dans la recherche ni dans l'export.
    let method = env.search_line("OrderService", "com.acme.core.OrderService.place");
    assert!(!method.contains(" - "), "{method}");
    assert!(env
        .export_node(METHOD_ID)
        .get("summary")
        .is_none_or(Value::is_null));
    let w = warnings(&err);
    assert_eq!(w.len(), 1, "{err}");
    assert_eq!(
        w[0],
        format!(
            "avertissement: {SVC} : 1 élément(s) d'enrichissement écarté(s) : résumé pour {METHOD_ID} (vide de sens)"
        )
    );
}

#[test]
fn req178_ac8_cut_summary_and_two_elements_in_one_line() {
    let env = Env::new(1, None);
    let long = long_summary();
    assert_eq!(long.chars().count(), 539);
    env.fixture(
        &format!("{SVC}.json"),
        &[(METHOD_ID, "N/A"), (TYPE_ID, &long)],
        json!([]),
    );
    let err = env.build_ok();
    let w = warnings_for(&err, "OrderService.java");
    assert_eq!(w.len(), 1, "{err}");
    assert_eq!(
        w[0],
        format!(
            "avertissement: {SVC} : 2 élément(s) d'enrichissement écarté(s) : résumé pour {METHOD_ID} (vide de sens) ; résumé pour {TYPE_ID} : coupé à 400 caractères, fin écartée (539 caractères reçus)"
        )
    );
    assert!(
        !err.lines()
            .any(|l| l.contains("mot00") || l.contains("mot89")),
        "{err}"
    );
    let line = env.search_line("OrderService", "com.acme.core.OrderService");
    assert!(line.ends_with('…'), "{line}");
    assert!(!line.contains("mot89"), "{line}");
    let summary = env.export_node(TYPE_ID)["summary"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(summary.chars().count() <= 400 && !summary.contains('\n'));
    assert!(summary.starts_with("mot00 mot01") && summary.ends_with('…'));
    assert!(env.status().contains("graphe à jour"), "{}", env.status());
}

const FIRST: &str = "Premier rôle du service.";
const SECOND: &str = "Second rôle du service.";
const DUP_WARNING: &str = "1 élément(s) d'enrichissement écarté(s) : résumé en double pour type:com.acme.core.OrderService (premier gardé)";

fn assert_first_kept(env: &Env) {
    let line = env.search_line("OrderService", "com.acme.core.OrderService");
    assert!(line.ends_with(&format!(" - {FIRST}")), "{line}");
    assert_eq!(env.export_node(TYPE_ID)["summary"], FIRST);
}

#[test]
fn req179_ac4_ac5_one_call_keeps_first_summary_with_exact_warning() {
    let env = Env::new(1, None);
    env.fixture(
        &format!("{SVC}.json"),
        &[(TYPE_ID, FIRST), (TYPE_ID, SECOND)],
        json!([]),
    );
    let out = env.cmd().arg("build").output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    assert_first_kept(&env);
    let w = warnings_for(&err, "OrderService.java");
    assert_eq!(w.len(), 1, "{err}");
    assert_eq!(w[0], format!("avertissement: {SVC} : {DUP_WARNING}"));
    assert!(!err.contains(SECOND), "{err}");
}

#[test]
fn req179_ac4_ac5_lot_keeps_first_summary_with_exact_warning() {
    let env = Env::new(2, None);
    env.fixture(
        &format!("{SVC}.json"),
        &[(TYPE_ID, FIRST), (TYPE_ID, SECOND)],
        json!([]),
    );
    let out = env.cmd().arg("build").output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let err = stderr(&out);
    // Le fichier a bien été enrichi dans un lot de deux fichiers.
    let lot: Vec<String> = env
        .log_lines()
        .into_iter()
        .filter(|l| l.contains("OrderService.java"))
        .collect();
    assert_eq!(lot.len(), 1, "{lot:?}");
    let files = lot[0].split('\t').next().unwrap();
    let others: Vec<&str> = files.split(';').filter(|f| *f != SVC).collect();
    assert_eq!(others.len(), 1, "{lot:?}");
    assert_first_kept(&env);
    let w = warnings_for(&err, "OrderService.java");
    assert_eq!(w.len(), 1, "{err}");
    assert_eq!(w[0], format!("avertissement: {SVC} : {DUP_WARNING}"));
    assert!(!err.contains(SECOND), "{err}");
    assert!(warnings_for(&err, others[0]).is_empty(), "{err}");
}

#[test]
fn req179_ac4_ac5_two_calls_keep_first_chunk_without_warning() {
    let env = Env::new(1, Some(120));
    env.fixture(&format!("{SVC}.1.json"), &[(TYPE_ID, FIRST)], json!([]));
    env.fixture(&format!("{SVC}.2.json"), &[(TYPE_ID, SECOND)], json!([]));
    let err = env.build_ok();
    let calls: Vec<String> = env
        .log_lines()
        .into_iter()
        .filter(|l| l.starts_with(SVC))
        .collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_first_kept(&env);
    assert!(warnings_for(&err, "OrderService.java").is_empty(), "{err}");
}

#[test]
fn req179_ac5_identical_summaries_give_no_warning() {
    let env = Env::new(1, None);
    env.fixture(
        &format!("{SVC}.json"),
        &[(TYPE_ID, FIRST), (TYPE_ID, FIRST)],
        json!([]),
    );
    let err = env.build_ok();
    assert_first_kept(&env);
    assert!(warnings_for(&err, "OrderService.java").is_empty(), "{err}");
}

#[test]
fn req179_ac5_same_concept_twice_in_one_markdown_gives_no_warning() {
    let env = Env::new(1, None);
    let sec = "section:docs/guide.md#guide";
    env.fixture(
        "docs/guide.md.json",
        &[],
        json!([
            {"name": "Facturation", "summary": "Émission des factures.", "sections": [sec]},
            {"name": "« facturation »", "summary": "Autre texte.", "sections": [sec]}
        ]),
    );
    let err = env.build_ok();
    assert!(warnings_for(&err, "docs/guide.md").is_empty(), "{err}");
    let nodes = env.export()["nodes"].as_array().unwrap().clone();
    let concepts: Vec<&Value> = nodes.iter().filter(|n| n["kind"] == "concept").collect();
    assert_eq!(concepts.len(), 1, "{concepts:?}");
    assert_eq!(concepts[0]["id"], "concept:facturation");
    assert_eq!(concepts[0]["summary"], "Émission des factures.");
}

/// Dépôt avec `docs/a.md` et `docs/b.md` (une section chacun), sans autre concept.
fn concept_env(a_concepts: Value) -> Env {
    let env = Env::new(1, None);
    env.write_repo("docs/a.md", "# Alpha\n\nTexte de a.\n");
    env.write_repo("docs/b.md", "# Beta\n\nTexte de b.\n");
    env.fixture("docs/guide.md.json", &[], json!([]));
    env.fixture("docs/a.md.json", &[], a_concepts);
    env.fixture(
        "docs/b.md.json",
        &[],
        json!([{"name": "Facturation.", "summary": "Factures.", "sections": ["section:docs/b.md#beta"]}]),
    );
    env
}

fn a_facturation() -> Value {
    json!({"name": "« Facturation »", "summary": "Émission.", "sections": ["section:docs/a.md#alpha"]})
}

fn assert_single_concept(env: &Env) {
    let out = env
        .cmd()
        .args(["query", "search", "facturation", "--json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let concepts: Vec<&Value> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "concept")
        .collect();
    assert_eq!(concepts.len(), 1, "{v}");
    assert_eq!(concepts[0]["qualified"], "facturation");
    let export = env.export();
    let ids: Vec<&str> = export["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    for id in &ids {
        assert!(
            !id.starts_with("concept:«")
                && !id.starts_with("concept:\"")
                && !id.starts_with("concept:'")
                && !(id.starts_with("concept:") && id.ends_with('.')),
            "{id}"
        );
    }
    assert_eq!(
        ids.iter().filter(|i| i.starts_with("concept:")).count(),
        1,
        "{ids:?}"
    );
    let mentions: Vec<&str> = export["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "mentions" && e["target"] == "concept:facturation")
        .map(|e| e["source"].as_str().unwrap())
        .collect();
    assert!(
        mentions.contains(&"section:docs/a.md#alpha"),
        "{mentions:?}"
    );
    assert!(mentions.contains(&"section:docs/b.md#beta"), "{mentions:?}");
}

#[test]
fn req180_ac3_ac4_quoted_and_dotted_names_give_one_concept_without_warning() {
    let env = concept_env(json!([a_facturation()]));
    let err = env.build_ok();
    assert_single_concept(&env);
    assert!(warnings_for(&err, "docs/a.md").is_empty(), "{err}");
    assert!(warnings_for(&err, "docs/b.md").is_empty(), "{err}");
}

#[test]
fn req180_ac4_nameless_concept_gives_the_exact_warning() {
    let env = concept_env(json!([
        a_facturation(),
        {"name": "\"...\"", "summary": "x", "sections": ["section:docs/a.md#alpha"]}
    ]));
    let err = env.build_ok();
    assert_single_concept(&env);
    let w = warnings_for(&err, "docs/a.md");
    assert_eq!(
        w,
        vec![
            "avertissement: docs/a.md : 1 élément(s) d'enrichissement écarté(s) : concept sans nom"
        ],
        "{err}"
    );
    assert!(warnings_for(&err, "docs/b.md").is_empty(), "{err}");
}
