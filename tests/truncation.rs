//! Réponse tronquée par la limite de jetons (REQ-046) : enrichissement, faux serveur HTTP.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const LABEL: &str = "réponse tronquée par la limite de jetons";
const ADVICE: &str = "réduisez max_chunk_chars ou batch_files, ou augmentez max_output_tokens";
const SECRET: &str = "SECRET_REPONSE_42";
const A_GOOD: &str = r#"{"version": 2, "summaries": [{"node": "type:A", "summary": "Classe A centrale."}], "concepts": []}"#;

const CUT: &str = r#"{"version": 2, "summaries": [{"node": "type:A", "summ"#;

fn truncated_text(secret: bool) -> String {
    if secret {
        format!("{CUT}{SECRET}")
    } else {
        CUT.to_string()
    }
}

fn anthropic_reply(text: &str, stop: Option<&str>) -> String {
    let mut v = serde_json::json!({"content":[{"type":"text","text":text}]});
    if let Some(s) = stop {
        v["stop_reason"] = s.into();
    }
    v.to_string()
}

fn openai_reply(text: &str, finish: Option<&str>) -> String {
    let mut v = serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]});
    if let Some(s) = finish {
        v["choices"][0]["finish_reason"] = s.into();
    }
    v.to_string()
}

fn trunc_with(provider: &str, secret: bool) -> String {
    match provider {
        "anthropic" => anthropic_reply(&truncated_text(secret), Some("max_tokens")),
        _ => openai_reply(&truncated_text(secret), Some("length")),
    }
}

fn trunc(provider: &str) -> String {
    trunc_with(provider, false)
}

struct Setup {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
    provider: String,
}

impl Setup {
    /// `responses` : réponses successives du faux serveur (la vérification de clé en consomme
    /// une par lancement) ; `extra` : lignes de configuration en plus.
    fn new(provider: &str, responses: Vec<String>, extra: &str) -> Setup {
        Setup::with_replies(
            provider,
            responses.into_iter().map(|b| (200, b)).collect(),
            extra,
        )
    }

    fn with_replies(provider: &str, replies: Vec<(u16, String)>, extra: &str) -> Setup {
        let server = FakeServer::start(replies);
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T100_KEY\"\nconcurrency = 1\n{extra}",
                server.base_url
            ),
        )
        .unwrap();
        Setup {
            _tmp: tmp,
            repo,
            config,
            server,
            provider: provider.into(),
        }
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.repo.join(name), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T100_KEY", "sk-test")
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    fn posts(&self) -> Vec<String> {
        let path = match self.provider.as_str() {
            "anthropic" => "/v1/messages",
            _ => "/v1/chat/completions",
        };
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == path)
            .map(|r| r.body)
            .collect()
    }

    fn summary_of(&self, id: &str) -> Option<String> {
        nephila::store::Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.id == id)
            .and_then(|n| n.summary)
    }

    fn to_enrich_paths(&self) -> Vec<String> {
        let mut v: Vec<String> = nephila::store::Store::open_readonly(&self.repo)
            .unwrap()
            .files_to_enrich()
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        v.sort();
        v
    }

    fn to_enrich(&self) -> usize {
        self.to_enrich_paths().len()
    }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn a5(path: &str) -> String {
    format!("avertissement: {path} : {LABEL} ; {ADVICE}")
}

/// Réponse de lot (schéma version 3) pour les classes `names`, fichiers `<n>.java`.
fn lot_reply(names: &[&str]) -> String {
    let files: Vec<serde_json::Value> = names
        .iter()
        .map(|n| {
            serde_json::json!({
                "file": format!("{n}.java"),
                "summaries": [{"node": format!("type:{n}"), "summary": format!("Classe {n} centrale.")}],
                "concepts": []
            })
        })
        .collect();
    serde_json::json!({"version": 3, "files": files}).to_string()
}

/// Noms (`A`, `B`, …) des fichiers `<n>.java` cités dans un corps de requête, dans l'ordre alphabétique.
fn files_in(body: &str) -> Vec<&'static str> {
    ["A", "B", "C", "D", "E"]
        .into_iter()
        .filter(|n| body.contains(&format!("{n}.java")))
        .collect()
}

const A_JAVA: &str = "public class A {}\n";

fn many(provider: &str, n: usize) -> Vec<String> {
    vec![trunc(provider); n]
}

fn forbidden_in(stderr: &str) {
    for bad in [SECRET, "non conforme", "essai", "max_tokens"] {
        assert!(!stderr.contains(bad), "{bad} dans {stderr}");
    }
}

fn check_un_seul_envoi(provider: &str) {
    let s = Setup::new(provider, many(provider, 10), "");
    s.write("A.java", A_JAVA);
    let o = s.run(&["build"]);
    assert!(!o.status.success(), "{provider}");
    assert_eq!(s.posts().len(), 1, "{provider} {}", err(&o));
    let e = err(&o);
    assert!(e.contains(&a5("A.java")), "{provider} {e}");
    assert!(
        out(&o).contains("1 fichiers à enrichir"),
        "{provider} {}",
        out(&o)
    );
    forbidden_in(&e);
    assert_eq!(s.to_enrich(), 1, "{provider}");
    assert_eq!(s.summary_of("type:A"), None, "{provider}");
    let o = s.run(&["update"]);
    assert!(!o.status.success(), "{provider}");
    assert_eq!(s.posts().len(), 2, "{provider} {}", err(&o));
    assert!(
        out(&o).contains("1 fichiers à enrichir"),
        "{provider} {}",
        out(&o)
    );
    assert!(err(&o).contains(&a5("A.java")), "{provider} {}", err(&o));
    forbidden_in(&err(&o));
    assert_eq!(s.summary_of("type:A"), None, "{provider}");
}

#[test]
fn req046_ac1_anthropic_tronquee_un_seul_envoi() {
    check_un_seul_envoi("anthropic");
}

#[test]
fn req046_ac2_openai_tronquee_un_seul_envoi() {
    check_un_seul_envoi("openai");
}

#[test]
fn req046_ac3_texte_conforme_mais_tronque_garde_le_resume() {
    for p in ["anthropic", "openai"] {
        let body = match p {
            "anthropic" => anthropic_reply(A_GOOD, Some("max_tokens")),
            _ => openai_reply(A_GOOD, Some("length")),
        };
        let s = Setup::new(p, vec![body; 10], "");
        s.write("A.java", A_JAVA);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        assert_eq!(s.posts().len(), 1, "{p}");
        assert_eq!(s.to_enrich(), 0, "{p}");
        assert_eq!(
            s.summary_of("type:A").as_deref(),
            Some("Classe A centrale."),
            "{p}"
        );
        assert!(!err(&o).contains("avertissement:"), "{p} {}", err(&o));
    }
}

#[test]
fn req046_ac3_autres_raisons_d_arret_appliquent_le_resume() {
    let cases = [
        ("anthropic", Some("end_turn")),
        ("anthropic", None),
        ("openai", Some("stop")),
        ("openai", None),
    ];
    for (p, reason) in cases {
        let body = match p {
            "anthropic" => anthropic_reply(A_GOOD, reason),
            _ => openai_reply(A_GOOD, reason),
        };
        let s = Setup::new(p, vec![body; 10], "");
        s.write("A.java", A_JAVA);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {reason:?} {}", err(&o));
        assert_eq!(s.posts().len(), 1);
        assert_eq!(s.to_enrich(), 0);
        assert_eq!(
            s.summary_of("type:A").as_deref(),
            Some("Classe A centrale."),
            "{p} {reason:?}"
        );
    }
}

#[test]
fn req046_ac4_format_retries_sans_effet_sur_la_troncature() {
    let cases = [
        ("anthropic", "format_retries = 5\n", vec!["build"]),
        ("anthropic", "", vec!["build", "--format-retries", "5"]),
        ("openai", "format_retries = 0\n", vec!["build"]),
    ];
    let mut lines = Vec::new();
    for (p, extra, args) in cases {
        let s = Setup::new(p, many(p, 10), extra);
        s.write("A.java", A_JAVA);
        let o = s.run(&args);
        assert_eq!(s.posts().len(), 1, "{p} {args:?}");
        let e = err(&o);
        assert_eq!(e.matches(&a5("A.java")).count(), 1, "{p} {e}");
        lines.push(e.lines().filter(|l| l.contains("A.java")).count());
    }
    assert_eq!(lines, [1, 1, 1]);
}

#[test]
fn req046_ac5_lot_tronque_coupe_en_moities_puis_fichiers_seuls() {
    // Première réponse tronquée seulement : 4 requêtes, 4 fichiers enrichis, aucun avertissement.
    let one = |n: &str| {
        format!(
            r#"{{"version": 2, "summaries": [{{"node": "type:{n}", "summary": "Classe {n} centrale."}}], "concepts": []}}"#
        )
    };
    let s = Setup::new(
        "anthropic",
        vec![
            trunc("anthropic"),
            anthropic_reply(&one("A"), Some("end_turn")),
            anthropic_reply(&one("B"), Some("end_turn")),
            anthropic_reply(&lot_reply(&["C", "D"]), Some("end_turn")),
        ],
        "batch_files = 2\n",
    );
    for n in ["A", "B", "C", "D"] {
        s.write(&format!("{n}.java"), &format!("public class {n} {{}}\n"));
    }
    let o = s.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    let posts = s.posts();
    assert_eq!(posts.len(), 4, "{}", err(&o));
    assert_eq!(files_in(&posts[0]), ["A", "B"]);
    assert_eq!(files_in(&posts[1]), ["A"]);
    assert_eq!(files_in(&posts[2]), ["B"]);
    assert_eq!(files_in(&posts[3]), ["C", "D"]);
    assert!(!err(&o).contains("avertissement:"), "{}", err(&o));
    assert!(s.to_enrich_paths().is_empty());

    // Toutes les réponses tronquées : 6 requêtes, 4 fichiers à enrichir, une ligne par fichier.
    let s = Setup::new("anthropic", many("anthropic", 30), "batch_files = 2\n");
    for n in ["A", "B", "C", "D"] {
        s.write(&format!("{n}.java"), &format!("public class {n} {{}}\n"));
    }
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 6, "{}", err(&o));
    let e = err(&o);
    for n in ["A.java", "B.java", "C.java", "D.java"] {
        assert_eq!(e.matches(&a5(n)).count(), 1, "{e}");
    }
    assert_eq!(
        s.to_enrich_paths(),
        ["A.java", "B.java", "C.java", "D.java"]
    );
}

#[test]
fn req046_ac14_conseil_inchange_avec_thinking_et_sans_fuite_de_champs() {
    for provider in ["anthropic", "openai"] {
        let s = Setup::new(
            provider,
            many(provider, 10),
            "thinking = true\neffort = \"high\"\n",
        );
        s.write("A.java", A_JAVA);
        for verbose in [false, true] {
            let mut args = vec!["build"];
            if verbose {
                args.push("--verbose");
            }
            let o = s.run(&args);
            let e = err(&o);
            let warns: Vec<&str> = e
                .lines()
                .filter(|l| l.starts_with("avertissement:"))
                .collect();
            assert_eq!(warns, [a5("A.java")], "{provider} {e}");
            for bad in ["max_tokens", "max_completion_tokens", "reasoning_effort"] {
                assert!(!e.contains(bad), "{provider} {bad} dans {e}");
            }
            for l in e.lines().filter(|l| l.starts_with("trace:")) {
                if !l.contains("config:") {
                    assert!(
                        !l.contains("augmentez") && !l.contains("max_output_tokens"),
                        "{l}"
                    );
                }
            }
        }
    }
}

#[test]
fn req046_ac14_lot_tronque_deux_lignes_dans_l_ordre() {
    let s = Setup::new("anthropic", many("anthropic", 10), "batch_files = 2\n");
    s.write("A.java", "public class A {}\n");
    s.write("B.java", "public class B {}\n");
    let o = s.run(&["build"]);
    assert_eq!(s.posts().len(), 3, "{}", err(&o));
    for bad in ["max_tokens", "max_completion_tokens", "reasoning_effort"] {
        assert!(!err(&o).contains(bad), "{bad} dans {}", err(&o));
    }
    let warns: Vec<String> = err(&o)
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect();
    assert_eq!(warns, [a5("A.java"), a5("B.java")], "{}", err(&o));
}

fn big_text() -> String {
    let mut text = String::from("public class A {\n");
    for i in 0..60 {
        text.push_str(&format!("    void m{i}() {{}}\n"));
    }
    text.push_str("}\n");
    text
}

#[test]
fn req046_ac5_gros_fichier_morceau_2_tronque() {
    let text = big_text();
    let cfg = format!("max_chunk_chars = {}\n", text.len() / 3 + 40);
    let ok = r#"{"version": 2, "summaries": [], "concepts": []}"#;

    // Témoin : même texte, même découpage, trois réponses conformes -> exactement 3 morceaux.
    let w = Setup::new(
        "openai",
        vec![
            openai_reply(ok, Some("stop")),
            openai_reply(ok, Some("stop")),
            openai_reply(ok, Some("stop")),
            openai_reply(ok, Some("stop")),
        ],
        &cfg,
    );
    w.write("A.java", &text);
    let o = w.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(w.posts().len(), 3, "{}", err(&o));

    // Le serveur tronque tout à partir du morceau 2 (REQ-230) : le morceau 2 et ses 3 sous-appels
    // en chaîne, puis l'échec.
    let mut replies = vec![openai_reply(ok, Some("stop"))];
    replies.extend(vec![trunc("openai"); 6]);
    let s = Setup::new("openai", replies, &cfg);
    s.write("A.java", &text);
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    let posts = s.posts();
    assert_eq!(posts.len(), 5, "{}", err(&o));
    // Le témoin montre un appel par morceau : la requête 2 porte le morceau 2 (REQ-169).
    let n = nephila::chunk::split(&text, text.len() / 3 + 40).len();
    assert_eq!(n, 3);
    assert!(
        posts[1].contains(&format!("(extrait 2/{n}, lignes ")),
        "{}",
        posts[1]
    );
    assert_eq!(err(&o).matches(&a5("A.java")).count(), 1, "{}", err(&o));
    assert_eq!(s.to_enrich(), 1);
}

#[test]
fn req046_ac6_huit_fichiers_tronques_pas_de_panne_reseau() {
    let s = Setup::new("anthropic", many("anthropic", 30), "batch_files = 1\n");
    for i in 0..8 {
        s.write(&format!("F{i}.java"), &format!("public class F{i} {{}}\n"));
    }
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 8, "{}", err(&o));
    let e = err(&o);
    let a5_lines: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect();
    let expected: Vec<String> = (0..8).map(|i| a5(&format!("F{i}.java"))).collect();
    let mut got: Vec<String> = a5_lines.iter().map(|l| l.to_string()).collect();
    got.sort();
    assert_eq!(got, expected, "{e}");
    assert!(!e.contains("panne réseau"), "{e}");
    assert!(!e.contains("réseau"), "{e}");
    assert_eq!(s.to_enrich(), 8);
    assert!(out(&o).contains("8 fichiers à enrichir"), "{}", out(&o));
}

#[test]
fn req046_ac9_ligne_a5_identique_quelle_que_soit_la_politique() {
    let mut lines = Vec::new();
    for provider in ["anthropic", "openai"] {
        for (extra, args) in [
            ("format_retries = 0\n", vec!["build"]),
            ("format_retries = 5\n", vec!["build"]),
            ("", vec!["build", "--format-retries", "5"]),
        ] {
            let s = Setup::new(provider, vec![trunc_with(provider, true); 10], extra);
            s.write("A.java", A_JAVA);
            for step in [args.clone(), vec!["update"]] {
                let o = s.run(&step);
                let e = err(&o);
                forbidden_in(&e);
                let named: Vec<&str> = e.lines().filter(|l| l.contains("A.java")).collect();
                assert_eq!(named, [a5("A.java").as_str()], "{provider} {step:?} {e}");
                lines.push(named[0].to_string());
            }
        }
    }
    assert!(lines.windows(2).all(|w| w[0] == w[1]));
}

// ---- T-101 : `query ask`, traces `--verbose` et progression ----

const E11: &str =
    "erreur: réponse tronquée par la limite de jetons ; posez une question plus ciblée ou augmentez max_output_tokens";

impl Setup {
    fn run_env(&self, args: &[&str], no_color: bool) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T100_KEY", "sk-test")
            .env_remove("NO_COLOR");
        if no_color {
            c.env("NO_COLOR", "1");
        }
        c.output().unwrap()
    }

    /// Graphe sans LLM, puis `query ask` : la première réponse du serveur sert à la
    /// vérification de clé, les suivantes à la génération.
    fn ask(&self, extra: &[&str]) -> Output {
        let mut args = vec!["query", "ask"];
        args.extend_from_slice(extra);
        args.push("Alpha");
        self.run(&args)
    }
}

fn ask_setup(provider: &str, secret: bool) -> Setup {
    let mut responses = Vec::new();
    responses.extend(vec![trunc_with(provider, secret); 10]);
    let s = Setup::new(provider, responses, "");
    s.write("A.java", "public class Alpha {}\n");
    let o = s.run(&["build", "--no-llm"]);
    assert!(o.status.success(), "{}", err(&o));
    s
}

#[test]
fn req046_ac7_ask_tronquee_un_seul_envoi() {
    for p in ["anthropic", "openai"] {
        let s = ask_setup(p, false);
        let o = s.ask(&[]);
        assert!(!o.status.success(), "{p}");
        assert_eq!(s.posts().len(), 1, "{p} {}", err(&o));
        assert!(out(&o).is_empty(), "{p} {}", out(&o));
        let e = err(&o);
        let lines: Vec<&str> = e.lines().filter(|l| l.starts_with("erreur:")).collect();
        assert_eq!(lines.len(), 1, "{p} {e}");
        assert!(lines[0].contains("réponse tronquée"), "{p} {e}");
        assert!(
            !e.contains("max_chunk_chars") && !e.contains("essais sur"),
            "{e}"
        );
    }
}

#[test]
fn req046_ac10_ask_ligne_e11_exacte() {
    for p in ["anthropic", "openai"] {
        for extra in [
            vec!["--format-retries", "0"],
            vec!["--format-retries", "4"],
            vec!["--json"],
        ] {
            let s = ask_setup(p, true);
            let o = s.ask(&extra);
            assert!(!o.status.success(), "{p} {extra:?}");
            assert!(out(&o).is_empty(), "{p} {extra:?} {}", out(&o));
            assert_eq!(s.posts().len(), 1, "{p} {extra:?}");
            let e = err(&o);
            assert_eq!(e, format!("{E11}\n"), "{p} {extra:?}");
            for bad in ["max_chunk_chars", "batch_files", "essais sur", SECRET] {
                assert!(!e.contains(bad), "{bad} dans {e}");
            }
        }
    }
}

fn trace_lines_of(e: &str) -> Vec<&str> {
    e.lines().filter(|l| l.starts_with("trace:")).collect()
}

fn llm_enrich_for<'a>(e: &'a str, needle: &str) -> Vec<&'a str> {
    trace_lines_of(e)
        .into_iter()
        .filter(|l| l.contains("llm enrich") && l.contains(needle))
        .collect()
}

fn no_forbidden_trace(e: &str) {
    for l in trace_lines_of(e) {
        for bad in ["réduisez", SECRET, "(essai", "politique:"] {
            assert!(!l.contains(bad), "{bad} dans {l}");
        }
    }
}

/// `^trace: \[[0-9]+\.[0-9]{3}s\] llm enrich fin <path> \[1/1\] tronquée [0-9]+ ms : <LABEL>$`
fn fin_matches(line: &str, path: &str) -> bool {
    let Some(rest) = line.strip_prefix("trace: [") else {
        return false;
    };
    let Some((time, rest)) = rest.split_once("s] ") else {
        return false;
    };
    let Some((sec, milli)) = time.split_once('.') else {
        return false;
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if !digits(sec) || milli.len() != 3 || !digits(milli) {
        return false;
    }
    let Some(rest) = rest.strip_prefix(&format!("llm enrich fin {path} [1/1] tronquée ")) else {
        return false;
    };
    let Some((ms, detail)) = rest.split_once(" ms : ") else {
        return false;
    };
    digits(ms) && detail == LABEL
}

#[test]
fn req046_ac8_ac11_trace_depart_et_fin_tronquee() {
    for p in ["anthropic", "openai"] {
        let s = Setup::new(p, vec![trunc_with(p, true); 10], "");
        s.write("A.java", A_JAVA);
        let o = s.run(&["build", "--verbose"]);
        let e = err(&o);
        let lines = llm_enrich_for(&e, "A.java");
        assert_eq!(lines.len(), 2, "{p} {e}");
        assert!(
            lines[0].contains("llm enrich départ A.java [1/1]"),
            "{}",
            lines[0]
        );
        assert!(fin_matches(lines[1], "A.java"), "{}", lines[1]);
        no_forbidden_trace(&e);
    }
}

#[test]
fn req046_ac11_lot_tronque_trace_et_lignes_a5() {
    let mk = || {
        let s = Setup::new("anthropic", many("anthropic", 10), "batch_files = 2\n");
        for n in ["A", "B"] {
            s.write(&format!("{n}.java"), &format!("public class {n} {{}}\n"));
        }
        s
    };
    let s = mk();
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    let fin: Vec<&str> = llm_enrich_for(&e, "llm enrich fin");
    assert_eq!(fin.len(), 3, "{e}");
    assert!(fin_matches(fin[0], "lot A.java +1"), "{}", fin[0]);
    assert!(fin_matches(fin[1], "A.java"), "{}", fin[1]);
    assert!(fin_matches(fin[2], "B.java"), "{}", fin[2]);
    no_forbidden_trace(&e);

    let s = mk();
    let o = s.run(&["build"]);
    let e = err(&o);
    let warns: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect();
    assert_eq!(warns, [a5("A.java"), a5("B.java")], "{e}");
}

#[test]
fn req046_ac12_gros_fichier_traces_par_morceau() {
    let text = big_text();
    let cfg = format!("max_chunk_chars = {}\n", text.len() / 3 + 40);
    let ok = r#"{"version": 2, "summaries": [], "concepts": []}"#;
    let reply = |k: usize| -> Vec<String> {
        let mut v = Vec::new();
        for i in 1..=8 {
            v.push(if i >= k {
                trunc("openai")
            } else {
                openai_reply(ok, Some("stop"))
            });
        }
        v
    };
    let s = Setup::new("openai", reply(2), &cfg);
    s.write("A.java", &text);
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    let lines = llm_enrich_for(&e, "A.java");
    assert!(lines.len() >= 7, "{e}");
    assert!(lines[0].contains("départ A.java [1/3]"), "{}", lines[0]);
    assert!(lines[1].contains("fin A.java [1/3] ok ("), "{}", lines[1]);
    assert!(lines[2].contains("départ A.java [2/3]"), "{}", lines[2]);
    assert!(
        lines[3].contains("fin A.java [2/3] tronquée"),
        "{}",
        lines[3]
    );
    // Découpe du morceau 2 puis son premier sous-appel, avec le `[2/3]` du plan.
    assert!(
        lines[4].contains("llm enrich découpe A.java [2/3] en ")
            && lines[4].ends_with(" sous-appels"),
        "{}",
        lines[4]
    );
    assert!(
        lines[5].contains("llm enrich départ A.java [2/3] "),
        "{}",
        lines[5]
    );
    assert!(lines[5].ends_with("(sous-appel 1)"), "{}", lines[5]);
    assert!(
        lines[6].contains("llm enrich fin A.java [2/3] "),
        "{}",
        lines[6]
    );
    assert!(lines[6].ends_with("(sous-appel 1)"), "{}", lines[6]);
    assert!(lines[4..].iter().all(|l| l.contains("[2/3]")), "{e}");
    assert!(!e.contains("[3/3]"), "{e}");
    assert_eq!(e.matches("avertissement:").count(), 1, "{e}");
    no_forbidden_trace(&e);

    for k in 1..=3 {
        let s = Setup::new("openai", reply(k), &cfg);
        s.write("A.java", &text);
        let o = s.run(&["build"]);
        let e = err(&o);
        let warns: Vec<&str> = e
            .lines()
            .filter(|l| l.starts_with("avertissement:"))
            .collect();
        assert_eq!(warns, [a5("A.java")], "k={k} {e}");
    }
}

#[test]
fn req046_ac13_progression_cinq_lignes() {
    let mut seen = Vec::new();
    for no_color in [false, true] {
        let s = Setup::new("anthropic", vec![trunc_with("anthropic", true); 10], "");
        s.write("A.java", A_JAVA);
        let o = s.run_env(&["build", "--show-progress"], no_color);
        let e = err(&o);
        // Hors l'erreur finale `erreur: 1 fichier(s) à enrichir` (code non nul, REQ-010).
        let lines: Vec<&str> = e.lines().filter(|l| !l.starts_with("erreur:")).collect();
        assert_eq!(
            e.lines().last(),
            Some("erreur: 1 fichier(s) à enrichir"),
            "{e}"
        );
        let expected = [
            "progression: extraction 0 % (0/1 fichiers)",
            "progression: extraction 100 % (1/1 fichiers)",
            "progression: enrichissement 0 % (0/1 fichiers)",
        ];
        assert_eq!(lines.len(), 5, "{e}");
        assert_eq!(&lines[..3], &expected, "{e}");
        assert_eq!(lines[3], a5("A.java"), "{e}");
        assert_eq!(
            lines[4], "progression: enrichissement 100 % (1/1 fichiers)",
            "{e}"
        );
        let so = out(&o);
        assert_eq!(so.lines().count(), 1, "{so}");
        assert!(
            so.starts_with("build terminé : 1 fichiers traités, "),
            "{so}"
        );
        assert!(
            so.trim_end()
                .ends_with(", 0 fichiers en attente, 1 fichiers à enrichir"),
            "{so}"
        );
        assert!(!o.stderr.contains(&0x1b) && !o.stdout.contains(&0x1b));
        assert!(!o.stderr.contains(&b'\r') && !o.stdout.contains(&b'\r'));
        seen.push((e, so));
    }
    assert_eq!(seen[0], seen[1]);
}

// ---- REQ-096 : lot tronqué coupé en deux moitiés ----

fn four_docs(s: &Setup) {
    for n in ["A", "B", "C", "D"] {
        s.write(&format!("{n}.java"), &format!("public class {n} {{}}\n"));
    }
}

/// Réponses d'AC-096-1 : le lot tronqué, puis les deux moitiés conformes.
fn split_replies(p: &str) -> Vec<String> {
    let ok = |names: &[&str]| match p {
        "anthropic" => anthropic_reply(&lot_reply(names), Some("end_turn")),
        _ => openai_reply(&lot_reply(names), Some("stop")),
    };
    vec![trunc(p), ok(&["A", "B"]), ok(&["C", "D"])]
}

#[test]
fn req096_ac1_lot_tronque_deux_moities_enrichies() {
    for p in ["openai", "anthropic"] {
        let s = Setup::new(p, split_replies(p), "batch_files = 4\n");
        four_docs(&s);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        let posts = s.posts();
        assert_eq!(posts.len(), 3, "{p} {}", err(&o));
        assert_eq!(files_in(&posts[0]), ["A", "B", "C", "D"], "{p}");
        assert_eq!(files_in(&posts[1]), ["A", "B"], "{p}");
        assert_eq!(files_in(&posts[2]), ["C", "D"], "{p}");
        for n in ["A", "B", "C", "D"] {
            assert_eq!(
                s.summary_of(&format!("type:{n}")),
                Some(format!("Classe {n} centrale.")),
                "{p}"
            );
        }
        assert!(!err(&o).contains("avertissement:"), "{p} {}", err(&o));
    }
}

#[test]
fn req096_ac2_tout_tronque_requetes_et_lignes_a5() {
    let s = Setup::new("anthropic", many("anthropic", 30), "batch_files = 2\n");
    s.write("A.java", "public class A {}\n");
    s.write("B.java", "public class B {}\n");
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    let posts = s.posts();
    assert_eq!(posts.len(), 3, "{}", err(&o));
    assert_eq!(files_in(&posts[0]), ["A", "B"]);
    assert_eq!(files_in(&posts[1]), ["A"]);
    assert_eq!(files_in(&posts[2]), ["B"]);
    let warns: Vec<String> = err(&o)
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect();
    assert_eq!(warns, [a5("A.java"), a5("B.java")], "{}", err(&o));

    let s = Setup::new("anthropic", many("anthropic", 30), "batch_files = 3\n");
    for n in ["A", "B", "C"] {
        s.write(&format!("{n}.java"), &format!("public class {n} {{}}\n"));
    }
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    let posts = s.posts();
    assert_eq!(posts.len(), 5, "{}", err(&o));
    let seen: Vec<Vec<&str>> = posts.iter().map(|b| files_in(b)).collect();
    assert_eq!(
        seen,
        [
            vec!["A", "B", "C"],
            vec!["A", "B"],
            vec!["A"],
            vec!["B"],
            vec!["C"]
        ]
    );
}

#[test]
fn req096_ac3_cle_refusee_apres_troncature_arrete_l_envoi() {
    let s = Setup::with_replies(
        "anthropic",
        vec![(200, trunc("anthropic")), (401, "{}".into())],
        "batch_files = 2\n",
    );
    four_docs(&s);
    let o = s.run(&["build"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_eq!(s.posts().len(), 2, "{}", err(&o));
    let errs: Vec<String> = err(&o)
        .lines()
        .filter(|l| l.starts_with("erreur:"))
        .map(String::from)
        .collect();
    assert_eq!(
        errs,
        ["erreur: clé d'API refusée par anthropic (HTTP 401) ; vérifiez la variable désignée par api_key_env"],
        "{}",
        err(&o)
    );
    assert_eq!(s.to_enrich(), 4);
}

/// Lignes `trace:` de l'enrichissement, sans l'horodatage, à partir de `llm enrich`.
fn llm_lines(e: &str) -> Vec<String> {
    trace_lines_of(e)
        .into_iter()
        .filter_map(|l| l.split_once("s] ").map(|(_, r)| r.to_string()))
        .filter(|l| l.starts_with("llm enrich") || l.starts_with("politique:"))
        .collect()
}

#[test]
fn req096_ac5_ac6_trace_verbose_de_la_decoupe() {
    let s = Setup::new("anthropic", split_replies("anthropic"), "batch_files = 4\n");
    four_docs(&s);
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    assert!(o.status.success(), "{e}");
    let lines = llm_lines(&e);
    let pos = lines
        .iter()
        .position(|l| l.starts_with("llm enrich fin lot A.java +3 [1/1] tronquée "))
        .unwrap_or_else(|| panic!("{e}"));
    assert_eq!(
        lines[pos + 1],
        "llm enrich découpe lot A.java +3 [1/1] en lot A.java +1 et lot C.java +1",
        "{e}"
    );
    assert!(
        lines[pos + 2].starts_with("llm enrich départ lot A.java +1 [1/1] "),
        "{e}"
    );
    assert!(
        lines[pos + 3].starts_with("llm enrich fin lot A.java +1 [1/1] ok"),
        "{e}"
    );
    assert!(
        lines[pos + 4].starts_with("llm enrich départ lot C.java +1 [1/1] "),
        "{e}"
    );
    assert!(!lines.iter().any(|l| l.starts_with("politique:")), "{e}");

    // Sans --verbose : aucune ligne ne parle de découpe.
    let s = Setup::new("anthropic", split_replies("anthropic"), "batch_files = 4\n");
    four_docs(&s);
    let o = s.run(&["build"]);
    assert!(!err(&o).contains("découpe"), "{}", err(&o));

    // Second scénario d'AC-096-2 : deux lignes de découpe, départs des fichiers seuls.
    let s = Setup::new("anthropic", many("anthropic", 30), "batch_files = 3\n");
    for n in ["A", "B", "C"] {
        s.write(&format!("{n}.java"), &format!("public class {n} {{}}\n"));
    }
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    let cuts: Vec<String> = llm_lines(&e)
        .into_iter()
        .filter(|l| l.contains("découpe"))
        .collect();
    assert_eq!(
        cuts,
        [
            "llm enrich découpe lot A.java +2 [1/1] en lot A.java +1 et C.java",
            "llm enrich découpe lot A.java +1 [1/1] en A.java et B.java"
        ],
        "{e}"
    );
    for n in ["A.java", "B.java", "C.java"] {
        assert!(
            llm_lines(&e)
                .iter()
                .any(|l| l.starts_with(&format!("llm enrich départ {n} [1/1] "))),
            "{n} {e}"
        );
    }
}

#[test]
fn req096_ac5_progression_enrichissement_complet() {
    let s = Setup::new("anthropic", split_replies("anthropic"), "batch_files = 4\n");
    four_docs(&s);
    let o = s.run(&["build", "--show-progress"]);
    let e = err(&o);
    assert!(o.status.success(), "{e}");
    assert!(!e.contains("avertissement:"), "{e}");
    let pcts: Vec<u32> = e
        .lines()
        .filter_map(|l| l.strip_prefix("progression: enrichissement "))
        .map(|l| l.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(pcts.windows(2).all(|w| w[0] < w[1]), "{e}");
    assert!(
        e.lines()
            .any(|l| l == "progression: enrichissement 100 % (4/4 fichiers)"),
        "{e}"
    );
}

// ---------------------------------------------------------------------------------------------
// REQ-230 : un appel tronqué d'un fichier envoyé seul est découpé (liste de nœuds).

/// Corps de la requête de l'utilisateur (texte du message), quel que soit le fournisseur.
fn user_text(body: &str) -> String {
    fn find(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::String(s) if s.contains("Nœuds du fichier") => Some(s.clone()),
            serde_json::Value::Array(a) => a.iter().find_map(find),
            serde_json::Value::Object(o) => o.values().find_map(find),
            _ => None,
        }
    }
    find(&serde_json::from_str(body).unwrap()).expect("message utilisateur")
}

/// Identifiants de la liste « Nœuds du fichier » d'une requête.
fn listed(body: &str) -> Vec<String> {
    let t = user_text(body);
    let list = t.split_once("Nœuds du fichier :").unwrap().1;
    list.lines()
        .filter_map(|l| {
            l.strip_prefix('[')
                .and_then(|r| r.split_once(" — "))
                .map(|(_, id)| id)
        })
        .map(String::from)
        .collect()
}

/// Partie « Contenu » d'une requête.
fn content_of(body: &str) -> String {
    let t = user_text(body);
    let rest = t
        .split_once("<file path=")
        .unwrap()
        .1
        .split_once(">\n")
        .unwrap()
        .1;
    rest.split_once("\n</file>\n\nNœuds du fichier :")
        .unwrap()
        .0
        .to_string()
}

fn ok_for(provider: &str, summaries: &[(&str, &str)]) -> String {
    let list: Vec<serde_json::Value> = summaries
        .iter()
        .map(|(n, t)| serde_json::json!({"node": n, "summary": t}))
        .collect();
    let text = serde_json::json!({"version": 2, "summaries": list, "concepts": []}).to_string();
    match provider {
        "anthropic" => anthropic_reply(&text, Some("end_turn")),
        _ => openai_reply(&text, Some("stop")),
    }
}

/// `A.java` de 8 méthodes : 9 nœuds avec le type.
fn nine_nodes() -> String {
    let mut t = String::from("public class A {\n");
    for i in 0..8 {
        t.push_str(&format!("    void m{i}() {{}}\n"));
    }
    t.push_str("}\n");
    t
}

const FIRST5: [&str; 5] = [
    "method:A.m0",
    "method:A.m1",
    "method:A.m2",
    "method:A.m3",
    "method:A.m4",
];
const LAST4: [&str; 4] = ["method:A.m5", "method:A.m6", "method:A.m7", "type:A"];

#[test]
fn req230_ac1_liste_coupee_en_deux_sous_appels() {
    for p in ["anthropic", "openai"] {
        let s = Setup::new(
            p,
            vec![
                trunc(p),
                ok_for(
                    p,
                    &[("method:A.m0", "Premier."), ("method:A.m4", "Cinquième.")],
                ),
                ok_for(
                    p,
                    &[
                        ("method:A.m5", "Sixième."),
                        ("type:A", "Classe A centrale."),
                    ],
                ),
            ],
            "",
        );
        s.write("A.java", &nine_nodes());
        let o = s.run(&["build"]);
        let posts = s.posts();
        assert_eq!(posts.len(), 3, "{p} {}", err(&o));
        assert_eq!(listed(&posts[1]), FIRST5, "{p}");
        assert_eq!(listed(&posts[2]), LAST4, "{p}");
        assert_eq!(listed(&posts[0]).len(), 9, "{p}");
        assert_eq!(content_of(&posts[1]), content_of(&posts[0]), "{p}");
        assert_eq!(content_of(&posts[2]), content_of(&posts[0]), "{p}");
        assert_eq!(
            s.summary_of("method:A.m0").as_deref(),
            Some("Premier."),
            "{p}"
        );
        assert_eq!(
            s.summary_of("method:A.m4").as_deref(),
            Some("Cinquième."),
            "{p}"
        );
        assert_eq!(
            s.summary_of("method:A.m5").as_deref(),
            Some("Sixième."),
            "{p}"
        );
        assert_eq!(
            s.summary_of("type:A").as_deref(),
            Some("Classe A centrale."),
            "{p} {}",
            err(&o)
        );
        assert!(o.status.success(), "{p} {}", err(&o));
        assert!(!err(&o).contains("avertissement:"), "{p} {}", err(&o));
        assert!(
            without_coverage(&out(&o)).ends_with(", 0 fichiers à enrichir"),
            "{p} {}",
            out(&o)
        );
    }
}

/// Config, résumés du morceau 2 (premier et dernier nœud de sa liste triée) du gros fichier.
fn ac2_setup() -> (String, String, [(String, String); 2]) {
    let text = big_text();
    let max = text.len() / 3 + 40;
    let chunks = nephila::chunk::split(&text, max);
    assert_eq!(chunks.len(), 3);
    let mut ids: Vec<String> = chunks[1]
        .split("void m")
        .skip(1)
        .map(|r| format!("method:A.m{}", r.split('(').next().unwrap()))
        .collect();
    ids.sort();
    let first = (ids[0].clone(), "Résumé du premier nœud.".to_string());
    let last = (
        ids[ids.len() - 1].clone(),
        "Résumé du dernier nœud.".to_string(),
    );
    (text, format!("max_chunk_chars = {max}\n"), [first, last])
}

fn ac2_replies(sub_calls: bool, sums: &[(String, String); 2]) -> Vec<String> {
    let p = "openai";
    let none = || ok_for(p, &[]);
    let one = |x: &(String, String)| ok_for(p, &[(x.0.as_str(), x.1.as_str())]);
    if sub_calls {
        vec![none(), trunc(p), one(&sums[0]), one(&sums[1]), none()]
    } else {
        let both = ok_for(
            p,
            &[
                (sums[0].0.as_str(), sums[0].1.as_str()),
                (sums[1].0.as_str(), sums[1].1.as_str()),
            ],
        );
        vec![none(), both, none()]
    }
}

#[test]
fn req230_ac2_gros_fichier_morceau_2_tronque_ses_deux_sous_appels() {
    let (text, cfg, sums) = ac2_setup();
    let run = |sub_calls: bool| {
        let s = Setup::new("openai", ac2_replies(sub_calls, &sums), &cfg);
        s.write("A.java", &text);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{}", err(&o));
        assert_eq!(s.to_enrich(), 0);
        s
    };
    let s = run(true);
    let posts = s.posts();
    assert_eq!(posts.len(), 5);
    let tag = |b: &str| {
        let t = user_text(b);
        t.split("(extrait ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(
        posts.iter().map(|b| tag(b)).collect::<Vec<_>>(),
        ["1/3", "2/3", "2/3", "2/3", "3/3"]
    );
    let (a, b) = (listed(&posts[2]), listed(&posts[3]));
    let parent = listed(&posts[1]);
    assert_eq!([a.clone(), b.clone()].concat(), parent);
    assert_eq!(a.len(), parent.len().div_ceil(2));
    // Témoin : même fichier, le morceau 2 répond en une fois avec les mêmes résumés.
    let w = run(false);
    assert_eq!(w.posts().len(), 3);
    let graph = |s: &Setup| -> Vec<(String, Option<String>)> {
        let mut v: Vec<_> = nephila::store::Store::open_readonly(&s.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .map(|n| (n.id, n.summary))
            .collect();
        v.sort();
        v
    };
    for (id, text) in &sums {
        assert_eq!(s.summary_of(id).as_deref(), Some(text.as_str()), "{id}");
    }
    assert_eq!(graph(&s), graph(&w));
}

#[test]
fn req230_ac9_progression_une_seule_fois_malgre_la_decoupe() {
    let (text, cfg, sums) = ac2_setup();
    let s = Setup::new("openai", ac2_replies(true, &sums), &cfg);
    s.write("A.java", &text);
    let o = s.run(&["build", "--show-progress"]);
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    let lines: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    assert_eq!(
        lines,
        [
            "progression: enrichissement 0 % (0/1 fichiers)",
            "progression: enrichissement 100 % (1/1 fichiers)"
        ],
        "{e}"
    );
}

#[test]
fn req230_ac5_tout_tronque_quatre_requetes_une_seule_ligne_a5() {
    let p = "openai";
    let mut t = String::from("public class A {\n");
    for i in 0..15 {
        t.push_str(&format!("    void m{i}() {{}}\n"));
    }
    t.push_str("}\n");
    let s = Setup::new(p, many(p, 10), "");
    s.write("A.java", &t);
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    let posts = s.posts();
    assert_eq!(posts.len(), 4, "{}", err(&o));
    let sizes: Vec<usize> = posts.iter().map(|b| listed(b).len()).collect();
    assert_eq!(sizes, [16, 8, 4, 2]);
    assert_eq!(err(&o).matches("avertissement:").count(), 1, "{}", err(&o));
    assert!(err(&o).contains(&a5("A.java")), "{}", err(&o));
    assert!(out(&o).contains("1 fichiers à enrichir"), "{}", out(&o));
    assert_eq!(s.to_enrich(), 1);
    let o = s.run(&["update"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 8, "{}", err(&o));
    assert_eq!(err(&o).matches("avertissement:").count(), 1, "{}", err(&o));
    assert!(err(&o).contains(&a5("A.java")), "{}", err(&o));
}

#[test]
fn req230_ac5_un_noeud_sans_coupe_une_seule_requete() {
    // Un nœud, texte sous 1 000 caractères : pas de coupe possible (T-318 ajoute le texte).
    let s = Setup::new("openai", many("openai", 3), "");
    s.write("A.java", A_JAVA);
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 1, "{}", err(&o));
    assert!(err(&o).contains(&a5("A.java")), "{}", err(&o));
}

fn kept_calls(s: &Setup) -> usize {
    nephila::store::Store::open_readonly(&s.repo)
        .unwrap()
        .call_results()
        .unwrap()
        .len()
}

#[test]
fn req230_ac7_max_calls_compte_les_sous_appels() {
    let p = "openai";
    let ok = || ok_for(p, &[("method:A.m0", "Premier résumé utile.")]);
    // 3 appels permis : l'appel tronqué et ses 2 sous-appels.
    let s = Setup::new(p, vec![trunc(p), ok(), ok()], "");
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build", "--max-calls", "3"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.posts().len(), 3, "{}", err(&o));
    assert_eq!(s.to_enrich(), 0);
    assert!(!err(&o).contains("plafond"), "{}", err(&o));
    assert!(!err(&o).contains("avertissement:"), "{}", err(&o));

    // 2 appels permis : la réservation des 2 sous-appels est refusée, le fichier est laissé.
    let s = Setup::new(p, vec![trunc(p), ok(), ok()], "");
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build", "--max-calls", "2", "--verbose"]);
    let e = err(&o);
    assert!(o.status.success(), "{e}");
    assert_eq!(s.posts().len(), 1, "{e}");
    assert!(!e.contains(&a5("A.java")), "{e}");
    assert!(
        e.contains("avertissement: plafond de 2 appels atteint : 1 fichiers laissés à enrichir"),
        "{e}"
    );
    assert_eq!(
        e.matches("politique: arrêt de l'envoi (plafond").count(),
        1,
        "{e}"
    );
    assert_eq!(s.to_enrich(), 1);
}

#[test]
fn req230_ac8_sous_appel_en_echec_ne_garde_pas_les_appels_du_sous_plan() {
    let p = "openai";
    let text = big_text();
    let cfg = format!(
        "max_chunk_chars = {}\nformat_retries = 0\n",
        text.len() / 2 + 40
    );
    let good = || ok_for(p, &[]);
    let bad = || openai_reply("pas du JSON", Some("stop"));
    // Plan de 2 appels : appel 1 conforme, appel 2 tronqué, sous-appel 1 conforme, sous-appel 2 non conforme.
    let s = Setup::new(p, vec![good(), trunc(p), good(), bad(), good()], &cfg);
    s.write("A.java", &text);
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 4, "{}", err(&o));
    assert_eq!(err(&o).matches("avertissement:").count(), 1, "{}", err(&o));
    assert!(err(&o).contains("non conforme"), "{}", err(&o));
    assert_eq!(kept_calls(&s), 1, "seul l'appel 1 est conservé");
    // `update` : l'appel 2 répond en entier, l'appel 1 est relu de la table.
    let o = s.run(&["update"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.posts().len(), 5, "{}", err(&o));
    assert_eq!(s.to_enrich(), 0);
    assert_eq!(kept_calls(&s), 0);

    // Plan d'un seul appel : même échec, table vide.
    let s = Setup::new(p, vec![trunc(p), good(), bad()], "format_retries = 0\n");
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 3, "{}", err(&o));
    assert_eq!(kept_calls(&s), 0);
}

/// Classe Java à un seul nœud (`type:A`), sur `n` lignes de commentaire.
fn one_node_text(n: usize) -> String {
    let mut t = String::from("public class A {\n");
    for i in 0..n {
        t.push_str(&format!(
            "    // commentaire numéro {i:04} qui remplit la ligne\n"
        ));
    }
    t.push_str("}\n");
    t
}

fn extract_of(body: &str) -> String {
    let t = user_text(body);
    let start = t.find("(extrait").expect("extrait");
    t[start..].split(')').next().unwrap().to_string() + ")"
}

#[test]
fn req230_ac3_un_noeud_texte_coupe() {
    let p = "openai";
    let s = Setup::new(
        p,
        vec![trunc(p), ok_for(p, &[("type:A", "Classe A coupée.")])],
        "",
    );
    let text = one_node_text(30);
    // L'en-tête répété (déclaration du type) réduit le budget des morceaux suivants : 3 morceaux.
    let spans = nephila::chunk::split_spans_for("A.java", &text, text.chars().count().div_ceil(2));
    let total = spans.len();
    assert!(text.chars().count() >= 1000);
    assert_eq!(total, 3);
    s.write("A.java", &text);
    let o = s.run(&["build"]);
    let posts = s.posts();
    assert_eq!(posts.len(), 2, "{}", err(&o));
    assert_eq!(listed(&posts[1]), ["type:A"]);
    let (first, second) = (content_of(&posts[0]), content_of(&posts[1]));
    assert!(second.len() < first.len(), "texte plus court");
    let lines = second.trim_matches('\n').lines().count();
    assert_eq!(
        extract_of(&posts[1]),
        format!("(extrait 1/{total}, lignes 1–{lines})")
    );
    assert_eq!(s.summary_of("type:A").as_deref(), Some("Classe A coupée."));
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.to_enrich(), 0);
}

#[test]
fn req230_ac4_markdown_texte_coupe_et_liste_coupee_sous_mille() {
    let p = "openai";
    let md_reply = |sec: &str, concept: &str| {
        let text =
            serde_json::json!({"version": 2, "summaries": [{"node": sec, "summary": "Résumé."}],
            "concepts": [{"name": concept, "summary": "Un concept.", "sections": [sec]}]})
            .to_string();
        openai_reply(&text, Some("stop"))
    };
    let body = |t: &str, n: usize| {
        format!(
            "# {t}\n\n{}\n",
            "Du texte de section qui remplit les lignes.\n".repeat(n)
        )
    };
    // Alpha + Beta et Gamma font exactement la moitié du texte chacun : 2 morceaux.
    let doc = format!(
        "{}{}{}{}",
        body("Alpha", 6),
        body("Beta", 6),
        body("Gamma", 12),
        "x".repeat(9)
    );
    let half = doc.chars().count().div_ceil(2);
    assert!(doc.chars().count() >= 1000);
    assert_eq!(nephila::chunk::split_spans_for("D.md", &doc, half).len(), 2);
    let s = Setup::new(
        p,
        vec![
            trunc(p),
            md_reply("section:D.md#alpha", "Concept commun"),
            md_reply("section:D.md#gamma", "Concept commun"),
        ],
        "",
    );
    s.write("D.md", &doc);
    let o = s.run(&["build"]);
    let posts = s.posts();
    assert_eq!(posts.len(), 3, "{}", err(&o));
    let all = listed(&posts[0]);
    assert_eq!(all.len(), 4, "{all:?}");
    let mut seen: Vec<String> = posts[1..].iter().flat_map(|b| listed(b)).collect();
    seen.sort();
    let mut want = all.clone();
    want.sort();
    assert_eq!(seen, want, "chaque section une seule fois");
    for b in &posts[1..] {
        let c = content_of(b);
        assert!(c.len() < content_of(&posts[0]).len());
        for id in listed(b).into_iter().filter(|i| i.starts_with("section:")) {
            let title = ["Alpha", "Beta", "Gamma"]
                .iter()
                .find(|t| id.to_lowercase().ends_with(&t.to_lowercase()))
                .unwrap();
            assert!(c.contains(&format!("# {title}")), "{id} dans sa part");
        }
    }
    assert!(o.status.success(), "{}", err(&o));
    // Concepts fusionnés par nom : un seul nœud, cité depuis les sections des deux parts.
    let store = nephila::store::Store::open_readonly(&s.repo).unwrap();
    let names: Vec<String> = store
        .nodes()
        .unwrap()
        .into_iter()
        .filter(|n| n.id.starts_with("concept:"))
        .map(|n| n.id)
        .collect();
    assert_eq!(names, ["concept:concept commun"]);
    let edges = store.edges().unwrap();
    for sec in ["section:D.md#alpha", "section:D.md#gamma"] {
        assert!(
            edges.iter().any(|e| e.source == sec
                && e.target == "concept:concept commun"
                && e.kind == nephila::model::EdgeKind::Mentions),
            "{sec} cite le concept"
        );
    }

    // Sous 1 000 caractères : la liste est coupée, même texte dans les deux sous-appels.
    let small = "# Alpha\n\nun\n\n# Beta\n\ndeux\n\n# Gamma\n\ntrois\n";
    let s = Setup::new(
        p,
        vec![
            trunc(p),
            md_reply("section:D.md#alpha", "c1"),
            md_reply("section:D.md#gamma", "c2"),
        ],
        "",
    );
    s.write("D.md", small);
    let o = s.run(&["build"]);
    let posts = s.posts();
    assert_eq!(posts.len(), 3, "{}", err(&o));
    assert_eq!(content_of(&posts[1]), content_of(&posts[0]));
    assert_eq!(content_of(&posts[2]), content_of(&posts[0]));
    assert_eq!(listed(&posts[1]).len() + listed(&posts[2]).len(), 4);
}

#[test]
fn req230_ac5_un_noeud_sur_une_seule_ligne_une_requete() {
    let s = Setup::new("openai", many("openai", 3), "");
    s.write(
        "A.java",
        &format!("public class A {{ /* {} */ }}\n", "x".repeat(3000)),
    );
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 1, "{}", err(&o));
    assert!(err(&o).contains(&a5("A.java")), "{}", err(&o));
}

// ---------------------------------------------------------------------------------------------
// REQ-231 : traces de la découpe d'un appel (design.md V-005).

/// Ajoute un relevé de jetons à une réponse du faux serveur.
fn with_tokens(provider: &str, reply: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(reply).unwrap();
    v["usage"] = if provider == "anthropic" {
        serde_json::json!({"input_tokens": 10, "output_tokens": 5})
    } else {
        serde_json::json!({"prompt_tokens": 10, "completion_tokens": 5})
    };
    v.to_string()
}

fn ok_nine(p: &str) -> (String, String) {
    (
        ok_for(p, &[("method:A.m0", "Premier.")]),
        ok_for(p, &[("type:A", "Classe A centrale.")]),
    )
}

/// Lignes `llm enrich` ou `politique:` de la sortie d'erreur, sans leur préfixe de temps.
fn run_lines(s: &Setup, args: &[&str]) -> (Vec<String>, String) {
    let o = s.run(args);
    let e = err(&o);
    (llm_lines(&e), e)
}

fn position_of(lines: &[String], prefix: &str, e: &str) -> usize {
    lines
        .iter()
        .position(|l| l.starts_with(prefix))
        .unwrap_or_else(|| panic!("{prefix} absent : {e}"))
}

#[test]
fn req231_ac1_ligne_de_decoupe_puis_sous_appels_marques() {
    for p in ["anthropic", "openai"] {
        let (a, b) = ok_nine(p);
        let s = Setup::new(p, vec![trunc(p), a, b], "");
        s.write("A.java", &nine_nodes());
        let (lines, e) = run_lines(&s, &["build", "--verbose"]);
        let i = position_of(&lines, "llm enrich fin A.java [1/1] tronquée ", &e);
        assert_eq!(
            lines[i + 1],
            "llm enrich découpe A.java [1/1] en 2 sous-appels",
            "{p} {e}"
        );
        let starts = ["départ", "fin", "départ", "fin"];
        for (k, kind) in starts.iter().enumerate() {
            let l = &lines[i + 2 + k];
            assert!(
                l.starts_with(&format!("llm enrich {kind} A.java [1/1] ")),
                "{p} {l}"
            );
            assert!(
                l.ends_with(&format!("(sous-appel {})", k / 2 + 1)),
                "{p} {l}"
            );
        }
        assert_eq!(lines.len(), i + 6, "{p} {e}");
        assert!(
            e.lines().any(|l| l.contains("] appels: 3 appels")),
            "{p} {e}"
        );
        assert!(!e.contains("politique:"), "{p} {e}");
        for l in &lines {
            assert!(
                !l.contains("groupe") && !l.contains("nœuds demandés"),
                "{l}"
            );
            assert!(!l.contains("(essai"), "{l}");
        }
    }
}

#[test]
fn req231_ac2_sans_verbose_rien_n_est_dit() {
    let p = "openai";
    let (a, b) = ok_nine(p);
    let s = Setup::new(p, vec![trunc(p), a, b], "");
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(err(&o), "", "stderr vide");
    assert_eq!(out(&o).trim_end().lines().count(), 1, "{}", out(&o));
    assert!(!out(&o).contains("sous-appel") && !out(&o).contains("découpe"));

    let (a, b) = ok_nine(p);
    let s = Setup::new(p, vec![trunc(p), a, b], "");
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build", "--show-progress"]);
    let e = err(&o);
    let lines: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    assert_eq!(
        lines.last().copied(),
        Some("progression: enrichissement 100 % (1/1 fichiers)"),
        "{e}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains("100 %")).count(),
        1,
        "{e}"
    );
}

/// `trace: [<s>.<mmm>s] <reste>` : renvoie le reste si le temps est de la forme attendue.
fn after_clock(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("trace: [")?;
    let (clock, rest) = rest.split_once("s] ")?;
    let (s, ms) = clock.split_once('.')?;
    (!s.is_empty()
        && s.bytes().all(|c| c.is_ascii_digit())
        && ms.len() == 3
        && ms.bytes().all(|c| c.is_ascii_digit()))
    .then_some(rest)
}

#[test]
fn req231_ac4_forme_exacte_et_marque_apres_les_jetons() {
    let p = "openai";
    let (a, b) = ok_nine(p);
    let replies = vec![
        with_tokens(p, &trunc(p)),
        with_tokens(p, &a),
        with_tokens(p, &b),
    ];
    let s = Setup::new(p, replies, "");
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    let traces = trace_lines_of(&e);
    let i = traces
        .iter()
        .position(|l| l.contains("llm enrich fin A.java [1/1] tronquée"))
        .unwrap_or_else(|| panic!("{e}"));
    assert!(!traces[i].contains("sous-appel"), "{}", traces[i]);
    assert_eq!(
        after_clock(traces[i + 1]),
        Some("llm enrich découpe A.java [1/1] en 2 sous-appels"),
        "{e}"
    );
    let ends = [
        " car. (sous-appel 1)",
        ") (sous-appel 1)",
        " car. (sous-appel 2)",
        ") (sous-appel 2)",
    ];
    for (k, end) in ends.iter().enumerate() {
        assert!(traces[i + 2 + k].ends_with(end), "{}", traces[i + 2 + k]);
    }
    assert!(traces[i + 3].contains("(jetons : "), "{}", traces[i + 3]);
    for l in e.lines() {
        if l.starts_with("avertissement:") || l.starts_with("erreur:") {
            assert!(!l.contains("sous-appel"), "{l}");
        }
    }
}

#[test]
fn req231_ac5_chemins_de_rang_a_chaque_profondeur() {
    let p = "openai";
    let mut t = String::from("public class A {\n");
    for i in 0..15 {
        t.push_str(&format!("    void m{i}() {{}}\n"));
    }
    t.push_str("}\n");
    let s = Setup::new(p, many(p, 10), "");
    s.write("A.java", &t);
    let (lines, e) = run_lines(&s, &["build", "--verbose"]);
    let cuts: Vec<&String> = lines.iter().filter(|l| l.contains("découpe")).collect();
    assert_eq!(cuts.len(), 3, "{e}");
    assert!(cuts[0].ends_with("en 2 sous-appels"), "{}", cuts[0]);
    assert!(cuts[1].ends_with("en 2 sous-appels (sous-appel 1)"));
    assert!(cuts[2].ends_with("en 2 sous-appels (sous-appel 1.1)"));
    let last_start = lines.iter().rposition(|l| l.contains("départ")).unwrap();
    assert!(lines[last_start].ends_with("(sous-appel 1.1.1)"), "{e}");
    assert!(lines[last_start + 1].starts_with("llm enrich fin "), "{e}");
    assert!(lines[last_start + 1].ends_with("(sous-appel 1.1.1)"), "{e}");
    assert_eq!(
        lines.len(),
        last_start + 2,
        "aucune ligne après la dernière fin : {e}"
    );
    let a5_at = e.find(&a5("A.java")).unwrap_or_else(|| panic!("{e}"));
    let last_fin = e.rfind("llm enrich fin A.java").unwrap();
    assert!(a5_at > last_fin, "A-5 après la dernière fin : {e}");
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("(sous-appel 2)") || l.contains("(sous-appel 1.2)")),
        "{e}"
    );
    assert!(lines.iter().all(|l| l.contains("[1/1]")), "{e}");

    // Un seul nœud, texte coupé : « en 1 sous-appels », puis le sous-appel 1.
    let s = Setup::new(
        p,
        vec![trunc(p), ok_for(p, &[("type:A", "Classe A coupée.")])],
        "",
    );
    s.write("A.java", &one_node_text(30));
    let (lines, e) = run_lines(&s, &["build", "--verbose"]);
    let i = position_of(
        &lines,
        "llm enrich découpe A.java [1/1] en 1 sous-appels",
        &e,
    );
    assert!(lines[i + 1].ends_with("(sous-appel 1)"), "{e}");
    assert!(lines[i + 1].starts_with("llm enrich départ"), "{e}");
}

#[test]
fn req231_ac6_essais_et_attente_gardent_la_marque_en_fin() {
    let p = "openai";
    let (a, b) = ok_nine(p);
    // Premier sous-appel non conforme, puis conforme.
    let bad = openai_reply("pas du JSON", Some("stop"));
    let s = Setup::new(p, vec![trunc(p), bad, a.clone(), b.clone()], "");
    s.write("A.java", &nine_nodes());
    let (lines, e) = run_lines(&s, &["build", "--verbose"]);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("llm enrich départ A.java [1/1] ")
                && l.ends_with(" car. (essai 2/3) (sous-appel 1)")),
        "{e}"
    );

    // Premier sous-appel servi en HTTP 503, puis conforme.
    let s = Setup::with_replies(
        p,
        vec![(200, trunc(p)), (503, String::new()), (200, a), (200, b)],
        "",
    );
    s.write("A.java", &nine_nodes());
    let o = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&s.repo)
        .args(["build", "--verbose"])
        .env("NEPHILA_CONFIG", &s.config)
        .env("NEPHILA_T100_KEY", "sk-test")
        .env("NEPHILA_RETRY_BASE_MS", "1")
        .output()
        .unwrap();
    let e = err(&o);
    let waits: Vec<&str> = e
        .lines()
        .filter(|l| l.contains("llm enrich attente"))
        .collect();
    assert_eq!(waits.len(), 1, "{e}");
    assert!(
        waits[0].ends_with(" (nouvel essai 1/4) (sous-appel 1)"),
        "{}",
        waits[0]
    );

    // Plafond de 2 appels : pas de découpe, donc aucune marque.
    let (a, b) = ok_nine(p);
    let s = Setup::new(p, vec![trunc(p), a, b], "");
    s.write("A.java", &nine_nodes());
    let (_, e) = run_lines(&s, &["build", "--verbose", "--max-calls", "2"]);
    assert!(!e.contains("découpe") && !e.contains("sous-appel"), "{e}");

    // Cas sans coupe (un nœud, texte court) et sans troncature : aucune marque.
    let s = Setup::new(p, many(p, 3), "");
    s.write("A.java", A_JAVA);
    let (_, e) = run_lines(&s, &["build", "--verbose"]);
    assert!(!e.contains("découpe") && !e.contains("sous-appel"), "{e}");
    let s = Setup::new(p, many(p, 3), "");
    s.write(
        "A.java",
        &format!("public class A {{ /* {} */ }}\n", "x".repeat(3000)),
    );
    let (_, e) = run_lines(&s, &["build", "--verbose"]);
    assert!(!e.contains("découpe") && !e.contains("sous-appel"), "{e}");
    let s = Setup::new(p, vec![ok_for(p, &[("type:A", "Classe A.")])], "");
    s.write("A.java", A_JAVA);
    let (_, e) = run_lines(&s, &["build", "--verbose"]);
    assert!(!e.contains("sous-appel"), "{e}");
}

#[test]
fn req232_ac17_ask_tronquee_au_texte_conforme_reste_refusee() {
    const ASK_OK: &str = r#"{"found":true,"answer":"réponse complète","nodes":["Alpha"]}"#;
    for p in ["anthropic", "openai"] {
        let reply = match p {
            "anthropic" => anthropic_reply(ASK_OK, Some("max_tokens")),
            _ => openai_reply(ASK_OK, Some("length")),
        };
        let s = Setup::new(p, vec![reply; 10], "");
        s.write("A.java", "public class Alpha {}\n");
        let o = s.run(&["build", "--no-llm"]);
        assert!(o.status.success(), "{}", err(&o));
        let o = s.ask(&[]);
        assert!(!o.status.success(), "{p}");
        assert_eq!(s.posts().len(), 1, "{p}");
        assert!(out(&o).is_empty(), "{p} {}", out(&o));
        assert_eq!(err(&o), format!("{E11}\n"), "{p}");
    }
}

// ---- REQ-232 : lot tronqué, entrées complètes gardées, seul le reste repart ----

/// Réponse de lot tronquée : les entrées complètes de `complete`, puis `cut` (début d'entrée).
fn lot_partial_text(complete: &[&str], cut: &str) -> String {
    let entries: Vec<String> = complete
        .iter()
        .map(|n| {
            serde_json::json!({
                "file": format!("{n}.java"),
                "summaries": [{"node": format!("type:{n}"), "summary": format!("Classe {n} centrale.")}],
                "concepts": []
            })
            .to_string()
        })
        .collect();
    let sep = if entries.is_empty() { "" } else { "," };
    format!(
        r#"{{"version": 3, "files": [{}{sep}{cut}"#,
        entries.join(",")
    )
}

fn trunc_text(p: &str, text: &str) -> String {
    match p {
        "anthropic" => anthropic_reply(text, Some("max_tokens")),
        _ => openai_reply(text, Some("length")),
    }
}

fn ok_text(p: &str, text: &str) -> String {
    match p {
        "anthropic" => anthropic_reply(text, Some("end_turn")),
        _ => openai_reply(text, Some("stop")),
    }
}

const CUT_C: &str = r#"{"file": "C.java", "summaries": [{"node": "type:C", "summ"#;

fn assert_all_summaries(s: &Setup, p: &str) {
    for n in ["A", "B", "C", "D"] {
        assert_eq!(
            s.summary_of(&format!("type:{n}")),
            Some(format!("Classe {n} centrale.")),
            "{p} {n}"
        );
    }
}

#[test]
fn req232_ac3_entrees_a_et_b_gardees_seul_le_reste_repart() {
    for p in ["anthropic", "openai"] {
        let replies = vec![
            trunc_text(p, &lot_partial_text(&["A", "B"], CUT_C)),
            ok_text(p, &lot_reply(&["C", "D"])),
        ];
        let s = Setup::new(p, replies, "batch_files = 4\n");
        four_docs(&s);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        let posts = s.posts();
        assert_eq!(posts.len(), 2, "{p} {}", err(&o));
        assert_eq!(files_in(&posts[0]), ["A", "B", "C", "D"], "{p}");
        assert_eq!(files_in(&posts[1]), ["C", "D"], "{p}");
        assert!(!posts[1].contains("class A"), "{p}");
        assert_all_summaries(&s, p);
        assert!(!err(&o).contains("avertissement:"), "{p} {}", err(&o));
        assert!(
            without_coverage(&out(&o)).ends_with(", 0 fichiers à enrichir"),
            "{p} {}",
            out(&o)
        );
    }
}

#[test]
fn req232_ac4_trois_entrees_puis_d_seul_et_quatre_entrees_une_requete() {
    for p in ["anthropic", "openai"] {
        let replies = vec![
            trunc_text(
                p,
                &lot_partial_text(&["A", "B", "C"], r#"{"file": "D.java", "su"#),
            ),
            ok_for(p, &[("type:D", "Classe D centrale.")]),
        ];
        let s = Setup::new(p, replies, "batch_files = 4\n");
        four_docs(&s);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        let posts = s.posts();
        assert_eq!(posts.len(), 2, "{p} {}", err(&o));
        assert_eq!(files_in(&posts[1]), ["D"], "{p}");
        assert_all_summaries(&s, p);

        let replies = vec![trunc_text(
            p,
            &lot_partial_text(&["A", "B", "C", "D"], r#",{"file": "E"#),
        )];
        let s = Setup::new(p, replies, "batch_files = 4\n");
        four_docs(&s);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        assert_eq!(s.posts().len(), 1, "{p} {}", err(&o));
        assert_all_summaries(&s, p);
        assert_eq!(s.to_enrich(), 0, "{p}");
    }
}

#[test]
fn req232_ac5_sans_entree_utile_les_moities_comme_avant() {
    for p in ["anthropic", "openai"] {
        for first in [
            lot_partial_text(&[], r#"{"file": "A.java", "summaries": [{"node": "ty"#),
            lot_partial_text(&["E"], r#"{"file": "A.java", "su"#),
        ] {
            let replies = vec![
                trunc_text(p, &first),
                ok_text(p, &lot_reply(&["A", "B"])),
                ok_text(p, &lot_reply(&["C", "D"])),
            ];
            let s = Setup::new(p, replies, "batch_files = 4\n");
            four_docs(&s);
            let o = s.run(&["build"]);
            assert!(o.status.success(), "{p} {}", err(&o));
            let posts = s.posts();
            assert_eq!(posts.len(), 3, "{p} {}", err(&o));
            assert_eq!(files_in(&posts[1]), ["A", "B"], "{p}");
            assert_eq!(files_in(&posts[2]), ["C", "D"], "{p}");
            assert!(!err(&o).contains("écarté"), "{p} {}", err(&o));
            assert_all_summaries(&s, p);
        }
    }
}

#[test]
fn req232_ac6_toujours_tronque_lots_de_4_3_2_puis_d_seul() {
    for p in ["anthropic", "openai"] {
        let replies = vec![
            trunc_text(p, &lot_partial_text(&["A"], r#"{"file": "B.java", "su"#)),
            trunc_text(p, &lot_partial_text(&["B"], r#"{"file": "C.java", "su"#)),
            trunc_text(p, &lot_partial_text(&["C"], r#"{"file": "D.java", "su"#)),
            ok_for(p, &[("type:D", "Classe D centrale.")]),
        ];
        let s = Setup::new(p, replies, "batch_files = 4\n");
        four_docs(&s);
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        let posts = s.posts();
        let seen: Vec<Vec<&str>> = posts.iter().map(|b| files_in(b)).collect();
        assert_eq!(
            seen,
            [
                vec!["A", "B", "C", "D"],
                vec!["B", "C", "D"],
                vec!["C", "D"],
                vec!["D"]
            ],
            "{p}"
        );
        assert_all_summaries(&s, p);
        assert!(!err(&o).contains("avertissement:"), "{p} {}", err(&o));
    }
}

#[test]
fn req232_ac6_lot_de_second_passage_tronque_reste_de_second_passage() {
    let p = "anthropic";
    let replies = vec![
        // Passage 1 : seul A répond ; B, C, D repartent en second passage.
        ok_text(p, &lot_reply(&["A"])),
        // Second passage tronqué avec B complet : C, D repartent, toujours en second passage.
        trunc_text(p, &lot_partial_text(&["B"], r#"{"file": "C.java", "su"#)),
        // Réponse conforme sans D : D échoue sans autre renvoi.
        ok_text(p, &lot_reply(&["C"])),
    ];
    let s = Setup::new(p, replies, "batch_files = 4\n");
    four_docs(&s);
    let o = s.run(&["build"]);
    let posts = s.posts();
    assert_eq!(posts.len(), 3, "{}", err(&o));
    assert_eq!(files_in(&posts[2]), ["C", "D"]);
    assert_eq!(s.to_enrich_paths(), ["D.java"], "{}", err(&o));
}

#[test]
fn req232_ac18_marqueur_de_l_element_coupe_nulle_part() {
    for p in ["anthropic", "openai"] {
        let cut = format!(
            r#"{{"file": "B.java", "summaries": [{{"node": "type:B", "summary": "{SECRET}"#
        );
        let replies = vec![
            trunc_text(p, &lot_partial_text(&["A"], &cut)),
            ok_text(p, &lot_reply(&["B", "C", "D"])),
        ];
        let s = Setup::new(p, replies, "batch_files = 4\n");
        four_docs(&s);
        let o = s.run(&["build", "--verbose"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        assert!(
            !out(&o).contains(SECRET) && !err(&o).contains(SECRET),
            "{p}"
        );
        for n in ["A", "B", "C", "D"] {
            let sum = s.summary_of(&format!("type:{n}")).unwrap_or_default();
            assert!(!sum.contains(SECRET), "{p} {n}");
        }
        assert_all_summaries(&s, p);
        let g = s.run(&["export", "--output", "-"]);
        assert!(g.status.success(), "{p} {}", err(&g));
        assert!(!out(&g).contains(SECRET), "{p}");
    }
}

#[test]
fn req232_ac16_lot_recupere_pas_de_ligne_politique_et_compteur_non_remis_a_zero() {
    let p = "anthropic";
    let mut replies: Vec<fake_server::Reply> = Vec::new();
    // Deux lots en erreur réseau (connexion fermée, tous les essais).
    for _ in 0..10 {
        replies.push(fake_server::Reply::Close);
    }
    // Lot tronqué avec l'entrée de E complète : F repart seul.
    let partial = lot_partial_text(&["E"], r#"{"file": "F.java", "su"#);
    replies.push(fake_server::Reply::status(200, &trunc_text(p, &partial)));
    // F seul : erreur réseau, la troisième de la suite.
    for _ in 0..5 {
        replies.push(fake_server::Reply::Close);
    }
    let server = FakeServer::start_replies(replies);
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "provider = \"anthropic\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T100_KEY\"\nconcurrency = 1\nbatch_files = 2\n",
            server.base_url
        ),
    )
    .unwrap();
    for n in ["A", "B", "C", "D", "E", "F", "G", "H"] {
        fs::write(
            repo.join(format!("{n}.java")),
            format!("public class {n} {{}}\n"),
        )
        .unwrap();
    }
    let o = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&repo)
        .args(["build", "--verbose"])
        .env("NEPHILA_CONFIG", &config)
        .env("NEPHILA_T100_KEY", "sk-test")
        .env("NEPHILA_RETRY_BASE_MS", "1")
        .output()
        .unwrap();
    let e = err(&o);
    let politique: Vec<&str> = e
        .lines()
        .filter(|l| l.contains("politique:"))
        .map(|l| l.split_once("] ").map_or(l, |x| x.1))
        .collect();
    assert_eq!(politique.len(), 4, "{e}");
    for (l, k) in politique.iter().zip(1..=3) {
        assert_eq!(
            *l,
            format!("politique: erreurs réseau consécutives {k}/3"),
            "{e}"
        );
    }
    assert!(
        politique[3].starts_with("politique: arrêt de l'envoi"),
        "{e}"
    );
}

// ---- REQ-232 : appel tronqué, résumés reçus gardés, seul le reste repart ----

/// Réponse d'appel tronquée : les résumés complets de `complete`, puis `cut` (début d'élément).
fn call_partial(p: &str, complete: &[&str], cut: &str) -> String {
    let items: Vec<String> = complete
        .iter()
        .map(|n| serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")}).to_string())
        .collect();
    let sep = if items.is_empty() { "" } else { "," };
    trunc_text(
        p,
        &format!(
            r#"{{"version": 2, "summaries": [{}{sep}{cut}"#,
            items.join(",")
        ),
    )
}

const NINE: [&str; 9] = [
    "method:A.m0",
    "method:A.m1",
    "method:A.m2",
    "method:A.m3",
    "method:A.m4",
    "method:A.m5",
    "method:A.m6",
    "method:A.m7",
    "type:A",
];

/// Réponse conforme avec le résumé `Résumé de <nœud>.` de chacun des nœuds.
fn ok_nodes(p: &str, nodes: &[&str]) -> String {
    let owned: Vec<(String, String)> = nodes
        .iter()
        .map(|n| (n.to_string(), format!("Résumé de {n}.")))
        .collect();
    let refs: Vec<(&str, &str)> = owned
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    ok_for(p, &refs)
}

const CUT_M4: &str = r#"{"node": "method:A.m4", "summ"#;

fn graph_of(s: &Setup) -> Vec<(String, Option<String>)> {
    let mut v: Vec<_> = nephila::store::Store::open_readonly(&s.repo)
        .unwrap()
        .nodes()
        .unwrap()
        .into_iter()
        .map(|n| (n.id, n.summary))
        .collect();
    v.sort();
    v
}

#[test]
fn req232_ac7_quatre_resumes_gardes_le_reste_repart() {
    for p in ["anthropic", "openai"] {
        let s = Setup::new(
            p,
            vec![call_partial(p, &NINE[..4], CUT_M4), ok_nodes(p, &NINE[4..])],
            "",
        );
        s.write("A.java", &nine_nodes());
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        let posts = s.posts();
        assert_eq!(posts.len(), 2, "{p} {}", err(&o));
        assert_eq!(listed(&posts[0]).len(), 9, "{p}");
        assert_eq!(listed(&posts[1]), NINE[4..], "{p}");
        assert_eq!(content_of(&posts[1]), content_of(&posts[0]), "{p}");
        for n in NINE {
            assert_eq!(
                s.summary_of(n),
                Some(format!("Résumé de {n}.")),
                "{p} {n} {}",
                err(&o)
            );
        }
        assert!(!err(&o).contains("avertissement:"), "{p} {}", err(&o));
        // Témoin : l'appel répond en une fois avec les mêmes résumés.
        let w = Setup::new(p, vec![ok_nodes(p, &NINE)], "");
        w.write("A.java", &nine_nodes());
        let wo = w.run(&["build"]);
        assert!(wo.status.success(), "{p} {}", err(&wo));
        assert_eq!(graph_of(&s), graph_of(&w), "{p}");
        let (ge, gw) = (
            s.run(&["export", "--output", "-"]),
            w.run(&["export", "--output", "-"]),
        );
        assert_eq!(out(&ge), out(&gw), "{p}");
    }
}

#[test]
fn req232_ac8_tout_recu_puis_coupe_dans_les_concepts_une_requete() {
    let p = "openai";
    let all = |tail: &str| {
        let items: Vec<String> = NINE
            .iter()
            .map(|n| {
                serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")}).to_string()
            })
            .collect();
        trunc_text(
            p,
            &format!(
                r#"{{"version": 2, "summaries": [{}]{tail}"#,
                items.join(",")
            ),
        )
    };
    for tail in [
        r#", "concepts": [{"name": "alpha", "su"#,
        r#", "concepts": ["#,
        "",
    ] {
        let s = Setup::new(p, vec![all(tail); 5], "");
        s.write("A.java", &nine_nodes());
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{tail} {}", err(&o));
        assert_eq!(s.posts().len(), 1, "{tail} {}", err(&o));
        assert_eq!(s.to_enrich(), 0, "{tail}");
        assert_eq!(
            s.summary_of("type:A").as_deref(),
            Some("Résumé de type:A."),
            "{tail}"
        );
    }
}

#[test]
fn req232_ac8_markdown_concept_complet_garde_et_liste_vide() {
    let p = "openai";
    // Trois sections : trois résumés et un concept complet, le second concept coupé.
    let doc = "# Alpha\n\ntexte a\n\n# Beta\n\ntexte b\n\n# Gamma\n\ntexte c\n";
    let text = r#"{"version": 2, "summaries": [{"node": "document:D.md", "summary": "R0."}, {"node": "section:D.md#alpha", "summary": "R1."}, {"node": "section:D.md#beta", "summary": "R2."}, {"node": "section:D.md#gamma", "summary": "R3."}], "concepts": [{"name": "Idée un", "summary": "C1.", "sections": ["section:D.md#alpha"]}, {"name": "Idée deux", "su"#;
    let s = Setup::new(p, vec![trunc_text(p, text); 5], "");
    s.write("D.md", doc);
    let o = s.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    let after_first = s.posts().len();
    let concepts: Vec<String> = nephila::store::Store::open_readonly(&s.repo)
        .unwrap()
        .nodes()
        .unwrap()
        .into_iter()
        .filter(|n| n.id.starts_with("concept:"))
        .map(|n| n.id)
        .collect();
    assert_eq!(concepts, ["concept:idée un"], "{}", err(&o));
    assert_eq!(after_first, 1, "{}", err(&o));

    // Morceau à liste vide : un concept complet puis la coupe, aucune requête de plus.
    let doc = "# t0\nxxxxxxx\n# t2\n";
    let ok = ok_for(p, &[]);
    let cut_concept = trunc_text(
        p,
        r#"{"version": 2, "summaries": [], "concepts": [{"name": "Idée vide", "summary": "C.", "sections": ["section:E.md#t0"]}, {"name": "Autre", "su"#,
    );
    let s = Setup::new(
        p,
        vec![ok.clone(), cut_concept, ok.clone(), ok.clone(), ok],
        "max_chunk_chars = 8\nformat_retries = 0\n",
    );
    s.write("E.md", doc);
    let o = s.run(&["build"]);
    assert_eq!(s.posts().len(), 3, "{}", err(&o));
    let has = nephila::store::Store::open_readonly(&s.repo)
        .unwrap()
        .nodes()
        .unwrap()
        .into_iter()
        .any(|n| n.id == "concept:idée vide");
    assert!(has, "{}", err(&o));
}

#[test]
fn req232_ac9_sans_resume_utile_la_decoupe_reste_la_regle() {
    let p = "openai";
    let partial = |cut: &str| trunc_text(p, &format!(r#"{{"version": 2, "summaries": [{cut}"#));
    let firsts = [
        // Résumé hors liste.
        partial(
            r#"{"node": "method:A.hors", "summary": "Hors liste."}, {"node": "method:A.m0", "su"#,
        ),
        // Résumé vide de sens.
        partial(r#"{"node": "method:A.m0", "summary": ""}, {"node": "method:A.m1", "su"#),
        // Un concept seul, sans résumé : rien d'utile pour un fichier Java.
        trunc_text(
            p,
            r#"{"version": 2, "summaries": [], "concepts": [{"name": "x", "summary": "y", "sections": []}, {"na"#,
        ),
    ];
    for (i, first) in firsts.into_iter().enumerate() {
        let s = Setup::new(
            p,
            vec![first, ok_nodes(p, &NINE[..5]), ok_nodes(p, &NINE[5..])],
            "",
        );
        s.write("A.java", &nine_nodes());
        let o = s.run(&["build"]);
        let posts = s.posts();
        assert_eq!(posts.len(), 3, "{i} {}", err(&o));
        assert_eq!(listed(&posts[1]), NINE[..5], "{i}");
        assert_eq!(listed(&posts[2]), NINE[5..], "{i}");
        assert!(
            !err(&o).contains("élément(s) d'enrichissement écarté(s)"),
            "{i} {}",
            err(&o)
        );
    }
}

#[test]
fn req232_ac9_morceau_2_un_resume_sur_deux_meme_extrait() {
    let (text, cfg, sums) = ac2_setup();
    let p = "openai";
    let none = || ok_for(p, &[]);
    let partial = trunc_text(
        p,
        &format!(
            r#"{{"version": 2, "summaries": [{{"node": "{}", "summary": "{}"}}, {{"node": "{}", "su"#,
            sums[0].0, sums[0].1, sums[1].0
        ),
    );
    let s = Setup::new(
        p,
        vec![
            none(),
            partial,
            ok_for(p, &[(sums[1].0.as_str(), sums[1].1.as_str())]),
            none(),
        ],
        &cfg,
    );
    s.write("A.java", &text);
    let o = s.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    let posts = s.posts();
    assert_eq!(posts.len(), 4, "{}", err(&o));
    assert_eq!(extract_of(&posts[2]), extract_of(&posts[1]));
    assert_eq!(content_of(&posts[2]), content_of(&posts[1]));
    let parent = listed(&posts[1]);
    let rest: Vec<String> = parent
        .iter()
        .filter(|n| **n != sums[0].0)
        .cloned()
        .collect();
    assert_eq!(listed(&posts[2]), rest);
    assert_eq!(s.to_enrich(), 0);
    for (id, t) in &sums {
        assert_eq!(s.summary_of(id).as_deref(), Some(t.as_str()), "{id}");
    }
}

#[test]
fn req232_ac10_toujours_tronque_avec_un_resume_quatre_requetes_une_ligne_a5() {
    let p = "openai";
    let mut t = String::from("public class A {\n");
    for i in 0..15 {
        t.push_str(&format!("    void m{i}() {{}}\n"));
    }
    t.push_str("}\n");
    // Ordre des nœuds demandés : tri des identifiants.
    let mut ids: Vec<String> = (0..15).map(|i| format!("method:A.m{i}")).collect();
    ids.push("type:A".into());
    ids.sort();
    let replies: Vec<String> = (0..8)
        .map(|k| {
            let id = ids[k % 4].as_str();
            call_partial(p, &[id], r#"{"node": "x", "summ"#)
        })
        .collect();
    // Les réponses tronquées 1 à 4 portent m0, m1, m10, m11 : le premier nœud de chaque liste.
    let replies = [
        replies[0].clone(),
        replies[1].clone(),
        replies[2].clone(),
        replies[3].clone(),
    ]
    .into_iter()
    .cycle()
    .take(8)
    .collect();
    let s = Setup::new(p, replies, "");
    s.write("A.java", &t);
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    let posts = s.posts();
    let sizes: Vec<usize> = posts.iter().map(|b| listed(b).len()).collect();
    assert_eq!(sizes, [16, 15, 14, 13], "{}", err(&o));
    assert_eq!(err(&o).matches("avertissement:").count(), 1, "{}", err(&o));
    assert!(err(&o).contains(&a5("A.java")), "{}", err(&o));
    assert_eq!(s.to_enrich(), 1);
    assert_eq!(s.summary_of("method:A.m0"), None);
    let o = s.run(&["update"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 8, "{}", err(&o));
    assert_eq!(err(&o).matches("avertissement:").count(), 1, "{}", err(&o));
    assert!(err(&o).contains(&a5("A.java")), "{}", err(&o));
}

#[test]
fn req232_ac18_appel_marqueur_de_l_element_coupe_nulle_part() {
    for p in ["anthropic", "openai"] {
        let cut = format!(r#"{{"node": "method:A.m4", "summary": "{SECRET}"#);
        let s = Setup::new(
            p,
            vec![call_partial(p, &NINE[..4], &cut), ok_nodes(p, &NINE[4..])],
            "",
        );
        s.write("A.java", &nine_nodes());
        let o = s.run(&["build", "--verbose"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        assert_eq!(s.posts().len(), 2, "{p}");
        assert!(
            !out(&o).contains(SECRET) && !err(&o).contains(SECRET),
            "{p}"
        );
        for n in NINE {
            let sum = s.summary_of(n).unwrap_or_default();
            assert!(!sum.contains(SECRET), "{p} {n}");
        }
        let g = s.run(&["export", "--output", "-"]);
        assert!(g.status.success(), "{p} {}", err(&g));
        assert!(!out(&g).contains(SECRET), "{p}");
    }
}

#[test]
fn req232_ac14_progression_une_seule_fois() {
    let p = "openai";
    let s = Setup::new(
        p,
        vec![call_partial(p, &NINE[..4], CUT_M4), ok_nodes(p, &NINE[4..])],
        "",
    );
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build", "--show-progress"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.posts().len(), 2);
    let e = err(&o);
    let lines: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    assert_eq!(
        lines,
        [
            "progression: enrichissement 0 % (0/1 fichiers)",
            "progression: enrichissement 100 % (1/1 fichiers)"
        ],
        "{e}"
    );
}

// ---- REQ-233 : traces de la récupération (design.md V-005) ----

/// Lignes `llm enrich récupération` de la sortie d'erreur, sans leur horodatage.
fn recovery_lines(e: &str) -> Vec<String> {
    llm_lines(e)
        .into_iter()
        .filter(|l| l.starts_with("llm enrich récupération"))
        .collect()
}

/// Lignes `trace:` complètes de récupération : forme `trace: [<s>.<ms>s] llm enrich récupération …`.
fn assert_recovery_shape(e: &str) {
    let re_ok = |l: &str| {
        let Some(rest) = l.strip_prefix("trace: [") else {
            return false;
        };
        let Some((clock, tail)) = rest.split_once("s] ") else {
            return false;
        };
        let Some((sec, ms)) = clock.split_once('.') else {
            return false;
        };
        sec.chars().all(|c| c.is_ascii_digit())
            && !sec.is_empty()
            && ms.len() == 3
            && ms.chars().all(|c| c.is_ascii_digit())
            && tail.starts_with("llm enrich récupération ")
    };
    for l in e.lines().filter(|l| l.contains("récupération")) {
        assert!(re_ok(l), "{l}");
    }
}

fn appels_line(e: &str) -> String {
    e.lines()
        .find(|l| l.contains("appels: "))
        .unwrap_or_else(|| panic!("ligne appels: absente : {e}"))
        .to_string()
}

#[test]
fn req233_ac1_appel_ligne_de_recuperation_puis_sous_appel() {
    for p in ["anthropic", "openai"] {
        let s = Setup::new(
            p,
            vec![
                with_tokens(p, &call_partial(p, &NINE[..4], CUT_M4)),
                with_tokens(p, &ok_nodes(p, &NINE[4..])),
            ],
            "",
        );
        s.write("A.java", &nine_nodes());
        let (lines, e) = run_lines(&s, &["build", "--verbose"]);
        let pos = position_of(&lines, "llm enrich fin A.java [1/1] tronquée ", &e);
        assert_eq!(
            lines[pos + 1],
            "llm enrich récupération A.java [1/1] : 4 résumés, 0 concepts gardés, repart 5 nœuds",
            "{p} {e}"
        );
        assert!(
            lines[pos + 2].starts_with("llm enrich départ A.java [1/1] ")
                && lines[pos + 2].ends_with(" (sous-appel 1)"),
            "{p} {e}"
        );
        assert!(
            lines[pos + 3].starts_with("llm enrich fin A.java [1/1] ok ")
                && lines[pos + 3].ends_with(" (sous-appel 1)"),
            "{p} {e}"
        );
        assert!(!e.contains("découpe"), "{p} {e}");
        assert!(!e.contains("politique:"), "{p} {e}");
        assert!(appels_line(&e).contains("appels: 2 appels"), "{p} {e}");
        assert_recovery_shape(&e);
    }
}

#[test]
fn req233_ac2_ac6_lot_ligne_de_recuperation_puis_lot_reduit() {
    for p in ["anthropic", "openai"] {
        // AC-232-3 : A et B gardés, C et D repartent.
        let s = Setup::new(
            p,
            vec![
                trunc_text(p, &lot_partial_text(&["A", "B"], CUT_C)),
                ok_text(p, &lot_reply(&["C", "D"])),
            ],
            "batch_files = 4\n",
        );
        four_docs(&s);
        let (lines, e) = run_lines(&s, &["build", "--verbose"]);
        let pos = position_of(&lines, "llm enrich fin lot A.java +3 [1/1] tronquée ", &e);
        assert_eq!(
            lines[pos + 1],
            "llm enrich récupération lot A.java +3 [1/1] : 2 fichiers gardés, repart lot C.java +1",
            "{p} {e}"
        );
        assert!(
            lines[pos + 2].starts_with("llm enrich départ lot C.java +1 [1/1] "),
            "{p} {e}"
        );
        assert!(!e.contains("découpe") && !e.contains("renvoi"), "{p} {e}");
        assert_recovery_shape(&e);

        // AC-232-4 : trois gardés, D seul repart.
        let s = Setup::new(
            p,
            vec![
                trunc_text(
                    p,
                    &lot_partial_text(&["A", "B", "C"], r#"{"file": "D.java", "su"#),
                ),
                ok_for(p, &[("type:D", "Classe D centrale.")]),
            ],
            "batch_files = 4\n",
        );
        four_docs(&s);
        let (lines, e) = run_lines(&s, &["build", "--verbose"]);
        let pos = position_of(&lines, "llm enrich fin lot A.java +3 [1/1] tronquée ", &e);
        assert_eq!(
            lines[pos + 1],
            "llm enrich récupération lot A.java +3 [1/1] : 3 fichiers gardés, repart D.java",
            "{p} {e}"
        );
        assert!(
            lines[pos + 2].starts_with("llm enrich départ D.java [1/1] "),
            "{p} {e}"
        );

        // AC-232-4, tous présents : aucun autre départ.
        let s = Setup::new(
            p,
            vec![trunc_text(
                p,
                &lot_partial_text(&["A", "B", "C", "D"], r#",{"file": "E"#),
            )],
            "batch_files = 4\n",
        );
        four_docs(&s);
        let (lines, e) = run_lines(&s, &["build", "--verbose"]);
        let pos = position_of(&lines, "llm enrich fin lot A.java +3 [1/1] tronquée ", &e);
        assert_eq!(
            lines[pos + 1],
            "llm enrich récupération lot A.java +3 [1/1] : 4 fichiers gardés, rien ne repart",
            "{p} {e}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("llm enrich départ"))
                .count(),
            1,
            "{p} {e}"
        );

        // AC-232-6 : toujours tronqué, trois lignes dans l'ordre.
        let s = Setup::new(
            p,
            vec![
                trunc_text(p, &lot_partial_text(&["A"], r#"{"file": "B.java", "su"#)),
                trunc_text(p, &lot_partial_text(&["B"], r#"{"file": "C.java", "su"#)),
                trunc_text(p, &lot_partial_text(&["C"], r#"{"file": "D.java", "su"#)),
                ok_for(p, &[("type:D", "Classe D centrale.")]),
            ],
            "batch_files = 4\n",
        );
        four_docs(&s);
        let (_, e) = run_lines(&s, &["build", "--verbose"]);
        assert_eq!(
            recovery_lines(&e),
            [
                "llm enrich récupération lot A.java +3 [1/1] : 1 fichiers gardés, repart lot B.java +2",
                "llm enrich récupération lot B.java +2 [1/1] : 1 fichiers gardés, repart lot C.java +1",
                "llm enrich récupération lot C.java +1 [1/1] : 1 fichiers gardés, repart D.java",
            ],
            "{p} {e}"
        );
    }
}

#[test]
fn req233_ac3_sans_verbose_rien_n_est_dit() {
    let p = "openai";
    let s = Setup::new(
        p,
        vec![call_partial(p, &NINE[..4], CUT_M4), ok_nodes(p, &NINE[4..])],
        "",
    );
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(err(&o), "", "stderr non vide");
    assert_eq!(out(&o).trim_end().lines().count(), 1, "{}", out(&o));
    assert!(without_coverage(&out(&o)).ends_with(", 0 fichiers à enrichir"));
    let s = Setup::new(
        p,
        vec![
            trunc_text(p, &lot_partial_text(&["A", "B"], CUT_C)),
            ok_text(p, &lot_reply(&["C", "D"])),
        ],
        "batch_files = 4\n",
    );
    four_docs(&s);
    let o = s.run(&["build"]);
    assert_eq!(err(&o), "");
    assert_eq!(out(&o).trim_end().lines().count(), 1, "{}", out(&o));
}

#[test]
fn req233_ac4_sans_element_utile_aucune_ligne_de_recuperation() {
    let p = "openai";
    // AC-232-5 : lot sans entrée utile, moitiés.
    let s = Setup::new(
        p,
        vec![
            trunc_text(
                p,
                &lot_partial_text(&[], r#"{"file": "A.java", "summaries": [{"node": "ty"#),
            ),
            ok_text(p, &lot_reply(&["A", "B"])),
            ok_text(p, &lot_reply(&["C", "D"])),
        ],
        "batch_files = 4\n",
    );
    four_docs(&s);
    let (lines, e) = run_lines(&s, &["build", "--verbose"]);
    assert!(recovery_lines(&e).is_empty(), "{e}");
    let pos = position_of(&lines, "llm enrich fin lot A.java +3 [1/1] tronquée ", &e);
    assert_eq!(
        lines[pos + 1],
        "llm enrich découpe lot A.java +3 [1/1] en lot A.java +1 et lot C.java +1",
        "{e}"
    );
    // AC-232-9 : résumé hors liste, appel découpé.
    let partial = trunc_text(
        p,
        r#"{"version": 2, "summaries": [{"node": "method:A.hors", "summary": "Hors."}, {"node": "method:A.m0", "su"#,
    );
    let s = Setup::new(
        p,
        vec![partial, ok_nodes(p, &NINE[..5]), ok_nodes(p, &NINE[5..])],
        "",
    );
    s.write("A.java", &nine_nodes());
    let (lines, e) = run_lines(&s, &["build", "--verbose"]);
    assert!(recovery_lines(&e).is_empty(), "{e}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("llm enrich découpe A.java [1/1] en 2 sous-appels")),
        "{e}"
    );
    // Pas de réponse tronquée : aucune ligne.
    let s = Setup::new(p, vec![ok_nodes(p, &NINE)], "");
    s.write("A.java", &nine_nodes());
    let (_, e) = run_lines(&s, &["build", "--verbose"]);
    assert!(!e.contains("récupération"), "{e}");
}

#[test]
fn req233_ac4_ac7_quatre_reponses_tronquees_trois_lignes_puis_a5() {
    let p = "openai";
    let mut t = String::from("public class A {\n");
    for i in 0..15 {
        t.push_str(&format!("    void m{i}() {{}}\n"));
    }
    t.push_str("}\n");
    let mut ids: Vec<String> = (0..15).map(|i| format!("method:A.m{i}")).collect();
    ids.push("type:A".into());
    ids.sort();
    // Le premier nœud demandé de chaque appel : m0, m1, m10, m11.
    let replies: Vec<String> = (0..4)
        .map(|k| call_partial(p, &[ids[k].as_str()], r#"{"node": "x", "summ"#))
        .collect();
    let s = Setup::new(p, replies, "");
    s.write("A.java", &t);
    let (lines, e) = run_lines(&s, &["build", "--verbose"]);
    let rec = recovery_lines(&e);
    assert_eq!(rec.len(), 3, "{e}");
    for (l, end) in rec.iter().zip([
        "1 résumés, 0 concepts gardés, repart 15 nœuds",
        "1 résumés, 0 concepts gardés, repart 14 nœuds (sous-appel 1)",
        "1 résumés, 0 concepts gardés, repart 13 nœuds (sous-appel 1.1)",
    ]) {
        assert!(l.ends_with(end) && l.contains(" [1/1] : "), "{l}");
    }
    let departs: Vec<&String> = lines
        .iter()
        .filter(|l| l.starts_with("llm enrich départ "))
        .collect();
    assert_eq!(departs.len(), 4, "{e}");
    for (l, end) in
        departs
            .iter()
            .skip(1)
            .zip(["(sous-appel 1)", "(sous-appel 1.1)", "(sous-appel 1.1.1)"])
    {
        assert!(l.ends_with(end), "{l}");
    }
    for l in &lines {
        assert!(
            !l.contains("découpe")
                && !l.contains("(sous-appel 0)")
                && !l.contains("(sous-appel 2)"),
            "{l}"
        );
    }
    assert!(appels_line(&e).contains("appels: 4 appels"), "{e}");
    // Aucune ligne de récupération après la quatrième fin tronquée, puis A-5.
    let last_fin = lines
        .iter()
        .rposition(|l| l.starts_with("llm enrich fin A.java [1/1] tronquée "))
        .unwrap();
    assert!(
        lines[last_fin + 1..]
            .iter()
            .all(|l| !l.contains("récupération")),
        "{e}"
    );
    assert!(e.contains(&a5("A.java")), "{e}");
    // Aucun contenu de la réponse.
    for l in &rec {
        assert!(!l.contains("method:") && !l.contains("Résumé"), "{l}");
    }

    // Quatrième réponse tronquée avec ses 13 résumés complets : fichier enrichi.
    let mut replies: Vec<String> = (0..3)
        .map(|k| call_partial(p, &[ids[k].as_str()], r#"{"node": "x", "summ"#))
        .collect();
    // La liste du quatrième appel : les 16 nœuds moins les trois premiers reçus (m0, m1, m10).
    let wanted: Vec<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|n| !ids[..3].iter().any(|g| g == n))
        .collect();
    replies.push(call_partial(p, &wanted, r#"{"node": "x", "summ"#));
    let s = Setup::new(p, replies, "");
    s.write("A.java", &t);
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    assert!(o.status.success(), "{e}");
    let rec = recovery_lines(&e);
    assert_eq!(rec.len(), 4, "{e}");
    assert!(
        rec[3].ends_with("13 résumés, 0 concepts gardés, rien ne repart (sous-appel 1.1.1)"),
        "{}",
        rec[3]
    );
    assert!(!e.contains("avertissement:"), "{e}");
    assert_eq!(s.to_enrich(), 0);
}

#[test]
fn req233_ac6_appel_tous_recus_liste_vide_et_markdown() {
    let p = "openai";
    // AC-232-8 : tous les résumés reçus, puis coupe dans les concepts.
    let items: Vec<String> = NINE
        .iter()
        .map(|n| serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")}).to_string())
        .collect();
    let text = format!(
        r#"{{"version": 2, "summaries": [{}], "concepts": [{{"name": "alpha", "su"#,
        items.join(",")
    );
    let s = Setup::new(p, vec![trunc_text(p, &text); 3], "");
    s.write("A.java", &nine_nodes());
    let (_, e) = run_lines(&s, &["build", "--verbose"]);
    assert_eq!(
        recovery_lines(&e),
        ["llm enrich récupération A.java [1/1] : 9 résumés, 0 concepts gardés, rien ne repart"],
        "{e}"
    );

    // Document Markdown : résumés et un concept complet, le second concept coupé.
    let doc = "# Alpha\n\ntexte a\n\n# Beta\n\ntexte b\n\n# Gamma\n\ntexte c\n";
    let text = r#"{"version": 2, "summaries": [{"node": "document:D.md", "summary": "R0."}, {"node": "section:D.md#alpha", "summary": "R1."}, {"node": "section:D.md#beta", "summary": "R2."}, {"node": "section:D.md#gamma", "summary": "R3."}], "concepts": [{"name": "Idée un", "summary": "C1.", "sections": ["section:D.md#alpha"]}, {"name": "Idée deux", "su"#;
    let s = Setup::new(p, vec![trunc_text(p, text); 3], "");
    s.write("D.md", doc);
    let (_, e) = run_lines(&s, &["build", "--verbose"]);
    assert_eq!(
        recovery_lines(&e),
        ["llm enrich récupération D.md [1/1] : 4 résumés, 1 concepts gardés, rien ne repart"],
        "{e}"
    );

    // Morceau à liste vide : un concept complet puis la coupe.
    let ok = ok_for(p, &[]);
    let cut_concept = trunc_text(
        p,
        r#"{"version": 2, "summaries": [], "concepts": [{"name": "Idée vide", "summary": "C.", "sections": ["section:E.md#t0"]}, {"name": "Autre", "su"#,
    );
    let s = Setup::new(
        p,
        vec![ok.clone(), cut_concept, ok.clone(), ok.clone(), ok],
        "max_chunk_chars = 8\nformat_retries = 0\n",
    );
    s.write("E.md", "# t0\nxxxxxxx\n# t2\n");
    let (_, e) = run_lines(&s, &["build", "--verbose"]);
    let rec = recovery_lines(&e);
    assert_eq!(rec.len(), 1, "{e}");
    assert!(
        rec[0].starts_with("llm enrich récupération E.md [")
            && rec[0].ends_with("] : 0 résumés, 1 concepts gardés, rien ne repart"),
        "{e}"
    );
}

#[test]
fn req233_ac9_resume_ecarte_repart_un_seul_avertissement_sans_marqueur() {
    let p = "openai";
    for verbose in [false, true] {
        let cut = format!(r#"{{"node": "method:A.m3", "summary": "{SECRET}"#);
        let first = trunc_text(
            p,
            &format!(
                r#"{{"version": 2, "summaries": [{{"node": "method:A.m0", "summary": "Premier."}}, {{"node": "method:A.m1", "summary": "N/A"}}, {{"node": "method:A.m2", "summary": "Troisième."}}, {cut}"#
            ),
        );
        let s = Setup::new(
            p,
            vec![
                first,
                ok_nodes(
                    p,
                    &["method:A.m1"]
                        .iter()
                        .chain(&NINE[3..])
                        .copied()
                        .collect::<Vec<_>>(),
                ),
            ],
            "",
        );
        s.write("A.java", &nine_nodes());
        let args: &[&str] = if verbose {
            &["build", "--verbose"]
        } else {
            &["build"]
        };
        let o = s.run(args);
        let e = err(&o);
        assert!(o.status.success(), "{verbose} {e}");
        assert!(
            !e.contains(SECRET) && !out(&o).contains(SECRET),
            "{verbose} {e}"
        );
        let posts = s.posts();
        assert_eq!(posts.len(), 2, "{verbose} {e}");
        assert!(
            listed(&posts[1]).contains(&"method:A.m1".to_string()),
            "{verbose}"
        );
        assert_eq!(listed(&posts[1]).len(), 7, "{verbose}");
        let warns: Vec<&str> = e
            .lines()
            .filter(|l| l.starts_with("avertissement:"))
            .collect();
        assert_eq!(
            warns,
            ["avertissement: A.java : 1 élément(s) d'enrichissement écarté(s) : résumé pour method:A.m1 (vide de sens)"],
            "{verbose} {e}"
        );
        if verbose {
            let rec = recovery_lines(&e);
            assert_eq!(rec.len(), 1, "{e}");
            assert!(
                rec[0].ends_with("2 résumés, 0 concepts gardés, repart 7 nœuds"),
                "{e}"
            );
        }
    }
}

#[test]
fn req233_ac3_lot_recupere_progression_et_bilan() {
    let p = "openai";
    let s = Setup::new(
        p,
        vec![
            trunc_text(p, &lot_partial_text(&["A", "B"], CUT_C)),
            ok_text(p, &lot_reply(&["C", "D"])),
        ],
        "batch_files = 4\n",
    );
    four_docs(&s);
    let o = s.run(&["build", "--show-progress"]);
    let e = err(&o);
    assert!(o.status.success(), "{e}");
    assert_eq!(s.posts().len(), 2, "{e}");
    let lines: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    assert_eq!(
        lines,
        [
            "progression: enrichissement 0 % (0/4 fichiers)",
            "progression: enrichissement 50 % (2/4 fichiers)",
            "progression: enrichissement 100 % (4/4 fichiers)"
        ],
        "{e}"
    );
    assert_eq!(
        e.matches("enrichissement 100 % (4/4 fichiers)").count(),
        1,
        "{e}"
    );
    assert!(!e.contains("récupération"), "{e}");
    assert_eq!(out(&o).trim_end().lines().count(), 1, "{}", out(&o));
    assert!(
        without_coverage(&out(&o)).ends_with(", 0 fichiers à enrichir"),
        "{}",
        out(&o)
    );
}

#[test]
fn req234_ac4_resume_vide_d_une_reponse_tronquee_redemande_et_signale() {
    let p = "openai";
    let cut = format!(r#"{{"node": "method:A.m3", "summary": "{SECRET}"#);
    let first = trunc_text(
        p,
        &format!(
            r#"{{"version": 2, "summaries": [{{"node": "method:A.m0", "summary": "Premier."}}, {{"node": "method:A.m1", "summary": ""}}, {{"node": "method:A.m2", "summary": "Troisième."}}, {cut}"#
        ),
    );
    let rest: Vec<&str> = ["method:A.m1"].iter().chain(&NINE[3..]).copied().collect();
    let s = Setup::new(p, vec![first, ok_nodes(p, &rest)], "");
    s.write("A.java", &nine_nodes());
    let o = s.run(&["build"]);
    let e = err(&o);
    assert!(o.status.success(), "{e}");
    let posts = s.posts();
    assert_eq!(posts.len(), 2, "{e}");
    assert!(
        listed(&posts[1]).contains(&"method:A.m1".to_string()),
        "{e}"
    );
    assert_eq!(listed(&posts[1]).len(), 7, "{e}");
    let warns: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect();
    assert_eq!(
        warns,
        ["avertissement: A.java : 1 élément(s) d'enrichissement écarté(s) : résumé pour method:A.m1 (vide)"],
        "{e}"
    );
    assert!(!e.contains(SECRET), "{e}");
}

/// Bilan sans son segment de couverture (REQ-235), qui doit être présent.
fn without_coverage(out: &str) -> String {
    let (head, tail) = out.trim_end().rsplit_once(", ").unwrap();
    assert!(tail.ends_with(" nœuds résumés"), "{out}");
    head.to_string()
}

// ---- T-368 : réponse bloquée par le modèle (REQ-274, DEC-203) ----

const BLOCKED_LABEL: &str = "réponse bloquée par le modèle";
const SECRET_REFUS: &str = "SECRET_REFUS_42";

fn a24(path: &str) -> String {
    format!("avertissement: {path} : {BLOCKED_LABEL} ; excluez ce fichier avec enrich_exclude dans .nephila.toml")
}

/// Réponse bloquée dont le texte est un JSON conforme portant le marqueur.
fn blocked_with(provider: &str, text: &str) -> String {
    match provider {
        "anthropic" => anthropic_reply(text, Some("refusal")),
        _ => openai_reply(text, Some("content_filter")),
    }
}

fn blocked(provider: &str) -> String {
    blocked_with(
        provider,
        &A_GOOD.replace("Classe A centrale.", SECRET_REFUS),
    )
}

fn blocked_ask(provider: &str) -> String {
    blocked_with(
        provider,
        &format!(r#"{{"found":true,"answer":"{SECRET_REFUS}","nodes":[]}}"#),
    )
}

fn forbidden_blocked(stderr: &str) {
    for bad in [
        "non conforme",
        "essai",
        "tronquée",
        "refus",
        "content_filter",
        SECRET_REFUS,
    ] {
        assert!(!stderr.contains(bad), "{bad} dans {stderr}");
    }
}

fn check_blocked_single(provider: &str) {
    let s = Setup::new(provider, vec![blocked(provider); 10], "");
    s.write("A.java", A_JAVA);
    let o = s.run(&["build"]);
    assert!(!o.status.success(), "{provider}");
    assert_eq!(s.posts().len(), 1, "{provider} {}", err(&o));
    let e = err(&o);
    assert_eq!(
        e.lines()
            .filter(|l| l.contains("A.java"))
            .collect::<Vec<_>>(),
        [a24("A.java").as_str()],
        "{provider} {e}"
    );
    assert!(out(&o).contains("1 fichiers à enrichir"), "{}", out(&o));
    forbidden_blocked(&e);
    assert_eq!(s.to_enrich(), 1, "{provider}");
    assert_eq!(s.summary_of("type:A"), None, "{provider}");
    let o = s.run(&["update"]);
    assert_eq!(s.posts().len(), 2, "{provider} {}", err(&o));
    assert!(err(&o).contains(&a24("A.java")), "{provider} {}", err(&o));
    forbidden_blocked(&err(&o));
}

#[test]
fn req274_ac4_anthropic_bloquee_un_seul_envoi() {
    check_blocked_single("anthropic");
}

#[test]
fn req274_ac5_openai_bloquee_un_seul_envoi() {
    check_blocked_single("openai");
}

#[test]
fn req274_ac5_openai_message_refusal_un_seul_envoi() {
    let body =
        r#"{"choices":[{"message":{"content":null,"refusal":"I can't"},"finish_reason":"stop"}]}"#;
    let s = Setup::new("openai", vec![body.to_string(); 10], "");
    s.write("A.java", A_JAVA);
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 1, "{}", err(&o));
    let e = err(&o);
    assert!(e.contains(&a24("A.java")), "{e}");
    assert!(!e.contains("I can't"), "{e}");
    forbidden_blocked(&e);
    let o = s.run(&["update"]);
    assert_eq!(s.posts().len(), 2, "{}", err(&o));
    assert!(!err(&o).contains("I can't"));

    let s = Setup::new("openai", vec![body.to_string(); 10], "");
    s.write("A.java", "public class Alpha {}\n");
    assert!(s.run(&["build", "--no-llm"]).status.success());
    let o = s.ask(&[]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 1);
    assert_eq!(
        err(&o),
        format!("erreur: {BLOCKED_LABEL} ; reformulez la question\n")
    );
}

#[test]
fn req274_ac6_format_retries_sans_effet_sur_le_blocage() {
    let cases = [
        ("anthropic", "format_retries = 5\n", vec!["build"]),
        ("anthropic", "", vec!["build", "--format-retries", "5"]),
        ("openai", "format_retries = 0\n", vec!["build"]),
        ("openai", "", vec!["build", "--format-retries", "5"]),
    ];
    for (p, extra, args) in cases {
        let s = Setup::new(p, vec![blocked(p); 10], extra);
        s.write("A.java", A_JAVA);
        let o = s.run(&args);
        assert_eq!(s.posts().len(), 1, "{p} {args:?}");
        let e = err(&o);
        assert_eq!(e.matches(&a24("A.java")).count(), 1, "{p} {e}");
        forbidden_blocked(&e);
    }
}

#[test]
fn req274_ac9_huit_fichiers_bloques_pas_de_panne_reseau() {
    let s = Setup::new(
        "anthropic",
        vec![blocked("anthropic"); 30],
        "batch_files = 1\n",
    );
    for i in 0..8 {
        s.write(&format!("F{i}.java"), &format!("public class F{i} {{}}\n"));
    }
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 8, "{}", err(&o));
    let e = err(&o);
    let mut got: Vec<String> = e
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect();
    got.sort();
    let expected: Vec<String> = (0..8).map(|i| a24(&format!("F{i}.java"))).collect();
    assert_eq!(got, expected, "{e}");
    assert!(!e.contains("réseau"), "{e}");
    assert_eq!(s.to_enrich(), 8);
}

#[test]
fn req274_ac10_ask_bloquee_ligne_e37_exacte() {
    for p in ["anthropic", "openai"] {
        for extra in [
            vec![],
            vec!["--json"],
            vec!["--format-retries", "0"],
            vec!["--format-retries", "4"],
        ] {
            let s = Setup::new(p, vec![blocked_ask(p); 10], "");
            s.write("A.java", "public class Alpha {}\n");
            let o = s.run(&["build", "--no-llm"]);
            assert!(o.status.success(), "{}", err(&o));
            let o = s.ask(&extra);
            assert!(!o.status.success(), "{p} {extra:?}");
            assert!(out(&o).is_empty(), "{p} {extra:?} {}", out(&o));
            assert_eq!(s.posts().len(), 1, "{p} {extra:?}");
            assert!(!out(&o).contains(SECRET_REFUS));
            assert_eq!(
                err(&o),
                format!("erreur: {BLOCKED_LABEL} ; reformulez la question\n"),
                "{p} {extra:?}"
            );
        }
    }
}

#[test]
fn req274_ac11_trace_fin_bloquee() {
    for p in ["anthropic", "openai"] {
        let s = Setup::new(p, vec![blocked(p); 10], "");
        s.write("A.java", A_JAVA);
        let o = s.run(&["build", "--verbose"]);
        let e = err(&o);
        let lines = llm_enrich_for(&e, "A.java");
        assert_eq!(lines.len(), 2, "{p} {e}");
        let rest = lines[1]
            .strip_prefix("trace: [")
            .and_then(|r| r.split_once("s] "))
            .map(|(_, r)| r)
            .unwrap_or_default();
        let rest = rest
            .strip_prefix("llm enrich fin A.java [1/1] bloquée ")
            .unwrap_or_else(|| panic!("{}", lines[1]));
        let (ms, detail) = rest.split_once(" ms : ").unwrap();
        assert!(
            !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()),
            "{}",
            lines[1]
        );
        assert!(
            detail == BLOCKED_LABEL
                || (detail.starts_with(&format!("{BLOCKED_LABEL} (jetons : "))
                    && detail.ends_with(')')),
            "{}",
            lines[1]
        );
        for l in trace_lines_of(&e) {
            for bad in ["(essai", "politique:", SECRET_REFUS] {
                assert!(!l.contains(bad), "{bad} dans {l}");
            }
        }
    }
}
