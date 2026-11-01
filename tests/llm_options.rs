//! Options LLM `thinking`, `effort` et `max_output_tokens` (REQ-058, T-134) : validation,
//! erreur E-15, ligne `config`, aide.
#![allow(non_snake_case)]

use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
#[path = "support/mod.rs"]
mod support;
use fake_server::FakeServer;

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
        fs::write(repo.join("A.java"), "package p;\npublic class A {}\n").unwrap();
        fs::write(repo.join("README.md"), "# Titre\n\nTexte.\n").unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, config).unwrap();
        Env {
            _tmp: tmp,
            repo,
            config: config_path,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn write_config(&self, text: &str) {
        fs::write(&self.config, text).unwrap();
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn config_line(o: &Output) -> String {
    stderr(o)
        .lines()
        .find(|l| l.contains("config: "))
        .unwrap_or_else(|| panic!("ligne config absente : {}", stderr(o)))
        .to_string()
}

const MOCK: &str = "provider = \"mock\"\n";
const EFFORT: &str = "\"low\", \"medium\", \"high\" ou \"default\"";
const BOOL: &str = "un booléen, true ou false";
const INT: &str = "un entier strictement positif";

/// (ligne TOML, clé, attendu, valeur réécrite)
fn invalid() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        ("effort = \"max\"", "effort", EFFORT, "\"max\""),
        ("effort = \"\"", "effort", EFFORT, "\"\""),
        ("effort = \"LOW\"", "effort", EFFORT, "\"LOW\""),
        ("effort = 1", "effort", EFFORT, "1"),
        ("effort = \"none\"", "effort", EFFORT, "\"none\""),
        ("effort = \"DEFAULT\"", "effort", EFFORT, "\"DEFAULT\""),
        ("effort = \"auto\"", "effort", EFFORT, "\"auto\""),
        ("thinking = \"oui\"", "thinking", BOOL, "\"oui\""),
        ("thinking = 1", "thinking", BOOL, "1"),
        ("max_output_tokens = 0", "max_output_tokens", INT, "0"),
        ("max_output_tokens = -5", "max_output_tokens", INT, "-5"),
        ("max_output_tokens = 1.5", "max_output_tokens", INT, "1.5"),
        (
            "max_output_tokens = \"16384\"",
            "max_output_tokens",
            INT,
            "\"16384\"",
        ),
    ]
}

#[test]
fn REQ058_AC2_AC14_valeur_invalide_refusee_avant_tout_parcours_et_tout_appel() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    for (line, key, expected, got) in invalid() {
        for provider in [
            "provider = \"mock\"\n".to_string(),
            format!("provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\n"),
        ] {
            let env = Env::new(&format!("{provider}{line}\n"));
            let want = format!(
                "erreur: configuration invalide ({}) : clé « {key} » doit être {expected} (reçu : {got})\n",
                env.config.display()
            );
            for args in [&["build"][..], &["update"][..], &["query", "ask", "x"][..]] {
                let o = env.run(args);
                assert!(!o.status.success(), "{line} {args:?}");
                assert!(o.stdout.is_empty(), "{line} {args:?}");
                assert_eq!(stderr(&o), want, "{line} {args:?}");
                assert!(!env.repo.join(".nephila").exists(), "{line} {args:?}");
            }
            for sub in ["build", "update"] {
                let o = env.run(&[sub, "--no-llm"]);
                assert_eq!(o.status.code(), Some(0), "{line} {sub}: {}", stderr(&o));
                assert_eq!(stderr(&o), "", "{line} {sub}");
            }
        }
    }
    assert!(
        server.accept().is_err(),
        "le faux serveur a reçu une connexion"
    );
}

#[test]
fn REQ058_AC2_configuration_invalide_laisse_le_graphe_intact() {
    let env = Env::new(MOCK);
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let db = env.repo.join(".nephila").join("graph.db");
    let before = fs::read(&db).unwrap();
    env.write_config(&format!("{MOCK}effort = \"max\"\n"));
    assert!(!env.run(&["update"]).status.success());
    assert_eq!(before, fs::read(&db).unwrap());
}

#[test]
fn REQ058_AC7_nephila_toml_refuse_les_trois_cles() {
    for line in [
        "effort = \"low\"",
        "thinking = true",
        "max_output_tokens = 100",
    ] {
        let env = Env::new(MOCK);
        fs::write(env.repo.join(".nephila.toml"), format!("{line}\n")).unwrap();
        let o = env.run(&["build"]);
        assert!(!o.status.success(), "{line}");
        let key = line.split(' ').next().unwrap();
        assert!(
            stderr(&o).contains(&format!("clé inconnue `{key}`")),
            "{line} : {}",
            stderr(&o)
        );
        assert!(stderr(&o).contains("clés permises :"), "{}", stderr(&o));
    }
}

#[test]
fn REQ058_AC10_AC13_ligne_config_mock() {
    let env = Env::new(MOCK);
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(
            " model=mock enrich_code=false summary_language=\"français\" thinking=false effort=low max_output_tokens=16384 concurrency=4 "
        ),
        "{l}"
    );
    assert!(l.ends_with(" batch_files=10 format_retries=2"), "{l}");

    env.write_config(&format!(
        "{MOCK}thinking = true\neffort = \"high\"\nmax_output_tokens = 4096\n"
    ));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(
            " enrich_code=false summary_language=\"français\" thinking=true effort=high max_output_tokens=4096 concurrency="
        ),
        "{l}"
    );

    env.write_config(&format!("{MOCK}enrich_code = true\neffort = \"medium\"\n"));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(
            " enrich_code=true summary_language=\"français\" thinking=false effort=medium max_output_tokens=16384 concurrency="
        ),
        "{l}"
    );

    env.write_config(MOCK);
    fs::write(env.repo.join(".nephila.toml"), "model = \"m-projet\"\n").unwrap();
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(l.ends_with(" (.nephila.toml : model)"), "{l}");
    let project = l.split("(.nephila.toml").nth(1).unwrap();
    for k in ["thinking", "effort", "max_output_tokens"] {
        assert!(!project.contains(k), "{l}");
    }
    fs::remove_file(env.repo.join(".nephila.toml")).unwrap();

    env.write_config(&format!("{MOCK}tls_insecure = true\n"));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(l.ends_with(" format_retries=2 tls_insecure"), "{l}");
    for k in [" thinking=", " effort=", " max_output_tokens="] {
        assert_eq!(l.matches(k).count(), 1, "{l}");
    }
}

#[test]
fn REQ058_AC10_ligne_config_openai() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let env = Env::new(&format!(
        "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\nthinking = true\nmax_output_tokens = 99\n"
    ));
    // Sans document à enrichir, aucun appel n'est émis : le faux serveur ne répond pas.
    fs::remove_file(env.repo.join("README.md")).unwrap();
    let o = env.run(&["build", "--verbose", "--no-llm"]);
    assert!(!stderr(&o).contains("thinking"), "{}", stderr(&o));
    let o = env.run(&["build", "--verbose"]);
    let l = config_line(&o);
    assert!(
        l.contains(" llm=openai model=m enrich_code=false summary_language=\"français\" thinking=true effort=low max_output_tokens=99 concurrency="),
        "{l}"
    );
    assert!(server.accept().is_err());
}

#[test]
fn REQ058_AC15_aide_sans_les_options_llm() {
    for args in [
        &["build", "--help"][..],
        &["update", "--help"][..],
        &["query", "ask", "--help"][..],
    ] {
        let o = Command::new(cargo_bin("nephila"))
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success());
        let h = String::from_utf8_lossy(&o.stdout);
        // Les deux lignes « Réglages de query ask seulement » de l'aide de `build` et `update`
        // (design.md V-001, REQ-164) nomment les clés de `[ask]` : hors de ces lignes seulement.
        let mut skip = 0u8;
        let h: String = h
            .lines()
            .filter(|l| {
                if l.starts_with("Réglages de query ask seulement") {
                    skip = 2;
                }
                let drop = skip > 0;
                skip = skip.saturating_sub(1);
                !drop
            })
            .collect::<Vec<_>>()
            .join("\n");
        for k in ["thinking", "effort", "max_output_tokens"] {
            assert!(!h.contains(k), "{args:?} contient {k}");
        }
    }
}

// ---- T-135 : envoi par fournisseur, réflexion ignorée, mock sans effet ----

const SECRET: &str = "SECRET_REFLEXION_7";
const KEY_VAR: &str = "NEPHILA_T135_KEY";
const SUMMARY: &str = "Résumé du document";
const ASK_REPLY: &str = r#"{"found":true,"answer":"réponse-ask","nodes":["Alpha"]}"#;

fn enrichment() -> String {
    serde_json::json!({
        "version": 2,
        "summaries": [{"node": "document:docs/d.md", "summary": SUMMARY}],
        "concepts": []
    })
    .to_string()
}

fn reply(provider: &str, t: &str) -> String {
    if provider == "anthropic" {
        serde_json::json!({"content":[{"type":"text","text":t}]}).to_string()
    } else {
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":t}}]}).to_string()
    }
}

/// Réponse avec réflexion à côté du texte.
fn reply_thinking(provider: &str, t: &str) -> String {
    if provider == "anthropic" {
        serde_json::json!({"content":[
            {"type":"thinking","thinking":SECRET,"signature":"s"},
            {"type":"redacted_thinking","data":"opaque"},
            {"type":"text","text":t}
        ]})
        .to_string()
    } else {
        serde_json::json!({"choices":[{"message":{
            "role":"assistant","content":t,"reasoning":SECRET,"reasoning_content":SECRET}}]})
        .to_string()
    }
}

struct Http {
    env: Env,
    server: FakeServer,
    provider: String,
}

impl Http {
    fn new(provider: &str, extra: &str, bodies: Vec<String>) -> Http {
        Http::with_model(provider, "m", extra, bodies)
    }

    fn with_model(provider: &str, model: &str, extra: &str, bodies: Vec<String>) -> Http {
        let server = FakeServer::start(bodies.into_iter().map(|b| (200, b)).collect());
        let env = Env::new(&format!(
            "provider = \"{provider}\"\nmodel = \"{model}\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\n{extra}",
            server.base_url
        ));
        fs::remove_file(env.repo.join("README.md")).unwrap();
        fs::create_dir_all(env.repo.join("docs")).unwrap();
        fs::write(env.repo.join("docs/d.md"), "# D\n\nTexte du document.\n").unwrap();
        fs::write(
            env.repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
        Http {
            env,
            server,
            provider: provider.into(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.env.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.env.config)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "5")
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    /// Corps JSON des requêtes de génération.
    fn bodies(&self) -> Vec<serde_json::Value> {
        let path = if self.provider == "anthropic" {
            "/v1/messages"
        } else {
            "/v1/chat/completions"
        };
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == path)
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect()
    }
}

/// Enrichissement d'un document : corps de la requête.
fn enrich_body(provider: &str, model: &str, extra: &str) -> serde_json::Value {
    let h = Http::with_model(
        provider,
        model,
        extra,
        vec![reply(provider, &enrichment()); 4],
    );
    let o = h.run(&["build"]);
    assert!(o.status.success(), "{provider} {extra}: {}", stderr(&o));
    let mut b = h.bodies();
    assert!(!b.is_empty(), "{provider} {extra}");
    b.remove(0)
}

/// `query ask` : corps de la requête.
fn ask_body(provider: &str, model: &str, extra: &str) -> serde_json::Value {
    let h = Http::with_model(provider, model, extra, vec![reply(provider, ASK_REPLY); 4]);
    let o = h.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let o = h.run(&["query", "ask", "Alpha"]);
    assert!(o.status.success(), "{provider} {extra}: {}", stderr(&o));
    let mut b = h.bodies();
    assert!(!b.is_empty(), "{provider} {extra}");
    b.remove(0)
}

fn no_fields(b: &serde_json::Value, keys: &[&str]) {
    for k in keys {
        assert!(b.get(*k).is_none(), "champ {k} présent : {b}");
    }
}

/// Corps reçus par les scénarios d'AC-157-4 et d'AC-157-5 (AC-157-6).
static SEEN: std::sync::Mutex<Vec<serde_json::Value>> = std::sync::Mutex::new(Vec::new());

fn seen(b: serde_json::Value) -> serde_json::Value {
    SEEN.lock().unwrap().push(b.clone());
    b
}

/// AC-157-6 : ni `disabled`, ni `between_tools`, ni `"none"` ; `thinking` adaptatif ; effort
/// parmi les trois niveaux.
fn check_seen(expected_min: usize) {
    let all = SEEN.lock().unwrap().clone();
    assert!(all.len() >= expected_min, "{} corps", all.len());
    for b in all {
        let text = b.to_string();
        for bad in ["disabled", "between_tools", "\"none\""] {
            assert!(!text.contains(bad), "{bad} dans {text}");
        }
        if let Some(t) = b.get("thinking") {
            assert_eq!(*t, serde_json::json!({"type": "adaptive"}));
        }
        for e in [
            b.get("reasoning_effort").cloned(),
            b.get("output_config").map(|o| o["effort"].clone()),
        ]
        .into_iter()
        .flatten()
        {
            assert!(
                ["low", "medium", "high"].contains(&e.as_str().unwrap()),
                "{e}"
            );
        }
    }
}

#[test]
fn REQ157_AC4_anthropic_corps_de_la_requete() {
    for which in ["enrich", "ask"] {
        let body = |model: &str, extra: &str| {
            seen(if which == "enrich" {
                enrich_body("anthropic", model, extra)
            } else {
                ask_body("anthropic", model, extra)
            })
        };
        let m = "claude-opus-5-5";
        let b = body(m, "");
        assert_eq!(b["max_tokens"], 16384, "{which}");
        assert_eq!(b["output_config"], serde_json::json!({"effort": "low"}));
        no_fields(&b, &["thinking", "max_completion_tokens"]);
        assert_eq!(body(m, "max_output_tokens = 1234\n")["max_tokens"], 1234);
        let b = body(m, "effort = \"high\"\n");
        assert_eq!(b["output_config"], serde_json::json!({"effort": "high"}));
        no_fields(&b, &["thinking"]);
        let b = body(m, "effort = \"default\"\n");
        no_fields(&b, &["thinking", "output_config"]);
        assert_eq!(b.as_object().unwrap().len(), 4, "{which}");
        let b = body(m, "thinking = true\n");
        assert_eq!(b["thinking"], serde_json::json!({"type": "adaptive"}));
        assert_eq!(b["output_config"], serde_json::json!({"effort": "low"}));
        assert_eq!(b["max_tokens"], 16384);
        let b = body(m, "thinking = true\neffort = \"high\"\n");
        assert_eq!(b["output_config"], serde_json::json!({"effort": "high"}));
        let b = body("claude-sonnet-4-5", "");
        no_fields(&b, &["thinking", "output_config"]);
        assert_eq!(b.as_object().unwrap().len(), 4, "{which}");
    }
    check_seen(14);
}

#[test]
fn REQ157_AC5_openai_corps_de_la_requete() {
    for which in ["enrich", "ask"] {
        let body = |extra: &str| {
            seen(if which == "enrich" {
                enrich_body("openai", "m", extra)
            } else {
                ask_body("openai", "m", extra)
            })
        };
        let b = body("");
        assert_eq!(b["max_completion_tokens"], 16384, "{which}");
        no_fields(
            &b,
            &[
                "max_tokens",
                "reasoning_effort",
                "thinking",
                "output_config",
            ],
        );
        assert_eq!(b.as_object().unwrap().len(), 3);
        assert_eq!(
            body("max_output_tokens = 1234\n")["max_completion_tokens"],
            1234
        );
        assert_eq!(body("effort = \"medium\"\n")["reasoning_effort"], "medium");
        assert_eq!(body("effort = \"low\"\n")["reasoning_effort"], "low");
        for extra in [
            "effort = \"default\"\n",
            "thinking = true\neffort = \"default\"\n",
        ] {
            let b = body(extra);
            no_fields(&b, &["reasoning_effort"]);
            assert_eq!(b.as_object().unwrap().len(), 3, "{extra}");
        }
        assert_eq!(body("thinking = true\n")["reasoning_effort"], "low");
        assert_eq!(
            body("thinking = true\neffort = \"medium\"\n")["reasoning_effort"],
            "medium"
        );
    }
    check_seen(16);
}

#[test]
fn REQ157_AC4_nouvel_essai_http_renvoie_le_meme_corps_sans_thinking() {
    let server = FakeServer::start(vec![
        (503, "{}".into()),
        (503, "{}".into()),
        (200, reply("anthropic", &enrichment())),
    ]);
    let h = Http {
        env: Env::new(&format!(
            "provider = \"anthropic\"\nmodel = \"claude-opus-5-5\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\n",
            server.base_url
        )),
        server,
        provider: "anthropic".into(),
    };
    fs::remove_file(h.env.repo.join("README.md")).unwrap();
    fs::create_dir_all(h.env.repo.join("docs")).unwrap();
    fs::write(h.env.repo.join("docs/d.md"), "# D\n\nTexte.\n").unwrap();
    let o = h.run(&["build"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let b = h.bodies();
    assert_eq!(b.len(), 3);
    assert!(b.iter().all(|x| *x == b[0]));
    assert_eq!(b[0]["output_config"], serde_json::json!({"effort": "low"}));
    no_fields(&b[0], &["thinking"]);
}

#[test]
fn REQ058_AC4_nouvel_essai_http_renvoie_le_meme_corps_avec_thinking() {
    for provider in ["openai", "anthropic"] {
        let server = FakeServer::start(vec![
            (503, "{}".into()),
            (503, "{}".into()),
            (200, reply(provider, &enrichment())),
        ]);
        let h = Http {
            env: Env::new(&format!(
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = 1\nthinking = true\neffort = \"high\"\n",
                server.base_url
            )),
            server,
            provider: provider.into(),
        };
        fs::remove_file(h.env.repo.join("README.md")).unwrap();
        fs::create_dir_all(h.env.repo.join("docs")).unwrap();
        fs::write(h.env.repo.join("docs/d.md"), "# D\n\nTexte.\n").unwrap();
        let o = h.run(&["build"]);
        assert!(o.status.success(), "{provider}: {}", stderr(&o));
        let b = h.bodies();
        assert_eq!(b.len(), 3, "{provider}");
        assert!(b.iter().all(|x| *x == b[0]), "{provider}");
        let effort = if provider == "openai" {
            b[0]["reasoning_effort"].clone()
        } else {
            b[0]["output_config"]["effort"].clone()
        };
        assert_eq!(effort, "high");
    }
}

#[test]
fn REQ058_AC5_reflexion_ignoree_et_jamais_affichee() {
    for provider in ["anthropic", "openai"] {
        let h = Http::new(
            provider,
            "thinking = true\n",
            vec![reply_thinking(provider, &enrichment()); 6],
        );
        for verbose in [false, true] {
            let mut args = vec!["build"];
            if verbose {
                args.push("--verbose");
            }
            let o = h.run(&args);
            assert!(o.status.success(), "{provider}: {}", stderr(&o));
            assert!(
                !stderr(&o).contains(SECRET)
                    && !String::from_utf8_lossy(&o.stdout).contains(SECRET)
            );
            // Réenrichissement forcé au tour suivant.
            let _ = fs::remove_dir_all(h.env.repo.join(".nephila"));
        }
        let o = h.run(&["build"]);
        assert!(o.status.success(), "{}", stderr(&o));
        let o = h.run(&["query", "search", "d.md", "--json"]);
        assert!(
            String::from_utf8_lossy(&o.stdout).contains(SUMMARY),
            "{provider}"
        );
        let o = h.run(&["export", "--output", "-"]);
        assert!(o.status.success(), "{}", stderr(&o));
        assert!(
            !String::from_utf8_lossy(&o.stdout).contains(SECRET),
            "{provider}"
        );
        assert!(!stderr(&o).contains(SECRET));

        // query ask
        let h = Http::new(
            provider,
            "thinking = true\n",
            vec![reply_thinking(provider, ASK_REPLY); 6],
        );
        assert!(h.run(&["build", "--no-llm"]).status.success());
        for _ in 0..2 {
            let o = h.run(&["query", "ask", "Alpha"]);
            assert_eq!(o.status.code(), Some(0), "{provider}: {}", stderr(&o));
            assert!(String::from_utf8_lossy(&o.stdout).contains("réponse-ask"));
            assert!(!String::from_utf8_lossy(&o.stdout).contains(SECRET));
            assert!(!stderr(&o).contains(SECRET));
        }
    }
}

#[test]
fn REQ058_AC9_openai_finish_reason_length_est_tronquee_une_seule_requete() {
    let body = serde_json::json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","content":"{\"version\": 2, \"summ"}}]}).to_string();
    let h = Http::new("openai", "max_output_tokens = 10\n", vec![body; 6]);
    let o = h.run(&["build"]);
    assert_eq!(h.bodies().len(), 1, "{}", stderr(&o));
    assert_eq!(h.bodies()[0]["max_completion_tokens"], 10);
    let warns: Vec<String> = stderr(&o)
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect();
    assert_eq!(
        warns,
        ["avertissement: docs/d.md : réponse tronquée par la limite de jetons ; réduisez max_chunk_chars ou batch_files, ou augmentez max_output_tokens"],
        "{}",
        stderr(&o)
    );
}

fn mock_run(env: &Env, log: &std::path::Path, args: &[&str]) -> Output {
    Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&env.repo)
        .args(args)
        .env("NEPHILA_CONFIG", &env.config)
        .env("NEPHILA_MOCK_LOG", log)
        .env_remove("NEPHILA_MOCK_DIR")
        .output()
        .unwrap()
}

#[test]
fn REQ058_AC6_mock_sans_effet() {
    let env = Env::new(MOCK);
    let log = env.repo.parent().unwrap().join("mock.log");
    let mut seen = Vec::new();
    for extra in [
        "",
        "thinking = true\neffort = \"high\"\nmax_output_tokens = 1\n",
    ] {
        env.write_config(&format!("{MOCK}{extra}"));
        let _ = fs::remove_dir_all(env.repo.join(".nephila"));
        let _ = fs::remove_file(&log);
        let b = mock_run(&env, &log, &["build"]);
        assert_eq!(b.status.code(), Some(0), "{}", stderr(&b));
        let e = mock_run(&env, &log, &["export", "--output", "-"]);
        assert_eq!(e.status.code(), Some(0));
        seen.push((
            fs::read_to_string(&log).unwrap_or_default(),
            b.stdout,
            support::normalized_export(&e.stdout),
        ));
    }
    assert!(!seen[0].0.is_empty(), "le journal du mock est vide");
    assert_eq!(seen[0], seen[1]);
}

#[test]
fn REQ058_AC8_options_changees_n_entrainent_aucun_reenrichissement() {
    let env = Env::new(MOCK);
    let log = env.repo.parent().unwrap().join("mock.log");
    let b = mock_run(&env, &log, &["build"]);
    assert_eq!(b.status.code(), Some(0), "{}", stderr(&b));
    let before = fs::read_to_string(&log).unwrap();
    env.write_config(&format!(
        "{MOCK}effort = \"high\"\nthinking = true\nmax_output_tokens = 4096\n"
    ));
    let u = mock_run(&env, &log, &["update"]);
    assert_eq!(u.status.code(), Some(0), "{}", stderr(&u));
    assert_eq!(String::from_utf8_lossy(&u.stdout).trim(), "graphe à jour");
    assert_eq!(before, fs::read_to_string(&log).unwrap());
}

#[test]
fn REQ157_AC7_ligne_config_effort_default() {
    let env = Env::new(MOCK);
    env.write_config(&format!("{MOCK}effort = \"default\"\n"));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(" effort=default max_output_tokens=16384 "),
        "{l}"
    );
}

#[test]
fn REQ157_AC8_effort_default_identique_en_mock_et_update_sans_appel() {
    let env = Env::new(MOCK);
    let log = env.repo.parent().unwrap().join("mock.log");
    let mut seen = Vec::new();
    for extra in ["", "effort = \"default\"\n"] {
        env.write_config(&format!("{MOCK}{extra}"));
        let _ = fs::remove_dir_all(env.repo.join(".nephila"));
        let _ = fs::remove_file(&log);
        let b = mock_run(&env, &log, &["build"]);
        assert_eq!(b.status.code(), Some(0), "{}", stderr(&b));
        let e = mock_run(&env, &log, &["export", "--output", "-"]);
        assert_eq!(e.status.code(), Some(0));
        seen.push((
            fs::read_to_string(&log).unwrap_or_default(),
            b.stdout,
            support::normalized_export(&e.stdout),
        ));
    }
    assert!(!seen[0].0.is_empty(), "le journal du mock est vide");
    assert_eq!(seen[0], seen[1]);

    env.write_config(MOCK);
    let _ = fs::remove_dir_all(env.repo.join(".nephila"));
    let b = mock_run(&env, &log, &["build"]);
    assert_eq!(b.status.code(), Some(0), "{}", stderr(&b));
    let before = fs::read_to_string(&log).unwrap();
    env.write_config(&format!("{MOCK}effort = \"default\"\n"));
    let u = mock_run(&env, &log, &["update"]);
    assert_eq!(u.status.code(), Some(0), "{}", stderr(&u));
    assert_eq!(String::from_utf8_lossy(&u.stdout).trim(), "graphe à jour");
    assert_eq!(before, fs::read_to_string(&log).unwrap());
}

#[test]
fn REQ157_AC12_ligne_e15_exacte_en_build_et_en_ask_sur_depot_indexe() {
    let env = Env::new(MOCK);
    assert!(env.run(&["build", "--no-llm"]).status.success());
    for (line, got) in [
        ("effort = \"none\"", "\"none\""),
        ("effort = \"DEFAULT\"", "\"DEFAULT\""),
        ("effort = 1", "1"),
    ] {
        env.write_config(&format!("{MOCK}{line}\n"));
        let want = format!(
            "erreur: configuration invalide ({}) : clé « effort » doit être \"low\", \"medium\", \"high\" ou \"default\" (reçu : {got})\n",
            env.config.display()
        );
        for args in [&["build"][..], &["query", "ask", "x"][..]] {
            let o = env.run(args);
            assert!(!o.status.success(), "{line} {args:?}");
            assert!(o.stdout.is_empty(), "{line} {args:?}");
            assert_eq!(stderr(&o), want, "{line} {args:?}");
        }
    }
    env.write_config(&format!("{MOCK}effort = \"none\"\n"));
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(!stderr(&o).contains("erreur:"));
}

// ---- T-248 : validation de `[ask]` dans la configuration personnelle et portée (REQ-162/163) ----

/// Fautes de la table `[ask]` personnelle : (lignes TOML, clé, attendu, valeur réécrite). Le
/// texte d'une faute de clé inconnue est celui du lecteur TOML (`known == false`).
struct AskFault {
    toml: &'static str,
    key: &'static str,
    expected: &'static str,
    got: &'static str,
    known: bool,
}

fn ask_faults() -> Vec<AskFault> {
    let f = |toml, key, expected, got| AskFault {
        toml,
        key,
        expected,
        got,
        known: true,
    };
    let u = |toml, key| AskFault {
        toml,
        key,
        expected: "",
        got: "",
        known: false,
    };
    vec![
        f(
            "[ask]\nmodel = \"\"",
            "ask.model",
            "une chaîne non vide",
            "\"\"",
        ),
        f("[ask]\nthinking = \"oui\"", "ask.thinking", BOOL, "\"oui\""),
        f("[ask]\neffort = \"max\"", "ask.effort", EFFORT, "\"max\""),
        f(
            "[ask]\nmax_output_tokens = 0",
            "ask.max_output_tokens",
            INT,
            "0",
        ),
        f(
            "[ask]\nmax_context_chars = 999",
            "ask.max_context_chars",
            "un entier d'au moins 1000",
            "999",
        ),
        f("[ask]\nmax_seeds = 0", "ask.max_seeds", INT, "0"),
        f("[ask]\nmax_nodes = -1", "ask.max_nodes", INT, "-1"),
        f("ask = 3", "ask", "une table [ask]", "3"),
        u("[ask]\nzz = 1", "zz"),
        u("[ask]\nbase_url = \"http://127.0.0.1:9\"", "base_url"),
    ]
}

/// Liste triée (chemin relatif, contenu) de tous les fichiers sous `dir`.
fn tree_digest(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p.strip_prefix(base).unwrap().display().to_string();
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn ask_want(env: &Env, f: &AskFault) -> String {
    format!(
        "erreur: configuration invalide ({}) : clé « {} » doit être {} (reçu : {})\n",
        env.config.display(),
        f.key,
        f.expected,
        f.got
    )
}

#[test]
fn req162_ac7_ask_personnel_invalide_refuse_avant_tout_parcours_et_tout_appel() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    for f in ask_faults() {
        let env = Env::new(&format!(
            "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\n{}\n",
            f.toml
        ));
        for args in [&["build"][..], &["update"][..], &["query", "ask", "x"][..]] {
            let o = env.run(args);
            assert!(!o.status.success(), "{} {args:?}", f.toml);
            assert!(o.stdout.is_empty(), "{} {args:?}", f.toml);
            let err = stderr(&o);
            if f.known {
                assert_eq!(err, ask_want(&env, &f), "{} {args:?}", f.toml);
            } else {
                let head = format!(
                    "erreur: configuration invalide ({}) : ",
                    env.config.display()
                );
                assert!(err.starts_with(&head), "{err}");
                assert!(err.contains(f.key), "{err}");
            }
            assert!(!env.repo.join(".nephila").exists(), "{} {args:?}", f.toml);
        }
        // `--no-llm` : la configuration personnelle n'est pas lue.
        for sub in ["build", "update"] {
            let o = env.run(&[sub, "--no-llm"]);
            assert_eq!(o.status.code(), Some(0), "{} {sub}: {}", f.toml, stderr(&o));
            assert_eq!(stderr(&o), "", "{} {sub}", f.toml);
        }
    }
    assert!(
        server.accept().is_err(),
        "le faux serveur a reçu une connexion"
    );
}

#[test]
fn req162_ac7_ask_personnel_invalide_graphe_existant_inchange() {
    let env = Env::new(MOCK);
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let before = tree_digest(&env.repo.join(".nephila"));
    for f in ask_faults() {
        env.write_config(&format!("{MOCK}{}\n", f.toml));
        for args in [&["build"][..], &["update"][..], &["query", "ask", "x"][..]] {
            let o = env.run(args);
            assert!(!o.status.success(), "{} {args:?}", f.toml);
            assert_eq!(
                tree_digest(&env.repo.join(".nephila")),
                before,
                "{} {args:?}",
                f.toml
            );
        }
    }
}

#[test]
fn req163_ac5_plafonds_personnels_absents() {
    let env = Env::new(MOCK);
    env.write_config(&format!(
        "{MOCK}[ask]\nmax_context_chars = 1000000\nmax_seeds = 1000\nmax_nodes = 10000\n"
    ));
    for args in [&["build"][..], &["update"][..]] {
        let o = env.run(args);
        assert_eq!(o.status.code(), Some(0), "{args:?} {}", stderr(&o));
        assert!(!stderr(&o).contains("erreur:"), "{}", stderr(&o));
    }
}

#[test]
fn req162_ac11_texte_exact_de_la_ligne_personnelle_en_build_et_en_ask() {
    for f in ask_faults().into_iter().filter(|f| f.known) {
        let env = Env::new(&format!("{MOCK}{}\n", f.toml));
        let want = ask_want(&env, &f);
        for args in [&["build"][..], &["query", "ask", "x"][..]] {
            let o = env.run(args);
            assert_eq!(o.status.code(), Some(1), "{} {args:?}", f.toml);
            assert!(o.stdout.is_empty(), "{} {args:?}", f.toml);
            let err = stderr(&o);
            assert_eq!(err, want, "{} {args:?}", f.toml);
            assert_eq!(err.lines().count(), 1);
            for banned in ["TOML", "invalid type", "127.0.0.1"] {
                assert!(!err.contains(banned), "{err}");
            }
        }
    }
}

const ASK_ALL: &str = "[ask]\nmodel = \"m-ask\"\nthinking = true\neffort = \"high\"\nmax_output_tokens = 4096\nmax_context_chars = 3000\nmax_seeds = 5\nmax_nodes = 30\n";

/// Ligne `config` de `build --verbose`, horodatage retiré.
fn bare_config_line(env: &Env, log: &std::path::Path) -> String {
    let o = mock_run(env, log, &["build", "--verbose"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let l = config_line(&o);
    l[l.find("config:").unwrap()..].to_string()
}

#[test]
fn req162_ac9_ask_sans_effet_sur_update_et_sur_la_ligne_config_de_build() {
    let env = Env::new(MOCK);
    let log = env.repo.parent().unwrap().join("mock.log");
    let b = mock_run(&env, &log, &["build"]);
    assert_eq!(b.status.code(), Some(0), "{}", stderr(&b));
    assert!(!fs::read_to_string(&log).unwrap().is_empty());
    let baseline = bare_config_line(&env, &log);
    assert!(!baseline.contains("ask"), "{baseline}");
    let project = env.repo.join(".nephila.toml");
    for place in ["personnelle", "projet"] {
        if place == "personnelle" {
            env.write_config(&format!("{MOCK}{ASK_ALL}"));
        } else {
            env.write_config(MOCK);
            fs::write(&project, ASK_ALL).unwrap();
        }
        let before = fs::read_to_string(&log).unwrap();
        let u = mock_run(&env, &log, &["update"]);
        assert_eq!(u.status.code(), Some(0), "{place}: {}", stderr(&u));
        assert_eq!(String::from_utf8_lossy(&u.stdout).trim(), "graphe à jour");
        assert_eq!(before, fs::read_to_string(&log).unwrap(), "{place}");
        let line = bare_config_line(&env, &log);
        assert_eq!(line, baseline, "{place}");
        assert!(!line.contains("ask"), "{line}");
    }
}

// ---- T-249 : `query ask` suit la section [ask] (REQ-162) ----

/// `build` puis `query ask Alpha` : (corps d'enrichissement, corps `ask`) ; `project` est écrit
/// dans `.nephila.toml` avant le `build` ; `project_ask` y est ajouté avant la question.
fn split_bodies(
    provider: &str,
    model: &str,
    extra: &str,
    project: Option<&str>,
    project_ask: Option<&str>,
) -> (serde_json::Value, serde_json::Value) {
    let h = Http::with_model(
        provider,
        model,
        extra,
        vec![
            reply(provider, &enrichment()),
            reply(provider, ASK_REPLY),
            reply(provider, ASK_REPLY),
        ],
    );
    let toml = h.env.repo.join(".nephila.toml");
    if let Some(p) = project {
        fs::write(&toml, p).unwrap();
    }
    let o = h.run(&["build"]);
    assert!(o.status.success(), "{}", stderr(&o));
    if let Some(p) = project_ask {
        fs::write(&toml, format!("{}{p}", project.unwrap_or(""))).unwrap();
    }
    let o = h.run(&["query", "ask", "Alpha"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let b = h.bodies();
    assert_eq!(b.len(), 2, "{b:?}");
    (b[0].clone(), b[1].clone())
}

#[test]
fn req162_ac4_modele_et_generation_propres_a_ask_openai() {
    let (e, a) = split_bodies(
        "openai",
        "m-enrich",
        "[ask]\nmodel = \"m-ask\"\nmax_output_tokens = 2048\neffort = \"medium\"\n",
        None,
        None,
    );
    assert_eq!(e["model"], "m-enrich");
    assert_eq!(e["max_completion_tokens"], 16384);
    no_fields(&e, &["reasoning_effort"]);
    assert_eq!(a["model"], "m-ask");
    assert_eq!(a["max_completion_tokens"], 2048);
    assert_eq!(a["reasoning_effort"], "medium");
}

#[test]
fn req162_ac4_modele_et_generation_propres_a_ask_anthropic() {
    let (e, a) = split_bodies(
        "anthropic",
        "m-enrich",
        "[ask]\nmodel = \"m-ask\"\nmax_output_tokens = 2048\neffort = \"medium\"\nthinking = true\n",
        None,
        None,
    );
    assert_eq!(e["model"], "m-enrich");
    assert_eq!(e["max_tokens"], 16384);
    assert_eq!(e["output_config"], serde_json::json!({"effort": "low"}));
    no_fields(&e, &["thinking"]);
    assert_eq!(a["model"], "m-ask");
    assert_eq!(a["max_tokens"], 2048);
    assert_eq!(a["thinking"], serde_json::json!({"type": "adaptive"}));
    assert_eq!(a["output_config"], serde_json::json!({"effort": "medium"}));
}

#[test]
fn req162_ac5_priorite_entre_les_fichiers() {
    let (e, a) = split_bodies(
        "openai",
        "m-perso",
        "[ask]\nmodel = \"m-ask\"\n",
        Some("model = \"m-projet\"\n"),
        None,
    );
    assert_eq!(e["model"], "m-projet");
    assert_eq!(a["model"], "m-ask");
    let (e, a) = split_bodies(
        "openai",
        "m-perso",
        "[ask]\nmodel = \"m-ask\"\n",
        Some("model = \"m-projet\"\n"),
        Some("[ask]\nmodel = \"m-ask-projet\"\n"),
    );
    assert_eq!(e["model"], "m-projet");
    assert_eq!(a["model"], "m-ask-projet");
}

#[test]
fn req162_ac6_ancien_modele_claude_dans_les_deux_sens() {
    let (e, a) = split_bodies(
        "anthropic",
        "claude-opus-5-5",
        "[ask]\nmodel = \"claude-sonnet-4-5\"\n",
        None,
        None,
    );
    assert_eq!(e["output_config"], serde_json::json!({"effort": "low"}));
    no_fields(&a, &["output_config"]);
    let (e, a) = split_bodies(
        "anthropic",
        "claude-sonnet-4-5",
        "[ask]\nmodel = \"claude-opus-5-5\"\n",
        None,
        None,
    );
    no_fields(&e, &["output_config"]);
    assert_eq!(a["output_config"], serde_json::json!({"effort": "low"}));
}

// ---- REQ-174 : `summary_language` de la configuration personnelle ----

#[test]
fn REQ174_AC1_AC6_ligne_config_de_la_langue_personnelle() {
    let env = Env::new(MOCK);
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(
        l.contains(" enrich_code=false summary_language=\"français\" thinking=false "),
        "{l}"
    );
    env.write_config(&format!("{MOCK}summary_language = \"  English  \"\n"));
    let l = config_line(&env.run(&["build", "--verbose"]));
    assert!(l.contains(" summary_language=\"English\" "), "{l}");
    assert!(!l.contains("(.nephila.toml"), "{l}");
    // `query ask --verbose` : ligne `config` sans la clé.
    let o = env.run(&["query", "ask", "--verbose", "x"]);
    assert!(stderr(&o).contains("] config: "), "{}", stderr(&o));
    assert!(!stderr(&o).contains("summary_language"), "{}", stderr(&o));
    // `--no-llm` : ligne inactive, sans la clé.
    let o = env.run(&["build", "--no-llm", "--verbose"]);
    assert!(stderr(&o).contains("config: llm inactif (--no-llm)\n"));
    assert!(!stderr(&o).contains("summary_language"));
}

#[test]
fn REQ174_AC2_AC7_valeur_invalide_de_la_configuration_personnelle() {
    let cases = [
        ("\"en_US\"", "\"en_US\""),
        ("\"\"", "\"\""),
        ("3", "3"),
        ("[\"fr\"]", "[\"fr\"]"),
        ("\"fr\\u0007\"", "\"fr\\u0007\""),
        ("\"fr\\u202E\"", "\"fr\\u{202E}\""),
    ];
    for (raw, shown) in cases {
        let env = Env::new(&format!("{MOCK}summary_language = {raw}\n"));
        let want = format!(
            "erreur: configuration invalide ({}) : clé « summary_language » doit être un nom de langue de 1 à 40 lettres, espaces, traits d'union ou apostrophes (reçu : {shown})\n",
            env.config.display()
        );
        for args in [&["build"][..], &["update"], &["query", "ask", "x"]] {
            let o = env.run(args);
            assert_eq!(o.status.code(), Some(1), "{raw} {args:?}");
            assert!(o.stdout.is_empty(), "{raw} {args:?}");
            assert_eq!(stderr(&o), want, "{raw} {args:?}");
            assert!(!env.repo.join(".nephila").exists(), "{raw} {args:?}");
        }
        let o = env.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{raw}");
        assert!(!stderr(&o).contains("summary_language"), "{raw}");
    }
}

// ---- REQ-195 : trace du modèle du dépôt ignoré sous `model_override = "deny"` ----

#[test]
fn req195_ac5_ac8_trace_du_modele_ignore_sous_deny() {
    let env = Env::new(MOCK);
    fs::write(env.repo.join(".nephila.toml"), "model = \"m-projet\"\n").unwrap();
    let ignored = "modèle: m-projet de .nephila.toml ignoré (model_override = \"deny\")";
    env.write_config(&format!("{MOCK}model_override = \"deny\"\n"));
    let err = stderr(&env.run(&["build", "--verbose"]));
    let lines: Vec<&str> = err.lines().collect();
    let at = lines.iter().position(|l| l.contains("] config: ")).unwrap();
    assert!(!lines[at].contains("model=m-projet"), "{err}");
    assert!(!lines[at].contains("(.nephila.toml"), "{err}");
    assert!(
        lines[at + 1].starts_with("trace: [") && lines[at + 1].ends_with(&format!("] {ignored}")),
        "{err}"
    );
    assert_eq!(err.matches("ignoré (model_override").count(), 1);
    // Sans --verbose, ni la clé ni le modèle du dépôt n'apparaissent.
    let quiet = stderr(&env.run(&["build"]));
    assert!(
        !quiet.contains("m-projet") && !quiet.contains("model_override"),
        "{quiet}"
    );
    // allow et warn : aucune trace ; --no-llm : aucune trace.
    for p in ["allow", "warn"] {
        env.write_config(&format!("{MOCK}model_override = \"{p}\"\n"));
        let l = config_line(&env.run(&["build", "--verbose"]));
        assert!(l.contains(" model=m-projet "), "{l}");
        assert!(!stderr(&env.run(&["build", "--verbose"])).contains("ignoré (model_override"));
    }
    env.write_config(&format!("{MOCK}model_override = \"deny\"\n"));
    assert!(
        !stderr(&env.run(&["build", "--no-llm", "--verbose"])).contains("ignoré (model_override")
    );
    // deny sans model dans le projet : aucune trace.
    fs::remove_file(env.repo.join(".nephila.toml")).unwrap();
    assert!(!stderr(&env.run(&["build", "--verbose"])).contains("ignoré (model_override"));
}
