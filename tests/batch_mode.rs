//! Option `--batch` de `build` et `update` (REQ-211) : portée, refus d'usage, refus propre du
//! fournisseur `mock`, non-régression de `.nephila.toml` (AC-211-1 à AC-211-5, AC-211-7,
//! AC-211-8 pour `mock`) ; pilote du mode lot contre le faux serveur de lots : collecte, envoi,
//! attente, dernière passe, lots suivants (REQ-211 à REQ-214, REQ-217).

#![allow(dead_code)]

#[path = "support/batch_server.rs"]
mod batch_server;
#[path = "support/fake_server.rs"]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use batch_server::{BatchServer, Outcome};
use fake_server::{FakeServer, Reply};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const KEY_VAR: &str = "NEPHILA_T305_KEY";
const E31: &str =
    "erreur: ce fournisseur n'a pas de mode lot ; relancez nephila update sans --batch";

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("code de sortie")
}

fn help(args: &[&str]) -> String {
    let o = Command::new(cargo_bin("nephila"))
        .args(args)
        .arg("--help")
        .output()
        .unwrap();
    out(&o).split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

impl Env {
    /// `config` : contenu de la configuration personnelle, ou `None` pour n'en avoir aucune.
    fn new(config: Option<&str>) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&mock).unwrap();
        let config_path = tmp.path().join("config.toml");
        if let Some(c) = config {
            fs::write(&config_path, c).unwrap();
        }
        fs::write(repo.join("a.md"), "# A\n\nTexte du document.\n").unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config: config_path,
        }
    }

    /// Dépôt de documents `docs/<n>.md`, configuration `provider` vers `url`, un fichier par appel ;
    /// `extra` : lignes de configuration en plus.
    fn http(provider: &str, url: &str, extra: &str, names: &[&str]) -> Env {
        // `batch_files = 1` sauf si `extra` le règle.
        let env = Env::new(Some(&format!(
            "provider = \"{provider}\"\nmodel = \"m\"\nbase_url = \"{url}\"\napi_key_env = \"{KEY_VAR}\"\nconcurrency = 1\n{}{extra}",
            if extra.contains("batch_files") { "" } else { "batch_files = 1\n" }
        )));
        fs::remove_file(env.repo.join("a.md")).unwrap();
        fs::create_dir_all(env.repo.join("docs")).unwrap();
        for n in names {
            fs::write(
                env.repo.join(path(n)),
                format!("# Document {n}\n\nTexte du document {n}.\n"),
            )
            .unwrap();
        }
        env
    }

    fn mock() -> Env {
        Env::new(Some("provider = \"mock\"\nmodel = \"mock\"\n"))
    }

    fn server(provider: &str, url: &str) -> Env {
        Env::new(Some(&format!(
            "provider = \"{provider}\"\nmodel = \"m\"\nbase_url = \"{url}\"\napi_key_env = \"{KEY_VAR}\"\n"
        )))
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_BATCH_POLL_MS", "5")
            .env("NEPHILA_RETRY_BASE_MS", "1")
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    fn calls(&self) -> usize {
        fs::read_to_string(&self.log)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn to_enrich(&self) -> usize {
        let o = self.run(&["status", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out(&o)).unwrap();
        v["to_enrich"].as_u64().unwrap() as usize
    }
}

#[test]
fn req211_ac1_aide_de_build_et_update_cite_batch_et_batch_files() {
    for cmd in ["build", "update"] {
        let h = help(&[cmd]);
        assert!(h.contains("--batch Confie"), "{cmd} : {h}");
        assert!(h.contains("--batch-files <N>"), "{cmd} : {h}");
    }
    for args in [
        vec!["status", "--batch"],
        vec!["query", "ask", "--batch", "x"],
    ] {
        let o = Command::new(cargo_bin("nephila"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(code(&o), 2, "{args:?}");
        assert!(
            err(&o).contains("unexpected argument '--batch' found"),
            "{args:?} : {}",
            err(&o)
        );
    }
}

#[test]
fn req211_ac2_no_llm_et_batch_sont_incompatibles() {
    for cmd in ["build", "update"] {
        let env = Env::mock();
        let o = env.run(&[cmd, "--no-llm", "--batch"]);
        assert_eq!(code(&o), 2, "{cmd}");
        let e = err(&o);
        assert!(
            e.contains("--no-llm") && e.contains("--batch"),
            "{cmd} : {e}"
        );
        assert!(!env.repo.join(".nephila").exists(), "{cmd}");
    }
}

#[test]
fn req211_ac3_sans_configuration_batch_ne_change_rien() {
    for cmd in ["build", "update"] {
        let plain = Env::new(None);
        let batch = Env::new(None);
        let a = plain.run(&[cmd]);
        let b = batch.run(&[cmd, "--batch"]);
        assert_eq!(code(&b), 0, "{cmd} : {}", err(&b));
        assert_eq!(code(&a), 0);
        assert_eq!(out(&a), out(&b), "{cmd}");
        assert_eq!(err(&a), err(&b), "{cmd}");
        assert_eq!(batch.calls(), 0);
    }
}

#[test]
fn req211_ac4_dry_run_batch_n_envoie_rien_et_ne_change_pas_la_sortie() {
    for provider in ["anthropic", "openai"] {
        let server = BatchServer::start();
        let env = Env::server(provider, &server.base_url);
        let plain = env.run(&["build", "--dry-run"]);
        let batch = env.run(&["build", "--dry-run", "--batch"]);
        assert_eq!(code(&plain), 0, "{provider} : {}", err(&plain));
        assert_eq!(code(&batch), 0, "{provider} : {}", err(&batch));
        assert_eq!(out(&plain), out(&batch), "{provider}");
        assert_eq!(err(&plain), err(&batch), "{provider}");
        assert!(server.requests().is_empty(), "{provider}");
    }
}

#[test]
fn req211_ac5_mock_refuse_le_mode_lot_et_garde_la_structure() {
    let env = Env::mock();
    let o = env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(err(&o).lines().last(), Some(E31), "{}", err(&o));
    assert!(!env.log.exists(), "le mock ne doit pas être appelé");
    let q = env.run(&["query", "search", "A"]);
    assert_eq!(code(&q), 0);
    assert!(out(&q).contains("a.md"), "{}", out(&q));
    assert_eq!(env.to_enrich(), 1);
}

#[test]
fn req211_ac5_rien_a_enrichir_graphe_a_jour_sans_erreur() {
    let env = Env::mock();
    let b = env.run(&["build"]);
    assert_eq!(code(&b), 0, "{}", err(&b));
    let calls = env.calls();
    let o = env.run(&["update", "--batch"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("graphe à jour"), "{}", out(&o));
    assert!(!err(&o).contains("erreur:"), "{}", err(&o));
    assert_eq!(env.calls(), calls);
}

#[test]
fn req211_ac7_batch_n_est_pas_une_cle_de_projet() {
    let env = Env::mock();
    fs::write(env.repo.join(".nephila.toml"), "batch = true\n").unwrap();
    let o = env.run(&["build"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(
        err(&o).contains(".nephila.toml : clé inconnue `batch` (clés permises : "),
        "{}",
        err(&o)
    );
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req211_ac8_texte_exact_du_refus_du_mock_apres_le_bilan() {
    for cmd in ["build", "update"] {
        let env = Env::mock();
        let o = env.run(&[cmd, "--batch"]);
        assert_eq!(code(&o), 1, "{cmd}");
        let e = err(&o);
        assert_eq!(e.lines().last(), Some(E31), "{cmd} : {e}");
        assert_eq!(e.matches("erreur:").count(), 1, "{cmd} : {e}");
        assert!(out(&o).contains("terminé"), "{cmd} : {}", out(&o));
        assert!(
            out(&o).contains("1 fichiers à enrichir"),
            "{cmd} : {}",
            out(&o)
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Pilote du mode lot (T-306) : faux serveur de lots, fournisseurs `anthropic` et `openai`.
// ---------------------------------------------------------------------------------------------

const PROVIDERS: [&str; 2] = ["anthropic", "openai"];
const NAMES: [&str; 4] = ["a", "b", "c", "d"];

fn path(n: &str) -> String {
    format!("docs/{n}.md")
}

fn announce(n: usize, id: &str) -> String {
    format!("avertissement: lot du fournisseur envoyé : {n} requêtes, identifiant {id}, réponse en 24 h au plus ; Ctrl-C ne perd rien : relancez nephila update --batch pour reprendre")
}

/// Premier lot créé par le faux serveur : son identifiant.
fn first_id(provider: &str) -> &'static str {
    if provider == "anthropic" {
        "msgbatch_1"
    } else {
        "batch_1"
    }
}

fn summary_text(n: &str) -> String {
    format!("Résumé du document {n}.")
}

/// Réponse d'un fichier seul pour le document `n`.
fn single(n: &str) -> String {
    json!({"version": 2,
           "summaries": [{"node": format!("document:{}", path(n)), "summary": summary_text(n)}],
           "concepts": []})
    .to_string()
}

/// Réponse d'un lot de fichiers pour les documents `names`.
fn lot(names: &[&str]) -> String {
    let files: Vec<Value> = names
        .iter()
        .map(|n| {
            json!({"file": path(n),
                   "summaries": [{"node": format!("document:{}", path(n)), "summary": summary_text(n)}],
                   "concepts": []})
        })
        .collect();
    json!({"version": 3, "files": files}).to_string()
}

/// Documents `docs/<n>.md` cités dans le corps d'une requête.
fn names_in(body: &Value) -> Vec<&'static str> {
    let text = body.to_string();
    NAMES
        .into_iter()
        .filter(|n| text.contains(&path(n)))
        .collect()
}

/// Réponse conforme à la requête.
fn good(body: &Value) -> Outcome {
    let names = names_in(body);
    Outcome::Text(if names.len() == 1 {
        single(names[0])
    } else {
        lot(&names)
    })
}

fn has_note(body: &Value) -> bool {
    body.to_string().contains("Correction :")
}

struct Fx {
    env: Env,
    server: BatchServer,
    provider: &'static str,
}

impl Fx {
    fn new(
        provider: &'static str,
        extra: &str,
        names: &[&str],
        in_progress: usize,
        answer: impl Fn(&Value) -> Outcome + Send + Sync + 'static,
    ) -> Fx {
        let server = BatchServer::start();
        server.simulate(provider, in_progress, answer);
        let env = Env::http(provider, &server.base_url, extra, names);
        Fx {
            env,
            server,
            provider,
        }
    }

    fn all_good(provider: &'static str, names: &[&str]) -> Fx {
        Fx::new(provider, "", names, 0, good)
    }

    fn create_path(&self) -> &'static str {
        if self.provider == "anthropic" {
            "/v1/messages/batches"
        } else {
            "/v1/files"
        }
    }

    fn sync_path(&self) -> &'static str {
        if self.provider == "anthropic" {
            "/v1/messages"
        } else {
            "/v1/chat/completions"
        }
    }

    /// Requêtes de création de lot reçues.
    fn creations(&self) -> usize {
        self.server.hits("POST", self.create_path()).len()
    }

    fn summary_of(&self, n: &str) -> Option<String> {
        let id = format!("document:{}", path(n));
        nephila::store::Store::open_readonly(&self.env.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|x| x.id == id)
            .and_then(|x| x.summary)
    }

    fn to_enrich(&self) -> Vec<String> {
        let mut v: Vec<String> = nephila::store::Store::open_readonly(&self.env.repo)
            .unwrap()
            .files_to_enrich()
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        v.sort();
        v
    }

    fn enrich_batches(&self) -> Vec<String> {
        nephila::store::Store::open_readonly(&self.env.repo)
            .unwrap()
            .enrich_batches()
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect()
    }

    fn ids_stored(&self) -> bool {
        !self.enrich_batches().is_empty()
    }
}

fn wrap(provider: &str, text: &str) -> String {
    if provider == "anthropic" {
        json!({"content": [{"type": "text", "text": text}], "stop_reason": "end_turn"}).to_string()
    } else {
        json!({"choices": [{"message": {"role": "assistant", "content": text},
                            "finish_reason": "stop"}]})
        .to_string()
    }
}

/// Corps reçus par un `build` sans `--batch` (un par fichier, dans l'ordre des fichiers) : la
/// référence des corps du mode lot.
fn sync_bodies(provider: &'static str, names: &[&str], extra: &str) -> Vec<Value> {
    let replies = names
        .iter()
        .map(|n| (200, wrap(provider, &single(n))))
        .collect();
    let server = FakeServer::start(replies);
    let env = Env::http(provider, &server.base_url, extra, names);
    let o = env.run(&["build"]);
    assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
    let p = if provider == "anthropic" {
        "/v1/messages"
    } else {
        "/v1/chat/completions"
    };
    server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path == p)
        .map(|r| serde_json::from_str(&r.body).unwrap())
        .collect()
}

fn last_line(o: &Output) -> String {
    err(o).lines().last().unwrap_or("").to_string()
}

fn lines_with<'a>(text: &'a str, needle: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.contains(needle)).collect()
}

/// Ligne de trace sans son préfixe `trace: [<s>.<mmm>s] `.
fn untrace(line: &str) -> Option<&str> {
    line.strip_prefix("trace: [")?
        .split_once("] ")
        .map(|(_, rest)| rest)
}

/// Corps de requête du document `n` seul, parmi les corps `bodies`.
fn body_for<'a>(bodies: &'a [Value], n: &str) -> &'a Value {
    bodies.iter().find(|b| names_in(b) == [n]).unwrap()
}

fn is_custom_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Lignes JSONL du fichier d'entrée déposé chez `openai` (partie `file` du formulaire).
fn multipart_lines(body: &str) -> Vec<Value> {
    body.split("application/jsonl\r\n\r\n")
        .nth(1)
        .unwrap()
        .split("\r\n--nephila")
        .next()
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn req211_ac6_serveur_sans_mode_lot_refuse_proprement() {
    for provider in PROVIDERS {
        for status in [404, 405, 501] {
            let fx = Fx::all_good(provider, &["a", "b", "c"]);
            fx.server
                .route("POST", fx.create_path(), vec![Reply::status(status, "{}")]);
            let o = fx.env.run(&["build", "--batch", "--verbose"]);
            let ctx = format!("{provider} {status} : {}", err(&o));
            assert_eq!(code(&o), 1, "{ctx}");
            assert_eq!(last_line(&o), E31, "{ctx}");
            assert_eq!(fx.server.requests().len(), 1, "{ctx}");
            assert!(fx.server.hits("POST", fx.sync_path()).is_empty(), "{ctx}");
            assert_eq!(fx.env.to_enrich(), 3, "{ctx}");
            assert!(!err(&o).contains("enrichissement:"), "{ctx}");
            assert!(!err(&o).contains("appels:"), "{ctx}");
            let q = fx.env.run(&["query", "search", "Document"]);
            assert!(out(&q).contains("docs/a.md"), "{ctx}");
            // AC-211-8 : la trace nomme le code et l'adresse, l'erreur ni l'un ni l'autre.
            let want = format!(
                "lot du fournisseur: création sans mode lot (HTTP {status}) sur {}{}",
                fx.server.base_url,
                fx.create_path()
            );
            assert!(
                err(&o).lines().any(|l| untrace(l) == Some(want.as_str())),
                "{ctx}"
            );
            assert!(!last_line(&o).contains(&status.to_string()), "{ctx}");
        }
    }
}

#[test]
fn req211_ac8_serveur_sans_lot_ligne_apres_le_bilan() {
    let fx = Fx::all_good("anthropic", &["a"]);
    fx.server
        .route("POST", fx.create_path(), vec![Reply::status(404, "{}")]);
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(last_line(&o), E31, "{}", err(&o));
    assert_eq!(err(&o).matches("erreur:").count(), 1, "{}", err(&o));
    assert!(out(&o).contains("terminé"), "{}", out(&o));
    assert!(!err(&o).contains("trace:"), "{}", err(&o));
}

#[test]
fn req212_ac1_un_lot_anthropic_aux_corps_du_synchrone() {
    let reference = sync_bodies("anthropic", &["a", "b", "c"], "");
    let fx = Fx::all_good("anthropic", &["a", "b", "c"]);
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let hits = fx.server.hits("POST", "/v1/messages/batches");
    assert_eq!(hits.len(), 1);
    assert!(fx.server.hits("POST", "/v1/messages").is_empty());
    assert_eq!(hits[0].headers["x-api-key"], "sk-test");
    assert!(hits[0].headers.contains_key("anthropic-version"));
    let sent: Value = serde_json::from_str(&hits[0].body).unwrap();
    let requests = sent["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 3);
    for r in requests {
        assert!(is_custom_id(r["custom_id"].as_str().unwrap()), "{r}");
        let n = names_in(&r["params"])[0];
        assert_eq!(&r["params"], body_for(&reference, n), "{n}");
    }
    assert_eq!(fx.to_enrich().len(), 0);
}

#[test]
fn req212_ac2_sondages_puis_un_seul_resultat_sans_suivre_results_url() {
    let other = BatchServer::start();
    let fx = Fx::all_good("anthropic", &["a", "b", "c"]);
    let counts = |p: u64, s: u64| json!({"processing": p, "succeeded": s, "errored": 0, "canceled": 0, "expired": 0});
    let in_progress = json!({"processing_status": "in_progress", "request_counts": counts(3, 0)});
    let ended = json!({
        "processing_status": "ended", "request_counts": counts(0, 3),
        "results_url": format!("{}/v1/messages/batches/msgbatch_1/results", other.base_url),
    });
    fx.server.route(
        "GET",
        "/v1/messages/batches/msgbatch_1",
        vec![
            Reply::status(200, &in_progress.to_string()),
            Reply::status(200, &in_progress.to_string()),
            Reply::status(200, &ended.to_string()),
        ],
    );
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let all = fx.server.requests();
    let status: Vec<usize> = (0..all.len())
        .filter(|i| all[*i].path == "/v1/messages/batches/msgbatch_1")
        .collect();
    let results: Vec<usize> = (0..all.len())
        .filter(|i| all[*i].path.ends_with("/results"))
        .collect();
    assert_eq!(status.len(), 3);
    assert_eq!(results.len(), 1);
    assert!(results[0] > status[2], "résultats lus avant la fin du lot");
    assert!(other.requests().is_empty(), "results_url a été contacté");
}

#[test]
fn req212_ac3_resultats_dans_un_autre_ordre_chaque_resume_a_son_fichier() {
    for provider in PROVIDERS {
        let fx = Fx::all_good(provider, &["a", "b", "c"]);
        fx.server.reverse_results();
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        for n in ["a", "b", "c"] {
            assert_eq!(fx.summary_of(n), Some(summary_text(n)), "{provider} {n}");
        }
        assert_eq!(fx.env.to_enrich(), 0);
    }
}

#[test]
fn req212_ac4_cle_refusee_a_la_creation_meme_ligne_que_le_synchrone() {
    for provider in PROVIDERS {
        let twin = FakeServer::start(vec![(401, "{}".into())]);
        let env = Env::http(provider, &twin.base_url, "", &["a", "b", "c"]);
        let sync = env.run(&["build"]);
        let fx = Fx::all_good(provider, &["a", "b", "c"]);
        fx.server
            .route("POST", fx.create_path(), vec![Reply::status(401, "{}")]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), code(&sync), "{provider}");
        assert_eq!(last_line(&o), last_line(&sync), "{provider}");
        assert!(last_line(&o).starts_with("erreur: clé d'API refusée par"));
        assert_eq!(fx.creations(), 1, "{provider}");
    }
}

#[test]
fn req212_ac4_cle_refusee_au_sondage_identifiant_garde() {
    for provider in PROVIDERS {
        let twin = FakeServer::start(vec![(401, "{}".into())]);
        let sync = Env::http(provider, &twin.base_url, "", &["a"]).run(&["build"]);
        let fx = Fx::all_good(provider, &["a", "b", "c"]);
        let status = if provider == "anthropic" {
            "/v1/messages/batches/msgbatch_1"
        } else {
            "/v1/batches/batch_1"
        };
        fx.server
            .route("GET", status, vec![Reply::status(401, "{}")]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(last_line(&o), last_line(&sync), "{provider}");
        assert_eq!(code(&o), code(&sync), "{provider}");
        assert_eq!(fx.enrich_batches(), [first_id(provider)], "{provider}");
        assert_eq!(fx.to_enrich().len(), 3, "{provider}");
    }
}

#[test]
fn req213_ac1_fichier_d_entree_et_lot_openai_aux_corps_du_synchrone() {
    let reference = sync_bodies("openai", &["a", "b", "c"], "");
    let fx = Fx::all_good("openai", &["a", "b", "c"]);
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let files = fx.server.hits("POST", "/v1/files");
    assert_eq!(files.len(), 1);
    assert!(files[0].body.contains("name=\"purpose\"\r\n\r\nbatch\r\n"));
    let lines = multipart_lines(&files[0].body);
    assert_eq!(lines.len(), 3);
    for l in &lines {
        assert!(is_custom_id(l["custom_id"].as_str().unwrap()), "{l}");
        assert_eq!(l["method"], "POST");
        assert_eq!(l["url"], "/v1/chat/completions");
        let n = names_in(&l["body"])[0];
        assert_eq!(&l["body"], body_for(&reference, n), "{n}");
    }
    let batches = fx.server.hits("POST", "/v1/batches");
    assert_eq!(batches.len(), 1);
    let b: Value = serde_json::from_str(&batches[0].body).unwrap();
    assert_eq!(
        b,
        json!({"input_file_id": "file_in_1", "endpoint": "/v1/chat/completions",
               "completion_window": "24h"})
    );
    assert!(fx.server.hits("POST", "/v1/chat/completions").is_empty());
}

#[test]
fn req213_ac2_statuts_intermediaires_puis_sortie_lue_une_fois() {
    let fx = Fx::all_good("openai", &["a", "b", "c"]);
    fx.server.reverse_results();
    let status = |s: &str, out: bool| {
        let mut v =
            json!({"status": s, "request_counts": {"total": 3, "completed": 0, "failed": 0}});
        if out {
            v["output_file_id"] = json!("file_out_1");
        }
        Reply::status(200, &v.to_string())
    };
    fx.server.route(
        "GET",
        "/v1/batches/batch_1",
        vec![
            status("validating", false),
            status("in_progress", false),
            status("finalizing", false),
            status("completed", true),
        ],
    );
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let all = fx.server.requests();
    let polls: Vec<usize> = (0..all.len())
        .filter(|i| all[*i].path == "/v1/batches/batch_1")
        .collect();
    let reads: Vec<usize> = (0..all.len())
        .filter(|i| all[*i].path == "/v1/files/file_out_1/content")
        .collect();
    assert_eq!(polls.len(), 4);
    assert_eq!(reads.len(), 1);
    assert!(reads[0] > polls[3]);
    for n in ["a", "b", "c"] {
        assert_eq!(fx.summary_of(n), Some(summary_text(n)), "{n}");
    }
    assert_eq!(fx.env.to_enrich(), 0);
}

#[test]
fn req213_ac3_une_ligne_dans_le_fichier_d_erreurs() {
    let fx = Fx::new("openai", "", &["a", "b", "c"], 0, |b| {
        if names_in(b) == ["c"] {
            Outcome::Errored("server_error".into(), "boom".into())
        } else {
            good(b)
        }
    });
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(fx.to_enrich(), ["docs/c.md"]);
    assert!(fx.summary_of("a").is_some() && fx.summary_of("b").is_some());
    assert_eq!(fx.env.to_enrich(), 1);
    assert!(err(&o).contains("avertissement: docs/c.md : requête du lot du fournisseur en échec"));
}

#[test]
fn req213_ac4_lot_refuse_et_lot_expire() {
    let fx = Fx::all_good("openai", &["a", "b", "c"]);
    fx.server.final_status("failed", "fichier refusé");
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(fx.env.to_enrich(), 3);
    assert!(err(&o).contains("fichier refusé"), "{}", err(&o));
    assert_eq!(fx.server.hits("POST", "/v1/batches").len(), 1);
    assert_eq!(fx.server.hits("POST", "/v1/files").len(), 1);

    let fx = Fx::new("openai", "", &["a", "b", "c"], 0, |b| {
        if names_in(b) == ["a"] {
            good(b)
        } else {
            Outcome::Missing
        }
    });
    fx.server.final_status("expired", "");
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(fx.env.to_enrich(), 2);
    assert!(fx.summary_of("a").is_some());
    assert!(err(&o).contains("docs/b.md : requête du lot du fournisseur en échec (expirée)"));
}

#[test]
fn req214_ac1_second_lot_d_une_requete_avec_la_note_de_format() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "format_retries = 1\n", &["a"], 0, |b| {
            if has_note(b) {
                good(b)
            } else {
                Outcome::Text("pas de JSON ici".into())
            }
        });
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 1);
        assert!(has_note(&lots[1][0]) && !has_note(&lots[0][0]));
        let id2 = if provider == "anthropic" {
            "msgbatch_2"
        } else {
            "batch_2"
        };
        let e = err(&o);
        assert_eq!(
            lines_with(&e, "lot du fournisseur envoyé"),
            [
                announce(1, first_id(provider)).as_str(),
                announce(1, id2).as_str()
            ],
            "{provider}"
        );
        assert_eq!(fx.env.to_enrich(), 0);
    }
}

#[test]
fn req214_ac1_sans_nouvel_essai_pas_de_second_lot_meme_avertissement() {
    for provider in PROVIDERS {
        let twin = FakeServer::start(vec![(200, wrap(provider, "pas de JSON ici"))]);
        let sync =
            Env::http(provider, &twin.base_url, "format_retries = 0\n", &["a"]).run(&["build"]);
        let fx = Fx::new(provider, "format_retries = 0\n", &["a"], 0, |_| {
            Outcome::Text("pas de JSON ici".into())
        });
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 1, "{provider}");
        assert_eq!(fx.server.lots().len(), 1, "{provider}");
        assert_eq!(
            lines_with(&err(&o), "avertissement: docs/a.md"),
            lines_with(&err(&sync), "avertissement: docs/a.md"),
            "{provider}"
        );
        assert_eq!(lines_with(&err(&o), "avertissement: docs/a.md").len(), 1);
    }
}

#[test]
fn req214_ac2_reponse_tronquee_second_lot_de_deux_moities() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "batch_files = 4\n", &NAMES, 0, |b| {
            if names_in(b).len() == 4 {
                Outcome::Truncated("{".into())
            } else {
                good(b)
            }
        });
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[0].len(), 1);
        assert_eq!(lots[1].len(), 2);
        assert_eq!(names_in(&lots[1][0]), ["a", "b"]);
        assert_eq!(names_in(&lots[1][1]), ["c", "d"]);
        assert_eq!(fx.env.to_enrich(), 0);
    }
}

#[test]
fn req214_ac3_fichier_absent_de_la_reponse_second_lot_pour_lui() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "batch_files = 2\n", &["a", "b"], 0, |b| {
            if names_in(b) == ["a", "b"] {
                Outcome::Text(lot(&["a"]))
            } else {
                good(b)
            }
        });
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 1);
        assert_eq!(names_in(&lots[1][0]), ["b"]);
        assert_eq!(fx.env.to_enrich(), 0);
        assert_eq!(fx.summary_of("a"), Some(summary_text("a")));
        assert_eq!(fx.summary_of("b"), Some(summary_text("b")));
    }
}

/// Document de trois sections, envoyé en trois appels par `max_chunk_chars = 200`.
fn big_doc(env: &Env) {
    let mut s = String::new();
    for k in 1..=3 {
        s.push_str(&format!("# Section{k}\n\n{}\n\n", "mot ".repeat(40)));
    }
    fs::write(env.repo.join("docs/a.md"), s).unwrap();
}

#[test]
fn req214_ac4_fichier_de_trois_appels_seul_le_non_conforme_repart() {
    for provider in PROVIDERS {
        let fx = Fx::new(
            provider,
            "format_retries = 1\nmax_chunk_chars = 200\n",
            &["a"],
            0,
            |b| {
                let t = b.to_string();
                if t.contains("(extrait 2/3") && !has_note(b) {
                    Outcome::Text("pas de JSON ici".into())
                } else if t.contains("(extrait 1/3") {
                    Outcome::Text(single("a"))
                } else {
                    Outcome::Text(
                        json!({"version": 2, "summaries": [], "concepts": []}).to_string(),
                    )
                }
            },
        );
        big_doc(&fx.env);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[0].len(), 3, "{provider}");
        assert_eq!(lots[1].len(), 1, "{provider}");
        assert!(lots[1][0].to_string().contains("(extrait 2/3"));
        assert!(has_note(&lots[1][0]));
        assert_eq!(fx.env.to_enrich(), 0);
    }
}

#[test]
fn req214_ac5_refus_de_schema_avec_structured_output_on_sans_second_lot() {
    let fx = Fx::new(
        "anthropic",
        "structured_output = \"on\"\n",
        &["a", "b"],
        0,
        |_| {
            Outcome::Errored(
                "invalid_request_error".into(),
                "output_config.format: schéma refusé".into(),
            )
        },
    );
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(fx.server.lots().len(), 1);
    assert_eq!(fx.env.to_enrich(), 2);
}

fn fixture_214_6() -> Fx {
    Fx::new("anthropic", "", &["a", "b", "c"], 0, |b| {
        match names_in(b)[0] {
            "a" => good(b),
            "b" => Outcome::Errored("api_error".into(), "boom".into()),
            _ => Outcome::Expired,
        }
    })
}

#[test]
fn req214_ac6_succes_erreur_et_expiration_puis_relance() {
    let fx = fixture_214_6();
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(fx.to_enrich(), ["docs/b.md", "docs/c.md"]);
    assert_eq!(fx.server.lots().len(), 1);
    assert_eq!(
        lines_with(&err(&o), "requête du lot du fournisseur en échec").len(),
        2
    );
    let o = fx.env.run(&["update", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    let lots = fx.server.lots();
    assert_eq!(lots.len(), 2);
    assert_eq!(lots[1].len(), 2);
}

#[test]
fn req214_ac7_resultats_de_plus_de_11_mo() {
    for provider in PROVIDERS {
        let big = "x".repeat(4_000_000);
        let fx = Fx::new(provider, "", &["a", "b", "c"], 0, move |b| {
            let n = names_in(b)[0];
            Outcome::Text(
                json!({"version": 2,
                       "summaries": [{"node": format!("document:{}", path(n)), "summary": big}],
                       "concepts": []})
                .to_string(),
            )
        });
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o).len());
        assert_eq!(fx.env.to_enrich(), 0, "{provider}");
        assert!(fx.summary_of("c").is_some(), "{provider}");
    }
}

#[test]
fn req214_ac9_connexion_coupee_a_la_creation_un_seul_envoi() {
    for provider in PROVIDERS {
        let fx = Fx::all_good(provider, &["a", "b", "c"]);
        fx.server
            .route("POST", fx.create_path(), vec![Reply::Close]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 1, "{provider}");
        assert_eq!(fx.creations(), 1, "{provider}");
        assert!(last_line(&o).starts_with("erreur: "), "{provider}");
    }
}

#[test]
fn req214_ac9_429_puis_reussite_deux_creations_lot_suivi() {
    for provider in PROVIDERS {
        let fx = Fx::all_good(provider, &["a", "b", "c"]);
        fx.server.fail_creates(vec![Reply::status(429, "{}")]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.creations(), 2, "{provider}");
        assert_eq!(fx.env.to_enrich(), 0);
    }
}

#[test]
fn req214_ac10_lignes_des_requetes_en_echec() {
    let fx = fixture_214_6();
    let o = fx.env.run(&["build", "--batch"]);
    let e = err(&o);
    assert!(e.lines().any(|l| l == "avertissement: docs/b.md : requête du lot du fournisseur en échec (api_error : boom) ; relancez nephila update --batch"), "{e}");
    assert!(e.lines().any(|l| l == "avertissement: docs/c.md : requête du lot du fournisseur en échec (expirée) ; relancez nephila update --batch"), "{e}");
    assert_eq!(last_line(&o), "erreur: 2 fichier(s) à enrichir");
}

#[test]
fn req214_ac10_groupe_sans_ligne_et_message_long() {
    let fx = Fx::new(
        "anthropic",
        "batch_files = 2\n",
        &["a", "b", "c"],
        0,
        |b| match names_in(b).as_slice() {
            ["a", "b"] => {
                Outcome::Errored("api_error".into(), format!("{}\nsuite", "m".repeat(500)))
            }
            _ => Outcome::Missing,
        },
    );
    let o = fx.env.run(&["build", "--batch"]);
    let e = err(&o);
    for n in ["a", "b"] {
        let l = e
            .lines()
            .find(|l| l.starts_with(&format!("avertissement: docs/{n}.md : ")))
            .unwrap_or_else(|| panic!("{e}"));
        let msg = l
            .split_once("(api_error : ")
            .unwrap()
            .1
            .split_once(") ; relancez")
            .unwrap()
            .0;
        assert!(msg.chars().count() <= 200 && msg.ends_with('…'), "{msg}");
        assert!(!msg.contains('\n'));
    }
    assert!(e.lines().any(|l| l == "avertissement: docs/c.md : requête du lot du fournisseur en échec (sans réponse) ; relancez nephila update --batch"), "{e}");
    assert_eq!(last_line(&o), "erreur: 3 fichier(s) à enrichir");
}

#[test]
fn req214_ac11_creation_en_echec_derniere_ligne() {
    let fx = Fx::all_good("anthropic", &["a", "b", "c"]);
    fx.server
        .route("POST", fx.create_path(), vec![Reply::Close]);
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1);
    assert_eq!(last_line(&o), "erreur: envoi interrompu : création du lot du fournisseur en échec (réseau) : vérifiez sa console puis relancez nephila update --batch ; 3 fichier(s) à enrichir");
    assert!(!fx.ids_stored());
    assert_eq!(fx.to_enrich().len(), 3);

    let fx = Fx::all_good("anthropic", &["a", "b", "c"]);
    fx.server
        .route("POST", fx.create_path(), vec![Reply::status(503, "{}")]);
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(last_line(&o), "erreur: envoi interrompu : création du lot du fournisseur en échec (HTTP 503) : vérifiez sa console puis relancez nephila update --batch ; 3 fichier(s) à enrichir");
}

#[test]
fn req217_ac1_une_annonce_par_lot_et_rien_d_autre() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a", "b", "c"], 2, good);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(lines_with(&err(&o), "lot du fournisseur envoyé").len(), 1);
        assert!(!err(&o).contains("trace:") && !err(&o).contains("progression:"));
        assert_eq!(err(&o).lines().count(), 1, "{}", err(&o));
        assert_eq!(out(&o).lines().count(), 1, "{}", out(&o));
        // Même forme de bilan que sans `--batch`.
        let twin = FakeServer::start(
            ["a", "b", "c"]
                .iter()
                .map(|n| (200, wrap(provider, &single(n))))
                .collect(),
        );
        let sync = Env::http(provider, &twin.base_url, "", &["a", "b", "c"]).run(&["build"]);
        let shape = |s: &str| s.replace(|c: char| c.is_ascii_digit(), "#");
        assert_eq!(shape(&out(&o)), shape(&out(&sync)), "{provider}");
    }
}

#[test]
fn req217_ac5_annonce_exacte_du_lot_cree() {
    for provider in PROVIDERS {
        let fx = Fx::all_good(provider, &["a", "b", "c"]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(
            lines_with(&err(&o), "lot du fournisseur envoyé"),
            [announce(3, first_id(provider)).as_str()],
            "{provider}"
        );
        assert!(
            !fx.ids_stored(),
            "identifiants oubliés à la fin : {provider}"
        );
    }
}

#[test]
fn req214_ac3_premier_fichier_non_ecrit_avant_la_derniere_passe() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "batch_files = 2\n", &["a", "b"], 0, |b| {
            if names_in(b) == ["a", "b"] {
                Outcome::Text(lot(&["a"]))
            } else {
                good(b)
            }
        });
        let second = if provider == "anthropic" {
            "/v1/messages/batches/msgbatch_2"
        } else {
            "/v1/batches/batch_2"
        };
        fx.server
            .route("GET", second, vec![Reply::status(401, "{}")]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
        assert_eq!(fx.summary_of("a"), None, "{provider}");
        assert_eq!(fx.to_enrich(), ["docs/a.md", "docs/b.md"], "{provider}");
    }
}
