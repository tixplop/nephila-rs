//! Connexions HTTP réutilisées sous concurrence (REQ-053, DEC-064), contre un faux serveur
//! keep-alive.

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use fake_server::{FakeServer, KeepAlive};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const KEY_VAR: &str = "NEPHILA_T116_KEY";

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tls")
            .join(name),
    )
    .unwrap()
}

/// Réponse conforme (fournisseur openai) sans résumé ni concept : valable pour tout document.
fn empty_enrichment() -> String {
    let content = serde_json::json!({"version": 2, "summaries": [], "concepts": []}).to_string();
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":content}}]}).to_string()
}

fn keep_alive(delay_ms: u64, max_per_connection: Option<usize>, tls: bool) -> FakeServer {
    FakeServer::start_keep_alive(KeepAlive {
        body: empty_enrichment(),
        delay_ms,
        max_per_connection,
        tls: tls.then(|| (fixture("server-ip.pem"), fixture("server-ip.key"))),
    })
}

struct Env {
    tmp: tempfile::TempDir,
    repo: PathBuf,
}

impl Env {
    fn new(base_url: &str, concurrency: usize, docs: usize, tls_ca: bool) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        for i in 0..docs {
            fs::write(
                repo.join(format!("docs/d{i:02}.md")),
                format!("# D{i}\n\nTexte {i}.\n"),
            )
            .unwrap();
        }
        let ca = tmp.path().join("ca.pem");
        fs::write(&ca, fixture("ca.pem")).unwrap();
        let mut conf = format!(
            "provider = \"openai\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{base_url}\"\nconcurrency = {concurrency}\nbatch_files = 1\n"
        );
        if tls_ca {
            conf.push_str(&format!("tls_ca_cert = \"{}\"\n", ca.display()));
        }
        fs::write(tmp.path().join("config.toml"), conf).unwrap();
        Env { tmp, repo }
    }

    fn build(&self) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .arg("build")
            .env("NEPHILA_CONFIG", self.tmp.path().join("config.toml"))
            .env(KEY_VAR, "sk-test")
            .env_remove("NEPHILA_RETRY_BASE_MS")
            .output()
            .unwrap()
    }
}

fn posts(s: &FakeServer) -> usize {
    s.requests().iter().filter(|r| r.method == "POST").count()
}

#[test]
fn req053_ac1_sequential_calls_share_one_connection() {
    let s = keep_alive(0, None, false);
    let o = Env::new(&s.base_url, 1, 10, false).build();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(posts(&s), 10);
    assert_eq!(s.connections(), 1);
}

#[test]
fn req053_ac2_concurrent_calls_use_at_most_concurrency_connections() {
    let s = keep_alive(100, None, false);
    let o = Env::new(&s.base_url, 4, 40, false).build();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(posts(&s), 40);
    let c = s.connections();
    assert!((1..=4).contains(&c), "{c} connexions");
}

#[test]
fn req053_ac3_tls_handshakes_are_at_most_concurrency() {
    let s = keep_alive(100, None, true);
    let o = Env::new(&s.base_url, 4, 40, true).build();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(posts(&s), 40);
    let h = s.handshakes();
    assert!((1..=4).contains(&h), "{h} poignées de main");
}

#[test]
fn req053_ac4_connection_closed_by_server_between_calls_does_not_fail_the_next() {
    let s = keep_alive(0, Some(1), false);
    let o = Env::new(&s.base_url, 1, 6, false).build();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(posts(&s) >= 6);
    assert!(
        !text(&o.stderr).contains("avertissement:"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn req053_ac4_close_per_request_server_still_works() {
    let body = empty_enrichment();
    let s = FakeServer::start((0..6).map(|_| (200, body.clone())).collect());
    let o = Env::new(&s.base_url, 2, 6, false).build();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(posts(&s), 6);
}

#[test]
fn req052_ac6_certificate_refusal_is_attempted_once() {
    // Sans tls_ca_cert : le certificat de test est refusé à la poignée de main.
    let s = FakeServer::start_tls(
        vec![(200, empty_enrichment())],
        &fixture("server-ip.pem"),
        &fixture("server-ip.key"),
    );
    let o = Env::new(&s.base_url, 1, 1, false).build();
    assert_ne!(o.status.code(), Some(0));
    assert!(
        text(&o.stderr).contains("certificat"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(s.connections(), 1, "une seule tentative de connexion");
    assert_eq!(s.handshakes(), 0);
    assert!(s.requests().is_empty());
}
