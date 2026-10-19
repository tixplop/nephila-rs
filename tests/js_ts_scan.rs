//! AC-004-7 : un graphe de la 0.9 (package-lock.json enregistré comme fichier de données).

use assert_cmd::cargo::cargo_bin;
use nephila::model::{FileRecord, FileState, Node, NodeKind, SubKind};
use nephila::store::Store;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("NEPHILA_CONFIG", repo.join("absent.toml"))
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn first_line(o: &Output) -> String {
    out(o).lines().next().unwrap_or_default().to_string()
}

#[test]
fn req004_ac7_ancien_package_lock_supprime_puis_graphe_a_jour() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path();
    fs::write(repo.join("README.md"), "# Titre\n").unwrap();
    let b = run(repo, &["build", "--no-llm"]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    assert_eq!(first_line(&run(repo, &["status"])), "graphe à jour");

    // Graphe 0.9 simulé : package-lock.json connu comme document de données, absent du parcours.
    fs::write(repo.join("package-lock.json"), "{\"lockfileVersion\": 3}").unwrap();
    {
        let mut store = Store::open_existing(repo).unwrap();
        store
            .insert_files(&[FileRecord {
                path: "package-lock.json".into(),
                hash: "h".into(),
                size: 1,
                mtime: 1,
                recorded_at: 0,
                state: FileState::Indexed,
            }])
            .unwrap();
        let doc = Node::new(
            NodeKind::Document,
            "package-lock.json",
            "package-lock.json",
            Some("package-lock.json"),
            Some(SubKind::Json),
            None,
        );
        let key = Node::new(
            NodeKind::Key,
            "package-lock.json#lockfileVersion",
            "lockfileVersion",
            Some("package-lock.json"),
            None,
            None,
        );
        store
            .replace_file("package-lock.json", &[doc, key], &[])
            .unwrap();
    }
    let s = run(repo, &["status"]);
    let o = out(&s);
    assert!(o.contains("supprimé\tpackage-lock.json"), "{o}");

    let u = run(repo, &["update", "--no-llm"]);
    assert!(u.status.success(), "{}", String::from_utf8_lossy(&u.stderr));
    let store = Store::open_readonly(repo).unwrap();
    assert!(store
        .nodes()
        .unwrap()
        .iter()
        .all(|n| n.file.as_deref() != Some("package-lock.json")));
    assert!(store
        .files()
        .unwrap()
        .iter()
        .all(|f| f.path != "package-lock.json"));
    assert_eq!(first_line(&run(repo, &["status"])), "graphe à jour");
}
