//! Export JSON en flux (REQ-080, DEC-092) : sorties de la commande, erreurs d'écriture,
//! sortie standard fermée par le lecteur.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    /// Dépôt de `files` classes Java de 8 méthodes : un export de plusieurs centaines de Kio,
    /// bien plus grand que le tampon d'un tube.
    fn big(files: usize) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("monrepo");
        for i in 0..files {
            let dir = repo.join(format!("src/p{}", i % 20));
            fs::create_dir_all(&dir).unwrap();
            let mut src = format!("package p{};\n\npublic class C{i} {{\n", i % 20);
            for m in 0..8 {
                src.push_str(&format!(
                    "    public int method{m}() {{ Ext{i}.go(); return new C{}().method{}(); }}\n",
                    (i + 1) % files,
                    (m + 1) % 8
                ));
            }
            src.push_str("}\n");
            fs::write(dir.join(format!("C{i}.java")), src).unwrap();
        }
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let env = Env {
            _tmp: tmp,
            repo,
            config,
        };
        assert!(env
            .command(&["build", "--no-llm"])
            .output()
            .unwrap()
            .status
            .success());
        env
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env_remove("NEPHILA_MOCK_DIR");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
}

/// AC-080-1 (commande) et AC-080-7 : `--output -` et `--output f.json` donnent le même document,
/// terminé par un seul saut de ligne, dans l'ordre de clés et le format compact de la 0.23.2
/// (relecture puis réécriture par serde_json : identité octet pour octet).
#[test]
fn req080_ac1_ac7_stdout_and_file_give_the_same_document() {
    let env = Env::big(30);
    let cwd = tempfile::tempdir().unwrap();
    let out = env.run(&["export", "--output", "-"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
    let on_stdout = text(&out.stdout);
    assert!(on_stdout.ends_with("}\n") && !on_stdout.ends_with("\n\n"));
    assert!(
        on_stdout.starts_with("{\"communities\":[{\"cohesion\":"),
        "{}",
        &on_stdout[..60]
    );

    let file = Command::new(cargo_bin("nephila"))
        .current_dir(cwd.path())
        .arg("-C")
        .arg(&env.repo)
        .args(["export", "--output", "f.json"])
        .env("NEPHILA_CONFIG", &env.config)
        .output()
        .unwrap();
    assert!(file.status.success());
    assert_eq!(text(&file.stdout), "f.json\n");
    assert_eq!(text(&file.stderr), "");
    let on_disk = fs::read_to_string(cwd.path().join("f.json")).unwrap();
    assert_eq!(on_disk, on_stdout);

    let doc: serde_json::Value = serde_json::from_str(on_disk.trim_end()).unwrap();
    assert_eq!(doc.to_string() + "\n", on_disk);
    assert!(doc["nodes"].as_array().unwrap().len() > 200);
    assert!(!doc["unresolved"].as_array().unwrap().is_empty());
}

/// AC-080-4 : sorties d'erreur d'un chemin inutilisable, textes et code de la 0.23.2.
#[test]
fn req080_ac4_unusable_output_paths_keep_their_messages() {
    let env = Env::big(3);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("d");
    fs::create_dir(&dir).unwrap();
    let as_dir = format!("{}/", dir.display());
    let out = env.run(&["export", "--output", &as_dir]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("erreur: écriture de {as_dir} impossible : Is a directory (os error 21)\n")
    );
    assert_eq!(text(&out.stdout), "");
    let absent = format!("{}/absent/x.json", tmp.path().display());
    let out = env.run(&["export", "--output", &absent]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!(
            "erreur: écriture de {absent} impossible : No such file or directory (os error 2)\n"
        )
    );
    assert_eq!(text(&out.stdout), "");
    assert!(!Path::new(&absent).exists());
}

/// Lance l'export sur stdout, lit 10 octets puis ferme le tube.
fn export_closed_after_10_bytes(env: &Env, format: &str) -> (Output, Vec<u8>) {
    let mut child = env
        .command(&["export", "--format", format, "--output", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut head = vec![0u8; 10];
    stdout.read_exact(&mut head).unwrap();
    drop(stdout);
    (child.wait_with_output().unwrap(), head)
}

/// AC-080-4 et AC-080-6 : sortie fermée par le lecteur, JSON puis HTML.
#[test]
fn req080_ac4_ac6_closed_stdout_is_one_error_line_without_panic() {
    let env = Env::big(400);
    for format in ["json", "html"] {
        let (out, head) = export_closed_after_10_bytes(&env, format);
        assert_eq!(head.len(), 10);
        assert!(!out.status.success(), "{format}");
        assert_ne!(out.status.code(), Some(101), "{format} : panique");
        let err = text(&out.stderr);
        assert!(
            err.starts_with("erreur: écriture sur stdout impossible : "),
            "{format} : {err:?}"
        );
        assert!(
            err.ends_with("\n") && err.matches('\n').count() == 1,
            "{format} : {err:?}"
        );
        for banned in ["panicked", "thread '", "\u{1b}"] {
            assert!(!err.contains(banned), "{format} : {err:?}");
        }
    }
}

/// AC-080-7 : export réussi vers un fichier ou stdout, stderr vide ; aucun graphe, aucun fichier
/// (AC-018-4, inchangé).
#[test]
fn req080_ac7_successful_exports_write_nothing_on_stderr() {
    let env = Env::big(3);
    let tmp = tempfile::tempdir().unwrap();
    for format in ["json", "html"] {
        let f = tmp.path().join(format!("o.{format}"));
        let out = env.run(&[
            "export",
            "--format",
            format,
            "--output",
            f.to_str().unwrap(),
        ]);
        assert!(out.status.success());
        assert_eq!(text(&out.stderr), "");
        assert_eq!(text(&out.stdout), format!("{}\n", f.display()));
        let out = env.run(&["export", "--format", format, "--output", "-"]);
        assert!(out.status.success());
        assert_eq!(text(&out.stderr), "");
    }
    let empty = tempfile::tempdir().unwrap();
    let out = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(empty.path())
        .args(["export", "--output", "x.json"])
        .current_dir(tmp.path())
        .env("NEPHILA_CONFIG", &env.config)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!tmp.path().join("x.json").exists());
}

/// Export normalisé : tableaux triés (l'ordre des lignes n'est pas garanti entre deux graphes).
fn normalized(doc: &str) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(doc.trim_end()).unwrap();
    for key in ["edges", "nodes", "unresolved"] {
        let a = v[key].as_array_mut().unwrap();
        a.sort_by_key(|x| x.to_string());
    }
    v
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".nephila" {
            continue;
        }
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn java_files(repo: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![repo.join("src")];
    while let Some(d) = dirs.pop() {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// AC-084-1 : un `update` qui supprime des fichiers pendant qu'un export lent est bloqué sur un
/// tube plein ne change pas le document exporté (un seul instantané de lecture).
#[test]
fn req084_ac1_export_is_one_snapshot_while_an_update_deletes_files() {
    let env = Env::big(400);
    let reference = env.run(&["export", "--output", "-"]);
    assert!(reference.status.success());
    let reference = text(&reference.stdout);
    let edges_start = reference.find("\"edges\":[").unwrap();
    let edges_end = reference.find("],\"nodes\":[").unwrap();
    assert!(
        edges_end - edges_start > 256 * 1024,
        "section edges de {} octets",
        edges_end - edges_start
    );

    let mut child = env
        .command(&["export", "--output", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut head = vec![0u8; 10];
    stdout.read_exact(&mut head).unwrap();

    for (i, file) in java_files(&env.repo).iter().enumerate() {
        if i % 2 == 0 {
            fs::remove_file(file).unwrap();
        }
    }
    let update = env.run(&["update", "--no-llm"]);
    assert_eq!(update.status.code(), Some(0), "{}", text(&update.stderr));
    assert!(
        child.try_wait().unwrap().is_none(),
        "l'export aurait dû rester bloqué sur le tube plein"
    );

    let mut rest = Vec::new();
    stdout.read_to_end(&mut rest).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(err, "");
    let mut exported = head;
    exported.extend(rest);
    let exported = text(&exported);

    let doc = normalized(&exported);
    let ids: std::collections::HashSet<&str> = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    for e in doc["edges"].as_array().unwrap() {
        assert!(ids.contains(e["source"].as_str().unwrap()), "{e}");
        assert!(ids.contains(e["target"].as_str().unwrap()), "{e}");
    }
    for u in doc["unresolved"].as_array().unwrap() {
        assert!(ids.contains(u["source"].as_str().unwrap()), "{u}");
    }
    assert_eq!(doc, normalized(&reference));

    // L'export suivant voit la suppression : égal à un build neuf de l'arbre réduit.
    let after = env.run(&["export", "--output", "-"]);
    assert!(after.status.success());
    let after = normalized(&text(&after.stdout));
    assert_ne!(after, doc);

    let tmp = tempfile::tempdir().unwrap();
    let fresh = Env {
        repo: tmp.path().join("monrepo"),
        config: env.config.clone(),
        _tmp: tmp,
    };
    fs::create_dir_all(&fresh.repo).unwrap();
    copy_tree(&env.repo, &fresh.repo);
    assert!(fresh
        .command(&["build", "--no-llm"])
        .output()
        .unwrap()
        .status
        .success());
    let built = fresh.run(&["export", "--output", "-"]);
    assert!(built.status.success());
    assert_eq!(after, normalized(&text(&built.stdout)));
}
