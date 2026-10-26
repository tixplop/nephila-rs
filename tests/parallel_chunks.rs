//! Morceaux d'un même gros fichier enrichis en parallèle (REQ-051, DEC-063).

use assert_cmd::Command;
use nephila::chunk::split;
use nephila::store::Store;
use std::fs;
use std::path::{Path, PathBuf};

const JAVA: &str = "src/main/java/com/acme/Big.java";
const SMALL: &str = "src/main/java/com/acme/Small.java";
const DOC: &str = "docs/guide.md";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new(max_chunk: usize, concurrency: usize, format_retries: usize) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&mock).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nmax_chunk_chars = {max_chunk}\nconcurrency = {concurrency}\nbatch_files = 1\nformat_retries = {format_retries}\n"
            ),
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

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Classe Java de 400 méthodes ; une fixture par morceau (4 morceaux), qui nomme une
    /// méthode située au milieu du morceau appelé.
    fn java(&self) -> String {
        let mut s = String::from("package com.acme;\n\npublic class Big {\n");
        for i in 0..400 {
            s.push_str(&format!("    void m{i:03}() {{}}\n"));
        }
        s.push_str("}\n");
        Self::write(&self.repo, JAVA, &s);
        for n in 1..=4 {
            let m = (n - 1) * 100 + 50;
            Self::write(
                &self.mock,
                &format!("{JAVA}.{n}.json"),
                &format!(
                    r#"{{"version":2,"summaries":[{{"node":"method:com.acme.Big.m{m:03}","summary":"part{n}"}}],"concepts":[]}}"#
                ),
            );
        }
        s
    }

    /// Document Markdown de 4 sections, une par morceau.
    fn markdown(&self) -> String {
        let mut s = String::new();
        for k in 1..=4 {
            s.push_str(&format!("# Section{k}\n\n{}\n\n", "mot ".repeat(40)));
        }
        Self::write(&self.repo, DOC, &s);
        let doc = format!("document:{DOC}");
        let sec = |k: usize| format!("section:{DOC}#section{k}");
        let fixtures = [
            format!(
                r#"{{"version":2,"summaries":[{{"node":"{doc}","summary":"resume-1"}}],"concepts":[{{"name":"Facturation","summary":"c1","sections":["{}"]}}]}}"#,
                sec(1)
            ),
            format!(
                r#"{{"version":2,"summaries":[],"concepts":[{{"name":"Facturation","summary":"c2","sections":["{}"]}}]}}"#,
                sec(2)
            ),
            format!(
                r#"{{"version":2,"summaries":[{{"node":"{doc}","summary":"resume-3"}}],"concepts":[]}}"#
            ),
            r#"{"version":2,"summaries":[],"concepts":[]}"#.to_string(),
        ];
        for (i, f) in fixtures.iter().enumerate() {
            Self::write(&self.mock, &format!("{DOC}.{}.json", i + 1), f);
        }
        s
    }

    fn build(&self, delay_ms: Option<&str>) -> std::process::Output {
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_INVALID_KEY")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        if let Some(d) = delay_ms {
            c.env("NEPHILA_MOCK_DELAY_MS", d);
        }
        c.arg("-C")
            .arg(&self.repo)
            .args(["build", "--verbose"])
            .output()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn store(&self) -> Store {
        Store::open_readonly(&self.repo).unwrap()
    }

    fn reset_log(&self) {
        let _ = fs::remove_file(&self.log);
    }
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn phase_ms(err: &str) -> u128 {
    let line = err
        .lines()
        .find(|l| l.contains("enrichissement: fin"))
        .unwrap_or_else(|| panic!("{err}"));
    line.split("fin ")
        .nth(1)
        .unwrap()
        .split(" ms")
        .next()
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Taille de morceau qui donne 4 morceaux pour `text`.
fn four_chunks(text: &str) -> usize {
    let size = (text.len() / 4 + 1..text.len())
        .find(|&m| split(text, m).len() == 4)
        .expect("découpage en 4 morceaux");
    assert_eq!(split(text, size).len(), 4);
    size
}

fn java_text() -> String {
    Env::new(1, 1, 0).java()
}

#[test]
fn req051_ac1_chunks_run_in_parallel() {
    let size = four_chunks(&java_text());
    let measure = |concurrency: usize| {
        let env = Env::new(size, concurrency, 0);
        env.java();
        let out = env.build(Some("400"));
        assert!(out.status.success(), "{}", stderr(&out));
        let calls = env.calls();
        assert_eq!(calls.len(), 4, "{calls:?}");
        phase_ms(&stderr(&out))
    };
    let serial = measure(1);
    assert!(serial >= 1_600, "concurrency=1 : {serial} ms");
    let parallel = measure(4);
    assert!(
        2 * parallel < serial,
        "parallèle {parallel} ms, série {serial} ms"
    );
}

#[test]
fn req051_ac2_markdown_merge_is_identical_whatever_concurrency() {
    let probe = Env::new(1, 1, 0).markdown();
    let size = four_chunks(&probe);
    let graph = |concurrency: usize| {
        let env = Env::new(size, concurrency, 0);
        env.markdown();
        let out = env.build(None);
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(env.calls().len(), 4);
        let store = env.store();
        assert_eq!(store.count_files_to_enrich().unwrap(), 0);
        let mut nodes: Vec<String> = store
            .nodes()
            .unwrap()
            .iter()
            .map(|n| format!("{n:?}"))
            .collect();
        nodes.sort();
        let mut edges: Vec<String> = store
            .edges()
            .unwrap()
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        edges.sort();
        (nodes, edges)
    };
    let (n1, e1) = graph(1);
    assert!(
        n1.iter().any(|n| n.contains("resume-1")),
        "résumé du morceau 1 : {n1:?}"
    );
    assert!(!n1.iter().any(|n| n.contains("resume-3")), "{n1:?}");
    let s1 = format!("section:{DOC}#section1");
    let s2 = format!("section:{DOC}#section2");
    let concept_edges: Vec<_> = e1
        .iter()
        .filter(|e| e.contains("Facturation") || e.contains("concept:"))
        .collect();
    assert!(
        concept_edges.iter().any(|e| e.contains(&s1))
            && concept_edges.iter().any(|e| e.contains(&s2)),
        "sections des deux morceaux : {e1:?}"
    );
    for c in [2, 4] {
        let (n, e) = graph(c);
        assert_eq!(n, n1, "concurrency={c}");
        assert_eq!(e, e1, "concurrency={c}");
    }
}

#[test]
fn req051_ac4_failed_chunk_gives_one_warning() {
    let size = four_chunks(&java_text());
    for concurrency in [1usize, 4] {
        let env = Env::new(size, concurrency, 0);
        env.java();
        Env::write(
            &env.repo,
            SMALL,
            "package com.acme;\n\npublic class Small {\n    void ping() {}\n}\n",
        );
        Env::write(
            &env.mock,
            &format!("{SMALL}.1.json"),
            r#"{"version":2,"summaries":[{"node":"method:com.acme.Small.ping","summary":"petit"}],"concepts":[]}"#,
        );
        Env::write(&env.mock, &format!("{JAVA}.2.json"), "{pas du json");
        env.reset_log();
        let out = env.build(None);
        assert!(!out.status.success(), "concurrency={concurrency}");
        let all_calls = env.calls();
        assert!(
            all_calls.iter().any(|l| l.starts_with(SMALL)),
            "{all_calls:?}"
        );
        let calls: Vec<String> = all_calls
            .into_iter()
            .filter(|l| !l.starts_with(SMALL))
            .collect();
        if concurrency == 1 {
            assert_eq!(
                calls,
                vec![format!("{JAVA}\t1"), format!("{JAVA}\t2")],
                "{calls:?}"
            );
        } else {
            assert!(
                (2..=4).contains(&calls.len()) && calls.iter().all(|l| l.starts_with(JAVA)),
                "{calls:?}"
            );
        }
        let err = stderr(&out);
        let warns: Vec<&str> = err
            .lines()
            .filter(|l| l.starts_with("avertissement:"))
            .collect();
        assert_eq!(warns.len(), 1, "concurrency={concurrency} : {err}");
        assert!(warns[0].contains(JAVA), "{err}");
        let store = env.store();
        let pending = store.files_to_enrich().unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].path, JAVA);
        let nodes = store.nodes().unwrap();
        assert!(nodes.iter().any(|n| n.id == "method:com.acme.Big.m000"));
        assert!(
            nodes
                .iter()
                .any(|n| n.id == "method:com.acme.Small.ping"
                    && n.summary.as_deref() == Some("petit")),
            "petit fichier enrichi"
        );
        assert!(nodes
            .iter()
            .filter(|n| n.id.starts_with("method:com.acme.Big."))
            .all(|n| n.summary.is_none()));
    }
}
