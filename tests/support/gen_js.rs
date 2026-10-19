//! Générateur de dépôt JavaScript/TypeScript synthétique (REQ-021, AC-021-7).

use std::fs;
use std::path::Path;

pub const FILES: usize = 3_000;
/// Fichier `.ts` modifié par le test (indice multiple de 6).
pub const EDITED: usize = 1236;
const PER_DIR: usize = 100;
const METHODS: usize = 5;
const FUNCS: usize = 5;
const _: () = assert!(FUNCS <= METHODS);

pub fn class_name(i: usize) -> String {
    format!("Gen{i:05}")
}

/// Deux tiers de `.ts`, le reste réparti entre `.js`, `.tsx` et `.jsx`.
fn ext(i: usize) -> &'static str {
    match i % 6 {
        0..=3 => "ts",
        4 => "js",
        _ => {
            if (i / 6).is_multiple_of(2) {
                "tsx"
            } else {
                "jsx"
            }
        }
    }
}

fn dir(i: usize) -> String {
    format!("src/p{:03}", i / PER_DIR)
}

pub fn path(i: usize) -> String {
    format!("{}/{}.{}", dir(i), class_name(i), ext(i))
}

fn is_typed(i: usize) -> bool {
    matches!(ext(i), "ts" | "tsx")
}

fn source(i: usize) -> String {
    let (cls, typed) = (class_name(i), is_typed(i));
    let (num, ret) = if typed {
        (": number", ": number")
    } else {
        ("", "")
    };
    let targets = [(i + 1) % FILES, (i * 7 + 13) % FILES];
    let mut s = String::new();
    for t in targets {
        let tc = class_name(t);
        let rel = if dir(t) == dir(i) {
            format!("./{tc}")
        } else {
            format!("../p{:03}/{tc}", t / PER_DIR)
        };
        let fns: Vec<String> = (0..METHODS).map(|k| format!("f{t}_{k}")).collect();
        s.push_str(&format!(
            "import {{ {tc}, {} }} from '{rel}';\n",
            fns.join(", ")
        ));
    }
    s.push_str(&format!("\nexport class {cls} {{\n"));
    for m in 0..METHODS {
        s.push_str(&format!(
            "  m{m}(x{num}){ret} {{\n    return f{}_{m}(x) + f{}_{m}(x + 1);\n  }}\n",
            targets[0], targets[1]
        ));
    }
    s.push_str("}\n");
    for k in 0..FUNCS {
        let (a, b) = (targets[0], targets[1]);
        let (c, d) = (k % 2, (k + 1) % 2);
        s.push_str(&format!(
            "\nexport function f{i}_{k}(x{num}){ret} {{\n  return f{a}_{c}(x) + f{b}_{d}(x);\n}}\n"
        ));
    }
    s.push_str(&format!(
        "\nexport function make{i}() {{\n  return new {}();\n}}\n",
        class_name(targets[0])
    ));
    if matches!(ext(i), "tsx" | "jsx") {
        s.push_str(&format!(
            "\nexport function View{i}() {{\n  return (\n    <section className=\"v{i}\">\n      <span>{{{i}}}</span>\n    </section>\n  );\n}}\n"
        ));
    }
    s
}

/// Écrit `FILES` fichiers sous `repo` et un `package.json` ; `mock` reçoit des fixtures
/// (schéma version 2) pour les fichiers dont l'indice est 4 modulo 10, et pour le fichier `EDITED`.
pub fn generate(repo: &Path, mock: &Path) {
    fs::create_dir_all(mock).unwrap();
    write(
        &repo.join("package.json"),
        r#"{"name":"gen-js","version":"1.0.0","dependencies":{"left-pad":"1.3.0"}}"#,
    );
    for i in 0..FILES {
        write(&repo.join(path(i)), &source(i));
    }
    for i in (0..FILES).filter(|i| i % 10 == 4 || *i == EDITED) {
        let node = format!(
            "type:{}.{}",
            path(i).rsplit_once('.').unwrap().0,
            class_name(i)
        );
        let word = if i == EDITED {
            SUMMARY_WORD
        } else {
            "standard"
        };
        write(
            &mock.join(format!("{}.json", path(i))),
            &format!(
                r#"{{"version":2,"summaries":[{{"node":"{node}","summary":"Résumé {word} de la classe numéro {i}"}}],"concepts":[{{"name":"concept{:02}","sections":[]}}]}}"#,
                i % 20
            ),
        );
    }
}

/// Mot unique du résumé de la fixture du fichier `EDITED`.
pub const SUMMARY_WORD: &str = "zorglubation";

fn write(p: &Path, content: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}
