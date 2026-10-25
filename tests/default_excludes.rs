//! Exclusions par défaut élargies et clé `exclude_defaults` (REQ-079, DEC-091).

#[path = "support/mod.rs"]
mod support;

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

const HELP_LINE: &str = "Exclus par défaut, à tout niveau : dist/, out/, coverage/, .next/, .nuxt/, vendor/, .idea/ et *.min.js ; pour les analyser : exclude_defaults = false dans le même fichier";

const INDEXED_DEFAULT: [&str; 4] = [
    "A.java",
    "lib/app.min.mjs",
    "outils/T.java",
    "distrib/D.java",
];
const DEFAULT_EXCLUDED: [&str; 10] = [
    "dist/a.js",
    "web/dist/b.js",
    "out/O.java",
    "m/out/P.java",
    "coverage/c.json",
    ".next/n.js",
    "app/.nuxt/x.js",
    "vendor/v.js",
    ".idea/workspace.xml",
    "lib/jquery.min.js",
];

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let e = Env {
            config: tmp.path().join("absent.toml"),
            log: tmp.path().join("mock.log"),
            repo,
            _tmp: tmp,
        };
        e.put("A.java", "package p;\npublic class A {}\n");
        e.put("dist/a.js", "function a() {}\n");
        e.put("web/dist/b.js", "function b() {}\n");
        e.put("out/O.java", "package o;\npublic class O {}\n");
        e.put("m/out/P.java", "package m;\npublic class P {}\n");
        e.put("coverage/c.json", "{\"c\": 1}\n");
        e.put(".next/n.js", "function n() {}\n");
        e.put("app/.nuxt/x.js", "function x() {}\n");
        e.put("vendor/v.js", "function v() {}\n");
        e.put(".idea/workspace.xml", "<project/>\n");
        e.put("lib/jquery.min.js", "function j() {}\n");
        e.put("lib/app.min.mjs", "function m() {}\n");
        e.put("lib/App.MIN.JS", "function M() {}\n");
        e.put("outils/T.java", "package t;\npublic class T {}\n");
        e.put("distrib/D.java", "package d;\npublic class D {}\n");
        e
    }

    fn put(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn project(&self, content: &str) {
        self.put(".nephila.toml", content);
    }

    fn command(&self) -> Command {
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env(
                "NEPHILA_MOCK_DIR",
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock"),
            );
        c
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.command()
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> (String, String) {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "{args:?} : {}",
            String::from_utf8_lossy(&o.stderr)
        );
        (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn export(&self) -> String {
        self.ok(&["export", "--output", "-"]).0
    }

    fn normalized(&self) -> String {
        support::normalized_export(self.export().as_bytes())
    }
}

fn assert_indexed(graph: &str, indexed: &[&str], absent: &[&str]) {
    for p in indexed {
        assert!(graph.contains(&format!("\"{p}\"")), "{p} absent du graphe");
    }
    for p in absent {
        assert!(
            !graph.contains(&format!("\"{p}\"")),
            "{p} présent dans le graphe"
        );
    }
}

#[test]
fn req079_ac1_exclusions_par_defaut() {
    let e = Env::new();
    let (out, err) = e.ok(&["build", "--no-llm"]);
    assert!(!out.contains("exclus"), "{out}");
    assert_eq!(err, "");
    let g = e.export();
    assert_indexed(&g, &INDEXED_DEFAULT, &DEFAULT_EXCLUDED);
    assert_eq!(e.ok(&["status"]).0.trim(), "graphe à jour");
    let (_, err) = e.ok(&["query", "search", "T", "--check-stale"]);
    assert_eq!(err, "");
}

#[test]
fn req079_ac2_desactivation() {
    let e = Env::new();
    for f in [
        "target/T1.java",
        "build/B1.java",
        ".gradle/G1.java",
        "node_modules/n/N1.js",
    ] {
        e.put(f, "class X {}\n");
    }
    e.project("exclude_defaults = false\n");
    e.ok(&["build", "--no-llm"]);
    let g = e.export();
    assert_indexed(
        &g,
        &[],
        &[
            "target/T1.java",
            "build/B1.java",
            ".gradle/G1.java",
            "node_modules/n/N1.js",
        ],
    );
    let all: Vec<&str> = INDEXED_DEFAULT
        .iter()
        .chain(DEFAULT_EXCLUDED.iter())
        .copied()
        .collect();
    assert_indexed(&g, &all, &["lib/App.MIN.JS"]);

    e.project("exclude_defaults = false\nexclude = [\"vendor/\", \"*.min.js\"]\n");
    let (out, _) = e.ok(&["build", "--no-llm"]);
    assert!(
        out.trim_end()
            .ends_with(", 2 fichiers exclus par les filtres"),
        "{out}"
    );
    let g = e.export();
    assert_indexed(
        &g,
        &["dist/a.js", "out/O.java"],
        &["vendor/v.js", "lib/jquery.min.js"],
    );

    // exclude_defaults = true : même graphe que sans la clé.
    let a = Env::new();
    a.ok(&["build", "--no-llm"]);
    let b = Env::new();
    b.project("exclude_defaults = true\n");
    b.ok(&["build", "--no-llm"]);
    assert_eq!(a.normalized(), b.normalized());
}

#[test]
fn req079_ac3_pas_de_reinclusion_par_motif() {
    let e = Env::new();
    e.project("exclude = [\"!dist/\", \"!**/*.min.js\"]\n");
    e.ok(&["build", "--no-llm", "--exclude", "!vendor/"]);
    let g = e.export();
    assert_indexed(
        &g,
        &["A.java"],
        &["dist/a.js", "lib/jquery.min.js", "vendor/v.js"],
    );
}

#[test]
fn req079_ac4_mise_a_niveau() {
    let e = Env::new();
    e.project("exclude_defaults = false\n");
    e.ok(&["build", "--no-llm"]);
    assert_indexed(&e.export(), &["dist/a.js", "vendor/v.js"], &[]);
    fs::remove_file(e.repo.join(".nephila.toml")).unwrap();

    let (out, _) = e.ok(&["status"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "graphe pas à jour");
    assert!(lines.contains(&"supprimé\tdist/a.js"), "{out}");
    assert!(lines.contains(&"supprimé\tvendor/v.js"), "{out}");

    // Configuration mock : l'update ne fait aucun appel LLM.
    fs::write(&e.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    e.ok(&["update"]);
    assert!(
        !e.log.exists() || fs::read_to_string(&e.log).unwrap().is_empty(),
        "appel LLM inattendu"
    );
    assert_eq!(e.ok(&["status"]).0.trim(), "graphe à jour");
    let fresh = Env::new();
    fresh.ok(&["build", "--no-llm"]);
    assert_eq!(e.normalized(), fresh.normalized());
}

const E2_KEYS: &str = "exclude, exclude_types, exclude_defaults, model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries";

#[test]
fn req079_ac6_ac9_valeur_invalide_et_cles_permises() {
    let e = Env::new();
    fs::write(&e.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    for bad in ["\"non\"", "\"false\"", "0", "1", "[]"] {
        e.project(&format!("exclude_defaults = {bad}\n"));
        let want = format!(
            "erreur: {}/.nephila.toml : clé `exclude_defaults` : booléen attendu, ex. exclude_defaults = false\n",
            e.repo.display()
        );
        for args in [
            &["build"][..],
            &["update"],
            &["status"],
            &["query", "search", "x"],
        ] {
            let o = e.run(args);
            assert!(!o.status.success(), "{args:?} {bad}");
            assert_eq!(String::from_utf8_lossy(&o.stderr), want, "{args:?} {bad}");
            assert!(o.stdout.is_empty());
            assert!(!e.repo.join(".nephila").exists());
            assert!(!e.log.exists());
        }
    }
    // Sans -C : chemin relatif.
    let o = e
        .command()
        .current_dir(&e.repo)
        .args(["build"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        "erreur: ./.nephila.toml : clé `exclude_defaults` : booléen attendu, ex. exclude_defaults = false\n"
    );
    // export et summary lisent un graphe existant sans le fichier de projet.
    e.project("");
    e.ok(&["build", "--no-llm"]);
    e.project("exclude_defaults = \"non\"\n");
    e.ok(&["export", "--output", "-"]);
    e.ok(&["summary"]);

    e.project("exclud = 1\n");
    let o = e.run(&["build"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stderr),
        format!(
            "erreur: {}/.nephila.toml : clé inconnue `exclud` (clés permises : {E2_KEYS})\n",
            e.repo.display()
        )
    );
}

#[test]
fn req079_ac6_configuration_personnelle() {
    let e = Env::new();
    fs::write(
        &e.config,
        "provider = \"mock\"\nmodel = \"mock\"\nexclude_defaults = false\n",
    )
    .unwrap();
    let o = e.run(&["build"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    let line = err.lines().next().unwrap();
    assert!(
        line.starts_with("erreur: configuration invalide ("),
        "{err}"
    );
    assert!(err.contains("exclude_defaults"), "{err}");
}

fn flat(s: &[u8]) -> String {
    String::from_utf8_lossy(s)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn req079_ac8_aide_de_build_et_update() {
    let e = Env::new();
    for cmd in ["build", "update"] {
        let o = e.run(&[cmd, "--help"]);
        assert!(o.status.success());
        assert!(!o.stdout.contains(&0x1b) && o.stderr.is_empty());
        let t = flat(&o.stdout);
        let ex = t.find("exclude_types = [\"json\"]").expect("exemple");
        let l = t
            .find(HELP_LINE)
            .unwrap_or_else(|| panic!("{cmd} : ligne absente\n{t}"));
        let llm = t.find("Réglages LLM du dépôt").unwrap();
        assert!(ex < l && l < llm, "{cmd} : ordre");
    }
    let o = e.run(&["status", "--help"]);
    assert!(!String::from_utf8_lossy(&o.stdout).contains("exclude_defaults"));
}

// ---- REQ-114 : trace `parcours` des exclusions par défaut ----

/// Ligne `parcours:` de stderr, sans le préfixe `trace: [..] `.
fn walk_line(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("trace: [") && l.contains("] parcours: "))
        .collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    lines[0].split_once("] ").unwrap().1.to_string()
}

fn verbose_walk(e: &Env, args: &[&str]) -> String {
    let (_, err) = e.ok(args);
    walk_line(&err)
}

#[test]
fn req114_ac1_build_compte_les_exclusions_par_defaut() {
    let e = Env::new();
    let want = "parcours: 4 fichiers (exclusions par défaut : 9 dossiers, 1 fichiers)";
    assert_eq!(verbose_walk(&e, &["build", "--no-llm", "--verbose"]), want);
    for jobs in ["1", "8"] {
        assert_eq!(
            verbose_walk(&e, &["build", "--no-llm", "--verbose", "--jobs", jobs]),
            want,
            "--jobs {jobs}"
        );
    }
    let quiet = e.ok(&["build", "--no-llm"]);
    let loud = e.ok(&["build", "--no-llm", "--verbose"]);
    assert_eq!(quiet.0, loud.0);
    assert!(!quiet.1.contains("exclusions par défaut"), "{}", quiet.1);
}

#[test]
fn req114_ac2_gitignore_et_desactivation() {
    let e = Env::new();
    e.put(".gitignore", "dist/\n");
    let line = verbose_walk(&e, &["build", "--no-llm", "--verbose"]);
    assert!(
        line.ends_with(" fichiers (exclusions par défaut : 7 dossiers, 1 fichiers)"),
        "{line}"
    );

    let e = Env::new();
    e.project("exclude_defaults = false\n");
    let line = verbose_walk(&e, &["build", "--no-llm", "--verbose"]);
    assert!(!line.contains("exclusions par défaut"), "{line}");
    assert!(!line.contains("(filtres"), "{line}");

    e.project("exclude_defaults = false\nexclude = [\"vendor/\", \"*.min.js\"]\n");
    let line = verbose_walk(&e, &["build", "--no-llm", "--verbose"]);
    assert!(
        line.ends_with(" (filtres : 2 motifs, types aucun, 2 fichiers exclus)"),
        "{line}"
    );
    assert!(!line.contains("exclusions par défaut"), "{line}");
}

#[test]
fn req114_ac3_update_porte_le_meme_segment() {
    let e = Env::new();
    e.ok(&["build", "--no-llm"]);
    e.put("A.java", "package p;\npublic class A { int x; }\n");
    let line = verbose_walk(&e, &["update", "--no-llm", "--verbose"]);
    assert_eq!(
        line,
        "parcours: 0 ajoutés, 1 modifiés, 0 supprimés, 0 en attente (exclusions par défaut : 9 dossiers, 1 fichiers)"
    );
}

#[test]
fn req114_ac5_texte_et_place_du_segment() {
    let e = Env::new();
    let line = verbose_walk(&e, &["build", "--no-llm", "--verbose"]);
    assert!(
        line.ends_with(" fichiers (exclusions par défaut : 9 dossiers, 1 fichiers)"),
        "{line}"
    );
    let line = verbose_walk(
        &e,
        &[
            "build",
            "--no-llm",
            "--verbose",
            "--exclude",
            "outils/T.java",
        ],
    );
    assert!(
        line.ends_with(
            " (exclusions par défaut : 9 dossiers, 1 fichiers) (filtres : 1 motifs, types aucun, 1 fichiers exclus)"
        ),
        "{line}"
    );

    // Seul chemin exclu par défaut : un fichier ; les deux nombres restent ensemble.
    let tmp = Env::new();
    fs::remove_dir_all(&tmp.repo).unwrap();
    fs::create_dir_all(&tmp.repo).unwrap();
    tmp.put("A.java", "package p;\npublic class A {}\n");
    tmp.put("lib/jquery.min.js", "function j() {}\n");
    let line = verbose_walk(&tmp, &["build", "--no-llm", "--verbose"]);
    assert_eq!(
        line,
        "parcours: 1 fichiers (exclusions par défaut : 0 dossiers, 1 fichiers)"
    );

    // Aucun chemin exclu par défaut : ligne de HEAD.
    tmp.put("lib/jquery.min.js", "function j() {}\n");
    fs::remove_file(tmp.repo.join("lib/jquery.min.js")).unwrap();
    let line = verbose_walk(&tmp, &["build", "--no-llm", "--verbose"]);
    assert_eq!(line, "parcours: 1 fichiers");
}
