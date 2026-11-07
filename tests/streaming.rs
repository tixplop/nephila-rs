//! Réponses LLM demandées et reçues en flux (REQ-249, T-339) : corps envoyé, assemblage d'un
//! flux `anthropic` et `openai` (jetons identiques à la réponse JSON), mode lot sans `stream`.
#![allow(non_snake_case)]

#[allow(dead_code)]
#[path = "support/batch_server.rs"]
mod batch_server;
#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use batch_server::{BatchServer, Outcome};
use fake_server::{FakeServer, Reply};
use serde_json::{json, Value};

const KEY_VAR: &str = "NEPHILA_T339_KEY";
const SUMMARY: &str = "Résumé du document";
const ASK_REPLY: &str = r#"{"found":true,"answer":"réponse-ask","nodes":["Alpha"]}"#;
const PROVIDERS: [&str; 2] = ["anthropic", "openai"];

fn enrichment() -> String {
    json!({
        "version": 2,
        "summaries": [{"node": "document:docs/d.md", "summary": SUMMARY}],
        "concepts": []
    })
    .to_string()
}

fn sse(provider: &str, text: &str) -> Reply {
    let chunks = if provider == "anthropic" {
        support::anthropic_sse(text, 4)
    } else {
        support::openai_sse(text, 4)
    };
    Reply::Sse { chunks, cut: false }
}

/// La même réponse, servie en JSON, avec les mêmes jetons (10 entrées, 7 sorties).
fn json_reply(provider: &str, text: &str) -> Reply {
    let body = if provider == "anthropic" {
        json!({"content":[{"type":"text","text":text}],"stop_reason":"end_turn",
            "usage":{"input_tokens":10,"output_tokens":7}})
    } else {
        json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":7}})
    };
    Reply::status(200, &body.to_string())
}

fn path_of(provider: &str) -> &'static str {
    if provider == "anthropic" {
        "/v1/messages"
    } else {
        "/v1/chat/completions"
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
    provider: &'static str,
}

impl Fx {
    fn new(provider: &'static str, replies: Vec<Reply>) -> Fx {
        let server = FakeServer::start_replies(replies);
        Fx::with_url(provider, server)
    }

    fn with_url(provider: &'static str, server: FakeServer) -> Fx {
        Fx::with_opts(provider, server, "", 1)
    }

    /// `extra` : lignes ajoutées à la configuration ; `docs` : nombre de documents Markdown.
    fn with_opts(provider: &'static str, server: FakeServer, extra: &str, docs: usize) -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/d.md"), "# D\n\nTexte du document.\n").unwrap();
        for i in 2..=docs {
            fs::write(
                repo.join(format!("docs/d{i}.md")),
                format!("# D{i}\n\nTexte {i}.\n"),
            )
            .unwrap();
        }
        fs::write(
            repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\n{extra}",
                server.base_url
            ),
        )
        .unwrap();
        Fx {
            _tmp: tmp,
            repo,
            config,
            server,
            provider,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_retry(args, "5")
    }

    fn run_retry(&self, args: &[&str], retry_base_ms: &str) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", retry_base_ms)
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    /// Corps JSON des requêtes de génération.
    fn bodies(&self) -> Vec<Value> {
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == path_of(self.provider))
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect()
    }

    fn to_enrich(&self) -> u64 {
        let o = self.run(&["status", "--json"]);
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        v["to_enrich"].as_u64().unwrap()
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn check_stream_fields(provider: &str, b: &Value) {
    assert_eq!(b["stream"], json!(true), "{provider} : {b}");
    if provider == "openai" {
        assert_eq!(b["stream_options"], json!({"include_usage": true}), "{b}");
    } else {
        assert!(b.get("stream_options").is_none(), "{b}");
    }
}

#[test]
fn REQ249_AC1_enrichissement_envoie_stream_true() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, vec![sse(provider, &enrichment())]);
        let o = fx.run(&["build"]);
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        let bodies = fx.bodies();
        assert_eq!(bodies.len(), 1, "{provider}");
        check_stream_fields(provider, &bodies[0]);
    }
}

#[test]
fn REQ249_AC1_query_ask_envoie_stream_true() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, vec![sse(provider, ASK_REPLY)]);
        let o = fx.run(&["build", "--no-llm"]);
        assert!(o.status.success(), "{}", err(&o));
        let o = fx.run(&["query", "ask", "Alpha"]);
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        let bodies = fx.bodies();
        assert_eq!(bodies.len(), 1, "{provider}");
        check_stream_fields(provider, &bodies[0]);
    }
}

#[test]
fn REQ249_AC1_mode_lot_sans_stream() {
    for provider in PROVIDERS {
        let server = BatchServer::start();
        let text = enrichment();
        server.simulate(provider, 0, move |_| Outcome::Text(text.clone()));
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::write(repo.join("docs/d.md"), "# D\n\nTexte du document.\n").unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\n",
                server.base_url
            ),
        )
        .unwrap();
        let o = Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&repo)
            .args(["build", "--batch"])
            .env("NEPHILA_CONFIG", &config)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_BATCH_POLL_MS", "5")
            .env("NEPHILA_RETRY_BASE_MS", "1")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        let lots = server.lots();
        assert_eq!(lots.len(), 1, "{provider}");
        assert!(!lots[0].is_empty());
        for body in lots.iter().flatten() {
            assert!(body.get("stream").is_none(), "{provider} : {body}");
            assert!(body.get("stream_options").is_none(), "{provider} : {body}");
        }
        for r in server.requests() {
            assert!(!r.body.contains("\"stream\""), "{provider} : {}", r.body);
        }
    }
}

/// Fin de la ligne `llm enrich fin`, à partir de ` (jetons : `.
fn tokens_tail(o: &Output) -> String {
    let e = err(o);
    let fins: Vec<&str> = e.lines().filter(|l| l.contains("llm enrich fin")).collect();
    assert_eq!(fins.len(), 1, "{e}");
    let at = fins[0]
        .find(" (jetons : ")
        .unwrap_or_else(|| panic!("{}", fins[0]));
    fins[0][at..].to_string()
}

fn same_tokens_as_json(provider: &'static str) {
    let streamed = Fx::new(provider, vec![sse(provider, &enrichment())]);
    let o = streamed.run(&["build", "--verbose"]);
    assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
    assert_eq!(streamed.to_enrich(), 0, "{provider}");
    let plain = Fx::new(provider, vec![json_reply(provider, &enrichment())]);
    let p = plain.run(&["build", "--verbose"]);
    assert_eq!(p.status.code(), Some(0), "{provider} : {}", err(&p));
    let tail = tokens_tail(&o);
    assert_eq!(tail, tokens_tail(&p), "{provider}");
    assert!(tail.contains("entrée 10"), "{tail}");
    assert!(tail.contains("sortie 7"), "{tail}");
}

#[test]
fn REQ249_AC2_anthropic_flux_donne_les_memes_jetons_que_le_json() {
    same_tokens_as_json("anthropic");
}

#[test]
fn REQ249_AC3_openai_flux_donne_les_memes_jetons_que_le_json() {
    same_tokens_as_json("openai");
}

#[test]
fn REQ249_AC3_query_ask_sur_un_flux_affiche_la_reponse() {
    for provider in PROVIDERS {
        let fx = Fx::new(provider, vec![sse(provider, ASK_REPLY)]);
        assert!(fx.run(&["build", "--no-llm"]).status.success());
        let o = fx.run(&["query", "ask", "Alpha"]);
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        assert!(out(&o).contains("réponse-ask"), "{provider} : {}", out(&o));
    }
}

// ---- Silences, erreurs et coupures au milieu d'un flux (REQ-250, REQ-251, REQ-094, T-340) ----

const TIMEOUT_CFG: &str = "timeout_secs = 1\n";
const E22: &str = "erreur: envoi interrompu : délais dépassés répétés : augmentez timeout_secs ou vérifiez le fournisseur ; 6 fichier(s) à enrichir";

fn lines_of(o: &Output) -> Vec<String> {
    err(o).lines().map(String::from).collect()
}

fn warnings_of(o: &Output) -> Vec<String> {
    lines_of(o)
        .into_iter()
        .filter(|l| l.starts_with("avertissement:"))
        .collect()
}

fn waits_of(o: &Output) -> Vec<String> {
    lines_of(o)
        .into_iter()
        .filter(|l| l.contains("llm enrich attente"))
        .collect()
}

/// Flux conforme dont les `n` premiers événements sont espacés de 400 ms.
fn slow_sse(provider: &str, n: usize) -> Reply {
    let Reply::Sse { mut chunks, cut } = sse(provider, &enrichment()) else {
        unreachable!()
    };
    assert!(chunks.len() >= n);
    for c in chunks.iter_mut().take(n) {
        c.0 = 400;
    }
    Reply::Sse { chunks, cut }
}

/// Premier événement, puis 5 s de silence, puis la fin du flux.
fn silent_sse(provider: &str) -> Reply {
    let Reply::Sse { mut chunks, cut } = sse(provider, &enrichment()) else {
        unreachable!()
    };
    chunks[1].0 = 5000;
    Reply::Sse { chunks, cut }
}

fn sse_chunks(provider: &str) -> Vec<(u64, String)> {
    let Reply::Sse { chunks, .. } = sse(provider, &enrichment()) else {
        unreachable!()
    };
    chunks
}

#[test]
fn REQ250_AC1_evenements_reguliers_plus_longs_que_le_delai_total() {
    for provider in PROVIDERS {
        // 8 événements à 400 ms : 3,2 s au total pour un délai d'inactivité de 1 s.
        let server = FakeServer::start_replies(vec![slow_sse(provider, 8)]);
        let fx = Fx::with_opts(provider, server, TIMEOUT_CFG, 1);
        let o = fx.run(&["build"]);
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        assert_eq!(fx.bodies().len(), 1, "{provider}");
        assert_eq!(fx.to_enrich(), 0, "{provider}");
        assert!(warnings_of(&o).is_empty(), "{provider} : {}", err(&o));
    }
}

#[test]
fn REQ250_AC2_silence_apres_le_premier_evenement_donne_delai_depasse() {
    for provider in PROVIDERS {
        let server = FakeServer::start_replies(vec![silent_sse(provider)]);
        let fx = Fx::with_opts(provider, server, TIMEOUT_CFG, 1);
        let o = fx.run(&["build"]);
        let reqs = fx.server.requests();
        let posts: Vec<_> = reqs.iter().filter(|r| r.method == "POST").collect();
        assert_eq!(posts.len(), 1, "{provider}");
        let elapsed = std::time::Instant::now().duration_since(posts[0].at);
        assert!(elapsed.as_secs_f64() < 3.0, "{provider} : {elapsed:?}");
        assert_eq!(fx.to_enrich(), 1, "{provider}");
        let w = warnings_of(&o);
        assert_eq!(w.len(), 1, "{provider} : {}", err(&o));
        assert!(w[0].contains("délai dépassé"), "{}", w[0]);

        let server = FakeServer::start_replies(vec![silent_sse(provider)]);
        let fx = Fx::with_opts(provider, server, TIMEOUT_CFG, 1);
        assert!(fx.run(&["build", "--no-llm"]).status.success());
        let o = fx.run(&["query", "ask", "Alpha"]);
        assert_eq!(o.status.code(), Some(1), "{provider} : {}", err(&o));
        assert_eq!(fx.bodies().len(), 1, "{provider}");
        assert!(
            lines_of(&o)
                .iter()
                .any(|l| l.starts_with("erreur:") && l.contains("délai dépassé")),
            "{provider} : {}",
            err(&o)
        );
    }
}

#[test]
fn REQ250_AC6_texte_du_delai_depasse_sans_et_avec_verbose() {
    let server = FakeServer::start_replies(vec![silent_sse("anthropic")]);
    let fx = Fx::with_opts("anthropic", server, TIMEOUT_CFG, 1);
    let o = fx.run(&["build"]);
    let w = warnings_of(&o);
    assert_eq!(w.len(), 1, "{}", err(&o));
    assert!(w[0].starts_with("avertissement: docs/d.md"), "{}", w[0]);
    assert!(
        w[0].contains(" : délai dépassé avec anthropic sur "),
        "{}",
        w[0]
    );
    assert!(w[0].ends_with(" ; augmentez timeout_secs"), "{}", w[0]);
    assert!(
        lines_of(&o)
            .iter()
            .all(|l| l.starts_with("avertissement:") || l.starts_with("erreur:")),
        "{}",
        err(&o)
    );
    no_stream_words(&o);

    let server = FakeServer::start_replies(vec![silent_sse("anthropic")]);
    let fx = Fx::with_opts("anthropic", server, TIMEOUT_CFG, 1);
    let o = fx.run(&["build", "--verbose"]);
    let fin: Vec<String> = lines_of(&o)
        .into_iter()
        .filter(|l| l.contains("llm enrich fin docs/d.md"))
        .collect();
    assert_eq!(fin.len(), 1, "{}", err(&o));
    assert!(fin[0].contains(" délai dépassé"), "{}", fin[0]);
}

fn no_stream_words(o: &Output) {
    for l in lines_of(o) {
        for w in ["inactivité", "plafond", "flux", "SSE"] {
            assert!(!l.contains(w), "{w} dans : {l}");
        }
    }
}

#[test]
fn REQ250_AC6_trois_silences_de_suite_arretent_l_envoi() {
    let server = FakeServer::start_replies((0..6).map(|_| silent_sse("anthropic")).collect());
    let fx = Fx::with_opts("anthropic", server, TIMEOUT_CFG, 6);
    let o = fx.run(&["build"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_eq!(
        lines_of(&o).last().map(String::as_str),
        Some(E22),
        "{}",
        err(&o)
    );
    assert_eq!(fx.bodies().len(), 3);
    no_stream_words(&o);
}

fn error_event(provider: &str, kind: &str) -> String {
    if provider == "anthropic" {
        format!("event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"{kind}\",\"message\":\"x\"}}}}\n\n")
    } else {
        "data: {\"error\": {\"message\": \"x\"}}\n\n".into()
    }
}

/// Début de flux, puis l'événement d'erreur, puis la fermeture propre.
fn error_sse(provider: &str, kind: &str) -> Reply {
    let mut chunks = sse_chunks(provider);
    chunks.truncate(1);
    chunks.push((0, error_event(provider, kind)));
    Reply::Sse { chunks, cut: false }
}

#[test]
fn REQ251_AC1_overloaded_error_en_plein_flux_est_reessaye() {
    let server = FakeServer::start_replies(vec![
        error_sse("anthropic", "overloaded_error"),
        sse("anthropic", &enrichment()),
    ]);
    let fx = Fx::with_opts("anthropic", server, "", 1);
    let o = fx.run_retry(&["build", "--verbose"], "10");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(fx.bodies().len(), 2);
    assert_eq!(fx.to_enrich(), 0);
    let w = waits_of(&o);
    assert_eq!(w.len(), 1, "{}", err(&o));
    assert!(
        w[0].ends_with("après HTTP 529 (nouvel essai 1/4)"),
        "{}",
        w[0]
    );

    // Toujours saturé : 5 requêtes, même avertissement que pour cinq réponses HTTP 529.
    let server = FakeServer::start_replies(
        (0..5)
            .map(|_| error_sse("anthropic", "overloaded_error"))
            .collect(),
    );
    let fx = Fx::with_opts("anthropic", server, "", 1);
    let o = fx.run_retry(&["build", "--verbose"], "10");
    assert_eq!(fx.bodies().len(), 5);
    assert_eq!(fx.to_enrich(), 1);
    let server = FakeServer::start_replies((0..5).map(|_| Reply::status(529, "{}")).collect());
    let http = Fx::with_opts("anthropic", server, "", 1);
    let h = http.run_retry(&["build", "--verbose"], "10");
    let norm = |o: &Output, fx: &Fx| -> Vec<String> {
        warnings_of(o)
            .into_iter()
            .map(|l| l.replace(&fx.server.base_url, "URL"))
            .collect()
    };
    assert_eq!(norm(&o, &fx).len(), 1, "{}", err(&o));
    assert_eq!(norm(&o, &fx), norm(&h, &http));
}

#[test]
fn REQ251_AC2_api_error_et_erreur_openai_en_plein_flux_sont_reessayees() {
    for provider in PROVIDERS {
        let server = FakeServer::start_replies(vec![
            error_sse(provider, "api_error"),
            sse(provider, &enrichment()),
        ]);
        let fx = Fx::with_opts(provider, server, "", 1);
        let o = fx.run_retry(&["build", "--verbose"], "10");
        assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
        assert_eq!(fx.bodies().len(), 2, "{provider}");
        assert_eq!(fx.to_enrich(), 0, "{provider}");
        let w = waits_of(&o);
        assert_eq!(w.len(), 1, "{provider} : {}", err(&o));
        assert!(
            w[0].ends_with("après HTTP 500 (nouvel essai 1/4)"),
            "{}",
            w[0]
        );
    }
}

/// Flux coupé après deux événements (connexion fermée sans la fin du corps).
fn cut_sse(provider: &str) -> Reply {
    let mut chunks = sse_chunks(provider);
    chunks.truncate(2);
    Reply::Sse { chunks, cut: true }
}

/// Corps fini proprement, sans `message_stop` (Anthropic) ou sans `finish_reason` ni `[DONE]`.
fn unfinished_sse(provider: &str) -> Reply {
    let mut chunks = sse_chunks(provider);
    let drop = if provider == "anthropic" { 1 } else { 3 };
    chunks.truncate(chunks.len() - drop);
    if provider == "openai" {
        // Le jeton de fin (usage) reste : il n'a ni `finish_reason` ni `[DONE]` avant lui.
        chunks.push((
            0,
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":7}}\n\n"
                .into(),
        ));
    }
    Reply::Sse { chunks, cut: false }
}

fn check_broken(provider: &'static str, broken: fn(&str) -> Reply) {
    // Rompu à chaque requête : 2 requêtes, document à enrichir, avertissement, code non nul.
    let server = FakeServer::start_replies((0..5).map(|_| broken(provider)).collect());
    let fx = Fx::with_opts(provider, server, "", 1);
    let o = fx.run_retry(&["build"], "10");
    assert_eq!(fx.bodies().len(), 2, "{provider} : {}", err(&o));
    assert_eq!(fx.to_enrich(), 1, "{provider}");
    assert!(
        warnings_of(&o)
            .iter()
            .any(|l| l.contains("lecture de la réponse")),
        "{provider} : {}",
        err(&o)
    );
    assert_ne!(o.status.code(), Some(0), "{provider}");

    // Rompu à la première requête seulement.
    let server = FakeServer::start_replies(vec![broken(provider), sse(provider, &enrichment())]);
    let fx = Fx::with_opts(provider, server, "", 1);
    let o = fx.run_retry(&["build", "--verbose"], "10");
    assert_eq!(o.status.code(), Some(0), "{provider} : {}", err(&o));
    assert_eq!(fx.bodies().len(), 2, "{provider}");
    assert_eq!(fx.to_enrich(), 0, "{provider}");
    let w = waits_of(&o);
    assert_eq!(w.len(), 1, "{provider} : {}", err(&o));
    assert!(
        w[0].ends_with("après lecture interrompue (nouvel essai 1/4)"),
        "{}",
        w[0]
    );
}

#[test]
fn REQ251_AC3_flux_coupe_est_reessaye_une_fois() {
    for provider in PROVIDERS {
        check_broken(provider, cut_sse);
    }
}

#[test]
fn REQ251_AC3_corps_fini_sans_marque_de_fin_donne_les_memes_constats() {
    for provider in PROVIDERS {
        check_broken(provider, unfinished_sse);
    }
}
