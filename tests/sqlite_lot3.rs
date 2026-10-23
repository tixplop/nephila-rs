//! Oracle de la 0.20.0 pour le lot 3 SQLite (REQ-065, AC-065-1) : pour chaque dépôt de
//! `tests/fixtures` et chaque étape (`build --no-llm`, `build` avec le mock, `update` après
//! modification, ajout et suppression, second `build`), empreinte BLAKE3 de l'export JSON, de
//! `status` et de `summary`, avec le code de sortie.
//!
//! `NEPHILA_ORACLE_CAPTURE=1` réécrit `tests/fixtures/oracle_0_20_0.txt` (une ligne
//! `dépôt<TAB>étape<TAB>sortie<TAB>code<TAB>empreinte`, triée) ; sans, le test compare.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::Path;
use std::process::Command;

struct Case {
    name: &'static str,
    repo: &'static str,
    mock: Option<&'static str>,
    modify: &'static str,
    add: (&'static str, &'static str),
    delete: &'static str,
}

const CASES: &[Case] = &[
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

const JAVA_ADDED: &str = "package com.acme.core;\n\npublic class Oracle {\n    public int value() {\n        return 42;\n    }\n}\n";

fn copy_dir(from: &Path, to: &Path) {
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

fn run(repo: &Path, config: &Path, mock: Option<&Path>, args: &[&str]) -> std::process::Output {
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

fn fingerprints(
    lines: &mut Vec<String>,
    case: &str,
    step: &str,
    repo: &Path,
    config: &Path,
    mock: Option<&Path>,
) {
    let outputs: [(&str, &[&str]); 3] = [
        ("export", &["export", "--format", "json", "--output", "-"]),
        ("status", &["status"]),
        ("summary", &["summary"]),
    ];
    for (label, args) in outputs {
        let out = run(repo, config, mock, args);
        let code = out.status.code().map_or(-1, i64::from);
        let hash = blake3::hash(&out.stdout).to_hex();
        lines.push(format!("{case}\t{step}\t{label}\t{code}\t{hash}"));
    }
}

fn scenario(c: &Case) -> Vec<String> {
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    copy_dir(&fx.join(c.repo), &repo);
    let mock = c.mock.map(|m| {
        let d = tmp.path().join("mock");
        copy_dir(&fx.join(m), &d);
        d
    });
    let mock = mock.as_deref();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\n",
    )
    .unwrap();
    let mut lines = Vec::new();
    let ok = |step: &str, args: &[&str]| {
        let out = run(&repo, &config, mock, args);
        assert!(
            out.status.success(),
            "{} / {step} : {}",
            c.name,
            String::from_utf8_lossy(&out.stderr)
        );
    };

    ok("build --no-llm", &["build", "--no-llm"]);
    fingerprints(&mut lines, c.name, "1-build-no-llm", &repo, &config, mock);
    ok("build", &["build"]);
    fingerprints(&mut lines, c.name, "2-build-mock", &repo, &config, mock);
    if mock.is_some() {
        let export = |l: &str| l.starts_with(&format!("{}\t", c.name)) && l.contains("\texport\t");
        let hash = |step: &str| {
            lines
                .iter()
                .find(|l| export(l) && l.contains(&format!("\t{step}\t")))
                .and_then(|l| l.rsplit('\t').next())
                .map(str::to_string)
        };
        assert_ne!(
            hash("1-build-no-llm"),
            hash("2-build-mock"),
            "{} : le mock n'enrichit pas le dépôt (enrich_code ?)",
            c.name
        );
    }

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
    ok("update", &["update"]);
    fingerprints(&mut lines, c.name, "3-update", &repo, &config, mock);
    ok("build", &["build"]);
    fingerprints(&mut lines, c.name, "4-build-final", &repo, &config, mock);
    lines
}

#[test]
fn req065_ac1_oracle_0_20_0() {
    let mut lines: Vec<String> = CASES.iter().flat_map(scenario).collect();
    lines.sort();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle_0_20_0.txt");
    let actual = lines.join("\n") + "\n";
    if std::env::var_os("NEPHILA_ORACLE_CAPTURE").is_some_and(|v| v == "1") {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).expect("oracle absent : NEPHILA_ORACLE_CAPTURE=1");
    let exp: Vec<&str> = expected.lines().collect();
    for i in 0..lines.len().max(exp.len()) {
        let (a, e) = (lines.get(i).map(String::as_str), exp.get(i).copied());
        assert_eq!(
            a,
            e,
            "première ligne différente de l'oracle (ligne {})",
            i + 1
        );
    }
}
