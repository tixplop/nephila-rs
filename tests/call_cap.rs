//! Plafond de 50 appels d'enrichissement par fichier (REQ-048) : politique, traces,
//! progression, bilan, exclusion. Tests d'intégration sur le LLM simulé ; la classe de
//! 20 000 méthodes ne part jamais au mock.

use assert_cmd::cargo::cargo_bin;
use nephila::chunk::split;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const BIG: &str = "src/Big.java";
const A6_PREFIX: &str = "avertissement: src/Big.java : trop d'appels d'enrichissement (";
const A6_SUFFIX: &str =
    " nécessaires, au plus 50 par fichier), aucun envoyé ; excluez ce fichier avec exclude dans .nephila.toml";

/// Classe Java `class` de `k` méthodes `m0()` … : `k + 1` nœuds.
fn java(class: &str, k: usize) -> String {
    let mut s = format!("public class {class} {{\n");
    for i in 0..k {
        s.push_str(&format!("    void m{i}() {{}}\n"));
    }
    s.push_str("}\n");
    s
}

fn small(class: &str) -> String {
    java(class, 1)
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new(files: &[(&str, String)], extra: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&mock).unwrap();
        for (path, content) in files {
            let p = repo.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!("provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n{extra}"),
        )
        .unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config,
        }
    }

    fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).args(args);
        for v in [
            "NEPHILA_MOCK_FAIL_AFTER",
            "NEPHILA_MOCK_DELAY_MS",
            "NEPHILA_MOCK_INVALID_KEY",
            "NEPHILA_MOCK_TIMEOUT_FILES",
            "NEPHILA_MOCK_KEY_REVOKED_AFTER",
            "NEPHILA_MOCK_MALFORMED",
            "NO_COLOR",
        ] {
            c.env_remove(v);
        }
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log);
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn project(&self, content: &str) {
        fs::write(self.repo.join(".nephila.toml"), content).unwrap();
    }
}

/// Vérifie le motif A-6 à la main et renvoie le nombre capturé.
fn a6_count(line: &str) -> usize {
    let mid = line
        .strip_prefix(A6_PREFIX)
        .unwrap_or_else(|| panic!("préfixe A-6 : {line}"))
        .strip_suffix(A6_SUFFIX)
        .unwrap_or_else(|| panic!("suffixe A-6 : {line}"));
    assert!(
        !mid.is_empty() && mid.chars().all(|c| c.is_ascii_digit()),
        "{line}"
    );
    mid.parse().unwrap()
}

fn warnings(err: &str) -> Vec<&str> {
    err.lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect()
}

fn a6_lines(err: &str) -> Vec<&str> {
    err.lines().filter(|l| l.contains(BIG)).collect()
}

const MAX_CHUNK: usize = 100_000;
const K: usize = 20_000;
/// Appels du plan de `Big` (K méthodes, morceaux de MAX_CHUNK), relevés sur le binaire de T-106.
const BIG_20000_CALLS: usize = 135;

fn scenario(extra: &str) -> Env {
    Env::new(
        &[(BIG, java("Big", K)), ("src/Small.java", small("Small"))],
        &format!("max_chunk_chars = {MAX_CHUNK}\nconcurrency = 1\n{extra}"),
    )
}

#[test]
fn req048_ac3_quatre_gros_fichiers_deux_petits() {
    let names = ["Big0", "Big1", "Big2", "Big3"];
    let mut files: Vec<(String, String)> = names
        .iter()
        .map(|n| (format!("src/{n}.java"), java(n, 800)))
        .collect();
    files.push(("src/S0.java".into(), small("S0")));
    files.push(("src/S1.java".into(), small("S1")));
    for (_, c) in files.iter().take(4) {
        assert!(split(c, 200).len() > 60);
    }
    let refs: Vec<(&str, String)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    for args in [vec!["build"], vec!["build", "--format-retries", "2"]] {
        let env = Env::new(&refs, "max_chunk_chars = 200\nconcurrency = 1\n");
        let o = env.run(&args, &[]);
        let (out, err) = text(&o);
        assert!(!o.status.success(), "{out} {err}");
        let w = warnings(&err);
        assert_eq!(w.len(), 4, "{err}");
        for n in names {
            assert_eq!(
                w.iter()
                    .filter(|l| l.contains(&format!("src/{n}.java")))
                    .count(),
                1,
                "{err}"
            );
        }
        assert!(
            !err.contains("interrompu") && !err.contains("arrêt de l'envoi"),
            "{err}"
        );
        let log = env.log_lines();
        assert!(log.iter().all(|l| !l.contains("Big")), "{log:?}");
        assert!(log.iter().any(|l| l.contains("S0.java")), "{log:?}");
        assert!(log.iter().any(|l| l.contains("S1.java")), "{log:?}");
        assert!(out.contains(", 4 fichiers à enrichir"), "{out}");
    }
}

#[test]
fn req048_ac4_verbose_et_progression() {
    let env = scenario("");
    let o = env.run(&["build", "--verbose"], &[]);
    let (_, err) = text(&o);
    assert!(!o.status.success());
    assert!(
        err.lines()
            .all(|l| !(l.contains("llm enrich") && l.contains(BIG))),
        "{err}"
    );

    let env = scenario("");
    let o = env.run(&["build", "--show-progress"], &[]);
    let (_, err) = text(&o);
    let prog: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    let last = prog.last().unwrap_or_else(|| panic!("{err}"));
    assert_eq!(*last, "progression: enrichissement 100 % (2/2 fichiers)");
}

#[test]
fn req048_ac5_ac7_ligne_a6_et_update() {
    let env = scenario("");
    let o = env.run(&["build"], &[]);
    let (_, err) = text(&o);
    assert!(!o.status.success());
    let lines = a6_lines(&err);
    assert_eq!(lines.len(), 1, "{err}");
    let n = a6_count(lines[0]);
    assert!(n > 50);
    assert_eq!(n, BIG_20000_CALLS);
    let morceaux = split(&java("Big", K), MAX_CHUNK).len();
    assert!(
        n < morceaux * (K + 1).div_ceil(150),
        "{n} appels, {morceaux} morceaux"
    );
    assert_eq!(warnings(&err).len(), 1, "{err}");

    let o = env.run(&["update"], &[]);
    let (_, err2) = text(&o);
    assert!(!o.status.success());
    let lines2 = a6_lines(&err2);
    assert_eq!(lines2, lines, "{err2}");
}

#[test]
fn req048_ac8_ligne_identique_et_sans_jargon() {
    let mut reference: Option<String> = None;
    let variants: [&[&str]; 5] = [
        &["build"],
        &["build", "--verbose"],
        &["build", "--show-progress"],
        &["build", "--format-retries", "0"],
        &["build", "--format-retries", "5"],
    ];
    for args in variants {
        let env = scenario("");
        let o = env.run(args, &[]);
        let (_, err) = text(&o);
        let lines: Vec<&str> = err
            .lines()
            .filter(|l| l.starts_with("avertissement:") && l.contains(BIG))
            .collect();
        assert_eq!(lines.len(), 1, "{args:?} {err}");
        let line = lines[0];
        a6_count(line);
        // « essai » comme mot : « nécessaires » (texte imposé d'A-6) le contient en sous-chaîne.
        assert!(
            !line
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w == "essai" || w == "essais"),
            "{line}"
        );
        for banned in [
            "150",
            "MAX_CALLS",
            "plafond",
            "groupe",
            "max_chunk_chars",
            "batch_files",
            "tronquée",
            "non conforme",
        ] {
            assert!(!line.contains(banned), "{banned} dans {line}");
        }
        match &reference {
            None => reference = Some(line.to_string()),
            Some(r) => assert_eq!(r, line, "{args:?}"),
        }
        if args.contains(&"--verbose") {
            assert!(
                err.lines().all(|l| !(l.starts_with("trace:")
                    && l.contains("llm enrich")
                    && l.contains(BIG))),
                "{err}"
            );
            assert!(err.lines().all(|l| !l.contains("politique:")), "{err}");
        }
    }
}

#[test]
fn req048_ac9_progression_bilan_et_status() {
    let env = scenario("");
    let o = env.run(&["build", "--show-progress"], &[]);
    let (out, err) = text(&o);
    assert!(!o.status.success());
    let a6: Vec<usize> = err
        .lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with("avertissement:"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(a6.len(), 1, "{err}");
    let all: Vec<&str> = err.lines().collect();
    let (last, others) = all.split_last().unwrap();
    // Message d'erreur final du code non nul (REQ-010), inchangé depuis T-033.
    assert_eq!(*last, "erreur: 1 fichier(s) à enrichir", "{err}");
    assert_eq!(
        all.iter().filter(|l| l.starts_with("erreur:")).count(),
        1,
        "{err}"
    );
    for l in others {
        if l.starts_with("avertissement:") {
            a6_count(l);
        } else {
            assert!(l.starts_with("progression: "), "ligne inattendue : {l}");
        }
    }
    let enrich: Vec<(usize, &str)> = err
        .lines()
        .enumerate()
        .filter(|(_, l)| l.starts_with("progression: enrichissement"))
        .collect();
    assert_eq!(
        enrich.first().unwrap().1,
        "progression: enrichissement 0 % (0/2 fichiers)"
    );
    assert_eq!(
        enrich.last().unwrap().1,
        "progression: enrichissement 100 % (2/2 fichiers)"
    );
    assert!(enrich.first().unwrap().0 < a6[0] && a6[0] < enrich.last().unwrap().0);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(
        out.starts_with("build terminé : 2 fichiers traités, "),
        "{out}"
    );
    assert!(
        out.trim_end()
            .ends_with(", 0 fichiers en attente, 1 fichiers à enrichir"),
        "{out}"
    );
    assert!(!out.contains('\x1b') && !err.contains('\x1b') && !err.contains('\r'));

    let env2 = scenario("");
    let o2 = env2.run(&["build", "--show-progress"], &[("NO_COLOR", "1")]);
    let (out2, err2) = text(&o2);
    assert_eq!(out, out2);
    assert_eq!(err, err2);

    let st = env.run(&["status"], &[]);
    let (sout, _) = text(&st);
    assert!(
        sout.lines()
            .any(|l| l == "1 fichiers à enrichir (résumés et concepts)"),
        "{sout}"
    );
}

#[test]
fn req048_ac10_exclusion_apres_le_build() {
    let env = scenario("");
    let o = env.run(&["build"], &[]);
    assert!(!o.status.success());
    env.project(&format!("exclude = [\"{BIG}\"]\n"));
    let st = env.run(&["status"], &[]);
    let (sout, _) = text(&st);
    assert!(
        sout.lines().any(|l| l == format!("supprimé\t{BIG}")),
        "{sout}"
    );
    let o = env.run(&["update"], &[]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out} {err}");
    assert!(warnings(&err).is_empty(), "{err}");
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(
        out.trim_end()
            .ends_with(", 0 fichiers à enrichir, 1 fichiers exclus par les filtres"),
        "{out}"
    );
}

#[test]
fn req047_ac14_deux_morceaux_quatre_lignes() {
    let src = java("Big", 199);
    let limit = src.chars().count() / 2 + 100;
    let sizes: Vec<usize> = split(&src, limit)
        .iter()
        .map(|p| p.chars().count())
        .collect();
    assert_eq!(sizes.len(), 2);
    let env = Env::new(
        &[(BIG, src)],
        &format!("max_chunk_chars = {limit}\nconcurrency = 1\n"),
    );
    let o = env.run(&["build", "--verbose"], &[]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("llm enrich") && l.contains(BIG))
        .collect();
    assert_eq!(lines.len(), 4, "{err}");
    for (i, c) in sizes.iter().enumerate() {
        let d = lines[i * 2];
        let f = lines[i * 2 + 1];
        assert!(
            d.contains(&format!("llm enrich départ {BIG} [{}/2] {c} car.", i + 1))
                && d.ends_with(&format!(" {c} car.")),
            "{d}"
        );
        assert!(
            f.contains(&format!("llm enrich fin {BIG} [{}/2] ok (", i + 1)),
            "{f}"
        );
    }
    for l in &lines {
        for banned in ["/4]", "lot ", "groupe", "nom court"] {
            assert!(!l.contains(banned), "{l}");
        }
    }

    let o = env.run(&["build"], &[]);
    let (out, err) = text(&o);
    assert_eq!(err, "");
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(
        out.trim_end()
            .ends_with(", 0 fichiers en attente, 0 fichiers à enrichir"),
        "{out}"
    );

    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    let o = env.run(&["build", "--show-progress"], &[]);
    let (_, err) = text(&o);
    let prog: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    assert_eq!(
        prog,
        [
            "progression: enrichissement 0 % (0/1 fichiers)",
            "progression: enrichissement 100 % (1/1 fichiers)"
        ]
    );
}
