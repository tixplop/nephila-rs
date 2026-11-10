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
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

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
    ["a", "b", "c", "d", "e"]
        .into_iter()
        .filter(|n| text.contains(&path(n)))
        .collect()
}

/// Texte d'une réponse conforme à la requête.
fn good_text(body: &Value) -> String {
    let names = names_in(body);
    if names.len() == 1 {
        single(names[0])
    } else {
        lot(&names)
    }
}

/// Réponse conforme à la requête.
fn good(body: &Value) -> Outcome {
    Outcome::Text(good_text(body))
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
        .map(|r| {
            // Le synchrone demande un flux (REQ-249) ; le lot non : seul écart permis.
            let mut b: Value = serde_json::from_str(&r.body).unwrap();
            assert_eq!(b["stream"], json!(true), "{b}");
            b.as_object_mut().unwrap().remove("stream");
            b.as_object_mut().unwrap().remove("stream_options");
            b
        })
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
fn req274_ac13_resultat_bloque_ligne_a24_sans_a17_ni_nouvelle_requete() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a"], 0, |b| Outcome::Blocked(good_text(b)));
        let o = fx.env.run(&["build", "--batch"]);
        assert_ne!(code(&o), 0, "{provider} : {}", err(&o));
        let e = err(&o);
        assert_eq!(
            lines_with(&e, "avertissement: docs/a.md"),
            ["avertissement: docs/a.md : réponse bloquée par le modèle ; excluez ce fichier avec enrich_exclude dans .nephila.toml"],
            "{provider} : {e}"
        );
        assert!(!e.contains("non conforme"), "{provider} : {e}");
        assert_eq!(fx.server.lots().len(), 1, "{provider}");
        assert_eq!(fx.env.to_enrich(), 1, "{provider}");
    }
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

// ---------------------------------------------------------------------------------------------
// Reprise, Ctrl-C, panne du suivi, oubli des identifiants, A-19 (T-307, REQ-215).
// ---------------------------------------------------------------------------------------------

const A16: &str = "avertissement: Ctrl-C reçu : plus rien ne part et les lots du fournisseur déjà envoyés ne sont pas annulés ; appuyez de nouveau sur Ctrl-C pour quitter sans attendre";
const E34: &str =
    "erreur: envoi interrompu : second Ctrl-C reçu : relancez nephila update --batch pour continuer";
const A19: &str = "avertissement: lot du fournisseur enregistré : les fichiers à enrichir seront payés de nouveau ; appuyez sur Ctrl-C et relancez nephila update --batch pour le reprendre";
const FOREVER: usize = usize::MAX;

fn e33(n: usize) -> String {
    format!("erreur: envoi interrompu : Ctrl-C reçu : relancez nephila update --batch pour continuer ; {n} fichier(s) à enrichir")
}

fn e35(n: usize) -> String {
    format!("erreur: envoi interrompu : suivi du lot du fournisseur impossible (3 échecs consécutifs) : relancez nephila update --batch pour continuer ; {n} fichier(s) à enrichir")
}

fn a15(id: &str) -> String {
    format!("avertissement: lot du fournisseur repris : identifiant {id} ; Ctrl-C ne perd rien : relancez nephila update --batch pour reprendre")
}

fn a18_other(id: &str) -> String {
    format!("avertissement: lot du fournisseur oublié : identifiant {id}, envoyé à un autre fournisseur ou une autre adresse ; ses requêtes repartent dans un nouveau lot")
}

fn a18_gone(id: &str) -> String {
    format!("avertissement: lot du fournisseur oublié : identifiant {id}, inconnu du fournisseur ou périmé ; ses requêtes repartent dans un nouveau lot")
}

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "délai dépassé : {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn reader(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = r.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    })
}

/// Enfant `nephila` dont stdout et stderr sont lus par des fils.
struct Running {
    child: Child,
    out: Option<std::thread::JoinHandle<String>>,
    err: Option<std::thread::JoinHandle<String>>,
    signalled: Option<Instant>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Done {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    signalled: Option<Instant>,
    after_signal: Option<Duration>,
}

impl Done {
    fn last(&self) -> &str {
        self.stderr.lines().last().unwrap_or("")
    }
}

impl Running {
    fn sigint(&mut self) {
        // SAFETY: le pid est celui d'un enfant vivant, non encore attendu.
        let r = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(r, 0);
        self.signalled = Some(Instant::now());
    }

    fn finish(mut self) -> Done {
        let start = Instant::now();
        let status = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s;
            }
            if start.elapsed() > Duration::from_secs(50) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("la commande ne finit pas");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let ended = Instant::now();
        Done {
            status,
            stdout: self.out.take().unwrap().join().unwrap(),
            stderr: self.err.take().unwrap().join().unwrap(),
            signalled: self.signalled,
            after_signal: self.signalled.map(|t| ended.duration_since(t)),
        }
    }
}

impl Env {
    fn spawn(&self, args: &[&str]) -> Running {
        let mut child = Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_BATCH_POLL_MS", "5")
            .env("NEPHILA_RETRY_BASE_MS", "1")
            .env_remove("NO_COLOR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Running {
            out: Some(reader(child.stdout.take().unwrap())),
            err: Some(reader(child.stderr.take().unwrap())),
            child,
            signalled: None,
        }
    }

    /// Remplace `from` par `to` dans la configuration personnelle.
    fn edit_config(&self, from: &str, to: &str) {
        let c = fs::read_to_string(&self.config).unwrap();
        assert!(c.contains(from), "{c}");
        fs::write(&self.config, c.replace(from, to)).unwrap();
    }
}

impl Fx {
    fn status_path(&self, k: usize) -> String {
        if self.provider == "anthropic" {
            format!("/v1/messages/batches/msgbatch_{k}")
        } else {
            format!("/v1/batches/batch_{k}")
        }
    }

    fn id(&self, k: usize) -> String {
        if self.provider == "anthropic" {
            format!("msgbatch_{k}")
        } else {
            format!("batch_{k}")
        }
    }

    fn polls(&self, k: usize) -> usize {
        self.server.hits("GET", &self.status_path(k)).len()
    }

    /// Lance `args`, envoie SIGINT dès que le lot `k` est sondé, rend la fin de la commande.
    fn interrupt_at(&self, k: usize, args: &[&str]) -> Done {
        let mut run = self.env.spawn(args);
        wait_until("sondage du lot", || self.polls(k) >= 1);
        run.sigint();
        run.finish()
    }

    /// `build --batch` interrompu au premier sondage du premier lot.
    fn interrupted(&self) -> Done {
        let d = self.interrupt_at(1, &["build", "--batch"]);
        assert_eq!(d.status.code(), Some(130), "{}", d.stderr);
        d
    }

    /// Contenu brut de la table `meta`.
    fn meta_dump(&self) -> String {
        let db = rusqlite::Connection::open_with_flags(
            self.env.repo.join(".nephila/graph.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut st = db.prepare("SELECT key, value FROM meta").unwrap();
        let rows: Vec<String> = st
            .query_map([], |r| {
                Ok(format!(
                    "{}={}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.join("\n")
    }
}

fn fixture_forever(provider: &'static str, names: &[&str]) -> Fx {
    Fx::new(provider, "", names, FOREVER, good)
}

#[test]
fn req215_ac1_ctrl_c_a_l_attente_enregistre_le_lot_et_s_arrete_vite() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        let d = fx.interrupted();
        assert!(
            d.after_signal.unwrap() < Duration::from_secs(2),
            "{provider}"
        );
        let t = d.signalled.unwrap();
        let reqs = fx.server.requests();
        assert!(
            reqs.iter().all(|r| !r.path.contains("cancel")),
            "{provider}"
        );
        // La veille lit le signal toutes les 20 ms : quelques sondages partent encore, pas après 1 s.
        assert!(
            reqs.iter().all(|r| r.at <= t + Duration::from_secs(1)),
            "{provider} : requêtes longtemps après le signal"
        );
        // Bilan sur stdout, de même forme que celui de l'arrêt E-35 (AC-215-8).
        assert!(d.stdout.contains("2 fichiers à enrichir"), "{}", d.stdout);
        let twin = Fx::new(provider, "", &["a", "b"], 0, good);
        twin.server
            .route("GET", &twin.status_path(1), vec![Reply::status(500, "{}")]);
        let o = twin.env.run(&["build", "--batch"]);
        let shape = |s: &str| s.replace(|c: char| c.is_ascii_digit(), "#");
        assert_eq!(shape(&d.stdout), shape(&out(&o)), "{provider}");
        assert_eq!(fx.env.to_enrich(), 2, "{provider}");
        assert_eq!(fx.enrich_batches(), [first_id(provider)], "{provider}");
        let meta = fx.meta_dump();
        let stored = nephila::store::Store::open_readonly(&fx.env.repo)
            .unwrap()
            .enrich_batches()
            .unwrap();
        assert_eq!(stored[0].provider, provider);
        assert_eq!(
            stored[0].address,
            nephila::store::address_fingerprint(&fx.server.base_url)
        );
        assert!(!meta.contains(&fx.server.base_url), "{meta}");
        assert!(
            !meta.contains("sk-test") && !meta.contains(KEY_VAR),
            "{meta}"
        );
        // AC-215-12 et AC-217-3 : textes exacts, rien sur des appels qui vont à leur terme.
        assert_eq!(
            lines_with(&d.stderr, "Ctrl-C reçu : plus"),
            [A16],
            "{provider}"
        );
        assert_eq!(d.last(), e33(2), "{provider}");
        assert!(d.last().contains("nephila update --batch"));
        assert!(!d.stderr.contains("à leur terme"), "{}", d.stderr);
        assert!(!d.stderr.contains("panicked"), "{}", d.stderr);
    }
}

#[test]
fn req215_ac2_la_reprise_ne_cree_rien_et_lit_le_lot_enregistre() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        fx.server.set_in_progress(0);
        let polls = fx.polls(1);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.creations(), 1, "{provider}");
        assert!(fx.polls(1) > polls, "{provider}");
        assert_eq!(fx.creations(), 1);
        for n in ["a", "b"] {
            assert_eq!(fx.summary_of(n), Some(summary_text(n)), "{provider} {n}");
        }
        assert_eq!(fx.env.to_enrich(), 0);
        assert!(!fx.ids_stored(), "{provider}");
        let seen = fx.server.requests().len();
        let again = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&again), 0, "{provider} : {}", err(&again));
        assert!(out(&again).contains("graphe à jour"), "{}", out(&again));
        assert_eq!(fx.server.requests().len(), seen, "{provider}");
        assert!(!fx.ids_stored(), "{provider}");
    }
}

#[test]
fn req215_ac3_build_batch_reprend_aussi_sans_creer() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        fx.server.set_in_progress(0);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.creations(), 1, "{provider}");
        assert_eq!(fx.env.to_enrich(), 0, "{provider}");
        assert!(!fx.ids_stored(), "{provider}");
    }
}

#[test]
fn req215_ac4_un_document_modifie_repart_seul_dans_un_nouveau_lot() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a", "b"], FOREVER, |b| {
            if names_in(b) == ["b"] && b.to_string().contains("MODIFIÉ") {
                Outcome::Text(
                    json!({"version": 2,
                           "summaries": [{"node": "document:docs/b.md", "summary": "Résumé modifié."}],
                           "concepts": []})
                    .to_string(),
                )
            } else {
                good(b)
            }
        });
        fx.interrupted();
        fx.server.set_in_progress(0);
        fs::write(fx.env.repo.join(path("b")), "# Document b\n\nMODIFIÉ.\n").unwrap();
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 1, "{provider}");
        assert_eq!(names_in(&lots[1][0]), ["b"], "{provider}");
        assert_eq!(
            fx.summary_of("b"),
            Some("Résumé modifié.".into()),
            "{provider}"
        );
        assert_eq!(fx.summary_of("a"), Some(summary_text("a")), "{provider}");
        assert!(fx.to_enrich().is_empty(), "{provider}");
    }
}

#[test]
fn req215_ac5_un_autre_modele_renvoie_toutes_les_requetes() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        fx.server.set_in_progress(0);
        fx.env.edit_config("model = \"m\"", "model = \"m2\"");
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 2, "{provider}");
        for body in &lots[1] {
            assert_eq!(body["model"], "m2", "{provider}");
        }
        assert!(fx.to_enrich().is_empty(), "{provider}");
    }
}

#[test]
fn req215_ac6_ac14_une_autre_adresse_oublie_le_lot_sans_requete() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        let before = fx.server.requests().len();
        let second = BatchServer::start();
        second.simulate(provider, 0, good);
        fx.env.edit_config(&fx.server.base_url, &second.base_url);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let e = err(&o);
        let id1 = first_id(provider);
        assert_eq!(
            lines_with(&e, "lot du fournisseur oublié"),
            [a18_other(id1)]
        );
        assert!(
            !e.contains(&fx.server.base_url) && !e.contains(&second.base_url),
            "{e}"
        );
        assert_eq!(fx.server.requests().len(), before, "{provider}");
        let reqs = second.requests();
        assert_eq!(reqs[0].method, "POST", "{provider}");
        assert_eq!(second.lots().len(), 1, "{provider}");
        assert!(lines_with(&e, "lot du fournisseur repris").is_empty());
        assert!(fx.to_enrich().is_empty(), "{provider}");
    }
}

#[test]
fn req215_ac7_ac14_lot_inconnu_du_fournisseur_oublie_puis_cree() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        fx.server.set_in_progress(0);
        fx.server
            .route("GET", &fx.status_path(1), vec![Reply::status(404, "{}")]);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(
            lines_with(&err(&o), "lot du fournisseur oublié"),
            [a18_gone(first_id(provider))],
            "{provider}"
        );
        assert_eq!(fx.creations(), 2, "{provider}");
        assert!(fx.to_enrich().is_empty(), "{provider}");
        assert!(!fx.ids_stored(), "{provider}");
    }
}

#[test]
fn req215_ac7_lot_inconnu_a_la_lecture_des_resultats() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        fx.server.set_in_progress(0);
        let results = if provider == "anthropic" {
            "/v1/messages/batches/msgbatch_1/results"
        } else {
            "/v1/files/file_out_1/content"
        };
        fx.server
            .route("GET", results, vec![Reply::status(404, "{}")]);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(
            lines_with(&err(&o), "lot du fournisseur oublié"),
            [a18_gone(first_id(provider))],
            "{provider}"
        );
        assert_eq!(fx.creations(), 2, "{provider}");
        assert!(fx.to_enrich().is_empty(), "{provider}");
    }
}

/// Réponse `ended` / `completed` du sondage d'un lot de deux requêtes.
fn ended_body(provider: &str) -> String {
    if provider == "anthropic" {
        json!({"processing_status": "ended",
               "request_counts": {"processing": 0, "succeeded": 2, "errored": 0, "canceled": 0, "expired": 0}})
        .to_string()
    } else {
        json!({"status": "completed", "output_file_id": "file_out_1",
               "request_counts": {"total": 2, "completed": 2, "failed": 0}})
        .to_string()
    }
}

#[test]
fn req215_ac8_ac13_trois_sondages_en_echec_arretent_la_commande() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a", "b"], 0, good);
        fx.server
            .route("GET", &fx.status_path(1), vec![Reply::status(500, "{}")]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
        assert_eq!(last_line(&o), e35(2), "{provider}");
        assert!(!err(&o).contains("panne réseau"), "{}", err(&o));
        assert!(out(&o).contains("2 fichiers à enrichir"), "{}", out(&o));
        assert_eq!(fx.enrich_batches(), [first_id(provider)], "{provider}");
        fx.server.clear_route("GET", &fx.status_path(1));
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.creations(), 1, "{provider}");
        assert!(fx.to_enrich().is_empty(), "{provider}");
    }
}

#[test]
fn req215_ac8_trois_sondages_refuses_en_400_arretent_aussi_par_e35() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a", "b"], 0, good);
        fx.server
            .route("GET", &fx.status_path(1), vec![Reply::status(400, "{}")]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
        assert_eq!(last_line(&o), e35(2), "{provider}");
        assert_eq!(fx.enrich_batches(), [first_id(provider)], "{provider}");
        assert_eq!(
            fx.server.hits("GET", &fx.status_path(1)).len(),
            3,
            "{provider}"
        );
    }
}

#[test]
fn req215_ac8_deux_echecs_puis_une_reussite_la_commande_continue() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a", "b"], 0, good);
        // Chaque sondage en échec fait 5 requêtes (1 + 4 nouveaux essais).
        let mut script = vec![Reply::status(500, "{}"); 10];
        script.push(Reply::status(200, &ended_body(provider)));
        fx.server.route("GET", &fx.status_path(1), script);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert!(fx.to_enrich().is_empty(), "{provider}");
        assert!(!err(&o).contains("erreur:"), "{}", err(&o));
    }
}

fn format_failing(b: &Value) -> Outcome {
    if has_note(b) {
        good(b)
    } else {
        Outcome::Text("pas de JSON ici".into())
    }
}

#[test]
fn req215_ac9_ctrl_c_pendant_le_second_lot_rien_n_est_ecrit_puis_reprise() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "format_retries = 1\n", &["a"], 0, format_failing);
        fx.server.hold(Some(2));
        let d = fx.interrupt_at(2, &["build", "--batch"]);
        assert_eq!(d.status.code(), Some(130), "{provider} : {}", d.stderr);
        assert_eq!(fx.summary_of("a"), None, "{provider}");
        assert_eq!(fx.to_enrich(), ["docs/a.md"], "{provider}");
        assert_eq!(fx.enrich_batches(), [fx.id(1), fx.id(2)], "{provider}");
        assert_eq!(d.last(), e33(1), "{provider}");
        fx.server.hold(None);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.creations(), 2, "{provider}");
        assert_eq!(fx.summary_of("a"), Some(summary_text("a")), "{provider}");
        assert!(fx.to_enrich().is_empty(), "{provider}");
    }
}

#[test]
fn req215_ac10_ac14_update_sans_batch_avertit_une_fois_et_oublie_a_la_fin() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        let seen = fx.server.requests().len();
        for args in [["update", "--no-llm"], ["update", "--dry-run"]] {
            let o = fx.env.run(&args);
            assert_eq!(code(&o), 0, "{provider} {args:?} : {}", err(&o));
            assert!(!err(&o).contains("lot du fournisseur"), "{}", err(&o));
            assert_eq!(fx.server.requests().len(), seen, "{provider} {args:?}");
            assert_eq!(fx.enrich_batches(), [first_id(provider)], "{provider}");
        }
        fx.server.route(
            "POST",
            fx.sync_path(),
            vec![
                Reply::status(200, &wrap(provider, &single("a"))),
                Reply::status(200, &wrap(provider, &single("b"))),
            ],
        );
        let o = fx.env.run(&["update"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        // stdout inchangé : celui du même `update` sans lot enregistré.
        let twin_server = FakeServer::start(vec![
            (200, wrap(provider, &single("a"))),
            (200, wrap(provider, &single("b"))),
        ]);
        let twin = Env::http(provider, &twin_server.base_url, "", &["a", "b"]);
        assert_eq!(code(&twin.run(&["build", "--no-llm"])), 0);
        let t = twin.run(&["update"]);
        assert_eq!(code(&t), 0, "{provider} : {}", err(&t));
        let shape = |s: &str| s.replace(|c: char| c.is_ascii_digit(), "#");
        assert_eq!(shape(&out(&o)), shape(&out(&t)), "{provider}");
        assert_eq!(
            lines_with(&err(&o), "lot du fournisseur"),
            [A19],
            "{provider}"
        );
        assert_eq!(err(&o).lines().next(), Some(A19), "{provider}");
        assert_eq!(
            fx.server.hits("POST", fx.sync_path()).len(),
            2,
            "{provider}"
        );
        assert_eq!(fx.creations(), 1, "{provider}");
        assert!(fx.to_enrich().is_empty(), "{provider}");
        assert!(!fx.ids_stored(), "{provider}");
        assert!(!fx.meta_dump().contains(first_id(provider)), "{provider}");
    }
}

#[test]
fn req215_ac11_la_structure_reste_lisible_et_le_schema_ne_change_pas() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        let search = |fx: &Fx| {
            let o = fx.env.run(&["query", "search", "Document", "--json"]);
            assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
            out(&o)
        };
        let mut run = fx.env.spawn(&["build", "--batch"]);
        wait_until("sondage du lot", || fx.polls(1) >= 1);
        assert!(search(&fx).contains("docs/a.md"), "{provider}");
        run.sigint();
        let d = run.finish();
        assert_eq!(d.status.code(), Some(130), "{provider} : {}", d.stderr);
        assert!(search(&fx).contains("docs/b.md"), "{provider}");
        let db = rusqlite::Connection::open(fx.env.repo.join(".nephila/graph.db")).unwrap();
        let version: i32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, nephila::store::SCHEMA_VERSION, "{provider}");
    }
}

#[test]
fn req215_ac11_ac12_second_ctrl_c_pendant_une_creation_retardee() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b"]);
        let slow = Reply::Http {
            status: 200,
            body: json!({"id": "x"}).to_string(),
            headers: vec![],
            delay_ms: 10_000,
        };
        fx.server.route("POST", fx.create_path(), vec![slow]);
        let mut run = fx.env.spawn(&["build", "--batch"]);
        wait_until("création reçue", || fx.creations() >= 1);
        run.sigint();
        std::thread::sleep(Duration::from_millis(300));
        run.sigint();
        let d = run.finish();
        assert_eq!(d.status.code(), Some(130), "{provider} : {}", d.stderr);
        assert!(
            d.after_signal.unwrap() < Duration::from_secs(1),
            "{provider}"
        );
        assert_eq!(d.stdout, "", "{provider}");
        assert_eq!(d.last(), E34, "{provider} : {}", d.stderr);
        assert_eq!(
            lines_with(&d.stderr, "Ctrl-C reçu : plus"),
            [A16],
            "{provider}"
        );
        assert_eq!(
            d.stderr
                .lines()
                .filter(|l| l.starts_with("erreur:"))
                .count(),
            1
        );
    }
}

#[test]
fn req215_ac14_sans_fichier_a_enrichir_l_identifiant_est_oublie_sans_ligne() {
    for provider in PROVIDERS {
        // Chemin « à jour » : plus rien à enrichir, rien d'autre n'a changé.
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        let seen = fx.server.requests().len();
        fs::write(
            fx.env.repo.join(".nephila.toml"),
            "enrich_exclude = [\"docs/**\", \"other/**\"]\n",
        )
        .unwrap();
        let o = fx.env.run(&["update"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert!(!err(&o).contains("lot du fournisseur"), "{}", err(&o));
        assert!(!fx.ids_stored(), "{provider}");
        assert_eq!(fx.server.requests().len(), seen, "{provider}");

        // Chemin de la fin de commande : un fichier ajouté, hors de la portée.
        let fx = fixture_forever(provider, &["a", "b"]);
        fx.interrupted();
        let seen = fx.server.requests().len();
        fs::write(
            fx.env.repo.join(".nephila.toml"),
            "enrich_exclude = [\"docs/**\", \"other/**\"]\n",
        )
        .unwrap();
        fs::create_dir_all(fx.env.repo.join("other")).unwrap();
        fs::write(fx.env.repo.join("other/x.md"), "# X\n\nTexte.\n").unwrap();
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert!(!err(&o).contains("lot du fournisseur"), "{}", err(&o));
        assert!(!fx.ids_stored(), "{provider}");
        assert_eq!(fx.server.requests().len(), seen, "{provider}");
    }
}

#[test]
fn req217_ac5_la_reprise_annonce_chaque_lot_une_fois_avant_le_sondage() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &["a", "b", "c"]);
        let d = fx.interrupted();
        let id1 = first_id(provider);
        assert_eq!(
            lines_with(&d.stderr, "lot du fournisseur envoyé"),
            [announce(3, id1)]
        );
        fx.server.set_in_progress(0);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(
            err(&o).lines().next(),
            Some(a15(id1).as_str()),
            "{provider}"
        );
        assert_eq!(lines_with(&err(&o), "lot du fournisseur repris").len(), 1);
        assert!(
            !err(&o).contains("lot du fournisseur envoyé"),
            "{}",
            err(&o)
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Plafond, progression et traces (T-308, REQ-216, REQ-217).
// ---------------------------------------------------------------------------------------------

const FIVE: [&str; 5] = ["a", "b", "c", "d", "e"];

fn e32(m: usize, r: usize) -> String {
    format!("erreur: plafond de {m} appels dépassé : le lot du fournisseur compte {r} requêtes, aucune envoyée ; relancez nephila update --batch --max-calls {r} ou réduisez la portée avec enrich_exclude")
}

fn a10_batch(m: usize, left: usize) -> String {
    format!("avertissement: plafond de {m} appels atteint : {left} fichiers laissés à enrichir ; relancez nephila update --batch pour continuer ou augmentez max_calls")
}

#[test]
fn req216_ac1_ac5_le_premier_lot_au_dessus_du_plafond_ne_part_pas() {
    for provider in PROVIDERS {
        let fx = Fx::all_good(provider, &FIVE);
        let o = fx.env.run(&["build", "--batch", "--max-calls", "3"]);
        assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
        assert_eq!(fx.creations(), 0, "{provider}");
        assert_eq!(fx.server.requests().len(), 0, "{provider}");
        assert_eq!(last_line(&o), e32(3, 5), "{provider}");
        assert!(!err(&o).contains("plafond de 3 appels atteint"));
        assert_eq!(fx.to_enrich().len(), 5, "{provider}");
        assert!(!fx.ids_stored(), "{provider}");
        // Structure écrite : les documents sont dans le graphe.
        let q = fx.env.run(&["query", "search", "Document"]);
        assert!(out(&q).contains("docs/a.md"), "{provider} : {}", out(&q));
    }
}

fn one_bad_format(b: &Value) -> Outcome {
    if names_in(b) == ["c"] {
        format_failing(b)
    } else {
        good(b)
    }
}

#[test]
fn req216_ac2_ac5_le_nouvel_essai_au_dessus_du_plafond_ne_part_pas() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "format_retries = 1\n", &FIVE, 0, one_bad_format);
        let o = fx.env.run(&["build", "--batch", "--max-calls", "5"]);
        assert_eq!(fx.creations(), 1, "{provider}");
        assert_eq!(fx.server.lots().len(), 1, "{provider}");
        assert_eq!(fx.server.lots()[0].len(), 5, "{provider}");
        // REQ-187 : le plafond est la seule raison des fichiers restants, code 0.
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(
            lines_with(&err(&o), "plafond"),
            [a10_batch(5, 1).as_str()],
            "{provider}"
        );
        assert_eq!(fx.to_enrich(), ["docs/c.md"], "{provider}");
        for n in ["a", "b", "d", "e"] {
            assert_eq!(fx.summary_of(n), Some(summary_text(n)), "{provider} {n}");
        }
        assert!(!err(&o).lines().any(|l| l.starts_with("erreur:")));
    }
}

#[test]
fn req216_ac3_le_nouvel_essai_dans_le_plafond_part_dans_un_second_lot() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "format_retries = 1\n", &FIVE, 0, one_bad_format);
        let o = fx.env.run(&["build", "--batch", "--max-calls", "6"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 1, "{provider}");
        assert!(!err(&o).contains("plafond"), "{}", err(&o));
        assert_eq!(fx.to_enrich().len(), 0, "{provider}");
        for n in FIVE {
            assert_eq!(fx.summary_of(n), Some(summary_text(n)), "{provider} {n}");
        }
    }
}

#[test]
fn req216_ac4_les_requetes_d_un_lot_repris_ne_comptent_pas() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &FIVE);
        fx.interrupted();
        fx.server.set_in_progress(0);
        let o = fx.env.run(&["update", "--batch", "--max-calls", "1"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.creations(), 1, "{provider}");
        assert!(!err(&o).contains("plafond"), "{provider} : {}", err(&o));
        assert_eq!(fx.to_enrich().len(), 0, "{provider}");
    }
}

#[test]
fn req217_ac6_lignes_de_progression_du_lot_entre_extraction_et_enrichissement() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &NAMES, 2, good);
        fx.server.progress(vec![0, 2]);
        let o = fx.env.run(&["build", "--batch", "--show-progress"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let e = err(&o);
        let lines: Vec<&str> = e
            .lines()
            .filter(|l| l.starts_with("progression:"))
            .collect();
        let batch: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.contains("lot du fournisseur"))
            .collect();
        assert_eq!(
            batch,
            [
                "progression: lot du fournisseur 0 % (0/4 requêtes)",
                "progression: lot du fournisseur 50 % (2/4 requêtes)",
                "progression: lot du fournisseur 100 % (4/4 requêtes)",
            ],
            "{provider}"
        );
        let at = |p: &str| lines.iter().position(|l| l.contains(p)).unwrap();
        let last = |p: &str| lines.iter().rposition(|l| l.contains(p)).unwrap();
        assert!(last("extraction") < at("lot du fournisseur"), "{lines:?}");
        assert!(
            last("lot du fournisseur") < at("enrichissement"),
            "{lines:?}"
        );
    }
}

#[test]
fn req217_ac2_ac6_compteurs_inchanges_aucune_ligne_de_plus_et_paliers_croissants() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &NAMES, 3, good);
        fx.server.progress(vec![0, 2, 2]);
        let o = fx.env.run(&["build", "--batch", "--show-progress"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.polls(1), 4, "{provider}");
        let percents: Vec<u32> = err(&o)
            .lines()
            .filter(|l| l.starts_with("progression: lot du fournisseur"))
            .map(|l| {
                l.trim_start_matches("progression: lot du fournisseur ")
                    .split(" %")
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect();
        assert_eq!(percents, [0, 50, 100], "{provider}");
    }
}

#[test]
fn req217_ac6_ctrl_c_pendant_l_attente_aucune_ligne_d_enrichissement() {
    for provider in PROVIDERS {
        let fx = fixture_forever(provider, &NAMES);
        fx.server.progress(vec![0, 2]);
        let d = fx.interrupt_at(1, &["build", "--batch", "--show-progress"]);
        assert_eq!(d.status.code(), Some(130), "{provider} : {}", d.stderr);
        assert!(
            !d.stderr.contains("progression: enrichissement"),
            "{provider} : {}",
            d.stderr
        );
        assert!(d.stderr.contains("progression: extraction"), "{provider}");
    }
}

#[test]
fn req217_ac2_verbose_une_trace_par_sondage_sans_resume_ni_cle() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &NAMES, 2, good);
        fx.server.progress(vec![0, 2]);
        let o = fx.env.run(&["build", "--batch", "--verbose"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let e = err(&o);
        let id = first_id(provider);
        let states: Vec<&str> = e
            .lines()
            .filter_map(untrace)
            .filter(|l| l.starts_with("lot du fournisseur: état "))
            .collect();
        assert_eq!(states.len(), fx.polls(1), "{provider} : {e}");
        assert_eq!(states.len(), 3);
        assert!(states[1].starts_with(&format!("lot du fournisseur: état {id} ")));
        assert!(states[1].ends_with("(2/4 requêtes)"), "{}", states[1]);
        assert!(states[2].ends_with("(4/4 requêtes)"), "{}", states[2]);
        let calls = lines_with(&e, "appels:");
        assert_eq!(calls.len(), 1, "{e}");
        // `<h>` : une requête par appel, comptée par le collecteur.
        assert!(
            calls[0].contains("appels: 4 appels, 0 nouveaux essais de format, 4 envois HTTP,"),
            "{}",
            calls[0]
        );
        for secret in ["Résumé du document", "Texte du document", "sk-test"] {
            assert!(!e.contains(secret), "{provider} : {secret} dans {e}");
        }
    }
}

#[test]
fn req217_ac7_formes_des_traces_et_ordre_des_lignes_llm_enrich() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &NAMES, 1, good);
        let o = fx.env.run(&["build", "--batch", "--verbose"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let e = err(&o);
        let id = first_id(provider);
        let own: Vec<&str> = e
            .lines()
            .filter_map(untrace)
            .filter(|l| l.starts_with("lot du fournisseur: "))
            .collect();
        for l in &own {
            assert!(
                ["créé ", "état ", "fini "]
                    .iter()
                    .any(|p| l.starts_with(&format!("lot du fournisseur: {p}"))),
                "{l}"
            );
        }
        assert_eq!(
            own.iter().filter(|l| l.contains(": créé ")).count(),
            1,
            "{own:?}"
        );
        assert!(own[0].starts_with(&format!("lot du fournisseur: créé {id}, 4 requêtes, ")));
        assert!(own[0].ends_with(" octets"), "{}", own[0]);
        let fini: Vec<&&str> = own.iter().filter(|l| l.contains(": fini ")).collect();
        assert_eq!(fini.len(), 1);
        assert!(fini[0].starts_with(&format!("lot du fournisseur: fini {id} ")));
        assert!(fini[0].contains(", 4 réponses en "), "{}", fini[0]);
        assert!(fini[0].ends_with(" ms"), "{}", fini[0]);
        let all: Vec<&str> = e.lines().collect();
        let begin = all
            .iter()
            .position(|l| l.contains("enrichissement: début"))
            .unwrap();
        assert!(
            all[..begin].iter().all(|l| !l.contains("llm enrich")),
            "{provider}"
        );
        let starts = all[begin..]
            .iter()
            .filter(|l| l.contains("llm enrich départ"))
            .count();
        assert_eq!(starts, 4, "{provider} : {e}");
        assert_eq!(lines_with(&e, "enrichissement: début").len(), 1);
    }
}

#[test]
fn req217_ac7_identifiant_avec_un_octet_1b_aucun_octet_1b_et_annonces_d_une_ligne() {
    for flags in [&["--verbose"][..], &[][..]] {
        // Création : l'identifiant rendu par le serveur porte un octet 0x1B, `check_id` le refuse.
        let fx = Fx::all_good("anthropic", &["a", "b"]);
        fx.server.route(
            "POST",
            fx.create_path(),
            vec![Reply::status(200, "{\"id\": \"msgbatch_1\\u001b[31m\"}")],
        );
        let args = [&["build", "--batch"][..], flags].concat();
        let o = fx.env.run(&args);
        assert_eq!(code(&o), 1, "{flags:?} : {}", err(&o));
        assert_eq!(
            last_line(&o),
            "erreur: identifiant de lot de anthropic illisible",
            "{flags:?}"
        );
        assert!(lines_with(&err(&o), "lot du fournisseur envoyé").is_empty());
        assert!(!o.stderr.contains(&0x1B) && !o.stdout.contains(&0x1B));

        // Reprise : un identifiant enregistré avec 0x1B est écrit échappé, sur une seule ligne.
        let fx = Fx::all_good("anthropic", &["a", "b"]);
        let o = fx.env.run(&["build", "--no-llm"]);
        assert_eq!(code(&o), 0, "{}", err(&o));
        let mut store = nephila::store::Store::open_or_create(&fx.env.repo).unwrap();
        store
            .set_enrich_batches(&[nephila::store::EnrichBatch {
                id: "x\u{1b}[31m\nfaux".into(),
                provider: "anthropic".into(),
                address: nephila::store::address_fingerprint(&fx.server.base_url),
            }])
            .unwrap();
        drop(store);
        let args = [&["update", "--batch"][..], flags].concat();
        let o = fx.env.run(&args);
        assert!(!o.stderr.contains(&0x1B), "{flags:?} : {}", err(&o));
        let e = err(&o);
        assert_eq!(
            lines_with(&e, "lot du fournisseur repris"),
            ["avertissement: lot du fournisseur repris : identifiant x\\u{001B}[31m\\u{000A}faux ; Ctrl-C ne perd rien : relancez nephila update --batch pour reprendre"],
            "{e}"
        );
        assert!(e.lines().all(|l| !l.starts_with("faux")), "{e}");
        if !flags.is_empty() {
            let states = lines_with(&e, "lot du fournisseur: état ");
            assert_eq!(states.len(), 3, "{e}");
            for l in states {
                assert!(l.contains("état x\\u{001B}[31m\\u{000A}faux échec "), "{l}");
            }
        }
    }
}

#[test]
fn req217_ac4_lignes_de_requete_en_echec_et_creation_en_delai_depasse() {
    // (fournisseur, statut final imposé, issue de la requête, cause attendue)
    type Case = (
        &'static str,
        Option<(&'static str, &'static str)>,
        Outcome,
        &'static str,
    );
    let cases: Vec<Case> = vec![
        (
            "openai",
            Some(("failed", "fichier refusé")),
            Outcome::Missing,
            "lot refusé : fichier refusé",
        ),
        (
            "openai",
            Some(("failed", "")),
            Outcome::Missing,
            "lot refusé",
        ),
        (
            "openai",
            Some(("cancelled", "")),
            Outcome::Missing,
            "annulée",
        ),
        (
            "openai",
            None,
            Outcome::Http(500, "panne".into()),
            "HTTP 500 : panne",
        ),
        (
            "openai",
            None,
            Outcome::Http(500, String::new()),
            "HTTP 500",
        ),
        (
            "openai",
            None,
            Outcome::Errored("x".into(), "message seul".into()),
            "message seul",
        ),
        (
            "anthropic",
            None,
            Outcome::Errored("api_error".into(), String::new()),
            "api_error",
        ),
    ];
    for (provider, final_status, outcome, cause) in cases {
        let o2 = outcome.clone();
        let fx = Fx::new(provider, "", &["a"], 0, move |_| o2.clone());
        if let Some((status, message)) = final_status {
            fx.server.final_status(status, message);
        }
        let o = fx.env.run(&["build", "--batch"]);
        let line = format!("avertissement: docs/a.md : requête du lot du fournisseur en échec ({cause}) ; relancez nephila update --batch");
        assert_eq!(
            lines_with(&err(&o), "docs/a.md"),
            [line.as_str()],
            "{}",
            err(&o)
        );
        assert_eq!(code(&o), 1, "{cause}");
    }
    // E-36, délai dépassé.
    let fx = Fx::new("anthropic", "timeout_secs = 1\n", &["a"], 0, good);
    fx.server.route(
        "POST",
        fx.create_path(),
        vec![Reply::Http {
            status: 200,
            body: "{}".into(),
            headers: vec![],
            delay_ms: 3000,
        }],
    );
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(last_line(&o), "erreur: envoi interrompu : création du lot du fournisseur en échec (délai dépassé) : vérifiez sa console puis relancez nephila update --batch ; 1 fichier(s) à enrichir");
}

#[test]
fn req217_verbose_sans_derniere_passe_ni_enrichissement_ni_appels() {
    let none = |e: &str, what: &str| {
        assert!(!e.contains("enrichissement:"), "{what} : {e}");
        assert!(!e.contains("appels:"), "{what} : {e}");
        assert!(!e.contains("politique:"), "{what} : {e}");
    };
    // E-32.
    let fx = Fx::all_good("anthropic", &FIVE);
    let o = fx
        .env
        .run(&["build", "--batch", "--verbose", "--max-calls", "3"]);
    assert_eq!(last_line(&o), e32(3, 5));
    none(&err(&o), "E-32");
    // Ctrl-C.
    let fx = fixture_forever("anthropic", &["a", "b"]);
    let d = fx.interrupt_at(1, &["build", "--batch", "--verbose"]);
    assert_eq!(d.status.code(), Some(130));
    none(&d.stderr, "Ctrl-C");
    // Panne du suivi.
    let fx = Fx::all_good("anthropic", &["a", "b"]);
    fx.server
        .route("GET", &fx.status_path(1), vec![Reply::status(500, "{}")]);
    let o = fx.env.run(&["build", "--batch", "--verbose"]);
    assert_eq!(last_line(&o), e35(2));
    none(&err(&o), "panne");
}

// ---------------------------------------------------------------------------------------------
// Appels conservés (T-316, REQ-228).
// ---------------------------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Fichier `docs/a.md` de trois appels. Tant que `healthy` est faux, les appels de rang `bad`
/// échouent chez le fournisseur ; les autres réussissent.
fn three_calls(provider: &'static str, bad: &'static [usize]) -> (Fx, Arc<AtomicBool>) {
    let healthy = Arc::new(AtomicBool::new(false));
    let h = healthy.clone();
    let fx = Fx::new(
        provider,
        "format_retries = 0\nmax_chunk_chars = 200\n",
        &["a"],
        0,
        move |b| {
            let t = b.to_string();
            let rank = (1..=3)
                .find(|k| t.contains(&format!("(extrait {k}/3")))
                .expect("rang de l'appel");
            if !h.load(Ordering::SeqCst) && bad.contains(&rank) {
                Outcome::Errored("api_error".into(), "boom".into())
            } else if rank == 1 {
                Outcome::Text(single("a"))
            } else {
                Outcome::Text(json!({"version": 2, "summaries": [], "concepts": []}).to_string())
            }
        },
    );
    big_doc(&fx.env);
    (fx, healthy)
}

fn kept_rows(fx: &Fx) -> usize {
    nephila::store::Store::open_readonly(&fx.env.repo)
        .unwrap()
        .call_results()
        .unwrap()
        .len()
}

#[test]
fn req228_ac2_le_lot_ne_porte_que_les_appels_manquants_et_le_plafond_les_compte() {
    for provider in PROVIDERS {
        let (fx, healthy) = three_calls(provider, &[2, 3]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
        assert_eq!(fx.server.lots()[0].len(), 3, "{provider}");
        assert_eq!(kept_rows(&fx), 1, "{provider}");
        healthy.store(true, Ordering::SeqCst);

        let o = fx.env.run(&["update", "--batch", "--max-calls", "1"]);
        assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
        assert_eq!(last_line(&o), e32(1, 2), "{provider}");
        assert_eq!(fx.server.lots().len(), 1, "{provider}");

        let o = fx.env.run(&["update", "--batch", "--max-calls", "2"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 2, "{provider}");
        assert_eq!(fx.to_enrich().len(), 0, "{provider}");
    }
}

#[test]
fn req228_ac3_derniere_passe_conserve_les_reponses_puis_reprise_des_appels_manquants() {
    for provider in PROVIDERS {
        let (fx, healthy) = three_calls(provider, &[2]);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
        assert_eq!(fx.to_enrich(), ["docs/a.md"], "{provider}");
        assert!(
            err(&o).lines().any(|l| l.starts_with(
                "avertissement: docs/a.md : requête du lot du fournisseur en échec ("
            ) && l.ends_with("boom) ; relancez nephila update --batch")),
            "{provider} : {}",
            err(&o)
        );
        assert_eq!(kept_rows(&fx), 2, "{provider}");
        healthy.store(true, Ordering::SeqCst);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 1, "{provider}");
        assert!(lots[1][0].to_string().contains("(extrait 2/3"));
        assert_eq!(fx.to_enrich().len(), 0, "{provider}");
        assert_eq!(kept_rows(&fx), 0, "{provider}");

        // Sans --batch : un seul appel.
        let (fx, healthy) = three_calls(provider, &[2]);
        fx.env.run(&["build", "--batch"]);
        assert_eq!(kept_rows(&fx), 2, "{provider}");
        healthy.store(true, Ordering::SeqCst);
        let empty = json!({"version": 2, "summaries": [], "concepts": []}).to_string();
        fx.server.route(
            "POST",
            fx.sync_path(),
            vec![Reply::status(200, &wrap(provider, &empty))],
        );
        let o = fx.env.run(&["update"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(
            fx.server.hits("POST", fx.sync_path()).len(),
            1,
            "{provider}"
        );
        assert_eq!(fx.to_enrich().len(), 0, "{provider}");
    }
}

#[test]
fn req228_ac3_passe_de_collecte_interrompue_n_ecrit_aucune_ligne() {
    for provider in PROVIDERS {
        let fx = Fx::new(
            provider,
            "format_retries = 0\nmax_chunk_chars = 200\n",
            &["a"],
            FOREVER,
            good,
        );
        big_doc(&fx.env);
        fx.interrupted();
        assert_eq!(kept_rows(&fx), 0, "{provider}");
        assert_eq!(fx.server.lots()[0].len(), 3, "{provider}");
    }
}

#[test]
fn req228_ac5_tous_les_appels_conserves_aucun_lot() {
    for provider in PROVIDERS {
        let (fx, healthy) = three_calls(provider, &[]);
        healthy.store(true, Ordering::SeqCst);
        // Un déclencheur garde les lignes à la réussite ; le fichier redevient « à enrichir ».
        let o = fx.env.run(&["build", "--no-llm"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let db = fx.env.repo.join(".nephila/graph.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER keep BEFORE DELETE ON enrich_calls BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
        drop(conn);
        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("DROP TRIGGER keep;").unwrap();
        conn.execute("DELETE FROM enrichments WHERE path = 'docs/a.md'", [])
            .unwrap();
        drop(conn);
        assert_eq!(kept_rows(&fx), 3, "{provider}");
        assert_eq!(fx.to_enrich(), ["docs/a.md"], "{provider}");
        let lots = fx.server.lots().len();

        let o = fx.env.run(&["update", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert_eq!(fx.server.lots().len(), lots, "{provider}");
        assert!(!err(&o).contains("lot du fournisseur"), "{}", err(&o));
        assert!(
            without_coverage(&out(&o)).ends_with(", 0 fichiers à enrichir"),
            "{provider} : {}",
            out(&o)
        );
    }
}

/// `docs/a.md` de 7 sections et d'un titre : 9 nœuds avec le document.
fn nine_nodes_doc(env: &Env) {
    let mut s = String::from("# Titre\n\n");
    for k in 1..=7 {
        s.push_str(&format!("## Section{k}\n\nTexte de la section {k}.\n\n"));
    }
    fs::write(env.repo.join("docs/a.md"), s).unwrap();
}

/// Liste de nœuds du fichier d'une requête (REQ-267) : avant `<file path=` dans un lot, entre le
/// bloc et la consigne pour un fichier seul.
fn list_segment(text: &str) -> &str {
    let start = text.find("Nœuds du fichier :").unwrap();
    let rest = &text[start..];
    let end = ["<file path=", "Réponds par l'objet JSON demandé, sur"]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap();
    &rest[..end]
}

/// Nombre de nœuds listés dans le corps d'une requête.
fn listed_nodes(body: &Value) -> usize {
    list_segment(&body.to_string()).matches("\\n[").count()
}

#[test]
fn req230_ac10_appel_tronque_en_mode_lot_deux_requetes_dans_le_lot_suivant() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a"], 0, |b| {
            if listed_nodes(b) == 9 {
                Outcome::Truncated("{".into())
            } else if b.to_string().contains("— document:docs/a.md") {
                Outcome::Text(single("a"))
            } else {
                Outcome::Text(json!({"version": 2, "summaries": [], "concepts": []}).to_string())
            }
        });
        nine_nodes_doc(&fx.env);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[0].len(), 1, "{provider}");
        assert_eq!(lots[1].len(), 2, "{provider}");
        assert_eq!(listed_nodes(&lots[1][0]), 5, "{provider}");
        assert_eq!(listed_nodes(&lots[1][1]), 4, "{provider}");
        assert_eq!(fx.env.to_enrich(), 0, "{provider}");
        assert_eq!(fx.summary_of("a"), Some(summary_text("a")), "{provider}");
        assert!(!err(&o).contains("tronquée"), "{provider} : {}", err(&o));
    }
    let fx = Fx::new("anthropic", "", &["a"], 0, good);
    nine_nodes_doc(&fx.env);
    let d = fx.env.run(&["build", "--dry-run", "--batch"]);
    assert!(
        out(&d).contains("estimation minimale : 1 appels, "),
        "{}",
        out(&d)
    );
}

#[test]
fn req232_ac15_lot_tronque_avec_entrees_completes_second_lot_du_reste() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "batch_files = 4\n", &NAMES, 0, |b| {
            if names_in(b).len() == 4 {
                let mut text = lot(&["a", "b"]);
                text.truncate(text.len() - 2);
                text.push_str(r#",{"file": "docs/c.md", "summaries": [{"node": "doc"#);
                Outcome::Truncated(text)
            } else {
                good(b)
            }
        });
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 1, "{provider}");
        assert_eq!(names_in(&lots[1][0]), ["c", "d"], "{provider}");
        assert_eq!(fx.env.to_enrich(), 0, "{provider}");
        for n in NAMES {
            assert_eq!(fx.summary_of(n), Some(summary_text(n)), "{provider}");
        }
        assert!(
            !err(&o).contains("avertissement: docs"),
            "{provider} : {}",
            err(&o)
        );
    }
}

/// Identifiants de la liste « Nœuds du fichier » d'une requête.
fn listed_ids(body: &Value) -> Vec<String> {
    let text = body.to_string();
    list_segment(&text)
        .split("\\n[")
        .skip(1)
        .map(|x| {
            let line = x.split("\\n").next().unwrap();
            line.split_once(" — ").unwrap().1.to_string()
        })
        .collect()
}

#[test]
fn req232_ac15_appel_tronque_avec_resumes_second_lot_d_une_requete_du_reste() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a"], 0, |b| {
            let ids = listed_ids(b);
            let item = |n: &String| json!({"node": n, "summary": format!("Résumé de {n}.")});
            if ids.len() == 9 {
                let first: Vec<String> = ids[..4].iter().map(|n| item(n).to_string()).collect();
                Outcome::Truncated(format!(
                    r#"{{"version": 2, "summaries": [{},{{"node": "{}", "summ"#,
                    first.join(","),
                    ids[4]
                ))
            } else {
                let all: Vec<Value> = ids.iter().map(item).collect();
                Outcome::Text(json!({"version": 2, "summaries": all, "concepts": []}).to_string())
            }
        });
        nine_nodes_doc(&fx.env);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let lots = fx.server.lots();
        assert_eq!(lots.len(), 2, "{provider}");
        assert_eq!(lots[1].len(), 1, "{provider}");
        let (first, second) = (listed_ids(&lots[0][0]), listed_ids(&lots[1][0]));
        assert_eq!(first.len(), 9, "{provider}");
        assert_eq!(second, first[4..], "{provider}");
        assert_eq!(fx.env.to_enrich(), 0, "{provider}");
        assert_eq!(
            fx.summary_of("a"),
            Some("Résumé de document:docs/a.md.".to_string()),
            "{provider}"
        );
        assert!(
            !err(&o).contains("avertissement: docs"),
            "{provider} : {}",
            err(&o)
        );
    }
}

/// Bilan sans son segment de couverture (REQ-235), qui doit être présent.
fn without_coverage(out: &str) -> String {
    let (head, tail) = out.trim_end().rsplit_once(", ").unwrap();
    assert!(tail.ends_with(" nœuds résumés"), "{out}");
    head.to_string()
}

/// Réponse à numéros pour la requête : le document est le nœud 1 de son fichier seul.
fn numbered_single(body: &Value) -> Outcome {
    let n = names_in(body)[0];
    Outcome::Text(
        json!({"summaries": [{"n": 1, "summary": summary_text(n)}], "concepts": []}).to_string(),
    )
}

#[test]
#[allow(non_snake_case)]
fn REQ237_AC8_reponses_a_numeros_reliees_par_empreinte_du_corps() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a", "b", "c"], 0, numbered_single);
        fx.server.reverse_results();
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        assert!(
            !err(&o).contains("avertissement: docs/"),
            "{provider} {}",
            err(&o)
        );
        for n in ["a", "b", "c"] {
            assert_eq!(fx.summary_of(n), Some(summary_text(n)), "{provider} {n}");
        }
        assert_eq!(fx.env.to_enrich(), 0);
    }
}

/// AC-241-2 : en mode lot, un JSON de 500 000 octets part réduit (marque) et son résumé est relié.
#[test]
fn req241_ac2_json_de_500_ko_en_mode_lot_porte_la_marque() {
    let fx = Fx::new("anthropic", "enrich_code = true\n", &[], 0, |_| {
        Outcome::Text(
            json!({"version": 2,
                   "summaries": [{"node": "document:config/big.json", "summary": "Gros JSON."}],
                   "concepts": []})
            .to_string(),
        )
    });
    let mut text = String::from("{\"premiere_cle\":1,\"liste\":[");
    while text.len() < 499_900 {
        text.push_str("{\"n\":123456},");
    }
    text.push_str("{\"n\":\"FIN_UNIQUE_ZZ\"}]}");
    text.push_str(&" ".repeat(500_000 - text.len()));
    fs::create_dir_all(fx.env.repo.join("config")).unwrap();
    fs::write(fx.env.repo.join("config/big.json"), &text).unwrap();
    let o = fx.env.run(&["build", "--batch"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let hits = fx.server.hits("POST", "/v1/messages/batches");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].body.contains("extrait tronqué de 489 Ko"));
    assert!(!hits[0].body.contains("FIN_UNIQUE_ZZ"));
    let summary = nephila::store::Store::open_readonly(&fx.env.repo)
        .unwrap()
        .nodes()
        .unwrap()
        .into_iter()
        .find(|n| n.id == "document:config/big.json")
        .and_then(|n| n.summary);
    assert_eq!(summary.as_deref(), Some("Gros JSON."));
}

#[test]
fn req275_ac5_chaque_requete_du_lot_porte_la_temperature() {
    for provider in ["anthropic", "openai"] {
        let fx = Fx::new(provider, "temperature = 0.2\n", &["a", "b"], 0, good);
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let hits = fx.server.hits("POST", fx.create_path());
        assert_eq!(hits.len(), 1, "{provider}");
        let text = if provider == "anthropic" {
            hits[0].body.clone()
        } else {
            let all: Vec<String> = fx
                .server
                .hits("POST", "/v1/files")
                .iter()
                .map(|h| h.body.clone())
                .collect();
            all.join("\n")
        };
        let n = text.matches("\"temperature\":0.2").count();
        assert_eq!(n, 2, "{provider} : {text}");
    }
}

#[test]
fn req276_ac9_fichier_reecrit_pendant_que_le_lot_est_tenu_n_est_pas_enregistre() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, "", &["a", "b"], 0, good);
        fx.server.hold(Some(1));
        let run = fx.env.spawn(&["build", "--batch"]);
        wait_until("sondage du lot", || fx.polls(1) >= 1);
        fs::write(
            fx.env.repo.join(path("a")),
            "# Document a\n\nTexte du document a, réécrit pendant la commande.\n",
        )
        .unwrap();
        fx.server.hold(None);
        let d = run.finish();
        let expected = format!(
            "avertissement: {} : modifié pendant la commande ; relancez nephila update --batch",
            path("a")
        );
        let lines: Vec<&str> = d.stderr.lines().filter(|l| *l == expected).collect();
        assert_eq!(lines.len(), 1, "{provider} : {}", d.stderr);
        assert_eq!(d.status.code(), Some(1), "{provider} : {}", d.stderr);
        assert_eq!(d.last(), "erreur: 1 fichier(s) à enrichir", "{provider}");
        assert_eq!(fx.summary_of("a"), None, "{provider}");
        assert_eq!(fx.summary_of("b"), Some(summary_text("b")), "{provider}");
        assert_eq!(fx.to_enrich(), [path("a")], "{provider}");
    }
}

// ---- REQ-278 : modèle servi lu dans le résultat de chaque requête du lot ----

#[test]
fn req278_ac4_chaque_entree_du_mode_lot_porte_le_nom_servi_de_son_resultat() {
    for provider in PROVIDERS {
        let fx = Fx::all_good(provider, &["a", "b"]);
        fx.server.served_model("m-20250929");
        let o = fx.env.run(&["build", "--batch"]);
        assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
        let conn = rusqlite::Connection::open(fx.env.repo.join(".nephila/graph.db")).unwrap();
        let mut q = conn
            .prepare("SELECT path, model, served_model FROM enrichments ORDER BY path")
            .unwrap();
        let rows: Vec<(String, String, Option<String>)> = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let want = |n: &str| (path(n), "m".to_string(), Some("m-20250929".to_string()));
        assert_eq!(rows, vec![want("a"), want("b")], "{provider}");
    }
}
