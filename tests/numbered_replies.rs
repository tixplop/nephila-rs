//! Réponses à numéros de bout en bout (REQ-237, REQ-238) : faux serveur HTTP et fournisseur simulé.
#![allow(non_snake_case)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::cargo::cargo_bin;
use serde_json::{json, Value};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const KEY_VAR: &str = "NEPHILA_T329_KEY";
const PROVIDERS: [&str; 2] = ["openai", "anthropic"];
const A_JAVA: &str = "package p;\npublic class A { void m() {} }\n";

/// Réponse complète du fournisseur (arrêt normal ou, avec `cut`, coupée par la limite de jetons).
fn wrap(provider: &str, text: &str, cut: bool) -> String {
    match (provider, cut) {
        ("anthropic", _) => json!({"content":[{"type":"text","text":text}],
            "stop_reason": if cut { "max_tokens" } else { "end_turn" }})
        .to_string(),
        (_, _) => json!({"choices":[{"message":{"role":"assistant","content":text},
            "finish_reason": if cut { "length" } else { "stop" }}]})
        .to_string(),
    }
}

/// Réponse d'un fichier seul : `(numéro, texte)`.
fn file_reply(provider: &str, sums: &[(i64, &str)]) -> String {
    let s: Vec<Value> = sums
        .iter()
        .map(|(n, t)| json!({"n": n, "summary": t}))
        .collect();
    wrap(
        provider,
        &json!({"summaries": s, "concepts": []}).to_string(),
        false,
    )
}

/// Entrée de lot : `(f, [(n, texte)])`.
fn entries(files: &[(i64, Vec<(i64, &str)>)]) -> Value {
    let v: Vec<Value> = files
        .iter()
        .map(|(f, sums)| {
            let s: Vec<Value> = sums
                .iter()
                .map(|(n, t)| json!({"n": n, "summary": t}))
                .collect();
            json!({"f": f, "summaries": s, "concepts": []})
        })
        .collect();
    json!({ "files": v })
}

fn lot_reply(provider: &str, files: &[(i64, Vec<(i64, &str)>)]) -> String {
    wrap(provider, &entries(files).to_string(), false)
}

struct Setup {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
    provider: String,
}

impl Setup {
    /// `files` : `(chemin, contenu)` ; `batch` : fichiers par requête ; une réponse par requête.
    fn new(provider: &str, replies: Vec<String>, files: &[(&str, &str)], batch: usize) -> Setup {
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
                "provider = \"{provider}\"\nmodel = \"m\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\nconcurrency = 1\nbatch_files = {batch}\nenrich_code = true\n",
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

    /// Texte utilisateur de chaque requête d'enrichissement.
    fn prompts(&self) -> Vec<String> {
        let path = if self.provider == "anthropic" {
            "/v1/messages"
        } else {
            "/v1/chat/completions"
        };
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == path)
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

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn warnings(o: &Output) -> Vec<String> {
    err(o)
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect()
}

const DOCS: [(&str, &str); 2] = [
    ("docs/a.md", "# A\n\ntexte a\n"),
    ("docs/b.md", "# B\n\ntexte b\n"),
];

#[test]
fn REQ237_AC6_reponse_a_numeros_une_requete_par_lot() {
    for p in PROVIDERS {
        for batch in [1, 2] {
            let replies = if batch == 1 {
                vec![
                    file_reply(p, &[(1, "Doc a"), (2, "Sec a")]),
                    file_reply(p, &[(1, "Doc b"), (2, "Sec b")]),
                ]
            } else {
                vec![lot_reply(
                    p,
                    &[
                        (1, vec![(1, "Doc a"), (2, "Sec a")]),
                        (2, vec![(3, "Doc b"), (4, "Sec b")]),
                    ],
                )]
            };
            let expected_requests = replies.len();
            let s = Setup::new(p, replies, &DOCS, batch);
            let o = s.run(&["build"]);
            let tag = format!("{p} batch={batch} {}", err(&o));
            assert!(o.status.success(), "{tag}");
            assert_eq!(s.prompts().len(), expected_requests, "{tag}");
            assert!(warnings(&o).is_empty(), "{tag}");
            for (id, t) in [
                ("document:docs/a.md", "Doc a"),
                ("section:docs/a.md#a", "Sec a"),
                ("document:docs/b.md", "Doc b"),
                ("section:docs/b.md#b", "Sec b"),
            ] {
                assert_eq!(s.summary_of(id).as_deref(), Some(t), "{id} {tag}");
            }
            assert!(out(&o).contains("4/4 nœuds résumés"), "{tag} {}", out(&o));
        }
    }
}

#[test]
fn REQ237_AC7_lot_reduit_et_sous_appel_renumerotes() {
    for p in PROVIDERS {
        // Lot de 3 fichiers coupé après l'entrée f = 1 : le reste repart renuméroté.
        let cut = r#"{"files":[{"f":1,"summaries":[{"n":1,"summary":"Premier."}],"concepts":[]},{"f":2,"summ"#;
        let s = Setup::new(
            p,
            vec![
                wrap(p, cut, true),
                lot_reply(p, &[(1, vec![(1, "Doc a")]), (2, vec![(3, "Doc b")])]),
            ],
            &[
                ("A.java", A_JAVA),
                ("docs/a.md", DOCS[0].1),
                ("docs/b.md", DOCS[1].1),
            ],
            3,
        );
        let o = s.run(&["build"]);
        let tag = format!("{p} {}", err(&o));
        assert!(o.status.success(), "{tag}");
        let prompts = s.prompts();
        assert_eq!(prompts.len(), 2, "{tag}");
        assert!(
            prompts[0].contains("=== FICHIER [3] : docs/b.md ==="),
            "{tag}"
        );
        assert!(
            prompts[1].contains("=== FICHIER [1] : docs/a.md ===")
                && prompts[1].contains("=== FICHIER [2] : docs/b.md ===")
                && !prompts[1].contains("A.java")
                && prompts[1].contains("[1] a.md — document:docs/a.md")
                && prompts[1].contains("[3] b.md — document:docs/b.md"),
            "{tag}\n{}",
            prompts[1]
        );
        assert_eq!(
            s.summary_of("method:p.A.m").as_deref(),
            Some("Premier."),
            "{tag}"
        );
        assert_eq!(
            s.summary_of("document:docs/a.md").as_deref(),
            Some("Doc a"),
            "{tag}"
        );
        assert_eq!(
            s.summary_of("document:docs/b.md").as_deref(),
            Some("Doc b"),
            "{tag}"
        );
        assert!(warnings(&o).is_empty(), "{tag}");

        // Appel tronqué sans élément complet : deux sous-appels, chacun numéroté depuis 1.
        let mut big = String::from("public class B {\n");
        for i in 0..8 {
            big.push_str(&format!("    void m{i}() {{}}\n"));
        }
        big.push_str("}\n");
        let s = Setup::new(
            p,
            vec![
                wrap(p, r#"{"summaries":[{"n":1,"summ"#, true),
                file_reply(p, &[(1, "Premier."), (5, "Cinquième.")]),
                file_reply(p, &[(1, "Sixième."), (4, "Classe B centrale du module.")]),
            ],
            &[("B.java", &big)],
            1,
        );
        let o = s.run(&["build"]);
        let tag = format!("{p} {}", err(&o));
        assert!(o.status.success(), "{tag}");
        let prompts = s.prompts();
        assert_eq!(prompts.len(), 3, "{tag}");
        assert!(prompts[1].contains("[1] m0 — method:B.m0"), "{tag}");
        assert!(prompts[2].contains("[1] m5 — method:B.m5"), "{tag}");
        assert!(!prompts[2].contains("[5]"), "{tag}");
        for (id, t) in [
            ("method:B.m0", "Premier."),
            ("method:B.m4", "Cinquième."),
            ("method:B.m5", "Sixième."),
            ("type:B", "Classe B centrale du module."),
        ] {
            assert_eq!(s.summary_of(id).as_deref(), Some(t), "{id} {tag}");
        }
        assert!(warnings(&o).is_empty(), "{tag}");

        // Appel tronqué après un élément complet : le sous-appel reçoit le reste, renuméroté.
        let s = Setup::new(
            p,
            vec![
                wrap(
                    p,
                    r#"{"summaries":[{"n":1,"summary":"Premier."},{"n":2,"summ"#,
                    true,
                ),
                file_reply(p, &[(1, "Deuxième méthode du module.")]),
            ],
            &[("B.java", &big)],
            1,
        );
        let o = s.run(&["build"]);
        let tag = format!("{p} {}", err(&o));
        assert!(o.status.success(), "{tag}");
        let prompts = s.prompts();
        assert_eq!(prompts.len(), 2, "{tag}");
        assert!(prompts[1].contains("[1] m1 — method:B.m1"), "{tag}");
        assert!(!prompts[1].contains("method:B.m0"), "{tag}");
        assert_eq!(
            s.summary_of("method:B.m0").as_deref(),
            Some("Premier."),
            "{tag}"
        );
        assert_eq!(
            s.summary_of("method:B.m1").as_deref(),
            Some("Deuxième méthode du module."),
            "{tag}"
        );
    }
}

/// Copie récursive d'un dossier.
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Construit `maven2` avec le simulé ; renvoie (stdout de l'export, avertissements, stderr).
fn mock_build(batch: usize, mock: &Path) -> (String, Vec<String>, String) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven2"),
        &repo,
    );
    let cfg = tmp.path().join("c.toml");
    fs::write(
        &cfg,
        format!("provider=\"mock\"\nenrich_code = true\nmodel=\"mock\"\nconcurrency=1\nbatch_files={batch}\n"),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("NEPHILA_CONFIG", &cfg)
            .env("NEPHILA_MOCK_DIR", mock)
            .output()
            .unwrap()
    };
    let b = run(&["build"]);
    let e = err(&b);
    let mut w: Vec<String> = e
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .map(String::from)
        .collect();
    w.sort();
    (out(&run(&["export", "--output", "-"])), w, e)
}

#[test]
fn REQ237_AC9_mock_fixtures_a_identifiants_inchangees() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock");
    let (g1, w1, e1) = mock_build(1, &fixtures);
    assert!(g1.contains("\"summary\":\"Place les commandes.\""), "{e1}");
    let (g2, w2, _) = mock_build(2, &fixtures);
    assert_eq!(g1, g2);
    assert_eq!(w1, w2);

    // Fixture sans `version` : même erreur qu'AC-201-5, quelle que soit la taille des lots.
    let tmp = tempfile::tempdir().unwrap();
    let mock = tmp.path().join("mock");
    copy_dir(&fixtures, &mock);
    let guide = mock.join("docs/guide.md.json");
    let text = fs::read_to_string(&guide).unwrap();
    fs::write(&guide, text.replacen("\"version\":2,", "", 1)).unwrap();
    for batch in [1, 2] {
        let (_, _, e) = mock_build(batch, &mock);
        assert!(e.contains("missing field `version`"), "batch={batch} {e}");
    }
}

#[test]
fn REQ238_AC1_numero_hors_liste_une_ligne() {
    for p in PROVIDERS {
        let s = Setup::new(
            p,
            vec![file_reply(
                p,
                &[(1, "Exécute le traitement principal."), (9, "Fantôme.")],
            )],
            &[("A.java", A_JAVA)],
            1,
        );
        let o = s.run(&["build"]);
        let tag = format!("{p} {}", err(&o));
        assert!(o.status.success(), "{tag}");
        assert_eq!(
            warnings(&o),
            ["avertissement: A.java : 1 élément(s) d'enrichissement écarté(s) : résumé pour le numéro 9 (hors liste)"],
            "{tag}"
        );
        assert_eq!(
            s.summary_of("method:p.A.m").as_deref(),
            Some("Exécute le traitement principal."),
            "{tag}"
        );
        assert!(
            out(&o).contains("0 fichiers à enrichir"),
            "{tag} {}",
            out(&o)
        );
    }
}

#[test]
fn REQ238_AC2_entree_hors_du_lot_sur_le_premier_fichier() {
    for p in PROVIDERS {
        let s = Setup::new(
            p,
            vec![lot_reply(
                p,
                &[
                    (1, vec![(1, "Exécute le traitement principal.")]),
                    (2, vec![(3, "Doc a")]),
                    (7, vec![(1, "Perdu.")]),
                ],
            )],
            &[("A.java", A_JAVA), ("docs/a.md", DOCS[0].1)],
            2,
        );
        let o = s.run(&["build"]);
        let tag = format!("{p} {}", err(&o));
        assert!(o.status.success(), "{tag}");
        let w = warnings(&o);
        assert_eq!(w.len(), 1, "{tag}");
        assert!(
            w[0].starts_with("avertissement: A.java : 1 élément(s) d'enrichissement écarté(s) : ")
                && w[0].ends_with("entrée pour le fichier numéro 7 (hors du lot)"),
            "{tag}"
        );
        assert_eq!(
            s.summary_of("document:docs/a.md").as_deref(),
            Some("Doc a"),
            "{tag}"
        );
    }
}

#[test]
fn REQ238_AC5_numero_d_un_autre_fichier_ecarte_dans_le_lot() {
    // `A.md` et non `a.md` : les fichiers sont triés par chemin, `B.java` passerait avant `a.md`.
    for p in PROVIDERS {
        let s = Setup::new(
            p,
            vec![lot_reply(
                p,
                &[
                    (1, vec![(1, "Document de présentation.")]),
                    (2, vec![(1, "Intrus.")]),
                ],
            )],
            &[
                ("A.md", "# A\n\ntexte a\n"),
                ("B.java", "package p;\nclass B {}\n"),
            ],
            2,
        );
        let o = s.run(&["build"]);
        let tag = format!("{p} {}", err(&o));
        assert!(o.status.success(), "{tag}");
        assert_eq!(
            warnings(&o),
            ["avertissement: B.java : 1 élément(s) d'enrichissement écarté(s) : résumé pour le numéro 1 (hors liste)"],
            "{tag}"
        );
        assert_eq!(s.summary_of("type:p.B"), None, "{tag}");
        assert_eq!(
            s.summary_of("document:A.md").as_deref(),
            Some("Document de présentation."),
            "{tag}"
        );
    }
}

#[test]
fn REQ238_AC6_bilan_et_ligne_fin_comptent_le_nœud_résumé() {
    let p = "openai";
    let s = Setup::new(
        p,
        vec![file_reply(
            p,
            &[(1, "Exécute le traitement principal."), (9, "Fantôme.")],
        )],
        &[("A.java", A_JAVA)],
        1,
    );
    let o = s.run(&["build", "--verbose"]);
    let tag = err(&o);
    assert!(o.status.success(), "{tag}");
    assert!(out(&o).contains("1/2 nœuds résumés"), "{}", out(&o));
    let fin: Vec<&str> = tag
        .lines()
        .filter(|l| l.contains("llm enrich fin") && l.contains("A.java"))
        .collect();
    assert_eq!(fin.len(), 1, "{tag}");
    assert!(
        fin[0].contains("ok (1 résumés, 0 concepts, 1/2 nœuds résumés)"),
        "{}",
        fin[0]
    );
}
