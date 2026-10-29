//! Aucun appel d'enrichissement sans nœud à demander (REQ-159, DEC-128) : tests d'intégration
//! sur le LLM simulé.

use assert_cmd::cargo::cargo_bin;
use nephila::chunk::split;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

impl Env {
    fn new(files: &[(&str, &str)], extra: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&mock).unwrap();
        for (path, content) in files {
            let p = repo.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!("provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n{extra}"),
        )
        .unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).args(args);
        for v in [
            "NEPHILA_MOCK_FAIL_AFTER",
            "NEPHILA_MOCK_DELAY_MS",
            "NEPHILA_MOCK_INVALID_KEY",
            "NEPHILA_MOCK_TIMEOUT_FILES",
            "NEPHILA_MOCK_KEY_REVOKED_AFTER",
            "NEPHILA_MOCK_MALFORMED",
            "NO_COLOR",
        ] {
            c.env_remove(v);
        }
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .output()
            .unwrap()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn fixture(&self, name: &str, node: &str, summary: &str) {
        let body = serde_json::json!({
            "version": 2,
            "summaries": [{"node": node, "summary": summary}],
            "concepts": []
        });
        let p = self.mock.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body.to_string()).unwrap();
    }
}

#[test]
fn req159_ac4_json_de_trois_morceaux_un_seul_appel() {
    let mut json = String::from("{\n");
    for i in 0..30 {
        json.push_str(&format!("  \"cle{i:02}\": \"valeur{i:02}\",\n"));
    }
    json.push_str("  \"fin\": 1\n}\n");
    let max = (json.len() * 2).div_ceil(5);
    assert_eq!(split(&json, max).len(), 3);
    let env = Env::new(
        &[("config/big.json", &json)],
        &format!("batch_files = 1\nconcurrency = 1\nmax_chunk_chars = {max}\n"),
    );
    env.fixture(
        "config/big.json.1.json",
        "document:config/big.json",
        "document de configuration zorglub",
    );
    let o = env.run(&["build"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out} {err}");
    assert_eq!(env.log_lines(), vec!["config/big.json\t1".to_string()]);
    assert!(out.contains(", 0 fichiers à enrichir"), "{out}");
    let (s, _) = text(&env.run(&["query", "search", "zorglub"]));
    assert!(s.contains("config/big.json"), "{s}");

    let o = env.run(&["update"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out} {err}");
    assert!(out.contains("graphe à jour"), "{out}");
    assert_eq!(env.log_lines().len(), 1);
}

const SIDE: &str = "console.log(1);\n";

#[test]
fn req159_ac5_fichier_sans_noeud_enregistre_sans_appel() {
    // Fichier seul (batch_files = 1).
    let env = Env::new(
        &[
            ("web/src/side.js", SIDE),
            ("src/A.java", "class A { void m() {} }\n"),
        ],
        "batch_files = 1\nconcurrency = 1\n",
    );
    let o = env.run(&["build", "--show-progress"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out} {err}");
    assert_eq!(env.log_lines(), vec!["src/A.java\t1".to_string()]);
    assert!(out.contains(", 0 fichiers à enrichir"), "{out}");
    let last = err
        .lines()
        .rfind(|l| l.starts_with("progression: enrichissement"))
        .unwrap_or_else(|| panic!("{err}"));
    assert_eq!(last, "progression: enrichissement 100 % (2/2 fichiers)");
    let so = env.run(&["status"]);
    let (st, serr) = text(&so);
    assert!(so.status.success(), "{st} {serr}");
    // `status` n'écrit la ligne « fichiers à enrichir » que si le compte est non nul.
    assert_eq!(st, "graphe à jour\n", "{serr}");
    let o = env.run(&["update"]);
    assert!(o.status.success());
    assert_eq!(env.log_lines().len(), 1);
    // Trace détaillée : aucune ligne d'enrichissement de side.js (fichiers forcés à neuf).
    let env2 = Env::new(
        &[
            ("web/src/side.js", SIDE),
            ("src/A.java", "class A { void m() {} }\n"),
        ],
        "batch_files = 1\nconcurrency = 1\n",
    );
    let o = env2.run(&["build", "--verbose"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out} {err}");
    assert!(
        err.lines()
            .filter(|l| l.starts_with("trace:"))
            .all(|l| !(l.contains("llm enrich") && l.contains("side.js"))),
        "{err}"
    );
    assert!(err.contains("llm enrich"), "{err}");

    // Lot (batch_files = 10) : side.js n'y figure pas.
    let env = Env::new(
        &[
            ("web/src/side.js", SIDE),
            ("src/A.java", "class A { void m() {} }\n"),
            ("src/B.java", "class B {}\n"),
        ],
        "batch_files = 10\nconcurrency = 1\n",
    );
    let o = env.run(&["build"]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{out} {err}");
    assert_eq!(
        env.log_lines(),
        vec!["src/A.java;src/B.java\t1".to_string()]
    );
    let o = env.run(&["update"]);
    assert!(o.status.success());
    assert_eq!(env.log_lines().len(), 1);
}
