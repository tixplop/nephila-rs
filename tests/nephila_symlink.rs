//! `.nephila` lien symbolique (REQ-098, DEC-102) : refusé avant toute création, par toutes les
//! commandes qui ouvrent le graphe. Unix seulement.
#![cfg(unix)]
#![allow(non_snake_case)]

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    victim: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            victim: tmp.path().join("victim"),
            mock: tmp.path().join("mock"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
        };
        fs::create_dir_all(&env.repo).unwrap();
        fs::create_dir_all(&env.mock).unwrap();
        fs::write(&env.config, "provider = \"mock\"\n").unwrap();
        fs::write(env.repo.join("A.java"), "class A { void run() {} }\n").unwrap();
        env
    }

    fn victim_dir(&self) -> &Path {
        fs::create_dir_all(&self.victim).unwrap();
        &self.victim
    }

    fn cmd(&self, repo: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(repo)
            .args(args)
            .current_dir(repo)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(&self.repo, args).output().unwrap()
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn e23(rel: &str) -> String {
    format!("erreur: {rel} est un lien symbolique ; supprimez-le\n")
}

fn assert_refused(o: &Output, rel: &str) {
    assert_eq!(o.status.code(), Some(1), "{}", err(o));
    assert_eq!(out(o), "");
    assert_eq!(err(o), e23(rel));
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => vec![],
    };
    v.sort();
    v
}

fn make_foreign_db(path: &Path) -> String {
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1); PRAGMA user_version = 2;",
    )
    .unwrap();
    drop(c);
    blake3::hash(&fs::read(path).unwrap()).to_hex().to_string()
}

#[test]
fn REQ098_AC1_AC8_nephila_lien_vers_dossier_vide() {
    let env = Env::new();
    symlink(env.victim_dir(), env.repo.join(".nephila")).unwrap();
    for cmd in ["build", "update"] {
        let o = env.run(&[cmd, "--no-llm"]);
        assert_refused(&o, ".nephila");
        assert_eq!(entries(&env.victim), Vec::<String>::new(), "{cmd}");
        assert_eq!(
            fs::read_link(env.repo.join(".nephila")).unwrap(),
            env.victim
        );
    }
}

#[test]
fn REQ098_AC1_AC8_nephila_lien_vers_chemin_absent() {
    let env = Env::new();
    symlink(&env.victim, env.repo.join(".nephila")).unwrap();
    for cmd in ["build", "update"] {
        let o = env.run(&[cmd, "--no-llm"]);
        assert_refused(&o, ".nephila");
        assert!(!env.victim.exists(), "{cmd}");
        assert!(!err(&o).contains("aucun graphe : build complet"));
    }
}

#[test]
fn REQ098_AC8_meme_ligne_avec_C_absolu_depuis_un_autre_dossier() {
    let env = Env::new();
    symlink(env.victim_dir(), env.repo.join(".nephila")).unwrap();
    let other = tempfile::tempdir().unwrap();
    let o = env
        .cmd(&env.repo, &["build", "--no-llm"])
        .current_dir(other.path())
        .output()
        .unwrap();
    assert_refused(&o, ".nephila");
}

#[test]
fn REQ098_AC2_graph_db_lien_vers_base_existante() {
    let env = Env::new();
    let base = env.victim_dir().join("base.db");
    let hash = make_foreign_db(&base);
    let before = entries(&env.victim);
    fs::create_dir(env.repo.join(".nephila")).unwrap();
    symlink(&base, env.repo.join(".nephila/graph.db")).unwrap();
    for cmd in ["build", "update"] {
        let o = env.run(&[cmd, "--no-llm"]);
        assert_refused(&o, ".nephila/graph.db");
        assert_eq!(
            blake3::hash(&fs::read(&base).unwrap()).to_hex().to_string(),
            hash
        );
        assert_eq!(entries(&env.victim), before);
        assert!(!env.repo.join(".nephila/lock").exists());
    }
}

#[test]
fn REQ098_AC2_graph_db_lien_vers_chemin_absent() {
    let env = Env::new();
    let target = env.victim_dir().join("absent.db");
    fs::create_dir(env.repo.join(".nephila")).unwrap();
    symlink(&target, env.repo.join(".nephila/graph.db")).unwrap();
    for cmd in ["build", "update"] {
        let o = env.run(&[cmd, "--no-llm"]);
        assert_refused(&o, ".nephila/graph.db");
        assert!(!target.exists());
        assert_eq!(entries(&env.victim), Vec::<String>::new());
    }
}

#[test]
fn REQ098_AC3_lock_lien() {
    let env = Env::new();
    fs::create_dir(env.repo.join(".nephila")).unwrap();
    let target = env.victim_dir().join("lock");
    symlink(&target, env.repo.join(".nephila/lock")).unwrap();
    let o = env.run(&["build", "--no-llm"]);
    assert_refused(&o, ".nephila/lock");
    assert!(!target.exists());
    assert!(!env.repo.join(".nephila/graph.db").exists());
}

#[test]
fn REQ098_AC3_wal_lien_apres_un_build() {
    let env = Env::new();
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let wal = env.repo.join(".nephila/graph.db-wal");
    let _ = fs::remove_file(&wal);
    let target = env.victim_dir().join("wal");
    symlink(&target, &wal).unwrap();
    for args in [&["update", "--no-llm"][..], &["query", "search", "A"][..]] {
        let o = env.run(args);
        assert_refused(&o, ".nephila/graph.db-wal");
        assert!(!target.exists());
    }
}

#[test]
fn REQ098_AC4_AC10_depot_lie_au_graphe_d_un_autre() {
    let env = Env::new();
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let a_nephila = env.repo.join(".nephila");
    let db = a_nephila.join("graph.db");
    let hash = blake3::hash(&fs::read(&db).unwrap());
    let listing = entries(&a_nephila);

    let b = env._tmp.path().join("b");
    fs::create_dir_all(&b).unwrap();
    fs::write(b.join("A.java"), "class A {}\n").unwrap();
    symlink(&a_nephila, b.join(".nephila")).unwrap();
    let commands: Vec<Vec<&str>> = vec![
        vec!["query", "search", "A"],
        vec!["query", "search", "A", "--check-stale"],
        vec!["query", "neighbors", "A"],
        vec!["query", "path", "A", "A"],
        vec!["query", "ask", "A ?"],
        vec!["status"],
        vec!["status", "--json"],
        vec!["summary"],
        vec!["summary", "--json"],
        vec!["export", "--output", "-"],
        vec!["export"],
        vec!["export", "--format", "html"],
        vec!["export", "--output", "f.json"],
        vec!["build", "--no-llm", "--show-progress"],
    ];
    for args in &commands {
        let o = env.cmd(&b, args).output().unwrap();
        assert_refused(&o, ".nephila");
    }
    assert_eq!(entries(&b), vec![".nephila", "A.java"]);
    assert_eq!(blake3::hash(&fs::read(&db).unwrap()), hash);
    assert_eq!(entries(&a_nephila), listing);

    // `graph.db` lien, `.nephila` réel.
    fs::remove_file(b.join(".nephila")).unwrap();
    fs::create_dir(b.join(".nephila")).unwrap();
    symlink(&db, b.join(".nephila/graph.db")).unwrap();
    for args in &commands {
        let o = env.cmd(&b, args).output().unwrap();
        assert_refused(&o, ".nephila/graph.db");
    }
    assert_eq!(blake3::hash(&fs::read(&db).unwrap()), hash);
    assert_eq!(entries(&a_nephila), listing);
    assert_eq!(entries(&b), vec![".nephila", "A.java"]);
}

#[test]
fn REQ098_AC9_E23_passe_avant_les_avertissements_de_parcours() {
    let env = Env::new();
    fs::write(env.repo.join("bad.md"), b"# t\n\xff\xfe\n").unwrap();
    symlink(env.victim_dir(), env.repo.join(".nephila")).unwrap();
    for args in [
        &["build", "--no-llm"][..],
        &["update", "--no-llm"][..],
        &["status"][..],
        &["build", "--no-llm", "--show-progress"][..],
    ] {
        let o = env.run(args);
        assert_refused(&o, ".nephila");
    }
    assert_eq!(entries(&env.victim), Vec::<String>::new());
}

#[test]
fn REQ098_AC9_erreur_de_fichier_de_projet_passe_avant_E23() {
    let env = Env::new();
    fs::write(env.repo.join(".nephila.toml"), "exclud = []\n").unwrap();
    symlink(env.victim_dir(), env.repo.join(".nephila")).unwrap();
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).starts_with("erreur:"), "{}", err(&o));
    assert!(!err(&o).contains("lien symbolique"), "{}", err(&o));
}

#[test]
fn REQ098_AC6_verbose_et_progression_n_ecrivent_rien_avant_E23() {
    let env = Env::new();
    symlink(env.victim_dir(), env.repo.join(".nephila")).unwrap();
    for flags in [&["--verbose"][..], &["--show-progress"][..]] {
        let mut args = vec!["build", "--no-llm"];
        args.extend_from_slice(flags);
        let o = env.run(&args);
        assert_eq!(o.status.code(), Some(1), "{}", err(&o));
        assert_eq!(out(&o), "");
        let text = err(&o);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("erreur:")).count(),
            1,
            "{text}"
        );
        assert_eq!(
            format!("{}\n", lines.last().unwrap()),
            e23(".nephila"),
            "{text}"
        );
        assert!(!text.contains("progression:") && !text.contains("parcours"));
        for bad in ["impossible de créer", "base du graphe :", "victim"] {
            assert!(!text.contains(bad), "{text}");
        }
        assert!(!text.contains(env.repo.to_str().unwrap()));
    }
}

#[test]
fn REQ098_AC5_apres_suppression_du_lien_build_cree_le_graphe() {
    let env = Env::new();
    symlink(env.victim_dir(), env.repo.join(".nephila")).unwrap();
    assert_refused(&env.run(&["build", "--no-llm"]), ".nephila");
    fs::remove_file(env.repo.join(".nephila")).unwrap();
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(env.repo.join(".nephila/graph.db").is_file());
    assert_eq!(entries(&env.victim), Vec::<String>::new());
}
