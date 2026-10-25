//! Imbrication des déclarations bornée à 1 000 niveaux (REQ-104, DEC-105).

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const A8: &str =
    "avertissement: Deep.java : déclarations imbriquées au-delà de 1000 niveaux ignorées";

fn run(repo: &Path, config: &Path, args: &[&str]) -> Output {
    Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("NEPHILA_CONFIG", config)
        .output()
        .unwrap()
}

/// Somme des tailles des fichiers du dossier `.nephila`.
fn nephila_size(repo: &Path) -> u64 {
    fs::read_dir(repo.join(".nephila"))
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum()
}

fn build_deep(source: &str) -> (String, u64) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(&config, "").unwrap();
    fs::write(repo.join("Deep.java"), source).unwrap();
    fs::write(repo.join("Ok.java"), "class Ok { void m() {} }\n").unwrap();
    let o = run(&repo, &config, &["build", "--no-llm"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        stdout.starts_with("build terminé : 2 fichiers traités,"),
        "{stdout}"
    );
    let q = run(&repo, &config, &["query", "search", "Ok"]);
    assert!(q.status.success());
    assert!(
        String::from_utf8_lossy(&q.stdout).contains("Ok"),
        "Ok introuvable"
    );
    (
        String::from_utf8_lossy(&o.stderr).into_owned(),
        nephila_size(&repo),
    )
}

/// Borne de taille d'un graphe à 1 000 niveaux (18 Mo mesurés, DEC-105 ; 50 Mo selon la spec).
const MAX_BYTES: u64 = 50_000_000;

#[test]
fn req104_ac1_forme_fermee_20000_niveaux() {
    let src = format!("{}{}", "class A {".repeat(20_000), "}".repeat(20_000));
    let (stderr, size) = build_deep(&src);
    assert_eq!(stderr.trim_end(), A8);
    assert!(size < MAX_BYTES, ".nephila fait {size} octets");
}

#[test]
fn req104_ac1_forme_sans_accolade_fermante() {
    let src = "class A {".repeat(20_000);
    // L'analyse tolérante ne tire aucune déclaration d'un fichier sans accolade fermante : seule la
    // taille du graphe est bornée.
    let (_stderr, size) = build_deep(&src);
    assert!(size < MAX_BYTES, ".nephila fait {size} octets");
}

/// AC-104-2 : fichier profond `deep` (`open` ×20 000, `}` ×20 000) et fichier sain `ok_name`.
fn build_deep_lang(deep_name: &str, open: &str, ok_name: &str, ok_src: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(&config, "").unwrap();
    let src = format!("{}{}", open.repeat(20_000), "}\n".repeat(20_000));
    fs::write(repo.join(deep_name), src).unwrap();
    fs::write(repo.join(ok_name), ok_src).unwrap();
    let o = run(&repo, &config, &["build", "--no-llm"]);
    let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(o.status.success(), "{stderr}");
    let q = run(&repo, &config, &["query", "search", "Ok"]);
    assert!(
        String::from_utf8_lossy(&q.stdout).contains("Ok"),
        "Ok introuvable"
    );
    assert!(
        nephila_size(&repo) < MAX_BYTES,
        "{deep_name} : .nephila trop gros"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines,
        vec![format!(
            "avertissement: {deep_name} : déclarations imbriquées au-delà de 1000 niveaux ignorées"
        )],
        "{deep_name}"
    );
}

#[test]
fn req104_ac2_kotlin() {
    build_deep_lang(
        "Deep.kt",
        "class A {\n",
        "Ok.kt",
        "class Ok { fun m() {} }\n",
    );
}

#[test]
fn req104_ac2_scala() {
    build_deep_lang(
        "Deep.scala",
        "class A {\n",
        "Ok.scala",
        "class Ok { def m() = 1 }\n",
    );
}

#[test]
fn req104_ac2_groovy() {
    build_deep_lang(
        "Deep.groovy",
        "class A {\n",
        "Ok.groovy",
        "class Ok { void m() {} }\n",
    );
}

#[test]
fn req104_ac2_typescript() {
    build_deep_lang(
        "deep.ts",
        "namespace A {\n",
        "ok.ts",
        "export class Ok {}\n",
    );
}

// ---- AC-104-5 : texte, place et ordre de l'avertissement ----

fn deep_src() -> String {
    format!("{}{}", "class A {".repeat(20_000), "}".repeat(20_000))
}

fn setup(files: &[(&str, String)]) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(&config, "").unwrap();
    for (name, content) in files {
        let p = repo.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    (tmp, repo, config)
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn req104_ac5_chemins_apostrophe_et_controle() {
    let mut files = vec![
        ("d'x/Deep.java", deep_src()),
        ("Ok.java", "class Ok {}\n".to_string()),
    ];
    #[cfg(unix)]
    files.push(("e\u{1B}.java", deep_src()));
    let (_t, repo, config) = setup(&files);
    let o = run(&repo, &config, &["build", "--no-llm"]);
    assert!(o.status.success(), "{}", err(&o));
    let mut expected = vec![
        "avertissement: d'x/Deep.java : déclarations imbriquées au-delà de 1000 niveaux ignorées"
            .to_string(),
    ];
    #[cfg(unix)]
    expected.push(
        "avertissement: e\\x1B.java : déclarations imbriquées au-delà de 1000 niveaux ignorées"
            .to_string(),
    );
    let lines: Vec<String> = err(&o).lines().map(String::from).collect();
    assert_eq!(lines, expected);
}

#[test]
fn req104_ac5_deux_branches_coupees_une_seule_ligne() {
    let branch = format!("{}{}", "class A {".repeat(1_500), "}".repeat(1_500));
    let src = format!("{branch}\n{}\n", branch.replace("class A", "class B"));
    let (_t, repo, config) = setup(&[("Deep.java", src)]);
    let o = run(&repo, &config, &["build", "--no-llm"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(err(&o).lines().collect::<Vec<_>>(), vec![A8]);
}

const XML_WARN_PREFIX: &str = "avertissement: pom.xml : XML mal formé";

fn mixed_repo() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    setup(&[
        ("Deep.java", deep_src()),
        ("Ok.java", "class Ok { void m() {} }\n".to_string()),
        (
            "pom.xml",
            "<project><dependencies><dependency></project>".to_string(),
        ),
    ])
}

#[test]
fn req104_ac5_ordre_par_chemin_independant_des_fils() {
    let mut reference: Option<String> = None;
    for round in 0..6 {
        let (_t, repo, config) = mixed_repo();
        let jobs = if round == 0 { "1" } else { "8" };
        let o = run(&repo, &config, &["build", "--no-llm", "--jobs", jobs]);
        let e = err(&o);
        let deep = e.find(A8).unwrap_or_else(|| panic!("A-8 absent : {e}"));
        let xml = e
            .find(XML_WARN_PREFIX)
            .unwrap_or_else(|| panic!("XML absent : {e}"));
        assert!(deep < xml, "Deep.java doit précéder pom.xml : {e}");
        match &reference {
            None => reference = Some(e),
            Some(r) => assert_eq!(&e, r, "tour {round}, --jobs {jobs}"),
        }
    }
}

#[test]
fn req104_ac5_verbose_memes_lignes_avant_structure_fin() {
    let (_t, repo, config) = mixed_repo();
    let plain = run(&repo, &config, &["build", "--no-llm", "--jobs", "8"]);
    let (_t2, repo2, config2) = mixed_repo();
    let o = run(
        &repo2,
        &config2,
        &["build", "--no-llm", "--jobs", "8", "--verbose"],
    );
    let all = err(&o);
    let warns: Vec<&str> = all
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect();
    let expected: Vec<String> = err(&plain).lines().map(String::from).collect();
    assert_eq!(warns, expected);
    let fin = all
        .lines()
        .position(|l| l.contains("structure: fin"))
        .expect("structure: fin");
    let ls: Vec<&str> = all.lines().collect();
    let last_warn = ls
        .iter()
        .rposition(|l| l.starts_with("avertissement:"))
        .unwrap();
    assert!(last_warn < fin, "{all}");
}

#[test]
fn req104_ac5_update_et_status() {
    let (_t, repo, config) = setup(&[
        ("Deep.java", deep_src()),
        ("Ok.java", "class Ok {}\n".to_string()),
    ]);
    let o = run(&repo, &config, &["build", "--no-llm"]);
    assert!(err(&o).contains(A8));
    fs::write(repo.join("Ok.java"), "class Ok { void m() {} }\n").unwrap();
    let u = run(&repo, &config, &["update", "--no-llm"]);
    assert!(u.status.success(), "{}", err(&u));
    assert!(!err(&u).contains("imbriquées"), "{}", err(&u));
    fs::write(repo.join("Deep.java"), format!("class A {{{}", deep_src())).unwrap();
    let u = run(&repo, &config, &["update", "--no-llm"]);
    assert!(u.status.success(), "{}", err(&u));
    assert_eq!(err(&u).matches(A8).count(), 1, "{}", err(&u));
    let s = run(&repo, &config, &["status"]);
    assert!(!err(&s).contains("imbriquées"), "{}", err(&s));
    assert!(!String::from_utf8_lossy(&s.stdout).contains("imbriquées"));
}
