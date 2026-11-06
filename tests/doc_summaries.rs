//! Nœud documenté : sa doc est son résumé, il n'est plus demandé au LLM (REQ-244, DEC-180) :
//! tests d'intégration sur le faux serveur `openai` et sur `build --no-llm`.
#![allow(non_snake_case)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use serde_json::{json, Value};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const KEY_VAR: &str = "NEPHILA_T334_KEY";
const LIVRE: &str = "package p;\n\n/** Un livre du catalogue de la librairie. */\npublic class Livre {\n    /** Renvoie le titre complet du livre. */\n    public String titre() { return \"t\"; }\n    public int prix() { return 1; }\n}\n";
const LIVRE_PATH: &str = "src/p/Livre.java";
const CLASSE: &str = "type:p.Livre";
const TITRE: &str = "method:p.Livre.titre";
const PRIX: &str = "method:p.Livre.prix";
const DOC_CLASSE: &str = "Un livre du catalogue de la librairie.";
const DOC_TITRE: &str = "Renvoie le titre complet du livre.";
const TOUT_DOCUMENTE: &str = "package p;\n\n/** Un livre du catalogue de la librairie. */\npublic class Doc {\n    /** Renvoie le titre complet du livre. */\n    public String titre() { return \"t\"; }\n}\n";

fn wrap(text: &str) -> String {
    json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]})
        .to_string()
}

/// Réponse d'un fichier seul : `(numéro, texte)`.
fn file_reply(sums: &[(i64, &str)]) -> String {
    let s: Vec<Value> = sums
        .iter()
        .map(|(n, t)| json!({"n": n, "summary": t}))
        .collect();
    wrap(&json!({"summaries": s, "concepts": []}).to_string())
}

struct Setup {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
}

impl Setup {
    fn new(replies: Vec<String>, files: &[(&str, &str)], batch: usize) -> Setup {
        let server = FakeServer::start(replies.into_iter().map(|b| (200, b)).collect());
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        for (p, c) in files {
            let f = repo.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, c).unwrap();
        }
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"openai\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = {batch}\nenrich_code = true\n",
                server.base_url
            ),
        )
        .unwrap();
        Setup {
            _tmp: tmp,
            repo,
            config,
            server,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env(KEY_VAR, "sk-test")
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(o.status.success(), "{args:?} : {}", text(&o.stderr));
        o
    }

    fn write(&self, path: &str, content: &str) {
        fs::write(self.repo.join(path), content).unwrap();
    }

    /// Texte utilisateur de chaque requête d'enrichissement.
    fn prompts(&self) -> Vec<String> {
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == "/v1/chat/completions")
            .map(|r| user_text(&serde_json::from_str(&r.body).unwrap()))
            .collect()
    }

    fn summary_of(&self, id: &str) -> Option<String> {
        nephila::store::Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|x| x.id == id)
            .and_then(|x| x.summary)
    }
}

fn user_text(v: &Value) -> String {
    match v {
        Value::String(s) if s.contains("Nœuds du fichier") => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(user_text)
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        Value::Object(o) => o
            .values()
            .map(user_text)
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// La liste numérotée d'un prompt, sans le contenu du fichier.
fn list_of(prompt: &str) -> &str {
    let start = prompt.find("Nœuds du fichier :").unwrap();
    let end = prompt.find("Contenu :").unwrap();
    &prompt[start..end]
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn out(o: &Output) -> String {
    text(&o.stdout)
}

#[test]
fn REQ244_AC1_no_llm_le_resume_d_un_noeud_documente_est_sa_doc() {
    let s = Setup::new(vec![], &[(LIVRE_PATH, LIVRE)], 1);
    s.ok(&["build", "--no-llm"]);
    let o = s.ok(&["export", "--format", "json", "--output", "-"]);
    let doc: Value = serde_json::from_str(&out(&o)).unwrap();
    let summary = |id: &str| -> Value {
        doc["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap_or_else(|| panic!("{id} absent"))["summary"]
            .clone()
    };
    assert_eq!(summary(CLASSE), json!(DOC_CLASSE));
    assert_eq!(summary(TITRE), json!(DOC_TITRE));
    assert!(summary(PRIX).is_null());
    assert!(s.server.requests().is_empty());
}

#[test]
fn REQ244_AC2_un_seul_noeud_demande_les_documentes_gardent_leur_doc() {
    let s = Setup::new(vec![file_reply(&[(1, "Rôle.")])], &[(LIVRE_PATH, LIVRE)], 1);
    let o = s.ok(&["build", "--verbose"]);
    let prompts = s.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    let list = list_of(&prompts[0]);
    assert!(list.contains(PRIX), "{list}");
    assert!(!list.contains(CLASSE) && !list.contains(TITRE), "{list}");
    assert_eq!(list.matches("[1]").count(), 1);
    assert!(!list.contains("[2]"), "{list}");
    assert_eq!(s.summary_of(PRIX).as_deref(), Some("Rôle."));
    assert_eq!(s.summary_of(CLASSE).as_deref(), Some(DOC_CLASSE));
    assert_eq!(s.summary_of(TITRE).as_deref(), Some(DOC_TITRE));
    assert!(out(&o).contains(", 0 fichiers à enrichir"), "{}", out(&o));
    assert!(
        text(&o.stderr).contains("1/1 nœuds résumés"),
        "{}",
        text(&o.stderr)
    );
    s.ok(&["update"]);
    assert_eq!(s.prompts().len(), 1);
}

#[test]
fn REQ244_AC3_fichier_entierement_documente_aucune_requete() {
    let s = Setup::new(vec![], &[("src/p/Doc.java", TOUT_DOCUMENTE)], 1);
    let d = s.ok(&["build", "--dry-run"]);
    assert!(
        out(&d).contains("estimation minimale : 0 appels, "),
        "{}",
        out(&d)
    );
    let o = s.ok(&["build"]);
    assert!(s.server.requests().is_empty());
    assert!(out(&o).contains(", 0 fichiers à enrichir"), "{}", out(&o));
    assert_eq!(s.summary_of("type:p.Doc").as_deref(), Some(DOC_CLASSE));
    assert_eq!(
        s.summary_of("method:p.Doc.titre").as_deref(),
        Some(DOC_TITRE)
    );
}

#[test]
fn REQ244_AC4_un_fichier_documente_sort_du_lot() {
    let b = "package p;\npublic class B {\n    public int prix() { return 1; }\n}\n";
    // Lot de B.java (type, méthode) et c.md (document, section) : numéros 1 à 4.
    let lot = json!({"files": [
        {"f": 1, "summaries": [{"n": 1, "summary": "B."}, {"n": 2, "summary": "prix."}], "concepts": []},
        {"f": 2, "summaries": [{"n": 3, "summary": "C."}, {"n": 4, "summary": "Sec."}], "concepts": []}
    ]});
    let s = Setup::new(
        vec![wrap(&lot.to_string())],
        &[
            ("src/p/A.java", TOUT_DOCUMENTE),
            ("src/p/B.java", b),
            ("c.md", "# C\n\ntexte de c\n"),
        ],
        3,
    );
    let o = s.ok(&["build"]);
    let requests = s.server.requests();
    let posts: Vec<_> = requests.iter().filter(|r| r.method == "POST").collect();
    assert_eq!(posts.len(), 1, "{}", text(&o.stderr));
    let body = &posts[0].body;
    assert!(body.contains("src/p/B.java") && body.contains("c.md"));
    assert!(!body.contains("=== FICHIER [1] : src/p/A.java"), "{body}");
    assert!(!body.contains(": src/p/A.java"), "{body}");
    assert!(out(&o).contains(", 0 fichiers à enrichir"), "{}", out(&o));
}

#[test]
fn REQ244_AC5_relecture_et_modifications_demandent_les_bons_noeuds() {
    let s = Setup::new(
        vec![
            file_reply(&[(1, "Rôle.")]),
            file_reply(&[(1, "Rôle bis.")]),
            file_reply(&[(1, "Rôle ter."), (2, "Titre bis.")]),
        ],
        &[(LIVRE_PATH, LIVRE)],
        1,
    );
    s.ok(&["build"]);
    assert_eq!(s.prompts().len(), 1);
    // `build` sans changement : aucune requête, mêmes résumés.
    s.ok(&["build"]);
    assert_eq!(s.prompts().len(), 1);
    assert_eq!(s.summary_of(PRIX).as_deref(), Some("Rôle."));
    assert_eq!(s.summary_of(CLASSE).as_deref(), Some(DOC_CLASSE));
    // Le corps de `prix()` change : seul `prix` est demandé.
    s.write(LIVRE_PATH, &LIVRE.replace("return 1;", "return 2;"));
    s.ok(&["update"]);
    let prompts = s.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    let list = list_of(&prompts[1]);
    assert!(
        list.contains(PRIX) && !list.contains(TITRE) && !list.contains(CLASSE),
        "{list}"
    );
    assert_eq!(s.summary_of(PRIX).as_deref(), Some("Rôle bis."));
    assert_eq!(s.summary_of(CLASSE).as_deref(), Some(DOC_CLASSE));
    assert_eq!(s.summary_of(TITRE).as_deref(), Some(DOC_TITRE));
    // La doc de `titre()` est retirée : `titre` et `prix` sont demandés.
    s.write(
        LIVRE_PATH,
        &LIVRE
            .replace("return 1;", "return 2;")
            .replace("    /** Renvoie le titre complet du livre. */\n", ""),
    );
    s.ok(&["update"]);
    let prompts = s.prompts();
    assert_eq!(prompts.len(), 3, "{prompts:?}");
    let list = list_of(&prompts[2]);
    assert!(
        list.contains(PRIX) && list.contains(TITRE) && !list.contains(CLASSE),
        "{list}"
    );
    assert_eq!(s.summary_of(TITRE).as_deref(), Some("Titre bis."));
    assert_eq!(s.summary_of(PRIX).as_deref(), Some("Rôle ter."));
    assert_eq!(s.summary_of(CLASSE).as_deref(), Some(DOC_CLASSE));
}
