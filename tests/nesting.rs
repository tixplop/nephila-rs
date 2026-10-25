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
