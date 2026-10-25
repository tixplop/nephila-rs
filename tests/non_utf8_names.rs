//! Fichiers aux noms non UTF-8 (REQ-004, DEC-070) : lus par leur chemin réel à l'analyse et à
//! l'enrichissement, jamais en attente. Unix seulement ; non applicable si le système de fichiers
//! refuse la création (APFS, règle d'AC-004-8).
#![cfg(unix)]
#![allow(non_snake_case)]

use assert_cmd::cargo::cargo_bin;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

impl Env {
    /// `None` si le système de fichiers refuse les noms non UTF-8.
    fn new(config: &str) -> Option<Env> {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            mock: tmp.path().join("mock"),
            log: tmp.path().join("mock.log"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
        };
        fs::create_dir_all(&env.repo).unwrap();
        fs::create_dir_all(&env.mock).unwrap();
        fs::write(&env.config, config).unwrap();
        let name = OsStr::from_bytes(b"A\xff.java");
        if fs::write(env.repo.join(name), "class A { void run() {} }\n").is_err() {
            return None;
        }
        fs::write(env.repo.join("B.java"), "class B {}\n").unwrap();
        Some(env)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .output()
            .unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn first_line(o: &Output) -> String {
    out(o).lines().next().unwrap_or("").to_string()
}

#[test]
fn REQ004_AC9_sans_llm_le_fichier_n_est_pas_en_attente() {
    let Some(env) = Env::new("provider = \"mock\"\n") else {
        return;
    };
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!err(&o).contains("avertissement:"), "{}", err(&o));
    assert!(out(&o).contains(", 0 fichiers en attente,"), "{}", out(&o));
    let o = env.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    let o = env.run(&["query", "search", "run"]);
    assert!(out(&o).contains("A.run"), "{}", out(&o));
    assert!(out(&o).contains("A\u{FFFD}.java"), "{}", out(&o));
    let o = env.run(&["update", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = env.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    let o = env.run(&["query", "search", "A", "--check-stale"]);
    assert_eq!(err(&o), "");
}

#[test]
fn REQ004_AC9_avec_llm_simule_le_fichier_est_enrichi() {
    let Some(env) = Env::new("provider = \"mock\"\nenrich_code = true\n") else {
        return;
    };
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let log = fs::read_to_string(&env.log).unwrap_or_default();
    assert!(
        log.lines().any(|l| l.contains("A\u{FFFD}.java")),
        "journal : {log:?}"
    );
    let o = env.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    assert!(!out(&o).contains("à enrichir"), "{}", out(&o));
    assert!(!out(&o).contains("en attente"), "{}", out(&o));
    let _: &Path = &env.mock;
}

// ---------------------------------------------------------------------------------------------
// Même chemin après conversion (REQ-004, AC-004-10 à AC-004-15, DEC-071)
// ---------------------------------------------------------------------------------------------

const A7_FE_FF: &str = "avertissement: A\\xFF.java ignoré : même chemin dans le graphe que A\\xFE.java (A\u{FFFD}.java), seul retenu ; renommez ce fichier";

/// Dépôt temporaire dont les fichiers sont créés dans l'ordre donné ; `None` si le système de
/// fichiers refuse un nom non UTF-8.
struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
}

impl Repo {
    fn new(files: &[(&[u8], &str)]) -> Option<Repo> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let config = tmp.path().join("config.toml");
        fs::create_dir_all(&root).unwrap();
        fs::write(&config, "provider = \"mock\"\n").unwrap();
        for (name, content) in files {
            let p = root.join(OsStr::from_bytes(name));
            if let Some(parent) = p.parent() {
                if fs::create_dir_all(parent).is_err() {
                    return None;
                }
            }
            if fs::write(&p, content).is_err() {
                return None;
            }
        }
        Some(Repo {
            _tmp: tmp,
            root,
            config,
        })
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .output()
            .unwrap()
    }
}

/// Crée le même dépôt dans l'ordre donné puis dans l'ordre inverse.
fn both_orders(files: &[(&[u8], &str)]) -> Option<(Repo, Repo)> {
    let mut rev = files.to_vec();
    rev.reverse();
    Some((Repo::new(files)?, Repo::new(&rev)?))
}

fn a7_lines(o: &Output) -> Vec<Vec<u8>> {
    o.stderr
        .split(|b| *b == b'\n')
        .filter(|l| l.starts_with(b"avertissement:"))
        .map(<[u8]>::to_vec)
        .collect()
}

const PAIR: [(&[u8], &str); 3] = [
    (b"A\xfe.java", "class A {}\n"),
    (b"A\xff.java", "class A2 {}\n"),
    (b"B.java", "class B {}\n"),
];

#[test]
fn REQ004_AC10_scan_et_walk_ne_retiennent_qu_un_fichier() {
    use nephila::filter::Matcher;
    let files: [(&[u8], &str); 2] = [
        (b"A\xfe.java", "class A {}\n"),
        (b"A\xff.java", "class A2 {}\n"),
    ];
    let Some((r1, r2)) = both_orders(&files) else {
        return;
    };
    let mut seen = Vec::new();
    for r in [&r1, &r2] {
        let res = nephila::scan::scan(&r.root, &[], &Matcher::none(), 1).unwrap();
        assert_eq!(res.files.len(), 1, "{:?}", res.warnings);
        assert_eq!(res.files[0].path, "A\u{FFFD}.java");
        assert_eq!(
            res.files[0].hash,
            blake3::hash(b"class A {}\n").to_hex().to_string()
        );
        assert_eq!(res.warnings.len(), 1, "{:?}", res.warnings);
        for part in ["A\u{FFFD}.java", "A\\xFF.java", "A\\xFE.java"] {
            assert!(res.warnings[0].contains(part), "{:?}", res.warnings);
        }
        let w = nephila::scan::walk(&r.root, &Matcher::none(), 1, &[]).unwrap();
        assert_eq!(w.files.len(), 1);
        assert_eq!(w.files[0].path, "A\u{FFFD}.java");
        assert_eq!(w.collisions.len(), 1);
        seen.push((res.warnings.clone(), w.files[0].path.clone()));
    }
    assert_eq!(seen[0], seen[1]);
}

#[test]
fn REQ004_AC10_nom_utf8_valide_contre_nom_non_utf8() {
    use nephila::filter::Matcher;
    let files: [(&[u8], &str); 2] = [
        ("A\u{FFFD}.java".as_bytes(), "class A {}\n"),
        (b"A\x80.java", "class A2 {}\n"),
    ];
    let Some((r1, r2)) = both_orders(&files) else {
        return;
    };
    for r in [&r1, &r2] {
        let res = nephila::scan::scan(&r.root, &[], &Matcher::none(), 1).unwrap();
        assert_eq!(res.files.len(), 1, "{:?}", res.warnings);
        assert_eq!(
            res.files[0].hash,
            blake3::hash(b"class A {}\n").to_hex().to_string()
        );
        assert_eq!(res.warnings.len(), 1);
    }
}

#[test]
fn REQ004_AC10_build_update_status_et_check_stale() {
    let Some((r1, r2)) = both_orders(&PAIR) else {
        return;
    };
    for r in [&r1, &r2] {
        let o = r.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert!(!err(&o).contains("UNIQUE"), "{}", err(&o));
        assert_eq!(a7_lines(&o).len(), 1, "{}", err(&o));
        let o = r.run(&["query", "search", "A"]);
        assert!(out(&o).contains("A\u{FFFD}.java"), "{}", out(&o));
        assert!(!out(&o).contains("A2"), "{}", out(&o));
        let o = r.run(&["update", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        let o = r.run(&["status"]);
        assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
        let o = r.run(&["query", "search", "B", "--check-stale"]);
        assert_eq!(err(&o), "");
    }
}

#[test]
fn REQ004_AC11_ligne_a7_identique_octet_pour_octet() {
    let Some((r1, r2)) = both_orders(&PAIR) else {
        return;
    };
    let expected = A7_FE_FF.as_bytes().to_vec();
    let mut line = expected.clone();
    line.push(b'\n');
    for r in [&r1, &r2] {
        let o = r.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(a7_lines(&o), vec![expected.clone()], "{}", err(&o));
        assert!(
            out(&o).starts_with("build terminé : 2 fichiers traités,"),
            "{}",
            out(&o)
        );
        let o = r.run(&["update", "--no-llm"]);
        assert_eq!(out(&o).trim_end(), "graphe à jour");
        assert_eq!(o.stderr, line);
        let o = r.run(&["status"]);
        assert_eq!(first_line(&o), "graphe à jour");
        assert_eq!(o.stderr, line);
        let o = r.run(&["query", "search", "B", "--check-stale"]);
        assert_eq!(err(&o), "");
    }
    for extra in ["--verbose", "--show-progress"] {
        let Some((r1, r2)) = both_orders(&PAIR) else {
            return;
        };
        for r in [&r1, &r2] {
            let o = r.run(&["build", "--no-llm", extra]);
            assert_eq!(o.status.code(), Some(0), "{}", err(&o));
            assert_eq!(a7_lines(&o), vec![expected.clone()], "{}", err(&o));
            if extra == "--show-progress" {
                assert!(
                    err(&o).contains("progression: extraction 100 % (2/2 fichiers)"),
                    "{}",
                    err(&o)
                );
            }
        }
    }
}

#[test]
fn REQ004_AC12_trois_fichiers_deux_lignes_ordonnees() {
    let files: [(&[u8], &str); 3] = [
        (b"A\xfd.java", "class A {}\n"),
        (b"A\xfe.java", "class A2 {}\n"),
        (b"A\xff.java", "class A3 {}\n"),
    ];
    let Some((r1, r2)) = both_orders(&files) else {
        return;
    };
    let expected: Vec<Vec<u8>> = ["FE", "FF"]
        .iter()
        .map(|x| {
            format!("avertissement: A\\x{x}.java ignoré : même chemin dans le graphe que A\\xFD.java (A\u{FFFD}.java), seul retenu ; renommez ce fichier")
                .into_bytes()
        })
        .collect();
    for r in [&r1, &r2] {
        let o = r.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(a7_lines(&o), expected, "{}", err(&o));
    }
}

#[test]
fn REQ004_AC13_nom_utf8_valide_retenu() {
    let files: [(&[u8], &str); 2] = [
        ("A\u{FFFD}.java".as_bytes(), "class A {}\n"),
        (b"A\x80.java", "class A2 {}\n"),
    ];
    let Some((r1, r2)) = both_orders(&files) else {
        return;
    };
    let expected = "avertissement: A\\x80.java ignoré : même chemin dans le graphe que A\u{FFFD}.java (A\u{FFFD}.java), seul retenu ; renommez ce fichier\n";
    for r in [&r1, &r2] {
        let o = r.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(o.stderr, expected.as_bytes());
    }
}

#[test]
fn REQ004_AC14_apostrophe_et_stderr_propre() {
    let files: [(&[u8], &str); 2] = [
        (b"src/l'a\xfe.java", "class A {}\n"),
        (b"src/l'a\xff.java", "class A2 {}\n"),
    ];
    let Some((r1, r2)) = both_orders(&files) else {
        return;
    };
    let expected = "avertissement: src/l\\'a\\xFF.java ignoré : même chemin dans le graphe que src/l\\'a\\xFE.java (src/l'a\u{FFFD}.java), seul retenu ; renommez ce fichier\n";
    for r in [&r1, &r2] {
        let o = r.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(o.stderr, expected.as_bytes());
        assert!(std::str::from_utf8(&o.stderr).is_ok());
        assert!(!o.stderr.contains(&0x1B) && !o.stderr.contains(&0x0D));
    }
}

#[test]
fn REQ004_AC15_renommer_le_fichier_ignore() {
    let Some((r1, r2)) = both_orders(&PAIR) else {
        return;
    };
    for r in [&r1, &r2] {
        let o = r.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        fs::rename(
            r.root.join(OsStr::from_bytes(b"A\xff.java")),
            r.root.join("A2.java"),
        )
        .unwrap();
        let o = r.run(&["status"]);
        assert!(
            out(&o).lines().any(|l| l == "ajouté\tA2.java"),
            "{}",
            out(&o)
        );
        assert!(a7_lines(&o).is_empty(), "{}", err(&o));
        let o = r.run(&["update", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(err(&o), "");
        let o = r.run(&["status"]);
        assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
        assert_eq!(err(&o), "");
    }
}

// ---------------------------------------------------------------------------------------------
// Chemin converti échappé et contrôles C1 / marques de direction (REQ-100, REQ-101, DEC-103)
// ---------------------------------------------------------------------------------------------

#[test]
fn REQ100_AC1_chemin_converti_echappe_dans_a7() {
    let files: [(&[u8], &str); 2] = [
        (b"\x1b[31mX\xfe.java", "class X {}\n"),
        (b"\x1b[31mX\xff.java", "class X2 {}\n"),
    ];
    let Some((r1, _)) = both_orders(&files) else {
        return;
    };
    let expected = "avertissement: \\x1B[31mX\\xFF.java ignoré : même chemin dans le graphe que \\x1B[31mX\\xFE.java (\\x1B[31mX\u{FFFD}.java), seul retenu ; renommez ce fichier\n";
    for args in [
        &["build", "--no-llm"][..],
        &["update", "--no-llm"],
        &["status"],
    ] {
        let o = r1.run(args);
        assert_eq!(err(&o), expected, "{args:?}");
        assert!(!o.stderr.contains(&0x1B), "{args:?}");
    }
}

#[test]
fn REQ101_AC2_marque_de_direction_dans_a7() {
    let files: [(&[u8], &str); 2] = [
        (b"\xe2\x80\xaeA\xfe.java", "class A {}\n"),
        (b"\xe2\x80\xaeA\xff.java", "class A2 {}\n"),
    ];
    let Some((r1, _)) = both_orders(&files) else {
        return;
    };
    let o = r1.run(&["build", "--no-llm"]);
    let expected = "avertissement: \\u{202E}A\\xFF.java ignoré : même chemin dans le graphe que \\u{202E}A\\xFE.java (\\u{202E}A\u{FFFD}.java), seul retenu ; renommez ce fichier\n";
    assert_eq!(err(&o), expected);
    assert!(!o.stderr.windows(3).any(|w| w == b"\xe2\x80\xae"));
}

#[test]
fn REQ100_AC3_non_utf8_et_lecture_impossible_echappes() {
    use std::os::unix::fs::PermissionsExt;
    let files: [(&[u8], &str); 2] = [
        (b"\x1b[1mbad.java", "x"),
        (b"\x1b[1mro.java", "class R {}\n"),
    ];
    let Some(r) = Repo::new(&files) else {
        return;
    };
    fs::write(
        r.root.join(OsStr::from_bytes(b"\x1b[1mbad.java")),
        b"class \xff\xfe {}",
    )
    .unwrap();
    let ro = r.root.join(OsStr::from_bytes(b"\x1b[1mro.java"));
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read(&ro).is_ok();
    let o = r.run(&["build", "--no-llm"]);
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o644)).unwrap();
    let e = err(&o);
    assert!(!o.stderr.contains(&0x1B), "{e}");
    assert!(
        e.lines()
            .any(|l| l == "avertissement: \\x1B[1mbad.java ignoré : fichier non UTF-8"),
        "{e}"
    );
    if !readable {
        assert!(
            e.lines()
                .any(|l| l
                    .starts_with("avertissement: \\x1B[1mro.java ignoré : lecture impossible (")),
            "{e}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Fichier déjà indexé retenu en cas de collision (REQ-103, DEC-104) ; couverture d'`update`
// ---------------------------------------------------------------------------------------------

const A7_FF_FE: &str = "avertissement: A\\xFE.java ignoré : même chemin dans le graphe que A\\xFF.java (A\u{FFFD}.java), seul retenu ; renommez ce fichier";

/// Noms des types trouvés par `query search` (sortie brute, pour les recherches ciblées).
fn search(r: &Repo, q: &str) -> String {
    out(&r.run(&["query", "search", q]))
}

#[test]
fn REQ004_AC16_update_avec_llm_simule_enrichit_le_nouveau_fichier() {
    let Some(env) = Env::new("provider = \"mock\"\nenrich_code = true\n") else {
        return;
    };
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let before = fs::read_to_string(&env.log)
        .unwrap_or_default()
        .lines()
        .count();
    if fs::write(
        env.repo.join(OsStr::from_bytes(b"C\xff.java")),
        "class C { void go() {} }\n",
    )
    .is_err()
    {
        return;
    }
    let o = env.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(!err(&o).contains("avertissement:"), "{}", err(&o));
    let log = fs::read_to_string(&env.log).unwrap_or_default();
    let added: Vec<&str> = log.lines().skip(before).collect();
    assert!(
        added.iter().any(|l| l.contains("C\u{FFFD}.java")),
        "ajouts au journal : {added:?}"
    );
    let o = env.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    assert!(!out(&o).contains("à enrichir"), "{}", out(&o));
    assert!(!out(&o).contains("en attente"), "{}", out(&o));
    let o = env.run(&["query", "search", "go"]);
    assert!(out(&o).contains("C.go"), "{}", out(&o));
    assert!(out(&o).contains("C\u{FFFD}.java"), "{}", out(&o));
}

#[test]
fn REQ004_AC17_collision_de_dossiers_non_utf8() {
    let files: [(&[u8], &str); 3] = [
        (b"d\xfe/A.java", "class A {}\n"),
        (b"d\xff/A.java", "class A2 {}\n"),
        (b"B.java", "class B {}\n"),
    ];
    let Some((r1, r2)) = both_orders(&files) else {
        return;
    };
    let line = "avertissement: d\\xFF/A.java ignoré : même chemin dans le graphe que d\\xFE/A.java (d\u{FFFD}/A.java), seul retenu ; renommez ce fichier\n";
    for r in [&r1, &r2] {
        let o = r.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert!(
            out(&o).starts_with("build terminé : 2 fichiers traités,"),
            "{}",
            out(&o)
        );
        assert_eq!(o.stderr, line.as_bytes());
        let s = search(r, "A");
        assert!(s.contains("d\u{FFFD}/A.java"), "{s}");
        assert!(!s.contains("A2"), "{s}");
        let o = r.run(&["update", "--no-llm"]);
        assert_eq!(out(&o).trim_end(), "graphe à jour");
        assert_eq!(o.stderr, line.as_bytes());
        let o = r.run(&["status"]);
        assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
        assert_eq!(o.stderr, line.as_bytes());
        let o = r.run(&["query", "search", "B", "--check-stale"]);
        assert_eq!(err(&o), "");
    }
}

const INDEXED_FF: [(&[u8], &str); 2] = [
    (b"A\xff.java", "class A2 {}\n"),
    (b"B.java", "class B {}\n"),
];

#[test]
fn REQ103_AC1_le_fichier_deja_indexe_reste_retenu() {
    let Some(r) = Repo::new(&INDEXED_FF) else {
        return;
    };
    let o = r.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    if fs::write(
        r.root.join(OsStr::from_bytes(b"A\xfe.java")),
        "class A {}\n",
    )
    .is_err()
    {
        return;
    }
    let line = format!("{A7_FF_FE}\n");
    let o = r.run(&["status"]);
    assert_eq!(first_line(&o), "graphe à jour", "{}", out(&o));
    assert_eq!(o.stderr, line.as_bytes());
    let o = r.run(&["update", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o).trim_end(), "graphe à jour");
    assert_eq!(o.stderr, line.as_bytes());
    assert!(search(&r, "A2").contains("A2"));
    let types = search(&r, "A");
    assert!(
        !types
            .lines()
            .any(|l| l.split_whitespace().any(|w| w == "A")),
        "{types}"
    );
    let o = r.run(&["query", "search", "B", "--check-stale"]);
    assert_eq!(err(&o), "");
    let o = r.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(o.stderr, line.as_bytes());
    assert!(search(&r, "A2").contains("A2"));
}

#[test]
fn REQ103_AC2_le_fichier_modifie_perd_sa_priorite() {
    let Some(r) = Repo::new(&INDEXED_FF) else {
        return;
    };
    let o = r.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    if fs::write(
        r.root.join(OsStr::from_bytes(b"A\xfe.java")),
        "class A {}\n",
    )
    .is_err()
    {
        return;
    }
    fs::write(
        r.root.join(OsStr::from_bytes(b"A\xff.java")),
        "class A22 {}\n",
    )
    .unwrap();
    let o = r.run(&["update", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(o.stderr, format!("{A7_FE_FF}\n").as_bytes());
    let s = search(&r, "A");
    assert!(s.contains("A\u{FFFD}.java"), "{s}");
    assert!(!s.contains("A22") && !s.contains("A2"), "{s}");
}

#[test]
fn REQ103_AC4_textes_a7_octet_pour_octet() {
    let Some(r) = Repo::new(&INDEXED_FF) else {
        return;
    };
    r.run(&["build", "--no-llm"]);
    if fs::write(
        r.root.join(OsStr::from_bytes(b"A\xfe.java")),
        "class A {}\n",
    )
    .is_err()
    {
        return;
    }
    let o = r.run(&["update", "--no-llm"]);
    assert_eq!(a7_lines(&o), vec![A7_FF_FE.as_bytes().to_vec()]);
    assert_eq!(o.stderr.iter().filter(|b| **b == b'\n').count(), 1);
    fs::write(
        r.root.join(OsStr::from_bytes(b"A\xff.java")),
        "class A22 {}\n",
    )
    .unwrap();
    let o = r.run(&["update", "--no-llm"]);
    assert_eq!(a7_lines(&o), vec![A7_FE_FF.as_bytes().to_vec()]);
    assert_eq!(o.stderr.iter().filter(|b| **b == b'\n').count(), 1);
}
