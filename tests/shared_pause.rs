//! Pause partagée après un 429 ou un 529 (REQ-254, DEC-186) : tant qu'un appel attend son nouvel
//! essai, aucun autre appel de la commande n'envoie. Le binaire est lancé contre un faux serveur
//! qui retient la réponse 429 jusqu'à la deuxième requête reçue.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

#[path = "support/plan_server.rs"]
mod plan_server;
use plan_server::{Act, Server};

const KEY_VAR: &str = "NEPHILA_T343_KEY";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
}

impl Env {
    fn new(n: usize, url: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        for i in 1..=n {
            fs::write(
                repo.join(format!("d{i}.md")),
                format!("# D{i}\n\nTexte du document {i}.\n"),
            )
            .unwrap();
        }
        fs::write(
            tmp.path().join("config.toml"),
            format!(
                "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\napi_key_env = \"{KEY_VAR}\"\nconcurrency = 2\nbatch_files = 1\nformat_retries = 0\n"
            ),
        )
        .unwrap();
        Env { _tmp: tmp, repo }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", self._tmp.path().join("config.toml"))
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "10")
            .env_remove("NO_COLOR")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

/// Fixture d'AC-254-1 : la première requête reçoit `status` avec `retry-after: 1`, écrit une fois
/// la deuxième requête reçue ; la deuxième est servie après 150 ms (la réponse d'erreur part
/// d'abord) ; les suivantes aussitôt.
fn fixture(status: u16) -> (Server, Output) {
    let server = Server::start(move |i| match i {
        0 => Act::HeldRetry(status, 2, "1"),
        1 => Act::Ok(150),
        _ => Act::Ok(0),
    });
    let env = Env::new(3, &server.base_url);
    let out = env.run(&["build", "--verbose"]);
    (server, out)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn enriched(o: &Output) -> usize {
    stderr(o).matches("llm enrich fin").count()
}

fn check_pause(status: u16) {
    let (server, out) = fixture(status);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert_eq!(server.requests(), 4, "{err}");
    let t = server.times();
    let answered = t[0].1.expect("réponse d'erreur écrite");
    for (i, (at, _)) in t.iter().enumerate().skip(2) {
        assert!(
            at.duration_since(answered) >= Duration::from_millis(950),
            "requête {i} partie {:?} après la réponse {status}\n{err}",
            at.duration_since(answered)
        );
    }
    assert_eq!(enriched(&out), 3, "{err}");
    assert!(!err.contains("avertissement:"), "{err}");
    assert!(
        !err.contains("politique: erreurs réseau consécutives"),
        "{err}"
    );
}

#[test]
fn req254_ac1_pause_apres_429() {
    check_pause(429);
}

#[test]
fn req254_ac2_pause_apres_529_et_pas_apres_503() {
    check_pause(529);
    let (server, out) = fixture(503);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert_eq!(server.requests(), 4, "{err}");
    let t = server.times();
    let answered = t[0].1.expect("réponse 503 écrite");
    // La requête du 3e document part sans attendre la fin du retry-after du 1er.
    let third = t[2].0.duration_since(answered);
    assert!(third < Duration::from_millis(500), "{third:?}\n{err}");
}

#[test]
fn req254_ac6_une_seule_ligne_de_pause() {
    let (_server, out) = fixture(429);
    let err = stderr(&out);
    let lines: Vec<&str> = err.lines().collect();
    let wait: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].contains("llm enrich attente"))
        .collect();
    let pause: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].contains("llm enrich pause"))
        .collect();
    assert_eq!(wait.len(), 1, "{err}");
    assert_eq!(pause.len(), 1, "{err}");
    assert_eq!(pause[0], wait[0] + 1, "{err}");
    let label = lines[wait[0]]
        .split("llm enrich attente ")
        .nth(1)
        .unwrap()
        .split(" 1000 ms après HTTP 429 (nouvel essai 1/4)")
        .next()
        .unwrap();
    assert!(label.ends_with(" [1/1]"), "{err}");
    assert!(
        lines[wait[0]].ends_with(&format!(
            "llm enrich attente {label} 1000 ms après HTTP 429 (nouvel essai 1/4)"
        )),
        "{err}"
    );
    assert!(
        lines[pause[0]].ends_with(&format!(
            "llm enrich pause {label} : aucun envoi pendant 1000 ms après HTTP 429"
        )),
        "{err}"
    );
    for l in &lines {
        if l.starts_with("avertissement:") || l.starts_with("erreur:") {
            assert!(!l.contains("pause"), "{err}");
        }
    }

    // 503 : aucune ligne de pause.
    let (_s, out) = fixture(503);
    assert!(
        !stderr(&out).contains("llm enrich pause"),
        "{}",
        stderr(&out)
    );

    // Sans --verbose : stderr identique à celui d'un serveur conforme du premier coup.
    let server = Server::start(move |i| match i {
        0 => Act::HeldRetry(429, 2, "1"),
        1 => Act::Ok(150),
        _ => Act::Ok(0),
    });
    let quiet = Env::new(3, &server.base_url).run(&["build"]);
    let ok = Server::start(|_| Act::Ok(0));
    let reference = Env::new(3, &ok.base_url).run(&["build"]);
    assert_eq!(quiet.status.code(), Some(0));
    assert_eq!(stderr(&quiet), stderr(&reference));
}
