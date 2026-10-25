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
const A_GOOD: &str =
    r#"{"version": 2, "summaries": [{"node": "type:A", "summary": "Classe A."}], "concepts": []}"#;

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
                "summaries": [{"node": format!("type:{n}"), "summary": format!("Classe {n}.")}],
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
fn req046_ac3_texte_conforme_mais_tronque_est_refuse() {
    for p in ["anthropic", "openai"] {
        let body = match p {
            "anthropic" => anthropic_reply(A_GOOD, Some("max_tokens")),
            _ => openai_reply(A_GOOD, Some("length")),
        };
        let s = Setup::new(p, vec![body; 10], "");
        s.write("A.java", A_JAVA);
        let o = s.run(&["build"]);
        assert!(!o.status.success(), "{p}");
        assert_eq!(s.posts().len(), 1, "{p}");
        assert_eq!(s.to_enrich(), 1, "{p}");
        assert_eq!(s.summary_of("type:A"), None, "{p}");
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
            Some("Classe A."),
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
            r#"{{"version": 2, "summaries": [{{"node": "type:{n}", "summary": "Classe {n}."}}], "concepts": []}}"#
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

    let s = Setup::new(
        "openai",
        vec![
            openai_reply(ok, Some("stop")),
            trunc("openai"),
            openai_reply(ok, Some("stop")),
        ],
        &cfg,
    );
    s.write("A.java", &text);
    let o = s.run(&["build"]);
    assert!(!o.status.success());
    let posts = s.posts();
    assert_eq!(posts.len(), 2, "{}", err(&o));
    assert!(posts[1].contains("(morceau 2)"), "{}", posts[1]);
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
        for i in 1..=3 {
            v.push(if i == k {
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
    assert_eq!(lines.len(), 4, "{e}");
    assert!(lines[0].contains("départ A.java [1/3]"), "{}", lines[0]);
    assert!(lines[1].contains("fin A.java [1/3] ok ("), "{}", lines[1]);
    assert!(lines[2].contains("départ A.java [2/3]"), "{}", lines[2]);
    assert!(
        lines[3].contains("fin A.java [2/3] tronquée"),
        "{}",
        lines[3]
    );
    assert!(!e.contains("[3/3]"), "{e}");
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
                Some(format!("Classe {n}.")),
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
