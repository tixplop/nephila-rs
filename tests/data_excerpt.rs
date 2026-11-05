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

/// Lance `nephila` dans `repo` contre le faux serveur `base_url`.
fn run_openai(repo: &std::path::Path, config: &std::path::Path, args: &[&str]) -> Output {
    Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("NEPHILA_CONFIG", config)
        .env(KEY_VAR, "sk-test")
        .env_remove("NEPHILA_MOCK_DIR")
        .env_remove("NEPHILA_MOCK_LOG")
        .output()
        .unwrap()
}

/// AC-241-1 : un lot de trois fichiers de données envoie l'extrait, le fichier court entier et la
/// marque du SQL, entre leurs lignes `=== FICHIER`.
#[test]
fn req241_ac1_lot_de_fichiers_de_donnees_envoie_le_texte_reduit() {
    let json_text = format!(
        "{{\"k\":\"{}FIN_UNIQUE_ZZ\"}}",
        "x".repeat(10_000 - "{\"k\":\"FIN_UNIQUE_ZZ\"}".len())
    );
    assert_eq!(json_text.len(), 10_000);
    let yaml_text = "court: oui\n";
    let mut sql = String::from("");
    let mut i = 0;
    while sql.len() < 5_000 {
        sql.push_str(&format!("INSERT INTO t VALUES ({i}, 'v');\n"));
        i += 1;
    }
    // Chaque fichier ne demande que son nœud document : [1], [2], [3] en numérotation continue.
    let lot = json!({"files":[
        {"f":1,"summaries":[{"n":1,"summary":"Résumé JSON."}],"concepts":[]},
        {"f":2,"summaries":[{"n":2,"summary":"Résumé YAML."}],"concepts":[]},
        {"f":3,"summaries":[{"n":3,"summary":"Résumé SQL."}],"concepts":[]}
    ]})
    .to_string();
    let server = FakeServer::start(vec![(200, reply(&lot)), (200, reply(&lot))]);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("a.json"), &json_text).unwrap();
    fs::write(repo.join("b.yaml"), yaml_text).unwrap();
    fs::write(repo.join("s.sql"), &sql).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nenrich_code = true\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\nbatch_files = 3\nconcurrency = 1\nmax_chunk_chars = 60000\n",
            server.base_url
        ),
    )
    .unwrap();
    let o = run_openai(&repo, &config, &["build"]);
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
    assert_eq!(posts.len(), 1, "{posts:?}");
    let body: Value = serde_json::from_str(&posts[0]).unwrap();
    let user = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    let section = |name: &str| -> String {
        let start = user
            .find(&format!(" : {name} ===\n"))
            .unwrap_or_else(|| panic!("{name} absent : {user}"));
        let end = user[start..].find(&format!("=== FIN DU FICHIER : {name}"));
        user[start..start + end.unwrap()].to_string()
    };
    let a = section("a.json");
    assert!(a.contains("extrait tronqué de 10 Ko"), "{a}");
    assert!(!a.contains("FIN_UNIQUE_ZZ"));
    assert!(section("b.yaml").contains(yaml_text));
    let s = section("s.sql");
    assert!(s.contains("instructions INSERT ou COPY retirées"), "{s}");
    assert!(s.contains("INSERT INTO t VALUES (0, 'v');"), "{s}");
    assert!(!s.contains(&format!("VALUES ({}, 'v')", i - 1)), "{s}");
    let store = nephila::store::Store::open_readonly(&repo).unwrap();
    let summary = |id: &str| {
        store
            .nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.id == id)
            .and_then(|n| n.summary)
    };
    assert_eq!(summary("document:a.json").as_deref(), Some("Résumé JSON."));
    assert_eq!(summary("document:b.yaml").as_deref(), Some("Résumé YAML."));
    assert_eq!(summary("document:s.sql").as_deref(), Some("Résumé SQL."));
}

/// AC-241-7 : l'analyse lit le fichier entier, la réduction ne touche que l'envoi.
#[test]
fn req241_ac7_analyse_lit_le_fichier_entier() {
    let json_text = format!(
        "{{\"debut\":\"{}\",\"cle_tardive\":1}}",
        "x".repeat(100_000)
    );
    let mut sql = String::new();
    let mut i = 0;
    while sql.len() < 100_000 {
        sql.push_str(&format!("INSERT INTO t VALUES ({i}, 'v');\n"));
        i += 1;
    }
    sql.push_str("CREATE TABLE tardive (id INT);\n");
    let tmp = tempfile::tempdir().unwrap();
    let mk = |name: &str| {
        let repo = tmp.path().join(name);
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("big.json"), &json_text).unwrap();
        fs::write(repo.join("big.sql"), &sql).unwrap();
        repo
    };
    let (plain, enriched) = (mk("plain"), mk("enriched"));
    let mock = tmp.path().join("mock");
    fs::create_dir_all(&mock).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\n",
    )
    .unwrap();
    let build = |repo: &std::path::Path, extra: &str| -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(repo).arg("build");
        if !extra.is_empty() {
            c.arg(extra);
        }
        c.env("NEPHILA_CONFIG", &config)
            .env("NEPHILA_MOCK_DIR", &mock)
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    };
    let a = build(&plain, "--no-llm");
    let b = build(&enriched, "");
    for o in [&a, &b] {
        assert_eq!(
            o.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let warnings = |o: &Output| -> Vec<String> {
        String::from_utf8_lossy(&o.stderr)
            .lines()
            .filter(|l| l.starts_with("avertissement"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(warnings(&a), warnings(&b));
    let ids = |repo: &std::path::Path| -> Vec<String> {
        nephila::store::Store::open_readonly(repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect()
    };
    for repo in [&plain, &enriched] {
        let ids = ids(repo);
        assert!(
            ids.contains(&"key:big.json#cle_tardive".to_string()),
            "{ids:?}"
        );
        assert!(ids.contains(&"table:tardive".to_string()), "{ids:?}");
    }
}
