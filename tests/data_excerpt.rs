//! JSON, YAML et XML de données envoyés en un appel, début et squelette (REQ-239, DEC-178).

use std::fs;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use serde_json::{json, Value};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const KEY_VAR: &str = "NEPHILA_T330_KEY";

fn reply(text: &str) -> String {
    json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string()
}

/// AC-239-7
#[test]
fn req239_ac7_json_de_500_ko_un_seul_appel() {
    let mut json_text = String::from("{\"premiere_cle\":1,\"liste\":[");
    while json_text.len() < 499_900 {
        json_text.push_str("{\"n\":123456},");
    }
    json_text.push_str("{\"n\":\"FIN_UNIQUE_ZZ\"}]}");
    json_text.push_str(&" ".repeat(500_000 - json_text.len()));
    assert_eq!(json_text.len(), 500_000);

    let ok = reply(r#"{"summaries":[{"n":1,"summary":"Rôle."}],"concepts":[]}"#);
    let server = FakeServer::start(vec![(200, ok.clone()), (200, ok)]);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("config")).unwrap();
    fs::write(repo.join("config/big.json"), &json_text).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nenrich_code = true\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\nbatch_files = 1\nconcurrency = 1\n",
            server.base_url
        ),
    )
    .unwrap();
    let run = |args: &[&str]| -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("NEPHILA_CONFIG", &config)
            .env(KEY_VAR, "sk-test")
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    };
    let o = run(&["build"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let posts: Vec<String> = server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .map(|r| r.body)
        .collect();
    assert_eq!(posts.len(), 1);
    let body: Value = serde_json::from_str(&posts[0]).unwrap();
    let sent = body.to_string();
    assert!(sent.contains("extrait tronqué de 489 Ko"), "{sent}");
    assert!(sent.contains("premiere_cle"));
    assert!(sent.contains("[squelette des clés]"));
    assert!(!sent.contains("FIN_UNIQUE_ZZ"));
    assert!(sent.len() < 20_000, "{}", sent.len());

    let out = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
    assert!(out.contains("0 fichiers à enrichir"), "{out}");
    let o = run(&["update"]);
    assert_eq!(o.status.code(), Some(0));
    let n = server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(n, 1, "update ne renvoie rien");
    let o = run(&["query", "search", "big", "--json"]);
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(s.contains("Rôle."), "{s}");
}
