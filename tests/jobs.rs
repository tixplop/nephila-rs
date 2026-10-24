//! Option `--jobs` et fils d'extraction indépendants de `concurrency` (REQ-071).

use assert_cmd::cargo::cargo_bin;
use nephila::index::{default_jobs, MAX_JOBS};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    /// 8 fichiers Java, configuration `mock` (`enrich_code`, `batch_files = 1`, `concurrency`).
    fn new(concurrency: usize) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(mock.join("src")).unwrap();
        for i in 0..8 {
            fs::write(
                repo.join(format!("src/F{i}.java")),
                format!("package p;\npublic class F{i} {{}}\n"),
            )
            .unwrap();
            fs::write(
                mock.join(format!("src/F{i}.java.json")),
                format!(
                    r#"{{"version":2,"summaries":[{{"node":"type:p.F{i}","summary":"résumé {i}"}}],"concepts":[]}}"#
                ),
            )
            .unwrap();
        }
        let config = tmp.path().join("config.toml");
        let env = Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config,
        };
        env.set_config(&format!(
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = {concurrency}\n"
        ));
        env
    }

    fn set_config(&self, content: &str) {
        fs::write(&self.config, content).unwrap();
    }

    fn run_in(&self, repo: &Path, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.repo, args)
    }

    fn reset(&self) {
        let _ = fs::remove_dir_all(self.repo.join(".nephila"));
    }
}

fn structure_line(err: &str) -> String {
    err.lines()
        .find(|l| l.contains("structure: début"))
        .unwrap_or_else(|| panic!("{err}"))
        .split("] ")
        .last()
        .unwrap()
        .to_string()
}

fn enrich_line(err: &str) -> String {
    err.lines()
        .find(|l| l.contains("enrichissement: début"))
        .unwrap_or_else(|| panic!("{err}"))
        .trim_end()
        .to_string()
}

#[test]
fn req071_ac1_jobs_regle_la_structure_pas_l_enrichissement() {
    let e = Env::new(1);
    let o = e.run(&["build", "--verbose"]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    let w = default_jobs().min(8);
    assert_eq!(
        structure_line(&err),
        format!("structure: début 8 fichiers, {w} fils")
    );
    assert!(enrich_line(&err).ends_with(", 1 fils"), "{err}");

    for (args, want) in [
        (vec!["build", "--verbose", "--jobs", "3"], "3"),
        (vec!["build", "--verbose", "--jobs", "20"], "8"),
    ] {
        e.reset();
        let err = text(&e.run(&args).stderr);
        assert_eq!(
            structure_line(&err),
            format!("structure: début 8 fichiers, {want} fils")
        );
        assert!(enrich_line(&err).ends_with(", 1 fils"), "{err}");
    }

    e.reset();
    fs::write(e.repo.join(".nephila.toml"), "concurrency = 2\n").unwrap();
    let err = text(&e.run(&["build", "--verbose", "--jobs", "1"]).stderr);
    assert_eq!(structure_line(&err), "structure: début 8 fichiers, 1 fils");
    assert!(enrich_line(&err).ends_with(", 2 fils"), "{err}");
    fs::remove_file(e.repo.join(".nephila.toml")).unwrap();

    for i in 0..4 {
        fs::write(
            e.repo.join(format!("src/F{i}.java")),
            format!("package p;\npublic class F{i} {{ int x; }}\n"),
        )
        .unwrap();
    }
    let o = e.run(&["update", "--verbose", "--jobs", "3"]);
    let err = text(&o.stderr);
    assert!(o.status.success(), "{err}");
    assert_eq!(structure_line(&err), "structure: début 4 fichiers, 3 fils");
}

#[test]
fn req071_ac2_sans_jobs_defaut_quel_que_soit_le_llm() {
    let e = Env::new(1);
    let w = default_jobs().min(8);
    let want = format!("structure: début 8 fichiers, {w} fils");
    let err = text(&e.run(&["build", "--no-llm", "--verbose"]).stderr);
    assert_eq!(structure_line(&err), want);
    e.reset();
    let absent = e.config.with_file_name("absent.toml");
    let o = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&e.repo)
        .args(["build", "--verbose"])
        .env("NEPHILA_CONFIG", &absent)
        .output()
        .unwrap();
    assert_eq!(structure_line(&text(&o.stderr)), want);
}

#[test]
fn req071_ac4_ac7_valeur_invalide_refusee_par_clap() {
    let e = Env::new(1);
    // Un fichier de projet invalide ne doit pas être lu avant le refus.
    fs::write(e.repo.join(".nephila.toml"), "jobs = 4\n").unwrap();
    let big = (MAX_JOBS + 1).to_string();
    for (sub, v) in [
        ("build", "0"),
        ("build", "-1"),
        ("build", "abc"),
        ("build", "1.5"),
        ("build", big.as_str()),
        ("update", "0"),
    ] {
        let o = e.run(&[sub, "--jobs", v]);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(2), "{sub} {v}: {err}");
        assert!(o.stdout.is_empty(), "{sub} {v}");
        let want = format!(
            "error: invalid value '{v}' for '--jobs <N>': `{v}` n'est pas un entier de 1 à {MAX_JOBS}"
        );
        assert!(err.lines().any(|l| l == want), "{sub} {v}: {err}");
        assert!(!err.contains("unexpected argument"), "{err}");
        assert!(!e.repo.join(".nephila").exists());
        assert!(!e.log.exists());
    }
    fs::remove_file(e.repo.join(".nephila.toml")).unwrap();
    for v in ["1", &MAX_JOBS.to_string()] {
        e.reset();
        let o = e.run(&["build", "--verbose", "--jobs", v]);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        let n: usize = v.parse().unwrap();
        assert_eq!(
            structure_line(&err),
            format!("structure: début 8 fichiers, {} fils", n.min(8))
        );
    }
}

fn normalized(b: &[u8]) -> String {
    text(b).split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn req071_ac5_ac8_aide_et_options_inconnues() {
    let e = Env::new(1);
    let help_text = "--jobs <N> Nombre de fils du parcours du dépôt et de l'extraction, de 1 à 256 (défaut : nombre de cœurs disponibles ; indépendant de `concurrency`, qui ne règle que les appels LLM)";
    assert_eq!(MAX_JOBS, 256);
    for sub in ["build", "update"] {
        let h = normalized(&e.run(&[sub, "--help"]).stdout);
        assert!(h.contains(help_text), "{sub}: {h}");
        let a = h.find("--show-progress").unwrap();
        let j = h.find("--jobs <N>").unwrap();
        let b = h.find("--batch-files <N>").unwrap();
        assert!(a < j && j < b, "{sub}: {h}");
        let after = &h[j..b];
        assert!(!after.contains("[default:"), "{after}");
    }
    for args in [
        vec!["status", "--help"],
        vec!["query", "ask", "--help"],
        vec!["query", "search", "--help"],
        vec!["export", "--help"],
        vec!["summary", "--help"],
    ] {
        let h = text(&e.run(&args).stdout);
        assert!(!h.contains("--jobs"), "{args:?}");
    }
    for args in [
        vec!["status", "--jobs", "2"],
        vec!["query", "search", "x", "--jobs", "2"],
    ] {
        let o = e.run(&args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(
            text(&o.stderr).contains("unexpected argument '--jobs'"),
            "{}",
            text(&o.stderr)
        );
    }
}

#[test]
fn req071_ac9_sorties_identiques_quel_que_soit_jobs() {
    let e = Env::new(1);
    let a = e.run(&["build", "--jobs", "1"]);
    e.reset();
    let _ = fs::remove_file(&e.log);
    let b = e.run(&["build", "--jobs", "8"]);
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);

    let strip = |s: &str| -> Vec<String> {
        s.lines()
            .filter(|l| l.contains("config:") || l.contains("parcours"))
            .map(|l| l.split_once("] ").map(|x| x.1).unwrap_or(l).to_string())
            .collect()
    };
    e.reset();
    let plain = text(&e.run(&["build", "--verbose"]).stderr);
    e.reset();
    let with = text(&e.run(&["build", "--verbose", "--jobs", "3"]).stderr);
    let (p, w) = (strip(&plain), strip(&with));
    assert!(!p.is_empty());
    assert_eq!(p.len(), w.len());
    // Lignes `config` et `parcours` identiques, durées éventuelles retirées.
    let no_time = |v: &[String]| -> Vec<String> {
        v.iter()
            .map(|l| {
                l.split_whitespace()
                    .filter(|w| {
                        !(w.ends_with("ms") || w.ends_with('s'))
                            || !w.chars().next().is_some_and(|c| c.is_ascii_digit())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    assert!(p.iter().any(|l| l.starts_with("parcours")), "{p:?}");
    assert_eq!(no_time(&p), no_time(&w));
    assert!(with.lines().all(|l| !l.contains("jobs")), "{with}");

    e.reset();
    let err = text(
        &e.run(&["build", "--no-llm", "--verbose", "--jobs", "3"])
            .stderr,
    );
    assert!(
        err.lines()
            .any(|l| l.ends_with("] config: llm inactif (--no-llm)")),
        "{err}"
    );
}

#[test]
fn req071_ac10_jobs_n_est_pas_une_cle_de_configuration() {
    let e = Env::new(1);
    let toml = e.repo.join(".nephila.toml");
    fs::write(&toml, "jobs = 4\n").unwrap();
    let want = format!(
        "erreur: {} : clé inconnue `jobs` (clés permises : exclude, exclude_types, model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries)\n",
        toml.display()
    );
    for args in [
        vec!["build"],
        vec!["build", "--jobs", "2"],
        vec!["build", "--no-llm"],
    ] {
        let o = e.run(&args);
        assert!(o.stdout.is_empty(), "{args:?}");
        assert!(!o.status.success());
        assert_eq!(text(&o.stderr), want, "{args:?}");
    }
    fs::remove_file(&toml).unwrap();
    e.set_config("provider = \"mock\"\nmodel = \"mock\"\njobs = 4\n");
    let o = e.run(&["build"]);
    let err = text(&o.stderr);
    assert!(!o.status.success());
    assert!(err.starts_with("erreur: configuration invalide ("), "{err}");
    assert!(err.contains("jobs"), "{err}");
    assert!(!e.repo.join(".nephila").exists());
    let o = e.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
}

#[test]
fn req071_ac9_deux_fichiers_non_utf8_stderr_identique() {
    let e = Env::new(1);
    fs::write(e.repo.join("z.md"), [0xff, 0xfe, b'z']).unwrap();
    fs::write(e.repo.join("a.md"), [0xff, 0xfe, b'a']).unwrap();
    let a = e.run(&["build", "--jobs", "1"]);
    e.reset();
    let b = e.run(&["build", "--jobs", "8"]);
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
    let err = text(&a.stderr);
    let (pa, pz) = (
        err.find("a.md ignoré : fichier non UTF-8"),
        err.find("z.md ignoré : fichier non UTF-8"),
    );
    assert!(pa.is_some() && pz.is_some() && pa < pz, "{err}");
}
