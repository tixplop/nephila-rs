//! Clés `enrich_include` et `enrich_exclude` (REQ-206, T-298) : lecture, erreurs, plafond, E-2.
#![allow(non_snake_case)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;

const MOCK: &str = "provider = \"mock\"\n";

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new(config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("README.md"), "# Titre\n\nTexte.\n").unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, config).unwrap();
        Env {
            _tmp: tmp,
            repo,
            config: config_path,
        }
    }

    /// Lancé depuis le dépôt : le fichier de projet s'affiche `./.nephila.toml`.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .args(args)
            .current_dir(&self.repo)
            .env("NEPHILA_CONFIG", &self.config)
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn project(&self, text: &str) {
        fs::write(self.repo.join(".nephila.toml"), text).unwrap();
    }

    fn graph_dir(&self) -> PathBuf {
        self.repo.join(".nephila")
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn personal_line(env: &Env, key: &str, got: &str) -> String {
    format!(
        "erreur: configuration invalide ({}) : clé « {key} » doit être un tableau de motifs valides (reçu : {got})\n",
        env.config.display()
    )
}

#[test]
fn REQ206_AC3_AC6_erreurs_de_la_configuration_personnelle() {
    let cases = [
        ("enrich_exclude = \"a\"", "enrich_exclude", "\"a\""),
        ("enrich_exclude = [1]", "enrich_exclude", "[1]"),
        ("enrich_include = [\"a[b\"]", "enrich_include", "\"a[b\""),
        (
            "enrich_include = [\"src/**\", \"a[b\"]",
            "enrich_include",
            "\"a[b\"",
        ),
    ];
    for (extra, key, got) in cases {
        let env = Env::new(&format!("{MOCK}{extra}\n"));
        let o = env.run(&["build"]);
        assert_eq!(o.status.code(), Some(1), "{extra}");
        assert_eq!(stderr(&o), personal_line(&env, key, got), "{extra}");
        assert!(!env.graph_dir().exists(), "{extra}");
        let o = env.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{extra} : {}", stderr(&o));
        assert!(!stderr(&o).contains("erreur:"), "{extra}");
    }
}

#[test]
fn REQ206_AC4_AC7_erreurs_du_fichier_de_projet() {
    let many = |n: usize| format!("enrich_exclude = [{}]\n", "\"a\", ".repeat(n));
    let mut with_invalid = many(100);
    with_invalid = with_invalid.replace("\"a\", ]", "\"a[b\", \"a\"]");
    let cases = [
        (
            "enrich_include = \"a\"\n".to_string(),
            "erreur: ./.nephila.toml : clé `enrich_include` : tableau de chaînes attendu, ex. enrich_include = [\"src/**\"]\n".to_string(),
        ),
        (
            "enrich_exclude = \"a\"\n".to_string(),
            "erreur: ./.nephila.toml : clé `enrich_exclude` : tableau de chaînes attendu, ex. enrich_exclude = [\"legacy/\"]\n".to_string(),
        ),
        (
            "enrich_exclude = [1]\n".to_string(),
            "erreur: ./.nephila.toml : clé `enrich_exclude` : tableau de chaînes attendu, ex. enrich_exclude = [\"legacy/\"]\n".to_string(),
        ),
        (
            many(101),
            "erreur: ./.nephila.toml : clé `enrich_exclude` : trop de motifs `101` (au plus 100)\n".to_string(),
        ),
        (
            with_invalid,
            "erreur: ./.nephila.toml : clé `enrich_exclude` : trop de motifs `101` (au plus 100)\n".to_string(),
        ),
    ];
    for (text, expected) in cases {
        for args in [&["build"][..], &["build", "--no-llm"][..]] {
            let env = Env::new(MOCK);
            env.project(&text);
            let o = env.run(args);
            assert_eq!(o.status.code(), Some(1), "{args:?}");
            assert_eq!(stderr(&o), expected, "{args:?}");
            assert!(!env.graph_dir().exists());
        }
    }
    for (key, args) in [
        ("enrich_exclude", &["build"][..]),
        ("enrich_exclude", &["build", "--no-llm"][..]),
        ("enrich_include", &["build"][..]),
        ("enrich_include", &["build", "--no-llm"][..]),
    ] {
        let env = Env::new(MOCK);
        env.project(&format!("{key} = [\"a[b\"]\n"));
        let o = env.run(args);
        assert_eq!(o.status.code(), Some(1), "{key} {args:?}");
        let err = stderr(&o);
        assert!(
            err.starts_with(&format!(
                "erreur: ./.nephila.toml : clé `{key}` : motif invalide `a[b` ("
            )),
            "{err}"
        );
        assert!(
            err.ends_with(")\n") && err.matches('\n').count() == 1,
            "{err}"
        );
        assert!(!env.graph_dir().exists(), "{key} {args:?}");
    }
    // 100 motifs : accepté.
    for args in [&["build"][..], &["build", "--no-llm"][..]] {
        let env = Env::new(MOCK);
        env.project(&many(100));
        let o = env.run(args);
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    }
}

const E2_KEYS: &str = "exclude, exclude_types, exclude_defaults, enrich_include, enrich_exclude, model, summary_language, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries, ask";

#[test]
fn REQ206_AC5_AC7_liste_des_cles_permises_d_e2() {
    let env = Env::new(MOCK);
    env.project("zz = 1\n");
    let o = env.run(&["build"]);
    assert_eq!(
        stderr(&o),
        format!("erreur: ./.nephila.toml : clé inconnue `zz` (clés permises : {E2_KEYS})\n")
    );
    // Les deux clés sont permises dans le fichier de projet.
    env.project("enrich_include = [\"src/**\"]\nenrich_exclude = [\"legacy/\"]\n");
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
}

#[test]
fn REQ206_AC7_section_ask_avec_enrich_exclude_garde_ses_sept_cles() {
    let env = Env::new(MOCK);
    env.project("[ask]\nenrich_exclude = [\"a\"]\n");
    let o = env.run(&["build"]);
    assert_eq!(
        stderr(&o),
        "erreur: ./.nephila.toml : clé inconnue `ask.enrich_exclude` (clés permises dans [ask] : model, thinking, effort, max_output_tokens, max_context_chars, max_seeds, max_nodes)\n"
    );
}

// ---- Portée appliquée aux fichiers à enrichir (REQ-207, T-299) ----

const SEG: &str = "fichiers hors portée d'enrichissement";

/// Dépôt des AC : `src/Main.java`, `src/test/MainTest.java`, `docs/a.md`, `docs/migrations/b.md`.
struct Repo {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

impl Repo {
    fn new(personal: &str) -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let r = Repo {
            config: tmp.path().join("config.toml"),
            log: tmp.path().join("mock.log"),
            repo,
            _tmp: tmp,
        };
        for (p, c) in [
            ("src/Main.java", "public class Main {}\n"),
            ("src/test/MainTest.java", "public class MainTest {}\n"),
            ("docs/a.md", "# A\n\nTexte.\n"),
            ("docs/migrations/b.md", "# B\n\nTexte.\n"),
        ] {
            r.write(p, c);
        }
        r.personal(personal);
        r
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// Configuration personnelle : `mock`, un fichier par appel, plus `extra`.
    fn personal(&self, extra: &str) {
        fs::write(
            &self.config,
            format!(
                "provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 1\nbatch_files = 1\nformat_retries = 0\n{extra}"
            ),
        )
        .unwrap();
    }

    fn project(&self, text: &str) {
        self.write(".nephila.toml", text);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(&self.config, args)
    }

    fn run_with(&self, config: &Path, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", config)
            .env_remove("NEPHILA_MOCK_DIR")
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    /// Fichiers envoyés depuis le dernier appel à `calls` (le journal est vidé).
    fn calls(&self) -> Vec<String> {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let _ = fs::remove_file(&self.log);
        let mut v: Vec<String> = text
            .lines()
            .map(|l| l.split('\t').next().unwrap().to_string())
            .collect();
        v.sort();
        v
    }

    fn has_summary(&self, path: &str) -> bool {
        nephila::store::Store::open_readonly(&self.repo)
            .unwrap()
            .enrichment_entry(path)
            .unwrap()
            .is_some()
    }

    fn missing_config(&self) -> PathBuf {
        self.config.with_file_name("absent.toml")
    }
}

fn so(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn first_line(o: &Output) -> String {
    so(o).lines().next().unwrap_or("").to_string()
}

fn bilan(o: &Output) -> String {
    so(o)
        .lines()
        .find(|l| l.contains(" terminé : "))
        .unwrap_or_else(|| panic!("bilan absent : {}", so(o)))
        .to_string()
}

fn ok(o: &Output) {
    assert_eq!(o.status.code(), Some(0), "{}{}", so(o), stderr(o));
}

#[test]
fn REQ207_AC4_build_n_envoie_que_la_portee_et_garde_la_structure() {
    let r = Repo::new("enrich_code = true\n");
    let o = r.run(&["build"]);
    ok(&o);
    assert_eq!(r.calls(), vec!["docs/a.md", "src/Main.java"]);
    let line = bilan(&o);
    assert!(line.contains(", 0 fichiers à enrichir"), "{line}");
    assert!(line.ends_with(&format!(", 2 {SEG}")), "{line}");
    let q = r.run(&["query", "search", "MainTest"]);
    ok(&q);
    assert!(so(&q).contains("MainTest"), "{}", so(&q));
    assert!(!r.has_summary("src/test/MainTest.java"));
    let s = r.run(&["status", "--json"]);
    ok(&s);
    let v: serde_json::Value = serde_json::from_str(&so(&s)).unwrap();
    assert_eq!(v["to_enrich"], 0, "{}", so(&s));
    assert!(!so(&s).contains("enrich_code") && !so(&s).contains("enrich_exclude"));
}

#[test]
fn REQ207_AC5_sans_enrich_code_seuls_les_documents_hors_portee_comptent() {
    let r = Repo::new("");
    let o = r.run(&["build"]);
    ok(&o);
    assert_eq!(r.calls(), vec!["docs/a.md"]);
    let line = bilan(&o);
    assert!(line.ends_with(&format!(", 1 {SEG}")), "{line}");
}

#[test]
fn REQ207_AC6_motifs_changes_sans_reenvoi_puis_entree_dans_la_portee() {
    let r = Repo::new("enrich_code = true\nenrich_exclude = []\n");
    ok(&r.run(&["build"]));
    assert_eq!(r.calls().len(), 4);
    assert!(r.has_summary("src/test/MainTest.java"));
    // Retour à la liste par défaut : rien n'est réenvoyé ni retiré.
    r.personal("enrich_code = true\n");
    let o = r.run(&["update"]);
    ok(&o);
    assert_eq!(first_line(&o), "graphe à jour", "{}", so(&o));
    assert!(r.calls().is_empty());
    assert!(r.has_summary("src/test/MainTest.java"));
    // Le fichier change : ses résumés disparaissent, il n'est pas renvoyé.
    r.write(
        "src/test/MainTest.java",
        "public class MainTest { int x; }\n",
    );
    let o = r.run(&["update"]);
    ok(&o);
    assert!(r.calls().is_empty());
    assert!(!r.has_summary("src/test/MainTest.java"));
    assert!(bilan(&o).contains(", 0 fichiers à enrichir"), "{}", so(&o));
}

#[test]
fn REQ207_AC6_sens_inverse_les_fichiers_entres_dans_la_portee_partent() {
    let r = Repo::new("enrich_code = true\n");
    ok(&r.run(&["build"]));
    assert_eq!(r.calls(), vec!["docs/a.md", "src/Main.java"]);
    r.personal("enrich_code = true\nenrich_exclude = []\n");
    let o = r.run(&["update"]);
    ok(&o);
    assert_eq!(
        r.calls(),
        vec!["docs/migrations/b.md", "src/test/MainTest.java"]
    );
    assert!(!bilan(&o).contains(SEG), "{}", so(&o));
}

#[test]
fn REQ207_AC7_les_motifs_du_depot_sont_enregistres_et_relus_sans_configuration() {
    let r = Repo::new("enrich_code = true\n");
    r.project("enrich_exclude = [\"docs/\"]\n");
    ok(&r.run(&["build"]));
    assert_eq!(r.calls(), vec!["src/Main.java"]);
    let absent = r.missing_config();
    for args in [
        &["status", "--json"][..],
        &["build", "--no-llm"],
        &["status", "--json"],
    ] {
        let o = r.run_with(&absent, args);
        ok(&o);
    }
    let s = r.run_with(&absent, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&so(&s)).unwrap();
    assert_eq!(v["to_enrich"], 0, "{}", so(&s));
}

#[test]
fn REQ207_AC8_dry_run_evalue_les_motifs_effectifs_sans_les_enregistrer() {
    let r = Repo::new("enrich_code = true\nenrich_exclude = []\n");
    ok(&r.run(&["build"]));
    r.calls();
    let stored = nephila::store::Store::open_readonly(&r.repo)
        .unwrap()
        .enrich_scope()
        .unwrap();
    assert!(stored.personal_exclude.is_empty());
    r.personal("enrich_code = true\n");
    let o = r.run(&["build", "--dry-run"]);
    ok(&o);
    assert!(r.calls().is_empty());
    assert!(
        bilan(&o).ends_with(&format!(", enrichissement : estimé sans envoi, 2 {SEG}")),
        "{}",
        so(&o)
    );
    let after = nephila::store::Store::open_readonly(&r.repo)
        .unwrap()
        .enrich_scope()
        .unwrap();
    assert_eq!(after, stored);
}

#[test]
fn REQ207_AC9_sans_fichier_hors_portee_la_ligne_est_inchangee() {
    let r = Repo::new("enrich_code = true\n");
    fs::remove_file(r.repo.join("src/test/MainTest.java")).unwrap();
    fs::remove_file(r.repo.join("docs/migrations/b.md")).unwrap();
    let o = r.run(&["build"]);
    ok(&o);
    assert_eq!(
        bilan(&o),
        "build terminé : 2 fichiers traités, 3 nœuds, 1 liens, 0 références non résolues, 0 fichiers en attente, 0 fichiers à enrichir, 0/3 nœuds résumés"
    );
    assert!(!so(&o).contains("hors portée"));
    let o = r.run(&["update"]);
    ok(&o);
    assert_eq!(so(&o).trim_end(), "graphe à jour");
}

#[test]
fn REQ207_AC9_aucun_segment_sans_llm_actif_et_sorties_de_status_inchangees() {
    let r = Repo::new("enrich_code = true\n");
    let no_llm = r.run(&["build", "--no-llm"]);
    ok(&no_llm);
    assert!(!so(&no_llm).contains("hors portée"), "{}", so(&no_llm));
    let absent = r.missing_config();
    let none = r.run_with(&absent, &["build"]);
    ok(&none);
    assert!(!so(&none).contains("hors portée"), "{}", so(&none));
    ok(&r.run(&["build"]));
    let v = r.run(&["build", "--verbose"]);
    ok(&v);
    assert!(!stderr(&v).contains("enrich_include"), "{}", stderr(&v));
    assert!(!stderr(&v).contains("enrich_exclude"), "{}", stderr(&v));
    for args in [&["status"][..], &["status", "--json"]] {
        let s = r.run(args);
        ok(&s);
        assert!(
            !so(&s).contains("portée") && !so(&s).contains("scope"),
            "{}",
            so(&s)
        );
    }
}

#[test]
fn REQ207_AC10_un_fichier_exclu_par_exclude_n_est_pas_hors_portee() {
    let r = Repo::new("enrich_code = true\n");
    r.project("exclude = [\"src/test/\"]\n");
    let o = r.run(&["build"]);
    ok(&o);
    let line = bilan(&o);
    assert!(
        line.ends_with(&format!(", 1 {SEG}, 1 fichiers exclus par les filtres")),
        "{line}"
    );
}

#[test]
fn REQ207_AC11_place_du_segment_avec_et_sans_filtre_et_sous_dry_run() {
    let r = Repo::new("enrich_code = true\n");
    r.project("exclude = [\"docs/a.md\"]\n");
    let line = bilan(&r.run(&["build"]));
    assert!(
        line.ends_with(&format!(", 2 {SEG}, 1 fichiers exclus par les filtres")),
        "{line}"
    );
    let line = bilan(&r.run(&["build", "--dry-run"]));
    assert!(
        line.ends_with(&format!(
            ", enrichissement : estimé sans envoi, 2 {SEG}, 1 fichiers exclus par les filtres"
        )),
        "{line}"
    );
    fs::remove_file(r.repo.join(".nephila.toml")).unwrap();
    let line = bilan(&r.run(&["build", "--dry-run"]));
    assert!(
        line.ends_with(&format!(", enrichissement : estimé sans envoi, 2 {SEG}")),
        "{line}"
    );
    fs::remove_file(r.repo.join("src/test/MainTest.java")).unwrap();
    let line = bilan(&r.run(&["build"]));
    assert!(line.ends_with(&format!(", 1 {SEG}")), "{line}");
    assert!(!line.contains("0 fichiers hors portée"));
    fs::remove_file(r.repo.join("docs/migrations/b.md")).unwrap();
    let o = r.run(&["build"]);
    assert!(!so(&o).contains("0 fichiers hors portée"), "{}", so(&o));
    assert!(!so(&o).contains("hors portée"), "{}", so(&o));
}

#[test]
fn REQ207_AC12_dry_run_compte_avec_les_motifs_effectifs_et_ne_touche_pas_status() {
    let r = Repo::new("enrich_code = true\n");
    ok(&r.run(&["build"]));
    r.calls();
    let before = so(&r.run(&["status"]));
    r.personal("enrich_code = true\nenrich_exclude = [\"docs/\"]\n");
    let nollm = bilan(&r.run(&["build", "--no-llm"]));
    let o = r.run(&["build", "--dry-run"]);
    ok(&o);
    let line = bilan(&o);
    assert!(
        line.ends_with(&format!(", enrichissement : estimé sans envoi, 2 {SEG}")),
        "{line}"
    );
    // `fichiers à enrichir` garde le compte de `--no-llm`.
    let count = |l: &str| {
        l.split(", ")
            .find(|s| s.ends_with("fichiers à enrichir"))
            .unwrap()
            .to_string()
    };
    assert_eq!(count(&line), count(&nollm));
    // L'estimation ne compte que le fichier entré dans la portée (`MainTest.java`).
    let est: Vec<String> = so(&o)
        .lines()
        .filter(|l| l.starts_with("estimation"))
        .map(String::from)
        .collect();
    assert!(!est.is_empty(), "{}", so(&o));
    assert!(est.iter().any(|l| l.contains("1 fichiers")), "{est:?}");
    assert!(r.calls().is_empty());
    assert_eq!(so(&r.run(&["status"])), before);
}
