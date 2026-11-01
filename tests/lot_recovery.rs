//! Renvoi des fichiers absents d'une réponse de lot et découpe des lots non conformes
//! (REQ-198, REQ-199, REQ-200) : enrichissement de documents contre un faux serveur HTTP.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const PROVIDERS: [&str; 2] = ["openai", "anthropic"];
const NAMES: [&str; 4] = ["a", "b", "c", "d"];
const NOT_COMPLIANT: &str = r#"{"version": 2, "summaries": [], "concepts": []}"#;
const REFUSED: &str =
    "erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env";

fn path(n: &str) -> String {
    format!("docs/{n}.md")
}

fn wrap(provider: &str, text: &str) -> String {
    match provider {
        "anthropic" => serde_json::json!({
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn"
        })
        .to_string(),
        _ => serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": text},
                         "finish_reason": "stop"}]
        })
        .to_string(),
    }
}

/// Entrée de lot pour le document `n`, sous le chemin `file` (écrit comme le modèle le veut).
fn entry(file: &str, n: &str) -> serde_json::Value {
    serde_json::json!({
        "file": file,
        "summaries": [{"node": format!("document:{}", path(n)),
                       "summary": format!("Résumé du document {n}.")}],
        "concepts": []
    })
}

/// Réponse de lot (version 3) pour les documents `names`.
fn lot(provider: &str, names: &[&str]) -> String {
    let files: Vec<_> = names.iter().map(|n| entry(&path(n), n)).collect();
    wrap(
        provider,
        &serde_json::json!({"version": 3, "files": files}).to_string(),
    )
}

/// Réponse d'un document seul (version 2).
fn single(provider: &str, n: &str) -> String {
    wrap(
        provider,
        &serde_json::json!({
            "version": 2,
            "summaries": [{"node": format!("document:{}", path(n)),
                           "summary": format!("Résumé du document {n}.")}],
            "concepts": []
        })
        .to_string(),
    )
}

fn bad(provider: &str) -> String {
    wrap(provider, NOT_COMPLIANT)
}

struct Setup {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
    provider: String,
}

impl Setup {
    /// Documents `docs/<n>.md` pour les `count` premiers noms ; `extra` : lignes de configuration.
    fn new(provider: &str, replies: Vec<(u16, String)>, count: usize, extra: &str) -> Setup {
        let server = FakeServer::start(replies);
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join("docs")).unwrap();
        for i in 0..count {
            let n = NAMES
                .get(i)
                .map_or_else(|| format!("f{i}"), |n| n.to_string());
            fs::write(
                repo.join(path(&n)),
                format!("# Document {n}\n\nTexte du document {n}.\n"),
            )
            .unwrap();
        }
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"{provider}\"\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T291_KEY\"\nconcurrency = 1\n{extra}",
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

    fn ok(provider: &str, bodies: Vec<String>, count: usize, extra: &str) -> Setup {
        Setup::new(
            provider,
            bodies.into_iter().map(|b| (200, b)).collect(),
            count,
            extra,
        )
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T291_KEY", "sk-test")
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    fn posts(&self) -> Vec<String> {
        let p = match self.provider.as_str() {
            "anthropic" => "/v1/messages",
            _ => "/v1/chat/completions",
        };
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == p)
            .map(|r| r.body)
            .collect()
    }

    fn summary_of(&self, n: &str) -> Option<String> {
        let id = format!("document:{}", path(n));
        nephila::store::Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|x| x.id == id)
            .and_then(|x| x.summary)
    }

    fn enriched(&self, names: &[&str]) -> bool {
        names.iter().all(|n| self.summary_of(n).is_some())
    }

    fn to_enrich(&self) -> Vec<String> {
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
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Chemins `docs/<n>.md` cités dans un corps de requête, dans l'ordre alphabétique.
fn files_in(body: &str) -> Vec<&'static str> {
    NAMES
        .into_iter()
        .filter(|n| body.contains(&path(n)))
        .collect()
}

/// Ligne sans son préfixe `trace: [<s>.<mmm>s] `.
fn untrace(line: &str) -> Option<&str> {
    line.strip_prefix("trace: [")?
        .split_once("] ")
        .map(|(_, rest)| rest)
}

fn warnings(e: &str) -> Vec<&str> {
    e.lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect()
}

fn progress(e: &str) -> Vec<&str> {
    e.lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect()
}

#[test]
fn req198_ac3_chemins_normalises_une_seule_requete() {
    for p in PROVIDERS {
        let files = serde_json::json!({"version": 3, "files": [
            entry("./docs/a.md", "a"), entry("docs\\b.md", "b")]});
        let s = Setup::ok(p, vec![wrap(p, &files.to_string())], 2, "batch_files = 2\n");
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        assert_eq!(s.posts().len(), 1, "{p}");
        assert!(s.enriched(&["a", "b"]), "{p}");
        assert!(warnings(&err(&o)).is_empty(), "{p} {}", err(&o));
    }
}

#[test]
fn req199_ac1_fichier_absent_renvoye_seul() {
    for p in PROVIDERS {
        let s = Setup::ok(
            p,
            vec![lot(p, &["a", "c"]), single(p, "b")],
            3,
            "batch_files = 3\n",
        );
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        let posts = s.posts();
        assert_eq!(posts.len(), 2, "{p} {}", err(&o));
        assert_eq!(files_in(&posts[0]), ["a", "b", "c"], "{p}");
        assert_eq!(files_in(&posts[1]), ["b"], "{p}");
        assert!(s.enriched(&["a", "b", "c"]), "{p}");
        let e = err(&o);
        assert!(warnings(&e).is_empty(), "{p} {e}");
        assert!(
            !e.contains("renvoi") && !e.contains("sans entrée dans la réponse"),
            "{e}"
        );
    }
}

#[test]
fn req199_ac2_second_passage_absent_echoue_puis_update_envoie_c_seul() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![lot(p, &["a"]), lot(p, &["b", "d"]), single(p, "c")],
        4,
        "batch_files = 4\n",
    );
    let o = s.run(&["build"]);
    assert!(!o.status.success(), "{}", err(&o));
    assert_eq!(s.posts().len(), 2, "{}", err(&o));
    assert_eq!(files_in(&s.posts()[1]), ["b", "c", "d"]);
    assert!(s.enriched(&["a", "b", "d"]));
    assert_eq!(s.to_enrich(), [path("c")]);
    let e = err(&o);
    let w = warnings(&e);
    assert_eq!(w.len(), 1, "{e}");
    assert!(
        w[0].contains("docs/c.md") && w[0].contains("absent de la réponse du lot"),
        "{e}"
    );
    assert!(
        !e.contains("renvoi") && !e.contains("sans entrée dans la réponse"),
        "{e}"
    );
    let o = s.run(&["update"]);
    assert!(o.status.success(), "{}", err(&o));
    let posts = s.posts();
    assert_eq!(posts.len(), 3);
    assert_eq!(files_in(&posts[2]), ["c"]);
    assert!(s.enriched(&["c"]));
}

#[test]
fn req199_ac3_reponse_sans_fichier_les_deux_renvoyes() {
    let p = "openai";
    let empty = wrap(p, r#"{"version": 3, "files": []}"#);
    let s = Setup::ok(p, vec![empty, lot(p, &["a", "b"])], 2, "batch_files = 2\n");
    let o = s.run(&["build"]);
    assert!(o.status.success(), "{}", err(&o));
    let posts = s.posts();
    assert_eq!(posts.len(), 2, "{}", err(&o));
    assert_eq!(files_in(&posts[1]), ["a", "b"]);
    assert!(s.enriched(&["a", "b"]));
    let e = err(&o);
    assert!(
        !e.contains("renvoi") && !e.contains("sans entrée dans la réponse"),
        "{e}"
    );
}

#[test]
fn req199_ac5_cle_refusee_apres_le_lot_fichiers_a_enrichir() {
    let p = "openai";
    let s = Setup::new(
        p,
        vec![(200, lot(p, &["a"])), (401, "{}".into())],
        4,
        "batch_files = 2\n",
    );
    let o = s.run(&["build"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_eq!(s.posts().len(), 2, "{}", err(&o));
    let e = err(&o);
    let errors: Vec<&str> = e.lines().filter(|l| l.starts_with("erreur:")).collect();
    assert_eq!(errors, [REFUSED], "{e}");
    assert!(s.enriched(&["a"]));
    assert_eq!(s.to_enrich(), [path("b"), path("c"), path("d")]);
}

#[test]
fn req199_ac5_plafond_un_appel_laisse_trois_fichiers() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![lot(p, &["a"])],
        4,
        "batch_files = 2\nmax_calls = 1\n",
    );
    let o = s.run(&["build"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(s.posts().len(), 1, "{}", err(&o));
    let e = err(&o);
    assert!(s.enriched(&["a"]));
    assert_eq!(s.to_enrich(), [path("b"), path("c"), path("d")]);
    let w = warnings(&e);
    assert_eq!(
        w,
        ["avertissement: plafond de 1 appels atteint : 3 fichiers laissés à enrichir ; relancez nephila update pour continuer ou augmentez max_calls"],
        "{e}"
    );
    assert!(!e.contains("absent de la réponse du lot"), "{e}");
}

#[test]
fn req199_ac7_verbose_une_ligne_renvoi_entre_fin_et_depart() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![lot(p, &["a", "c"]), single(p, "b")],
        3,
        "batch_files = 3\n",
    );
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    let lines: Vec<&str> = e.lines().collect();
    let hits: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].contains("llm enrich renvoi"))
        .collect();
    assert_eq!(hits.len(), 1, "{e}");
    let i = hits[0];
    assert_eq!(
        untrace(lines[i]),
        Some("llm enrich renvoi docs/b.md : sans entrée dans la réponse de lot docs/a.md +2 [1/1]"),
        "{e}"
    );
    assert!(
        untrace(lines[i - 1])
            .is_some_and(|l| l.starts_with("llm enrich fin lot docs/a.md +2 [1/1] ok (")),
        "{e}"
    );
    assert!(
        lines[i + 1].starts_with("trace: [")
            && lines[i + 1].contains("llm enrich départ docs/b.md [1/1] "),
        "{e}"
    );
    for l in &lines[i - 1..=i + 1] {
        assert!(
            !l.starts_with("avertissement:") && !l.contains("] politique:"),
            "{l}"
        );
    }
    let appels = lines
        .iter()
        .find(|l| untrace(l).unwrap_or(l).starts_with("appels:"))
        .map(|l| untrace(l).unwrap_or(l))
        .unwrap_or_else(|| panic!("{e}"));
    assert!(
        appels.starts_with("appels: 2 appels, 0 nouveaux essais de format, "),
        "{appels}"
    );
}

#[test]
fn req199_ac8_verbose_renvoi_d_un_lot_et_aucun_pour_le_second_passage() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![lot(p, &["a"]), lot(p, &["b", "d"]), single(p, "c")],
        4,
        "batch_files = 4\n",
    );
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    let hits: Vec<&str> = e
        .lines()
        .filter(|l| l.contains("llm enrich renvoi"))
        .collect();
    assert_eq!(hits.len(), 1, "{e}");
    assert!(
        hits[0].ends_with(
            "llm enrich renvoi lot docs/b.md +2 : sans entrée dans la réponse de lot docs/a.md +3 [1/1]"
        ),
        "{e}"
    );

    let s = Setup::ok(
        p,
        vec![
            wrap(p, r#"{"version": 3, "files": []}"#),
            lot(p, &["a", "b"]),
        ],
        2,
        "batch_files = 2\n",
    );
    let o = s.run(&["build", "--verbose"]);
    let e = err(&o);
    let hits: Vec<&str> = e
        .lines()
        .filter(|l| l.contains("llm enrich renvoi"))
        .collect();
    assert_eq!(hits.len(), 1, "{e}");
    assert!(
        hits[0].ends_with(
            "llm enrich renvoi lot docs/a.md +1 : sans entrée dans la réponse de lot docs/a.md +1 [1/1]"
        ),
        "{e}"
    );
}

#[test]
fn req199_ac9_progression_un_fichier_renvoye_avance_une_fois() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![lot(p, &["a", "c"]), single(p, "b")],
        3,
        "batch_files = 3\n",
    );
    let o = s.run(&["build", "--show-progress"]);
    let e = err(&o);
    assert_eq!(
        progress(&e),
        [
            "progression: enrichissement 0 % (0/3 fichiers)",
            "progression: enrichissement 66 % (2/3 fichiers)",
            "progression: enrichissement 100 % (3/3 fichiers)",
        ],
        "{e}"
    );
}

#[test]
fn req200_ac2_lot_non_conforme_coupe_en_deux_moities() {
    for p in PROVIDERS {
        let s = Setup::ok(
            p,
            vec![bad(p), lot(p, &["a", "b"]), lot(p, &["c", "d"])],
            4,
            "batch_files = 4\nformat_retries = 0\n",
        );
        let o = s.run(&["build"]);
        assert!(o.status.success(), "{p} {}", err(&o));
        let posts = s.posts();
        assert_eq!(posts.len(), 3, "{p} {}", err(&o));
        assert_eq!(files_in(&posts[0]), ["a", "b", "c", "d"], "{p}");
        assert_eq!(files_in(&posts[1]), ["a", "b"], "{p}");
        assert_eq!(files_in(&posts[2]), ["c", "d"], "{p}");
        assert!(s.enriched(&NAMES), "{p}");
        assert!(warnings(&err(&o)).is_empty(), "{p} {}", err(&o));
    }
}

#[test]
fn req200_ac3_toujours_non_conforme_nombre_de_requetes() {
    let p = "openai";
    for (batch, count, requests) in [(4, 4, 3), (8, 8, 3), (3, 3, 1)] {
        let s = Setup::ok(
            p,
            vec![bad(p); 20],
            count,
            &format!("batch_files = {batch}\nformat_retries = 0\n"),
        );
        let o = s.run(&["build"]);
        assert!(!o.status.success(), "{batch}");
        assert_eq!(s.posts().len(), requests, "{batch} {}", err(&o));
        let e = err(&o);
        let w = warnings(&e);
        assert_eq!(w.len(), count, "{batch} {e}");
        if count == 4 {
            for (line, n) in w.iter().zip(NAMES) {
                assert!(
                    line.starts_with(&format!("avertissement: {}", path(n))),
                    "{line}"
                );
            }
        }
        assert!(!e.contains("essais sur"), "{e}");
    }
}

#[test]
fn req200_ac5_cle_refusee_apres_le_lot_non_conforme() {
    let p = "openai";
    let s = Setup::new(
        p,
        vec![(200, bad(p)), (401, "{}".into())],
        4,
        "batch_files = 4\nformat_retries = 0\n",
    );
    let o = s.run(&["build"]);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert_eq!(s.posts().len(), 2, "{}", err(&o));
    let e = err(&o);
    let errors: Vec<&str> = e.lines().filter(|l| l.starts_with("erreur:")).collect();
    assert_eq!(errors, [REFUSED], "{e}");
    assert_eq!(s.to_enrich().len(), 4);
}

#[test]
fn req200_ac6_verbose_ligne_decoupe_et_appels() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![bad(p), lot(p, &["a", "b"]), lot(p, &["c", "d"])],
        4,
        "batch_files = 4\nformat_retries = 0\n",
    );
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    let e = err(&o);
    let lines: Vec<&str> = e.lines().collect();
    let k = lines
        .iter()
        .position(|l| l.contains("llm enrich fin lot docs/a.md +3 [1/1] erreur "))
        .unwrap_or_else(|| panic!("{e}"));
    assert_eq!(
        untrace(lines[k + 1]),
        Some("llm enrich découpe lot docs/a.md +3 [1/1] en lot docs/a.md +1 et lot docs/c.md +1"),
        "{e}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("llm enrich découpe"))
            .count(),
        1
    );
    for l in &lines {
        assert!(
            !l.contains("politique:") && !l.contains("(essai ") && !l.starts_with("avertissement:"),
            "{l}"
        );
    }
    let appels = lines
        .iter()
        .find(|l| untrace(l).unwrap_or(l).starts_with("appels:"))
        .map(|l| untrace(l).unwrap_or(l))
        .unwrap_or_else(|| panic!("{e}"));
    assert!(
        appels.starts_with("appels: 3 appels, 0 nouveaux essais de format, 3 envois HTTP"),
        "{appels}"
    );
}

#[test]
fn req200_ac7_deux_essais_sur_deux_par_moitie() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![bad(p); 20],
        4,
        "batch_files = 4\nformat_retries = 1\n",
    );
    let o = s.run(&["build", "--verbose"]);
    assert!(!o.status.success());
    assert_eq!(s.posts().len(), 6, "{}", err(&o));
    let e = err(&o);
    let w = warnings(&e);
    assert_eq!(w.len(), 4, "{e}");
    for (line, n) in w.iter().zip(NAMES) {
        assert!(line.contains(&path(n)), "{line}");
        assert!(line.ends_with(" (2 essais sur 2)"), "{line}");
    }
    assert_eq!(
        e.lines()
            .filter(|l| l.contains("llm enrich découpe"))
            .count(),
        1,
        "{e}"
    );
}

#[test]
fn req200_ac8_progression_par_moitie() {
    let p = "openai";
    let s = Setup::ok(
        p,
        vec![bad(p), lot(p, &["a", "b"]), lot(p, &["c", "d"])],
        4,
        "batch_files = 4\nformat_retries = 0\n",
    );
    let o = s.run(&["build", "--show-progress"]);
    let e = err(&o);
    assert_eq!(
        progress(&e),
        [
            "progression: enrichissement 0 % (0/4 fichiers)",
            "progression: enrichissement 50 % (2/4 fichiers)",
            "progression: enrichissement 100 % (4/4 fichiers)",
        ],
        "{e}"
    );
}
