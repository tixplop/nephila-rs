//! Banc d'évaluation : corpus figé et questions (REQ-220, DEC-172).

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Empreinte du banc (AC-220-6) ; à recalculer si le corpus ou les questions changent.
const CORPUS_HASH: &str = "b0a0628e94ba50f54ae238502bb1ded346aff8e69285de00aab6f2fdc4bbaea1";

fn eval_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval")
}

/// Fichiers sous `dir`, chemins relatifs avec `/`, triés.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = format!("{prefix}{}", e.file_name().to_str().unwrap());
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), &format!("{name}/"), out);
            } else {
                out.push(name);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

fn corpus_files() -> Vec<String> {
    files_under(&eval_dir().join("corpus"))
}

fn questions() -> Vec<Value> {
    let raw = fs::read_to_string(eval_dir().join("questions.json")).unwrap();
    serde_json::from_str::<Value>(&raw)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// BLAKE3 de la suite `<chemin>\0<contenu>\0` du corpus trié, puis de `questions.json`.
fn corpus_hash() -> String {
    let mut h = blake3::Hasher::new();
    for f in corpus_files() {
        h.update(f.as_bytes());
        h.update(b"\0");
        h.update(&fs::read(eval_dir().join("corpus").join(&f)).unwrap());
        h.update(b"\0");
    }
    h.update(b"questions.json\0");
    h.update(&fs::read(eval_dir().join("questions.json")).unwrap());
    h.update(b"\0");
    h.finalize().to_hex().to_string()
}

/// Dépôt de test : copie du corpus sous `TMPDIR`, configuration `mock`, dossier de fixtures.
struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        let config = tmp.path().join("config.toml");
        for f in corpus_files() {
            let to = repo.join(&f);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(eval_dir().join("corpus").join(&f), to).unwrap();
        }
        fs::create_dir_all(&mock).unwrap();
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        Env {
            _tmp: tmp,
            repo,
            mock,
            config,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .output()
            .unwrap()
    }

    /// Graphe `--no-llm` du corpus, puis son export.
    fn build_and_export(&self) -> Vec<Value> {
        let b = self.run(&["build", "--no-llm"]);
        assert_eq!(b.status.code(), Some(0));
        assert!(
            b.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&b.stderr)
        );
        let e = self.run(&["export", "--output", "-"]);
        assert!(e.status.success());
        let v: Value = serde_json::from_slice(&e.stdout).unwrap();
        v["nodes"].as_array().unwrap().clone()
    }
}

fn ext_count(files: &[String], ext: &str) -> usize {
    files.iter().filter(|f| f.ends_with(ext)).count()
}

#[test]
fn req220_ac1_corpus_composition() {
    let files = corpus_files();
    assert!((30..=50).contains(&files.len()), "{} fichiers", files.len());
    for (ext, min) in [
        (".java", 8),
        (".kt", 3),
        (".scala", 1),
        (".groovy", 1),
        ("pom.xml", 1),
        (".js", 3),
        (".ts", 3),
        (".md", 4),
        (".sql", 4),
    ] {
        assert!(ext_count(&files, ext) >= min, "{ext} : moins de {min}");
    }
}

/// Premier indice sensible trouvé dans `text` (casse ignorée), sinon `None`.
fn sensitive(text: &str) -> Option<String> {
    let t = text.to_lowercase();
    for w in [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "credential",
        "privatekey",
        "private_key",
        "private-key",
    ] {
        if t.contains(w) {
            return Some(w.to_string());
        }
    }
    for (i, _) in t.match_indices("begin ") {
        let run: String = t[i + 6..]
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == ' ')
            .collect();
        if run.contains("private") {
            return Some("BEGIN … PRIVATE".into());
        }
    }
    let b = t.as_bytes();
    for (i, c) in b.iter().enumerate() {
        let around = i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_alphanumeric();
        if *c == b'@' && around && b[i + 1].is_ascii_alphanumeric() {
            return Some("adresse e-mail".into());
        }
    }
    for scheme in ["http://", "https://"] {
        for (i, _) in t.match_indices(scheme) {
            let host: String = t[i + scheme.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
                .collect();
            if host != "exemple.org" && !host.ends_with(".exemple.org") {
                return Some(format!("URL {host}"));
            }
        }
    }
    None
}

#[test]
fn req220_ac2_no_sensitive_data() {
    // Le détecteur lui-même.
    for bad in [
        "mot de passe : PassWord",
        "Secret",
        "x_api-key",
        "-----BEGIN RSA PRIVATE KEY-----",
        "a@b.fr",
        "http://exemple.com/x",
        "https://exemple.org.evil.net",
    ] {
        assert!(sensitive(bad).is_some(), "{bad}");
    }
    assert!(sensitive("voir https://exemple.org/a et https://www.exemple.org").is_none());

    let files = corpus_files();
    for f in &files {
        let content = fs::read_to_string(eval_dir().join("corpus").join(f)).unwrap();
        assert_eq!(sensitive(&content), None, "{f}");
        assert_eq!(sensitive(f), None, "{f}");
        let name = f.rsplit('/').next().unwrap();
        assert!(name != ".nephila.toml" && name != ".env", "{f}");
    }
    let q = fs::read_to_string(eval_dir().join("questions.json")).unwrap();
    assert_eq!(sensitive(&q), None, "questions.json");
}

#[test]
fn req220_ac3_graph_covers_every_file_and_dry_run_counts_them() {
    let env = Env::new();
    let nodes = env.build_and_export();
    for f in corpus_files() {
        let covered = nodes.iter().any(|n| {
            n["file"] == f.as_str()
                && !n["id"].as_str().unwrap().starts_with("package:")
                && !n["id"].as_str().unwrap().starts_with("key:")
        });
        assert!(covered, "aucun nœud pour {f}");
    }
    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(o.status.code(), Some(0));
    let want = format!(
        "estimation : {} fichiers à enrichir dont 0 envoyés seuls et 0 sans appel",
        corpus_files().len()
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(stdout.lines().any(|l| l == want), "{stdout}");
}

#[test]
fn req220_ac4_questions_are_well_formed_and_cover_families() {
    let env = Env::new();
    let nodes = env.build_and_export();
    let file_of = |q: &str| {
        nodes
            .iter()
            .find(|n| n["qualified"] == q)
            .unwrap_or_else(|| panic!("{q} absent de l'export"))["file"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let qs = questions();
    assert!((20..=30).contains(&qs.len()), "{} questions", qs.len());
    let mut ids = std::collections::HashSet::new();
    let mut families = [0usize; 4];
    for q in &qs {
        assert!(ids.insert(q["id"].as_str().unwrap().to_string()));
        assert!(!q["question"].as_str().unwrap().trim().is_empty());
        let expected = q["expected"].as_array().unwrap();
        assert!(!expected.is_empty() && expected.len() <= 4);
        let mut fam = [false; 4];
        for e in expected {
            let f = file_of(e.as_str().unwrap());
            let i = match f.rsplit('.').next().unwrap() {
                "java" | "kt" | "scala" | "groovy" | "xml" => 0,
                "js" | "ts" => 1,
                "md" => 2,
                "sql" => 3,
                other => panic!("famille inconnue : {other}"),
            };
            fam[i] = true;
        }
        for i in 0..4 {
            families[i] += fam[i] as usize;
        }
    }
    for (i, n) in families.iter().enumerate() {
        assert!(*n >= 2, "famille {i} : {n} questions");
    }
}

#[test]
fn req220_ac5_expected_nodes_are_reachable_by_ask() {
    let env = Env::new();
    env.build_and_export();
    for q in questions() {
        let expected = q["expected"].clone();
        let fixture = serde_json::json!({"found": true, "answer": "ok", "nodes": expected});
        fs::write(env.mock.join("_ask.json"), fixture.to_string()).unwrap();
        let o = env.run(&["query", "ask", "--json", q["question"].as_str().unwrap()]);
        let id = &q["id"];
        assert_eq!(
            o.status.code(),
            Some(0),
            "{id} : {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let got: Vec<&str> = v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["qualified"].as_str().unwrap())
            .collect();
        let want: Vec<&str> = expected
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert_eq!(got, want, "{id}");
    }
}

#[test]
fn req220_ac6_corpus_hash_is_frozen() {
    assert_eq!(corpus_hash(), CORPUS_HASH);
}
