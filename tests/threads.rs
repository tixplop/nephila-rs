//! Fils refusés par le système (REQ-111) : sous `RLIMIT_NPROC` = 1, le parcours du dépôt rend une
//! erreur au lieu de paniquer, et `--jobs 1` parcourt sans fil. Hors Linux, `RLIMIT_NPROC` ne
//! borne que les processus, pas les fils : le fichier n'y est pas compilé.
#![cfg(target_os = "linux")]

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn is_root() -> bool {
    // SAFETY: appel sans effet de bord.
    unsafe { libc::geteuid() == 0 }
}

fn skip(why: &str) {
    eprintln!("test non applicable : {why}");
}

fn parallelism_below_two() -> bool {
    std::thread::available_parallelism().map_or(true, |n| n.get() < 2)
}

impl Env {
    /// Dépôt indexé sans limite par `build --no-llm`.
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
        };
        fs::write(&env.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        fs::create_dir_all(env.repo.join("core")).unwrap();
        fs::write(
            env.repo.join("core/A.java"),
            "package com.acme.core;\n\npublic class A {\n}\n",
        )
        .unwrap();
        fs::write(
            env.repo.join("core/B.java"),
            "package com.acme.core;\n\npublic class B {\n    A a;\n}\n",
        )
        .unwrap();
        let out = env.run(&["build", "--no-llm"], false);
        assert!(out.status.success(), "{}", text(&out.stderr));
        env
    }

    fn copy(&self) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
        };
        fs::copy(&self.config, &env.config).unwrap();
        copy_dir(&self.repo, &env.repo);
        env
    }

    fn run(&self, args: &[&str], limited: bool) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env_remove("NEPHILA_MOCK_DIR");
        if limited {
            // SAFETY: setrlimit est async-signal-safe ; la limite ne vaut que pour le processus fils.
            unsafe {
                c.pre_exec(|| {
                    let lim = libc::rlimit {
                        rlim_cur: 1,
                        rlim_max: 1,
                    };
                    if libc::setrlimit(libc::RLIMIT_NPROC, &lim) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        c.output().unwrap()
    }
}

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

const WALK_ERR: &str = "erreur: parcours du dépôt impossible : ";

fn assert_walk_error(out: &Output, what: &str) {
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{what} : {err}");
    assert_eq!(err.lines().count(), 1, "{what} : {err}");
    assert!(err.starts_with(WALK_ERR), "{what} : {err}");
    assert!(
        !err.contains("panicked") && !err.contains("failed to spawn"),
        "{what} : {err}"
    );
}

#[test]
fn req111_ac3_parcours_refuse_est_une_erreur_une_ligne() {
    if is_root() {
        return skip("uid effectif 0");
    }
    let env = Env::new();
    for args in [
        &["build", "--no-llm", "--jobs", "4"][..],
        &["update", "--no-llm", "--jobs", "4"][..],
    ] {
        assert_walk_error(&env.run(args, true), &args.join(" "));
    }
    let st = env.run(&["status"], false);
    assert!(
        text(&st.stdout).contains("graphe à jour"),
        "{}",
        text(&st.stdout)
    );
    if parallelism_below_two() {
        return skip("available_parallelism() < 2 pour status et --check-stale");
    }
    assert_walk_error(&env.run(&["status"], true), "status");
    assert_walk_error(
        &env.run(&["query", "search", "A", "--check-stale"], true),
        "query --check-stale",
    );
}

#[test]
fn req111_ac4_jobs_un_parcourt_sans_fil() {
    if is_root() {
        return skip("uid effectif 0");
    }
    let env = Env::new().copy();
    let out = env.run(&["build", "--no-llm", "--jobs", "1"], true);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(!err.contains("parcours du dépôt impossible"), "{err}");
    assert!(!err.contains("panicked"), "{err}");
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("erreur:")).collect();
    assert_eq!(errors.len(), 1, "{err}");
    assert!(
        errors[0].starts_with("erreur: création des fils d'analyse impossible : "),
        "{err}"
    );
}

#[test]
fn req111_ac5_ac7_stderr_de_l_erreur_du_parcours() {
    if is_root() {
        return skip("uid effectif 0");
    }
    let env = Env::new();
    let locked = env.repo.join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("x.md"), "x").unwrap();
    use std::os::unix::fs::PermissionsExt;
    struct Unlock(PathBuf);
    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let _guard = Unlock(locked.clone());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        return skip("le dossier verrouillé reste lisible");
    }
    let re_ok = |err: &str| {
        let l = err.lines().next().unwrap_or("");
        l.starts_with(WALK_ERR) && l.ends_with(')') && l.contains("(os error ")
    };
    let mut cases: Vec<Vec<&str>> = vec![
        vec!["build", "--no-llm", "--jobs", "4"],
        vec!["update", "--no-llm", "--jobs", "4"],
    ];
    if !parallelism_below_two() {
        cases.push(vec!["status"]);
        cases.push(vec!["query", "search", "A", "--check-stale", "--json"]);
    }
    for args in cases {
        let out = env.run(&args, true);
        let err = text(&out.stderr);
        assert!(out.stdout.is_empty(), "{args:?}");
        assert_walk_error(&out, &args.join(" "));
        assert!(re_ok(&err), "{err}");
        for bad in [
            "entrée illisible",
            "parcours:",
            "progression:",
            "graphe pas à jour",
        ] {
            assert!(!err.contains(bad), "{args:?} : {err}");
        }
    }
    // AC-111-7 : avec --verbose, l'erreur est la dernière ligne, après la ligne de configuration.
    let out = env.run(&["build", "--no-llm", "--jobs", "4", "--verbose"], true);
    let err = text(&out.stderr);
    let lines: Vec<&str> = err.lines().collect();
    assert!(lines.last().unwrap().starts_with(WALK_ERR), "{err}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("config: llm inactif (--no-llm)")),
        "{err}"
    );
    for bad in ["parcours:", "structure:", "panicked", "failed to spawn"] {
        assert!(!err.contains(bad), "{err}");
    }
}
