//! Plafond d'appels d'enrichissement par commande (REQ-186, REQ-187) : tranches successives,
//! concurrence, nouveaux essais de format, fichier envoyé seul, attentes non interrompues,
//! autre cause d'arrêt, sorties et codes. Aucun test ne dépend d'un délai serré.

use assert_cmd::cargo::cargo_bin;
use nephila::chunk::split;
use nephila::store::Store;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[path = "support/plan_server.rs"]
mod plan_server;
use plan_server::{Act, Server};

const KEY_VAR: &str = "NEPHILA_T277_KEY";

fn a10(m: usize, f: usize) -> String {
    format!("avertissement: plafond de {m} appels atteint : {f} fichiers laissés à enrichir ; relancez nephila update pour continuer ou augmentez max_calls")
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn small(i: usize) -> String {
    format!("# D{i}\n\nTexte du document {i}.\n")
}

fn docs(n: usize) -> Vec<(String, String)> {
    (1..=n).map(|i| (format!("d{i}.md"), small(i))).collect()
}

fn names(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("d{i}.md")).collect()
}

/// `concurrency` 1, `batch_files` 1 et `format_retries` 0, sauf si `settings` les règle.
fn with_defaults(settings: &str) -> String {
    let mut s = String::new();
    for (k, v) in [
        ("concurrency", 1),
        ("batch_files", 1),
        ("format_retries", 0),
    ] {
        if !settings.contains(k) {
            s.push_str(&format!("{k} = {v}\n"));
        }
    }
    s + settings
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

impl Env {
    /// `settings` : lignes TOML de la configuration personnelle, après `provider` et `model`.
    fn with(files: &[(String, Vec<u8>)], provider_lines: &str, settings: &str) -> Env {
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
        fs::write(&config, format!("{provider_lines}{settings}")).unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config,
        }
    }

    /// Faux LLM `mock`, `concurrency` 1, `batch_files` 1, `format_retries` 0 par défaut.
    fn mock(files: &[(String, String)], settings: &str) -> Env {
        let f: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(p, c)| (p.clone(), c.clone().into_bytes()))
            .collect();
        Env::with(
            &f,
            "provider = \"mock\"\nmodel = \"mock\"\n",
            &with_defaults(settings),
        )
    }

    fn server(files: &[(String, String)], url: &str, settings: &str) -> Env {
        let f: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(p, c)| (p.clone(), c.clone().into_bytes()))
            .collect();
        Env::with(
            &f,
            &format!(
                "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\napi_key_env = \"{KEY_VAR}\"\n"
            ),
            &with_defaults(settings),
        )
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
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
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "10");
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    /// Une ligne par appel reçu par le `mock`.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn project(&self, content: &str) {
        fs::write(self.repo.join(".nephila.toml"), content).unwrap();
    }

    /// Parmi `paths`, ceux qui ont une entrée d'enrichissement.
    fn enriched(&self, paths: &[String]) -> Vec<String> {
        let store = Store::open_readonly(&self.repo).unwrap();
        paths
            .iter()
            .filter(|p| store.enrichment_entry(p).unwrap().is_some())
            .cloned()
            .collect()
    }
}

fn last(s: &str) -> &str {
    s.lines().last().unwrap_or("")
}

fn lines_of<'a>(s: &'a str, prefix: &str) -> Vec<&'a str> {
    s.lines().filter(|l| l.starts_with(prefix)).collect()
}

/// Bilan sans son dernier compte (« …, N fichiers à enrichir »).
fn without_count(out: &str) -> String {
    let (head, tail) = out.trim_end().rsplit_once(", ").unwrap();
    assert!(tail.ends_with("fichiers à enrichir"), "{out}");
    head.to_string()
}

// ---- AC-186-2, AC-187-1, AC-187-6, AC-187-7, AC-187-8 : tranches successives ----

#[test]
fn req186_ac2_req187_ac1_ac6_tranches_successives() {
    let all = names(7);
    let env = Env::mock(&docs(7), "max_calls = 3\n");
    let reference = Env::mock(&docs(7), "");
    let r = reference.run(&["build"]);
    assert_eq!(r.status.code(), Some(0));
    let (ref_out, ref_err) = text(&r);
    assert!(!ref_err.contains("plafond"), "{ref_err}");

    let o = env.run(&["build"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.calls().len(), 3);
    assert_eq!(env.enriched(&all), names(3));
    let st = text(&env.run(&["status"])).0;
    assert!(st.contains("4 fichiers à enrichir"), "{st}");
    // AC-187-1 : une seule ligne du plafond, dernière de stderr, aucune erreur ; stdout aux comptes près.
    assert_eq!(lines_of(&err, "avertissement:"), vec![a10(3, 4)], "{err}");
    assert_eq!(last(&err), a10(3, 4), "{err}");
    assert!(lines_of(&err, "erreur:").is_empty(), "{err}");
    assert_eq!(without_count(&out), without_count(&ref_out));
    assert!(out.contains(", 4 fichiers à enrichir"), "{out}");
    assert!(ref_out.contains(", 0 fichiers à enrichir"), "{ref_out}");
    assert!(!out.contains("plafond"), "{out}");
    assert!(!err.contains('\u{1b}'));

    // update : 3 appels de plus, 1 fichier laissé.
    let o = env.run(&["update"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.calls().len(), 6);
    assert_eq!(env.enriched(&all), names(6));
    assert_eq!(last(&err), a10(3, 1), "{err}");

    // update : 1 appel, tout enrichi, aucune ligne du plafond.
    let o = env.run(&["update"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.calls().len(), 7);
    assert_eq!(env.enriched(&all), all);
    assert!(
        !err.contains("plafond") && !out.contains("plafond"),
        "{err}"
    );
    assert!(out.contains(", 0 fichiers à enrichir"), "{out}");
}

#[test]
fn req187_ac7_progression_avant_l_avertissement() {
    let env = Env::mock(&docs(7), "max_calls = 3\n");
    let o = env.run(&["build", "--show-progress"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let progress = lines_of(&err, "progression: enrichissement");
    // Pourcentage entier de 3/7 (DEC-053) : ⌊100·3/7⌋ = 42.
    assert_eq!(
        progress.last().copied(),
        Some("progression: enrichissement 42 % (3/7 fichiers)"),
        "{err}"
    );
    let all: Vec<&str> = err.lines().collect();
    let pos = all
        .iter()
        .rposition(|l| l.starts_with("progression:"))
        .unwrap();
    assert_eq!(all[pos + 1..].to_vec(), vec![a10(3, 4)], "{err}");
}

#[test]
fn req187_ac7_ac8_verbose_une_trace_et_aucun_depart_apres() {
    let env = Env::mock(&docs(7), "max_calls = 3\n");
    let o = env.run(&["build", "--verbose"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let all: Vec<&str> = err.lines().collect();
    let policy: Vec<usize> = (0..all.len())
        .filter(|&i| all[i].contains("politique: arrêt de l'envoi"))
        .collect();
    assert_eq!(policy.len(), 1, "{err}");
    assert!(
        all[policy[0]].ends_with(
            "politique: arrêt de l'envoi (plafond de 3 appels atteint), 4 fichiers non envoyés"
        ),
        "{}",
        all[policy[0]]
    );
    assert!(
        all[policy[0] + 1..]
            .iter()
            .all(|l| !l.contains("llm enrich départ")),
        "{err}"
    );
    assert!(all.iter().any(|l| l.contains("appels: 3 appels")), "{err}");
    let config = all.iter().find(|l| l.contains("config:")).unwrap();
    assert!(
        config.ends_with(" batch_files=1 format_retries=0 max_calls=3"),
        "{config}"
    );
    // A-10 est la dernière ligne ; aucune trace après elle.
    assert_eq!(*all.last().unwrap(), a10(3, 4), "{err}");
}

// ---- AC-186-3 : concurrence ----

#[test]
fn req186_ac3_concurrence_le_plafond_borne_les_requetes() {
    let server = Server::start(|_| Act::Ok(200));
    let env = Env::server(&docs(20), &server.base_url, "concurrency = 4\n");
    let o = env.run(&["build", "--max-calls", "5", "--verbose"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(server.requests() <= 5, "{} requêtes", server.requests());
    assert_eq!(env.enriched(&names(20)).len(), 5, "{err}");
    assert!(err.lines().any(|l| l.contains("appels: 5 appels")), "{err}");
    assert_eq!(last(&err), a10(5, 15), "{err}");
}

// ---- AC-186-4 : travail qui tient exactement dans le plafond ----

#[test]
fn req186_ac4_travail_exactement_au_plafond_sans_trace() {
    let capped = Env::mock(&docs(5), "");
    let plain = Env::mock(&docs(5), "");
    let a = capped.run(&["build", "--max-calls", "5"]);
    let b = plain.run(&["build"]);
    assert_eq!(capped.calls().len(), 5);
    assert_eq!(a.status.code(), Some(0));
    assert_eq!(a.status.code(), b.status.code());
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
    assert!(!text(&a).1.contains("plafond"));
    assert_eq!(capped.enriched(&names(5)), names(5));

    // Sous --verbose : seule la ligne config diffère, par son champ max_calls.
    let a = capped.run(&["update", "--max-calls", "5", "--verbose"]);
    let b = plain.run(&["update", "--verbose"]);
    // Horodatage `[0.123s] ` retiré, durées en ms normalisées ; seule la ligne config est exclue.
    let strip = |o: &Output| -> Vec<String> {
        text(o)
            .1
            .lines()
            .filter(|l| !l.contains("config:"))
            .map(|l| {
                let l = l.split_once("s] ").map_or(l, |(_, rest)| rest);
                let words: Vec<&str> = l.split(' ').collect();
                (0..words.len())
                    .map(|i| {
                        if words.get(i + 1) == Some(&"ms") && words[i].parse::<u64>().is_ok() {
                            "N"
                        } else {
                            words[i]
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    };
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(strip(&a), strip(&b));
    assert!(!text(&a).1.contains("plafond de"));
}

// ---- AC-186-5 : nouveaux essais de format ----

#[test]
fn req186_ac5_nouvel_essai_de_format_refuse_laisse_le_fichier() {
    let env = Env::mock(&docs(3), "format_retries = 2\n");
    let o = env.run_env(
        &["build", "--max-calls", "3"],
        &[("NEPHILA_MOCK_MALFORMED", "d1.md:1,d2.md:1,d3.md:1")],
    );
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.calls().len(), 3, "{:?}", env.calls());
    assert_eq!(env.enriched(&names(3)), names(1));
    assert!(!err.contains(" essais sur "), "{err}");
    assert_eq!(last(&err), a10(3, 2), "{err}");
    assert!(lines_of(&err, "erreur:").is_empty(), "{err}");
    let st = text(&env.run(&["status"])).0;
    assert!(st.contains("2 fichiers à enrichir"), "{st}");
    assert!(out.contains("2 fichiers à enrichir"), "{out}");
}

// ---- AC-186-6, AC-186-13 : fichier envoyé seul ----

fn big_doc() -> String {
    (1..=3)
        .map(|i| format!("# Section {i}\n\n{}\n\n", "mot ".repeat(40)))
        .collect()
}

const MAX_CHUNK: usize = 250;

fn tranche_env(files: &[(&str, String)]) -> Env {
    let f: Vec<(String, String)> = files
        .iter()
        .map(|(p, c)| (p.to_string(), c.clone()))
        .collect();
    let env = Env::mock(&f, "");
    env.project(&format!("max_chunk_chars = {MAX_CHUNK}\n"));
    env
}

fn files_with_big() -> Vec<(&'static str, String)> {
    vec![
        ("a1.md", small(1)),
        ("a2.md", small(2)),
        ("z_big.md", big_doc()),
    ]
}

#[test]
fn req186_ac6_gros_fichier_reserve_ses_appels() {
    assert_eq!(split(&big_doc(), MAX_CHUNK).len(), 3);
    let paths: Vec<String> = vec!["a1.md".into(), "a2.md".into(), "z_big.md".into()];

    let env = tranche_env(&files_with_big());
    let o = env.run(&["build", "--max-calls", "4"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.calls().len(), 2, "{:?}", env.calls());
    assert_eq!(env.enriched(&paths), vec!["a1.md", "a2.md"]);
    assert_eq!(last(&err), a10(4, 1), "{err}");
    assert!(lines_of(&err, "erreur:").is_empty(), "{err}");

    let env = tranche_env(&files_with_big());
    let o = env.run(&["build", "--max-calls", "5"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.calls().len(), 5, "{:?}", env.calls());
    assert_eq!(env.enriched(&paths), paths);
    assert!(!err.contains("plafond"), "{err}");
}

#[test]
fn req186_ac13_gros_fichier_plus_gros_que_le_plafond() {
    let a11 = "avertissement: z_big.md : trop d'appels d'enrichissement (3 nécessaires, au plus 2 par commande), aucun envoyé ; augmentez max_calls ou excluez ce fichier avec exclude dans .nephila.toml";
    let env = tranche_env(&[("z_big.md", big_doc())]);
    let o = env.run(&["build", "--max-calls", "2"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(env.calls().is_empty());
    assert!(err.lines().any(|l| l == a11), "{err}");
    assert_eq!(last(&err), "erreur: 1 fichier(s) à enrichir", "{err}");
    assert!(!err.contains("plafond de"), "{err}");

    // Un petit document placé après lui par chemin part quand même.
    let env = tranche_env(&[("a0.md", big_doc()), ("b.md", small(9))]);
    let o = env.run(&["build", "--max-calls", "2"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(env.calls().len(), 1, "{:?}", env.calls());
    assert_eq!(env.enriched(&["a0.md".into(), "b.md".into()]), vec!["b.md"]);
    assert!(
        err.contains(
            "a0.md : trop d'appels d'enrichissement (3 nécessaires, au plus 2 par commande)"
        ),
        "{err}"
    );
    assert!(!err.contains("plafond de"), "{err}");
    assert_eq!(last(&err), "erreur: 1 fichier(s) à enrichir", "{err}");
}

// ---- AC-186-7 : les attentes en cours ne sont pas interrompues ----

#[test]
fn req186_ac7_attente_non_interrompue_par_le_plafond() {
    let server = Server::start(|i| match i {
        1 => Act::Status(429, Some("1")),
        _ => Act::Ok(0),
    });
    let env = Env::server(&docs(3), &server.base_url, "concurrency = 2\n");
    let o = env.run(&["build", "--max-calls", "2"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(server.requests(), 3, "{err}");
    assert_eq!(env.enriched(&names(3)).len(), 2, "{err}");
    assert!(!err.contains("échec"), "{err}");
    assert_eq!(lines_of(&err, "avertissement:"), vec![a10(2, 1)], "{err}");
    assert!(lines_of(&err, "erreur:").is_empty(), "{err}");
}

// ---- AC-186-8 : une autre cause garde sa ligne ----

#[test]
fn req186_ac8_cle_refusee_sans_mention_du_plafond() {
    let server = Server::start(|_| Act::Status(401, None));
    let env = Env::server(&docs(3), &server.base_url, "");
    let o = env.run(&["build", "--max-calls", "10"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    let errors = lines_of(&err, "erreur:");
    assert_eq!(errors.len(), 1, "{err}");
    assert_eq!(
        errors[0],
        "erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env",
        "{err}"
    );
    assert_eq!(last(&err), errors[0]);
    assert!(
        !err.contains("plafond") && !err.contains("max_calls"),
        "{err}"
    );
}

// ---- AC-187-2, AC-187-3 : plafond atteint et autre raison d'échec ----

#[test]
fn req187_ac2_plafond_et_fichier_non_conforme() {
    let server = Server::start(|i| if i == 0 { Act::Garbage } else { Act::Ok(0) });
    let env = Env::server(&docs(4), &server.base_url, "");
    let o = env.run(&["build", "--max-calls", "2"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(server.requests(), 2);
    let all: Vec<&str> = err.lines().collect();
    let n = all.len();
    assert_eq!(all[n - 2], a10(2, 2), "{err}");
    assert_eq!(all[n - 1], "erreur: 1 fichier(s) à enrichir", "{err}");
    assert!(!all[n - 1].contains("plafond") && !all[n - 1].contains("max_calls"));
    assert_eq!(env.enriched(&names(4)), vec!["d2.md"]);
}

#[cfg(unix)]
#[test]
fn req187_ac3_plafond_et_fichier_en_attente() {
    use std::os::unix::fs::PermissionsExt;
    let mut files = docs(3);
    files.push(("a0.md".into(), small(0)));
    let env = Env::mock(&files, "");
    // Horodatage ancien : l'empreinte est réutilisée sans relecture au build suivant.
    let path = env.repo.join("a0.md");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        eprintln!("test non applicable : les droits ne protègent rien (administrateur)");
        return;
    }
    let o = env.run(&["build", "--max-calls", "1"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(env.calls().len(), 1);
    let all: Vec<&str> = err.lines().collect();
    assert_eq!(all[all.len() - 2], a10(1, 2), "{err}");
    assert_eq!(last(&err), "erreur: 1 fichier(s) en attente", "{err}");
}

// ---- AC-187-8 : ligne config, estimation ----

fn config_line(err: &str) -> String {
    err.lines()
        .find(|l| l.contains("config:"))
        .unwrap_or_else(|| panic!("{err}"))
        .to_string()
}

#[test]
fn req187_ac8_tls_insecure_suit_max_calls() {
    let env = Env::mock(&docs(7), "max_calls = 3\ntls_insecure = true\n");
    let o = env.run(&["build", "--verbose"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(
        config_line(&err).ends_with(" max_calls=3 tls_insecure"),
        "{err}"
    );
}

#[test]
fn req187_ac8_dry_run_estimation_inchangee() {
    let env = Env::mock(&docs(7), "");
    let plain = env.run(&["build", "--dry-run"]);
    let capped = env.run(&["build", "--dry-run", "--max-calls", "3", "--verbose"]);
    assert_eq!(plain.status.code(), Some(0), "{}", text(&plain).1);
    assert_eq!(capped.status.code(), Some(0), "{}", text(&capped).1);
    assert!(config_line(&text(&capped).1).contains(" max_calls=3"));
    let est = |o: &Output| -> Vec<String> {
        text(o)
            .0
            .lines()
            .filter(|l| l.starts_with("estimation"))
            .map(String::from)
            .collect()
    };
    assert!(!est(&plain).is_empty());
    assert_eq!(est(&plain), est(&capped));
}

#[test]
fn req187_ac8_no_llm_sans_max_calls() {
    let env = Env::mock(&docs(7), "");
    let o = env.run(&["build", "--no-llm", "--max-calls", "3", "--verbose"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let line = config_line(&err);
    assert!(line.ends_with("config: llm inactif (--no-llm)"), "{line}");
    assert!(!err.contains("max_calls"), "{err}");
}

// ---- AC-187-6 : plafond de la configuration et option, sans couleur ----

#[test]
fn req187_ac6_option_prioritaire_et_aucun_octet_d_echappement() {
    let env = Env::mock(&docs(7), "max_calls = 3\n");
    let o = env.run(&["build", "--max-calls", "5"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.calls().len(), 5);
    assert_eq!(last(&err), a10(5, 2), "{err}");

    for no_color in [None, Some("1")] {
        let env = Env::mock(&docs(7), "max_calls = 3\n");
        let extra: Vec<(&str, &str)> = no_color.map(|v| ("NO_COLOR", v)).into_iter().collect();
        let o = env.run_env(&["build"], &extra);
        assert!(!o.stderr.contains(&0x1b) && !o.stdout.contains(&0x1b));
        assert_eq!(last(&text(&o).1), a10(3, 4));
    }
}

// ---- AC-186-13 : plus de 50 appels ----

#[test]
fn req186_ac13_plus_de_cinquante_appels_donne_a6_seul() {
    let mut class = String::from("public class Big {\n");
    for i in 0..800 {
        class.push_str(&format!("    void m{i}() {{}}\n"));
    }
    class.push_str("}\n");
    let env = Env::mock(&[("src/Big.java".into(), class)], "enrich_code = true\n");
    env.project("max_chunk_chars = 200\n");
    let o = env.run(&["build", "--max-calls", "2"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(env.calls().is_empty());
    let w = lines_of(&err, "avertissement:");
    assert_eq!(w.len(), 1, "{err}");
    assert!(w[0].contains("au plus 50 par fichier"), "{}", w[0]);
    assert!(
        !err.contains("par commande") && !err.contains("plafond de"),
        "{err}"
    );
}
