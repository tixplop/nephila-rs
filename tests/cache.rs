#![allow(non_snake_case)]
//! `nephila cache export|import` (REQ-295, DEC-226) : AC-295-1 et lignes de résultat d'AC-295-5.

use assert_cmd::Command;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "support/mod.rs"]
mod support;

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    mock: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let root = tmp.path().to_path_buf();
        support::copy_dir(&fx.join("maven2"), &root.join("repo"));
        support::copy_dir(&fx.join("mock"), &root.join("mock"));
        let config = root.join("config.toml");
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\n",
        )
        .unwrap();
        Env {
            _tmp: tmp,
            mock: root.join("mock"),
            config,
            root,
        }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// Copie des fichiers du dépôt, sans `.nephila/`.
    fn copy(&self, name: &str) -> PathBuf {
        let to = self.root.join(name);
        support::copy_dir(&self.repo(), &to);
        let _ = fs::remove_dir_all(to.join(".nephila"));
        to
    }

    fn log(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.log"))
    }

    fn run(&self, repo: &Path, log: &str, args: &[&str]) -> Output {
        self.run_with(&self.config, repo, log, args)
    }

    fn run_with(&self, config: &Path, repo: &Path, log: &str, args: &[&str]) -> Output {
        Command::cargo_bin("nephila")
            .unwrap()
            .env("NEPHILA_CONFIG", config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", self.log(log))
            .env_remove("NEPHILA_MOCK_FAIL_AFTER")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn log_is_empty(&self, name: &str) -> bool {
        fs::read_to_string(self.log(name)).map_or(true, |s| s.trim().is_empty())
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn ok(o: &Output) -> String {
    assert!(
        o.status.success(),
        "échec : {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.stderr.is_empty(), "stderr : {:?}", o.stderr);
    stdout(o)
}

fn db(repo: &Path) -> Connection {
    Connection::open(repo.join(".nephila/graph.db")).unwrap()
}

fn count(repo: &Path) -> usize {
    db(repo)
        .query_row("SELECT count(*) FROM enrichments", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap() as usize
}

fn rows(repo: &Path, sql: &str) -> Vec<Vec<Option<String>>> {
    let c = db(repo);
    let mut q = c.prepare(sql).unwrap();
    let n = q.column_count();
    q.query_map([], |r| {
        (0..n).map(|i| r.get::<_, Option<String>>(i)).collect()
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

const NODES: &str = "SELECT id, kind, qualified, summary FROM nodes ORDER BY id";
const MENTIONS: &str = "SELECT source, target FROM edges WHERE kind = 'mentions' ORDER BY 1, 2";

#[test]
fn REQ295_AC1_build_export_import_build_sans_appel() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build"]));
    assert!(!e.log_is_empty("origin"));
    let n = count(&repo);
    assert!(n > 0);
    let file = e.root.join("enrichissements.db");
    let file_s = file.to_str().unwrap();

    let out = ok(&e.run(&repo, "export", &["cache", "export", file_s]));
    assert_eq!(
        out,
        format!("cache export terminé : {n} enrichissements écrits\n")
    );

    let copy = e.copy("copie");
    let out = ok(&e.run(&copy, "import", &["cache", "import", file_s]));
    assert_eq!(
        out,
        format!("cache import terminé : {n} enrichissements ajoutés, 0 déjà présents ; lancez nephila build pour les appliquer\n")
    );
    assert!(e.log_is_empty("import"));

    ok(&e.run(&copy, "build", &["build"]));
    assert!(e.log_is_empty("build"), "le build a appelé le LLM");
    let status = ok(&e.run(&copy, "status", &["status", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(v["to_enrich"], 0);
    assert_eq!(rows(&copy, NODES), rows(&repo, NODES));
    assert_eq!(rows(&copy, MENTIONS), rows(&repo, MENTIONS));
    assert!(!rows(&repo, MENTIONS).is_empty());

    // Second import : tout est déjà présent.
    let out = ok(&e.run(&copy, "import2", &["cache", "import", file_s]));
    assert_eq!(
        out,
        format!("cache import terminé : 0 enrichissements ajoutés, {n} déjà présents ; lancez nephila build pour les appliquer\n")
    );
}

#[test]
fn REQ295_AC1_import_puis_build_no_llm() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build"]));
    let file = e.root.join("enrichissements.db");
    ok(&e.run(
        &repo,
        "export",
        &["cache", "export", file.to_str().unwrap()],
    ));

    let copy = e.copy("copie");
    ok(&e.run(
        &copy,
        "import",
        &["cache", "import", file.to_str().unwrap()],
    ));
    ok(&e.run(&copy, "build", &["build", "--no-llm"]));
    assert!(e.log_is_empty("build"));
    assert_eq!(rows(&copy, NODES), rows(&repo, NODES));
}

#[test]
fn REQ295_AC1_AC5_export_sans_enrichissement() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build", "--no-llm"]));
    let file = e.root.join("vide.db");
    let out = ok(&e.run(
        &repo,
        "export",
        &["cache", "export", file.to_str().unwrap()],
    ));
    assert_eq!(out, "cache export terminé : 0 enrichissements écrits\n");
    assert!(file.is_file());
}

// ---- AC-295-3, AC-295-4, AC-295-7 (c) (d) : erreurs, verrou, sortie sobre ----

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn refused(o: &Output, line: &str) {
    assert_eq!(o.status.code(), Some(1), "{}", err(o));
    assert_eq!(stdout(o), "");
    assert_eq!(err(o), format!("{line}\n"));
}

fn hash(p: &Path) -> String {
    blake3::hash(&fs::read(p).unwrap()).to_hex().to_string()
}

fn graph_hash(repo: &Path) -> String {
    hash(&repo.join(".nephila/graph.db"))
}

/// Fichier d'export valide dont la `version` est ramenée à 2.
fn bad_version_file(e: &Env, repo: &Path, path: &Path) {
    ok(&e.run(repo, "bv", &["cache", "export", path.to_str().unwrap()]));
    Connection::open(path)
        .unwrap()
        .execute("UPDATE meta SET value = '2' WHERE key = 'version'", [])
        .unwrap();
}

#[test]
fn REQ295_AC3_AC7_import_refuse_fichier_invalide() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build", "--no-llm"]));
    let before = graph_hash(&repo);

    let absent = e.root.join("absent.db");
    let texte = e.root.join("texte.db");
    fs::write(&texte, "ceci n'est pas une base\n").unwrap();
    let sans_meta = e.root.join("sans-meta.db");
    Connection::open(&sans_meta)
        .unwrap()
        .execute_batch("CREATE TABLE t(x INTEGER);")
        .unwrap();
    let v2 = e.root.join("v2.db");
    bad_version_file(&e, &repo, &v2);

    let cases = [
        (
            &absent,
            format!(
                "erreur: {} : lecture impossible : No such file or directory (os error 2)",
                absent.display()
            ),
        ),
        (
            &texte,
            format!(
                "erreur: {} : ce fichier n'a pas été écrit par nephila cache export",
                texte.display()
            ),
        ),
        (
            &sans_meta,
            format!(
                "erreur: {} : ce fichier n'a pas été écrit par nephila cache export",
                sans_meta.display()
            ),
        ),
        (
            &v2,
            format!(
                "erreur: {} : version 2 du fichier non prise en charge (attendue 1) ; relancez nephila cache export avec cette version de nephila",
                v2.display()
            ),
        ),
    ];
    for (f, line) in &cases {
        let o = e.run(&repo, "imp", &["cache", "import", f.to_str().unwrap()]);
        refused(&o, line);
        assert_eq!(graph_hash(&repo), before, "{}", f.display());
    }

    // Dépôt sans `.nephila/` : rien n'est créé.
    let copy = e.copy("copie");
    for (f, line) in &cases {
        let o = e.run(&copy, "imp", &["cache", "import", f.to_str().unwrap()]);
        refused(&o, line);
        assert!(!copy.join(".nephila").exists(), "{}", f.display());
    }
}

#[test]
fn REQ295_AC3_AC7_export_erreurs() {
    let e = Env::new();
    let repo = e.repo();
    let file = e.root.join("sortie.db");
    let file_s = file.to_str().unwrap();

    // Sans graphe.
    let o = e.run(&repo, "x", &["cache", "export", file_s]);
    refused(
        &o,
        &format!(
            "erreur: aucun graphe dans {} : lancez `nephila build`",
            repo.display()
        ),
    );
    assert!(!file.exists());
    assert!(!repo.join(".nephila").exists());

    ok(&e.run(&repo, "origin", &["build", "--no-llm"]));
    let before = graph_hash(&repo);

    // Dossier absent.
    let cible = e.root.join("absent").join("sortie.db");
    let o = e.run(&repo, "x", &["cache", "export", cible.to_str().unwrap()]);
    refused(
        &o,
        &format!(
            "erreur: écriture de {} impossible : No such file or directory (os error 2)",
            cible.display()
        ),
    );
    assert!(!e.root.join("absent").exists());

    // Schéma 16.
    Connection::open(repo.join(".nephila/graph.db"))
        .unwrap()
        .execute_batch(
            "BEGIN;
             ALTER TABLE enrichments DROP COLUMN nodes_hash;
             UPDATE meta SET value = '16' WHERE key = 'schema_version';
             PRAGMA user_version = 16;
             COMMIT;",
        )
        .unwrap();
    let old = graph_hash(&repo);
    assert_ne!(old, before);
    let o = e.run(&repo, "x", &["cache", "export", file_s]);
    refused(
        &o,
        &format!(
            "erreur: graphe d'une version précédente dans {} : lancez `nephila update`",
            repo.display()
        ),
    );
    assert_eq!(graph_hash(&repo), old);
    assert!(!file.exists());

    ok(&e.run(&repo, "up", &["update", "--no-llm"]));
    let out = ok(&e.run(&repo, "x", &["cache", "export", file_s]));
    assert_eq!(out, "cache export terminé : 0 enrichissements écrits\n");
}

#[cfg(unix)]
#[test]
fn REQ295_AC3_export_en_echec_laisse_le_fichier_existant() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build", "--no-llm"]));
    let dir = e.root.join("sortie");
    fs::create_dir(&dir).unwrap();
    let file = dir.join("c.db");
    fs::write(&file, "ancien contenu").unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let writable = fs::File::create(dir.join("sonde")).is_ok();
    let o = e.run(&repo, "x", &["cache", "export", file.to_str().unwrap()]);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return; // exécuté en superutilisateur : le dossier reste inscriptible
    }
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_eq!(stdout(&o), "");
    assert!(err(&o).starts_with(&format!(
        "erreur: écriture de {} impossible : ",
        file.display()
    )));
    assert_eq!(fs::read_to_string(&file).unwrap(), "ancien contenu");
}

#[cfg(unix)]
#[test]
fn REQ295_AC3_nephila_lien_symbolique() {
    let e = Env::new();
    let repo = e.repo();
    let victim = e.root.join("victim");
    fs::create_dir(&victim).unwrap();
    let file = e.root.join("f.db");
    let source = e.root.join("source.db");
    let origine = e.copy("origine");
    ok(&e.run(&origine, "o", &["build", "--no-llm"]));
    ok(&e.run(
        &origine,
        "o",
        &["cache", "export", source.to_str().unwrap()],
    ));
    std::os::unix::fs::symlink(&victim, repo.join(".nephila")).unwrap();
    let line = "erreur: .nephila est un lien symbolique ; supprimez-le";
    refused(
        &e.run(&repo, "x", &["cache", "export", file.to_str().unwrap()]),
        line,
    );
    refused(
        &e.run(&repo, "x", &["cache", "import", source.to_str().unwrap()]),
        line,
    );
    assert_eq!(fs::read_dir(&victim).unwrap().count(), 0);
    assert!(!file.exists());
}

#[test]
fn REQ295_AC4_import_refuse_pendant_un_verrou() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build"]));
    let file = e.root.join("c.db");
    let file_s = file.to_str().unwrap();
    let n = count(&repo);
    assert!(n > 0);
    ok(&e.run(&repo, "export", &["cache", "export", file_s]));
    db(&repo).execute("DELETE FROM enrichments", []).unwrap();

    let lock = nephila::index::lock::RepoLock::acquire(&repo).unwrap();
    let o = e.run(&repo, "imp", &["cache", "import", file_s]);
    refused(&o, "erreur: indexation déjà en cours sur ce dépôt");
    assert_eq!(count(&repo), 0);
    // L'export n'a pas besoin du verrou.
    db(&repo).execute("DELETE FROM enrichments", []).unwrap();
    let out = ok(&e.run(&repo, "export2", &["cache", "export", file_s]));
    assert_eq!(out, "cache export terminé : 0 enrichissements écrits\n");
    drop(lock);
}

#[test]
fn REQ295_AC4_import_autre_modele_renvoie_toute_la_portee() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build"]));
    let file = e.root.join("c.db");
    ok(&e.run(
        &repo,
        "export",
        &["cache", "export", file.to_str().unwrap()],
    ));
    let autre = e.root.join("autre.toml");
    fs::write(
        &autre,
        "provider = \"mock\"\nenrich_code = true\nmodel = \"autre\"\nbatch_files = 1\nconcurrency = 1\n",
    )
    .unwrap();

    let sans = e.copy("sans");
    ok(&e.run_with(&autre, &sans, "sans", &["build"]));
    let appels = fs::read_to_string(e.log("sans")).unwrap().lines().count();
    assert!(appels > 0);

    let avec = e.copy("avec");
    ok(&e.run_with(
        &autre,
        &avec,
        "imp",
        &["cache", "import", file.to_str().unwrap()],
    ));
    ok(&e.run_with(&autre, &avec, "avec", &["build"]));
    let appels_avec = fs::read_to_string(e.log("avec")).unwrap().lines().count();
    assert_eq!(appels_avec, appels);
}

#[test]
fn REQ295_AC7_sortie_sobre_sans_couleur_ni_entree() {
    let e = Env::new();
    let repo = e.repo();
    ok(&e.run(&repo, "origin", &["build"]));
    let file = e.root.join("c.db");
    let file_s = file.to_str().unwrap();
    let absent = e.root.join("absent.db");
    let copy = e.copy("copie");
    let mut sorties: Vec<(Option<i32>, Vec<u8>, Vec<u8>)> = vec![];
    for no_color in [false, true] {
        let run = |dir: &Path, args: &[&str]| {
            let mut c = Command::cargo_bin("nephila").unwrap();
            c.env("NEPHILA_CONFIG", &e.config)
                .env("NEPHILA_MOCK_DIR", &e.mock)
                .env("NEPHILA_MOCK_LOG", e.log("sobre"))
                .arg("-C")
                .arg(dir)
                .args(args);
            if no_color {
                c.env("NO_COLOR", "1");
            } else {
                c.env_remove("NO_COLOR");
            }
            c.output().unwrap()
        };
        for o in [
            run(&repo, &["cache", "export", file_s]),
            run(&copy, &["cache", "import", file_s]),
            run(&copy, &["cache", "import", absent.to_str().unwrap()]),
        ] {
            for flux in [&o.stdout, &o.stderr] {
                let t = String::from_utf8_lossy(flux);
                assert!(!flux.contains(&0x1B) && !flux.contains(&b'\r'), "{t:?}");
                for l in t.lines() {
                    for p in ["trace:", "progression:", "avertissement:"] {
                        assert!(!l.starts_with(p), "{t:?}");
                    }
                }
            }
            sorties.push((o.status.code(), o.stdout.clone(), o.stderr.clone()));
        }
    }
    // Avec ou sans NO_COLOR : même résultat (le second import ajoute 0).
    assert_eq!(sorties[0], sorties[3]);
    assert_eq!(sorties[2], sorties[5]);
    assert_eq!(sorties[0].0, Some(0));
    assert_eq!(sorties[2].0, Some(1));
}
