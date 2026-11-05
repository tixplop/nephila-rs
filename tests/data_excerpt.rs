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

/// AC-240-6
#[test]
fn req240_ac6_sql_un_seul_appel_sans_insert() {
    let inserts = |tag: &str| {
        let mut t = String::new();
        let mut n = 0;
        while t.len() < 200_000 {
            t.push_str(&format!("INSERT INTO {tag} VALUES ({n}, 'ligne_{tag}');\n"));
            n += 1;
        }
        t
    };
    let sql = format!(
        "CREATE TABLE a (id INT);\n{}CREATE TABLE b (id INT);\n{}CREATE TABLE c (id INT);\n",
        inserts("a"),
        inserts("b")
    );
    let ok = reply(r#"{"summaries":[{"n":1,"summary":"Rôle."}],"concepts":[]}"#);
    let server = FakeServer::start(vec![(200, ok.clone()), (200, ok)]);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("db")).unwrap();
    fs::write(repo.join("db/a.sql"), &sql).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nenrich_code = true\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\nbatch_files = 1\nconcurrency = 1\nmax_chunk_chars = 60000\n",
            server.base_url
        ),
    )
    .unwrap();
    let o = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&repo)
        .arg("build")
        .env("NEPHILA_CONFIG", &config)
        .env(KEY_VAR, "sk-test")
        .env_remove("NEPHILA_MOCK_DIR")
        .env_remove("NEPHILA_MOCK_LOG")
        .output()
        .unwrap();
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
    assert!(
        sent.contains("instructions INSERT ou COPY retirées"),
        "{sent}"
    );
    for t in ["a", "b", "c"] {
        assert!(sent.contains(&format!("CREATE TABLE {t} (id INT);")), "{t}");
    }
    assert!(sent.contains("document:db/a.sql"));
    for t in ["a", "b", "c"] {
        assert!(sent.contains(&format!("table:{t}")), "{t}");
    }
    assert!(sent.len() < 20_000, "{}", sent.len());
}

/// Requêtes POST envoyées par `build` pour `db/a.sql`.
fn sql_posts(sql: &str, max_chunk_chars: usize) -> Vec<String> {
    let ok = reply(r#"{"summaries":[{"n":1,"summary":"Rôle."}],"concepts":[]}"#);
    let server = FakeServer::start((0..20).map(|_| (200, ok.clone())).collect());
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("db")).unwrap();
    fs::write(repo.join("db/a.sql"), sql).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nenrich_code = true\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\nbatch_files = 1\nconcurrency = 1\nmax_chunk_chars = {max_chunk_chars}\n",
            server.base_url
        ),
    )
    .unwrap();
    let o = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&repo)
        .arg("build")
        .env("NEPHILA_CONFIG", &config)
        .env(KEY_VAR, "sk-test")
        .env_remove("NEPHILA_MOCK_DIR")
        .env_remove("NEPHILA_MOCK_LOG")
        .output()
        .unwrap();
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .map(|r| r.body)
        .collect()
}

/// AC-240-6 : un SQL de DDL pur garde son plan, il reste découpé.
#[test]
fn req240_ac6_ddl_pur_garde_son_plan() {
    let columns = (0..60)
        .map(|c| format!("col{c} INT"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = String::new();
    let mut n = 0;
    loop {
        let stmt = format!("CREATE TABLE tbl{n} ({columns});\n");
        if sql.len() + stmt.len() > 60_000 {
            break;
        }
        sql.push_str(&stmt);
        n += 1;
    }
    let posts = sql_posts(&sql, 60_000);
    assert_eq!(posts.len(), 1);
    let sent = serde_json::from_str::<Value>(&posts[0])
        .unwrap()
        .to_string();
    assert!(!sent.contains("instructions INSERT ou COPY"), "{sent}");
    assert!(sent.contains("tbl0 ("));
    assert!(sent.contains(&format!("tbl{} (", n - 1)));

    let posts = sql_posts(&sql, 20_000);
    assert!(posts.len() > 1, "{}", posts.len());
}
