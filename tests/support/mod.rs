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
