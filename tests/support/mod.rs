//! Assistants partagés par `tests/sqlite_lot3.rs` et `tests/incremental.rs` : dépôts de
//! l'oracle de REQ-065 (`CASES`), copie d'arbres et lancement du binaire.

#![allow(dead_code)]

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct Case {
    pub name: &'static str,
    pub repo: &'static str,
    pub mock: Option<&'static str>,
    pub modify: &'static str,
    pub add: (&'static str, &'static str),
    pub delete: &'static str,
}

pub const CASES: &[Case] = &[
    Case {
        name: "maven3",
        repo: "maven3/repo",
        mock: Some("maven3/mock"),
        modify: "core/src/main/java/com/acme/core/OrderService.java",
        add: ("core/src/main/java/com/acme/core/Oracle.java", JAVA_ADDED),
        delete: "docs/guide.md",
    },
    Case {
        name: "maven2",
        repo: "maven2",
        mock: Some("mock"),
        modify: "core/src/main/java/com/acme/core/OrderRepository.java",
        add: ("core/src/main/java/com/acme/core/Oracle.java", JAVA_ADDED),
        delete: "core/src/main/java/com/acme/core/Broken.java",
    },
    Case {
        name: "gradle-edge",
        repo: "gradle/edge",
        mock: None,
        modify: "app/build.gradle",
        add: ("app/src/main/java/com/acme/core/Oracle.java", JAVA_ADDED),
        delete: "core/build.gradle",
    },
    Case {
        name: "gradle-groovy",
        repo: "gradle/groovy",
        mock: None,
        modify: "core/src/main/java/com/acme/core/Core.java",
        add: ("core/src/main/java/com/acme/core/Oracle.java", JAVA_ADDED),
        delete: "api/src/main/java/com/acme/api/Api.java",
    },
    Case {
        name: "gradle-kotlin",
        repo: "gradle/kotlin",
        mock: None,
        modify: "api/src/main/java/com/acme/api/Api.java",
        add: ("core/src/main/java/com/acme/core/Oracle.java", JAVA_ADDED),
        delete: "core/build.gradle.kts",
    },
    Case {
        name: "ivy",
        repo: "ivy",
        mock: None,
        modify: "src/com/acme/legacy/Old.java",
        add: ("src/com/acme/legacy/Oracle.java", JAVA_ADDED),
        delete: "ivy.xml",
    },
    Case {
        name: "data",
        repo: "data/repo",
        mock: Some("data/mock"),
        modify: "db/V2__fk.sql",
        add: (
            "db/V4__oracle.sql",
            "CREATE TABLE oracle (id INT PRIMARY KEY);\n",
        ),
        delete: "config/app.json",
    },
    Case {
        name: "js-mixed",
        repo: "js/mixed",
        mock: Some("js/mock"),
        modify: "web/src/api/client.ts",
        add: (
            "web/src/api/oracle.ts",
            "export function oracle(): number {\n  return 1;\n}\n",
        ),
        delete: "web/src/api/types.ts",
    },
    Case {
        name: "langs-mixed",
        repo: "langs/mixed",
        mock: None,
        modify: "src/main/kotlin/com/acme/kt/Util.kt",
        add: ("src/main/java/com/acme/core/Oracle.java", JAVA_ADDED),
        delete: "src/main/scala/com/acme/sc/Pricing.scala",
    },
];

pub const JAVA_ADDED: &str = "package com.acme.core;\n\npublic class Oracle {\n    public int value() {\n        return 42;\n    }\n}\n";

pub fn copy_dir(from: &Path, to: &Path) {
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

pub fn run(repo: &Path, config: &Path, mock: Option<&Path>, args: &[&str]) -> std::process::Output {
    let mut c = Command::new(cargo_bin("nephila"));
    c.arg("-C")
        .arg(repo)
        .args(args)
        .env("NEPHILA_CONFIG", config)
        .env_remove("NEPHILA_MOCK_LOG")
        .env_remove("NEPHILA_MOCK_FAIL_AFTER")
        .env_remove("NEPHILA_MOCK_DELAY_MS")
        .env_remove("NEPHILA_MOCK_INVALID_KEY");
    match mock {
        Some(m) => c.env("NEPHILA_MOCK_DIR", m),
        None => c.env_remove("NEPHILA_MOCK_DIR"),
    };
    c.output().unwrap()
}

/// Modification, ajout et suppression d'un scénario de REQ-065 (étape `update`).
pub fn apply_case(repo: &Path, c: &Case) {
    let modified = repo.join(c.modify);
    let mut text = fs::read_to_string(&modified).unwrap();
    text.push_str(if c.modify.ends_with(".sql") {
        "\nCREATE TABLE oracle_mod (id INT);\n"
    } else if c.modify.ends_with(".kt")
        || c.modify.ends_with(".java")
        || c.modify.ends_with(".ts")
        || c.modify.ends_with(".gradle")
    {
        "\n// modifié par l'oracle\n"
    } else {
        "\nmodifié par l'oracle\n"
    });
    fs::write(&modified, text).unwrap();
    let added = repo.join(c.add.0);
    fs::create_dir_all(added.parent().unwrap()).unwrap();
    fs::write(&added, c.add.1).unwrap();
    fs::remove_file(repo.join(c.delete)).unwrap();
}

/// Export JSON normalisé : le tableau `unresolved` est trié par (`source`, `kind`, `target`),
/// doublons conservés (son ordre suit les identifiants de `refs`, qu'un `update` change : DEC-080) ;
/// `nodes` et `edges` sont intacts.
pub fn normalized_export(raw: &[u8]) -> String {
    let mut doc: serde_json::Value = serde_json::from_slice(raw).expect("export JSON illisible");
    if let Some(list) = doc.get_mut("unresolved").and_then(|u| u.as_array_mut()) {
        let key = |v: &serde_json::Value| {
            let f = |k: &str| v.get(k).map(|x| x.to_string()).unwrap_or_default();
            (f("source"), f("kind"), f("target"))
        };
        list.sort_by_key(key);
    }
    serde_json::to_string(&doc).unwrap()
}

/// Export normalisé comme `normalized_export`, sans la clé `communities` ni les clés `community`
/// des nœuds (REQ-149, AC-149-2) : les oracles des versions antérieures à la 0.25.0 les ignorent.
pub fn normalized_export_without_communities(raw: &[u8]) -> String {
    let mut doc: serde_json::Value = serde_json::from_str(&normalized_export(raw)).unwrap();
    let obj = doc.as_object_mut().expect("export JSON : objet");
    obj.remove("communities");
    if let Some(nodes) = obj.get_mut("nodes").and_then(|n| n.as_array_mut()) {
        for n in nodes {
            n.as_object_mut().unwrap().remove("community");
        }
    }
    serde_json::to_string(&doc).unwrap()
}

/// Empreinte d'un export JSON : BLAKE3 (hexadécimal) de l'export normalisé, sans les clés de
/// communauté (REQ-149). L'ordre de `unresolved` n'est pas garanti d'un lancement à l'autre
/// (DEC-081) ; seule définition, utilisée par tous les oracles.
pub fn export_fingerprint(raw: &[u8]) -> String {
    blake3::hash(normalized_export_without_communities(raw).as_bytes())
        .to_hex()
        .to_string()
}

/// Découpe `text` en `parts` morceaux au plus, sur des frontières de caractères.
fn split_text(text: &str, parts: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let size = chars.len().div_ceil(parts.max(1)).max(1);
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

fn event(name: &str, data: serde_json::Value) -> (u64, String) {
    (0, format!("event: {name}\ndata: {data}\n\n"))
}

/// Flux SSE Anthropic (REQ-249, AC-249-2) : `message_start` (`usage` 10 entrées), bloc `thinking`
/// à deux `thinking_delta`, `ping`, bloc `text` en `parts` `text_delta`, `message_delta`
/// (`end_turn`, 7 sorties), `message_stop`.
pub fn anthropic_sse(text: &str, parts: usize) -> Vec<(u64, String)> {
    use serde_json::json;
    let mut v = vec![
        event(
            "message_start",
            json!({"type":"message_start","message":{"id":"m","role":"assistant","content":[],
                "usage":{"input_tokens":10,"output_tokens":1}}}),
        ),
        event(
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        ),
    ];
    for t in ["je réfléchis ", "puis je réponds"] {
        v.push(event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":t}}),
        ));
    }
    v.push(event(
        "content_block_stop",
        json!({"type":"content_block_stop","index":0}),
    ));
    v.push(event("ping", json!({"type":"ping"})));
    v.push(event(
        "content_block_start",
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
    ));
    for t in split_text(text, parts) {
        v.push(event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":t}}),
        ));
    }
    v.push(event(
        "content_block_stop",
        json!({"type":"content_block_stop","index":1}),
    ));
    v.push(event(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
    ));
    v.push(event("message_stop", json!({"type":"message_stop"})));
    v
}

/// Flux SSE OpenAI (REQ-249, AC-249-3) : morceau `role`, `parts` morceaux `delta.content`,
/// morceau `finish_reason` `stop`, morceau `usage` (10 entrées, 7 sorties) à `choices` vide,
/// `data: [DONE]`.
pub fn openai_sse(text: &str, parts: usize) -> Vec<(u64, String)> {
    use serde_json::json;
    let data = |v: serde_json::Value| (0, format!("data: {v}\n\n"));
    let mut v = vec![data(
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
    )];
    for t in split_text(text, parts) {
        v.push(data(
            json!({"choices":[{"index":0,"delta":{"content":t},"finish_reason":null}]}),
        ));
    }
    v.push(data(
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ));
    v.push(data(
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":7}}),
    ));
    v.push((0, "data: [DONE]\n\n".into()));
    v
}
