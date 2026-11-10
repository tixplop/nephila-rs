//! Passe `--subsystems` de `build` et `update` (REQ-261, REQ-262, T-353) : sélection des
//! communautés, cache, plafond, échecs, arrêt, usage, textes de stderr.
#![cfg(unix)]

use assert_cmd::cargo::cargo_bin;
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[path = "support/fake_server.rs"]
#[allow(dead_code)]
mod fake_server;
use fake_server::{FakeServer, Reply};

const KEY_VAR: &str = "NEPHILA_T353_KEY";

/// Groupe de classes en anneau (`C<i>` référence `C<i+1>` et `C<i+2>`), dans le paquet `p<g>` :
/// `classes` classes plus le nœud du paquet. Les `docs` premières ont un commentaire de
/// documentation, donc un résumé sans appel LLM.
fn group(files: &mut Vec<(String, String)>, g: usize, classes: usize, docs: usize) {
    for i in 0..classes {
        let (n, m) = ((i + 1) % classes, (i + 2) % classes);
        let doc = if i < docs {
            format!("/** Classe {i} du groupe {g}. */\n")
        } else {
            String::new()
        };
        files.push((
            format!("src/p{g}/C{i}.java"),
            format!(
                "package p{g};\n{doc}public class C{i} {{\n  private C{n} next;\n  private C{m} other;\n}}\n"
            ),
        ));
    }
}

/// Trois communautés de 14, 13 et 12 membres, chacune avec 5 résumés.
fn three_groups() -> Vec<(String, String)> {
    let mut f = Vec::new();
    group(&mut f, 0, 13, 5);
    group(&mut f, 1, 12, 5);
    group(&mut f, 2, 11, 5);
    f
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

impl Env {
    fn with(files: &[(String, String)], config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&mock).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let env = Env {
            log: tmp.path().join("mock.log"),
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
            repo,
            mock,
        };
        for (p, c) in files {
            env.write(p, c);
        }
        fs::write(&env.config, config).unwrap();
        env
    }

    /// LLM simulé, `concurrency` 1 et `format_retries` 2 (défaut), `settings` en plus.
    fn mock(files: &[(String, String)], settings: &str) -> Env {
        Env::with(
            files,
            &format!("provider = \"mock\"\nmodel = \"mock\"\nconcurrency = 1\nbatch_files = 1\n{settings}"),
        )
    }

    fn server(files: &[(String, String)], url: &str, settings: &str) -> Env {
        Env::with(
            files,
            &format!(
                "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\napi_key_env = \"{KEY_VAR}\"\nconcurrency = 1\nstructured_output = \"off\"\n{settings}"
            ),
        )
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
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

    /// Lignes du journal du LLM simulé.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn subsystem_calls(&self) -> usize {
        self.calls()
            .iter()
            .filter(|l| l.starts_with("_subsystem"))
            .count()
    }

    fn db(&self) -> Connection {
        Connection::open(self.repo.join(".nephila/graph.db")).unwrap()
    }

    /// `(libellé, description)` de la communauté qui contient le paquet `p<g>`.
    fn community_of(&self, g: usize) -> (String, Option<String>) {
        self.db()
            .query_row(
                "SELECT c.label, c.description FROM communities c
                 JOIN node_communities m ON m.community = c.id
                 JOIN nodes n ON n.id = m.node
                 WHERE n.id LIKE ?1 ORDER BY c.size DESC LIMIT 1",
                [format!("%p{g}.C0")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    fn labels(&self) -> Vec<String> {
        let db = self.db();
        let mut st = db
            .prepare("SELECT label FROM communities ORDER BY id")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Contenu des tables que la passe écrit (`communities`, `subsystem_names`), rowid compris.
    /// Le fichier lui-même est réécrit par toute commande `update` avec LLM (méta-données).
    fn state(&self) -> Vec<String> {
        let db = self.db();
        let mut rows = Vec::new();
        for (table, cols) in [
            (
                "communities",
                "id, label, pivot, size, cohesion, description",
            ),
            ("subsystem_names", "rowid, fingerprint, name, description"),
        ] {
            let mut st = db
                .prepare(&format!("SELECT {cols} FROM {table} ORDER BY 1"))
                .unwrap();
            let n = st.column_count();
            let found = st
                .query_map([], |r| {
                    let mut line = format!("{table}:");
                    for i in 0..n {
                        line.push_str(&format!("{:?}|", r.get_ref(i).unwrap()));
                    }
                    Ok(line)
                })
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            rows.extend(found);
        }
        rows
    }
}

fn last(s: &str) -> &str {
    s.lines().last().unwrap_or("")
}

fn named_count(env: &Env) -> usize {
    env.db()
        .query_row(
            "SELECT COUNT(*) FROM communities WHERE description IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as usize
}

fn valid_reply(name: &str) -> String {
    let content = serde_json::json!({"name": name, "description": "Ce qu'il fait."}).to_string();
    serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]
    })
    .to_string()
}

// ---- AC-261-2 : communautés retenues ----

#[test]
fn req261_ac2_only_the_retained_community_is_named() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5); // 12 membres, 5 résumés : retenue
    group(&mut f, 1, 8, 5); // 9 membres : trop petite
    group(&mut f, 2, 11, 2); // 12 membres, 2 résumés : trop peu de lignes
    let env = Env::mock(&f, "");
    let o = env.run(&["build", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.subsystem_calls(), 1, "{:?}", env.calls());
    let (label, description) = env.community_of(0);
    assert!(label.starts_with("Sous-système"), "{label}");
    assert!(description.is_some());
    for g in [1, 2] {
        let (label, description) = env.community_of(g);
        assert_eq!(label, format!("p{g}"));
        assert_eq!(description, None);
    }
}

// ---- AC-261-3 : contenu de la requête ----

#[test]
fn req261_ac3_request_carries_the_exact_lines_and_no_file_content() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    f[0].1.push_str("// SECRET_SOURCE_BODY\n");
    let server = FakeServer::start(vec![(200, valid_reply("Noyau"))]);
    let env = Env::server(&f, &server.base_url, "");
    let o = env.run(&["build", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    let body: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    let sent = body.to_string();
    assert!(!sent.contains("SECRET_SOURCE_BODY"), "{sent}");
    assert!(!sent.contains("Liens :"), "{sent}");
    let user = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let lines: Vec<&str> = user.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(lines.len(), 5, "{user}");
    for i in 0..5 {
        let expected = format!("- type:p0.C{i} : Classe {i} du groupe 0.");
        assert!(lines.contains(&expected.as_str()), "{expected} / {user}");
    }
    assert_eq!(env.community_of(0).0, "Noyau");
}

// ---- AC-261-4 : sans l'option, rien ne change ; usage ----

#[test]
fn req261_ac4_without_the_option_no_subsystem_call() {
    let env = Env::mock(&three_groups(), "");
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(0));
    let (out, err) = text(&env.run(&["update"]));
    assert_eq!(out, "graphe à jour\n");
    assert_eq!(err, "");
    assert_eq!(env.subsystem_calls(), 0);
    assert!(!text(&o).0.contains("sous-systèmes"));
    assert_eq!(named_count(&env), 0);
}

#[test]
fn req261_ac4_ac14_usage_errors_write_nothing() {
    let env = Env::mock(&three_groups(), "");
    for other in ["--no-llm", "--dry-run"] {
        for args in [
            ["build", "--subsystems", other],
            ["build", other, "--subsystems"],
        ] {
            let o = env.run(&args);
            let (_, err) = text(&o);
            assert_eq!(o.status.code(), Some(2), "{err}");
            assert!(err.contains("cannot be used with"), "{err}");
            assert!(err.contains("--subsystems") && err.contains(other), "{err}");
        }
    }
    let o = env.run(&["update", "--subsystems", "--no-llm"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(!env.repo.join(".nephila").exists());
    let o = env.run(&["status", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        err.contains("unexpected argument '--subsystems' found"),
        "{err}"
    );
}

// ---- AC-261-5, AC-261-13 : fichiers à enrichir ----

#[test]
fn req261_ac5_ac13_files_left_to_enrich_skip_the_pass() {
    let mut f = three_groups();
    f.push(("doc.md".into(), "# Doc\n\nTexte.\n".into()));
    let env = Env::mock(&f, "");
    let o = env.run_env(
        &["build", "--subsystems"],
        &[("NEPHILA_MOCK_MALFORMED", "doc.md:9")],
    );
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(env.subsystem_calls(), 0, "{:?}", env.calls());
    assert!(
        err.lines().any(|l| l
            == "avertissement: sous-systèmes non nommés : 1 fichiers restent à enrichir ; relancez nephila update --subsystems"),
        "{err}"
    );
    // A-21 : la ligne est écrite, chaque communauté retenue est laissée (V-002).
    assert!(
        out.lines().any(|l| l
            == "sous-systèmes : 0 communautés nommées, 0 reprises sans appel, 3 laissées, 1 non retenues (moins de 10 membres ou de 3 résumés)"),
        "{out}"
    );
    // Le code et la dernière ligne sont ceux d'un build sans l'option.
    assert_eq!(last(&err), "erreur: 1 fichier(s) à enrichir");
    assert_eq!(named_count(&env), 0);
}

#[test]
fn req261_ac13_without_configuration_a_warning_and_code_zero() {
    let env = Env::mock(&three_groups(), "");
    let missing = env.config.with_file_name("absente.toml");
    for cmd in ["build", "update"] {
        let o = env.run_env(
            &[cmd, "--subsystems"],
            &[("NEPHILA_CONFIG", missing.to_str().unwrap())],
        );
        let (out, err) = text(&o);
        assert_eq!(o.status.code(), Some(0), "{err}");
        let expected = format!(
            "avertissement: sous-systèmes non nommés : aucune configuration LLM ; créez {}",
            missing.display()
        );
        assert!(err.lines().any(|l| l == expected), "{err}");
        assert!(!out.contains("sous-systèmes :"), "{out}");
    }
    assert_eq!(env.subsystem_calls(), 0);
}

// ---- AC-261-6, AC-261-13 : réponses invalides ----

#[test]
fn req261_ac13_invalid_replies_use_fixed_labels_and_leak_nothing() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    let env = Env::mock(&f, "");
    let long_name = format!("SECRETNAME{}", "x".repeat(51));
    fs::write(
        env.mock.join("_subsystem.json"),
        serde_json::json!({"name": long_name, "description": "SECRETDESC"}).to_string(),
    )
    .unwrap();
    let o = env.run(&["build", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(env.subsystem_calls(), 3, "trois essais");
    let warning = err.lines().find(|l| l.contains("communauté 0")).unwrap();
    assert_eq!(
        warning,
        "avertissement: communauté 0 p0 : réponse LLM non conforme : name vide ou de plus de 60 caractères (3 essais sur 3)"
    );
    assert!(!err.contains("SECRET"), "{err}");
    assert_eq!(last(&err), "erreur: 1 communauté(s) à nommer");
    assert_eq!(env.community_of(0), ("p0".to_string(), None));
}

// ---- AC-261-7 : un échec sur trois ; clé refusée ----

#[test]
fn req261_ac7_one_failure_in_three_keeps_the_others() {
    let env = Env::mock(&three_groups(), "");
    // Les trois premiers appels sont non conformes : la communauté 0 épuise ses 3 essais.
    let o = env.run_env(
        &["build", "--subsystems"],
        &[("NEPHILA_MOCK_MALFORMED", "_subsystem:3")],
    );
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(env.subsystem_calls(), 5);
    assert!(
        err.lines()
            .any(|l| l.starts_with("avertissement: communauté 0 p0 : ")
                && l.ends_with("(3 essais sur 3)")),
        "{err}"
    );
    assert_eq!(last(&err), "erreur: 1 communauté(s) à nommer");
    assert_eq!(named_count(&env), 2);
    assert_eq!(env.community_of(0), ("p0".to_string(), None));
    // Relance : seule la communauté restante repart.
    let o = env.run(&["update", "--subsystems"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    assert_eq!(env.subsystem_calls(), 6);
    assert_eq!(named_count(&env), 3);
}

#[test]
fn req261_ac7_refused_key_stops_the_pass_with_the_key_error() {
    let env = Env::mock(&three_groups(), "");
    let o = env.run_env(
        &["build", "--subsystems"],
        &[("NEPHILA_MOCK_KEY_REVOKED_AFTER", "0")],
    );
    let (_, err) = text(&o);
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(env.subsystem_calls(), 1, "aucune autre tentative");
    assert!(
        last(&err).starts_with("erreur: clé d'API refusée par le fournisseur"),
        "{err}"
    );
    assert!(!err.contains("communauté(s) à nommer"), "{err}");
}

#[test]
fn req261_rejected_request_leaves_the_community_and_goes_on() {
    let mut f = Vec::new();
    group(&mut f, 0, 12, 5);
    group(&mut f, 1, 11, 5);
    let server = FakeServer::start_replies(vec![
        Reply::status(400, "{\"error\":\"mauvaise requête\"}"),
        Reply::status(200, &valid_reply("Second")),
    ]);
    let env = Env::server(&f, &server.base_url, "");
    let o = env.run(&["build", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        err.lines()
            .any(|l| l.starts_with("avertissement: communauté 0 p0 : ")),
        "{err}"
    );
    assert_eq!(last(&err), "erreur: 1 communauté(s) à nommer");
    assert_eq!(env.community_of(1).0, "Second");
    assert_eq!(env.community_of(0), ("p0".to_string(), None));
}

/// Quatre groupes de 12 à 15 membres (dans l'ordre de leur numéro).
fn four_groups() -> Vec<(String, String)> {
    let mut f = Vec::new();
    group(&mut f, 0, 14, 5);
    group(&mut f, 1, 13, 5);
    group(&mut f, 2, 12, 5);
    group(&mut f, 3, 11, 5);
    f
}

fn server_errors(code: u16, n: usize) -> Vec<Reply> {
    (0..n)
        .map(|_| Reply::status(code, "{\"error\":\"x\"}"))
        .collect()
}

#[test]
fn req261_ac7_ac13_server_errors_for_one_community_in_three() {
    let mut replies = server_errors(500, 5);
    replies.push(Reply::status(200, &valid_reply("Deux")));
    replies.push(Reply::status(200, &valid_reply("Trois")));
    let server = FakeServer::start_replies(replies);
    let env = Env::server(&three_groups(), &server.base_url, "");
    let o = env.run(&["build", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(server.requests().len(), 7);
    let line = err
        .lines()
        .find(|l| l.starts_with("avertissement: communauté 0 p0 : "))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(line.contains(" : erreur de openai (HTTP 500)"), "{line}");
    assert_eq!(last(&err), "erreur: 1 communauté(s) à nommer");
    assert_eq!(named_count(&env), 2);
}

#[test]
fn req261_network_stop_during_the_pass_names_the_command() {
    let server = FakeServer::start_replies(server_errors(500, 15));
    let env = Env::server(&four_groups(), &server.base_url, "");
    let o = env.run(&["build", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(
        server.requests().len(),
        15,
        "la quatrième n'est pas envoyée"
    );
    assert_eq!(
        last(&err),
        "erreur: envoi interrompu : panne réseau répétée (3 erreurs réseau consécutives) ; 4 communauté(s) à nommer"
    );
}

#[test]
fn req261_saturated_stop_during_the_pass_relaunches_with_the_option() {
    let server = FakeServer::start_replies(server_errors(529, 15));
    let env = Env::server(&four_groups(), &server.base_url, "");
    let o = env.run_env(
        &["build", "--subsystems"],
        &[("NEPHILA_RETRY_BASE_MS", "1")],
    );
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(
        last(&err).starts_with("erreur: envoi interrompu : fournisseur saturé : relancez nephila update --subsystems plus tard"),
        "{err}"
    );
}

#[test]
fn req261_stop_with_files_left_still_says_a21() {
    let mut f = three_groups();
    for i in 0..3 {
        f.push((format!("d{i}.md"), format!("# D{i}\n\nTexte.\n")));
    }
    let server = FakeServer::start_replies(server_errors(500, 15));
    let env = Env::server(&f, &server.base_url, "batch_files = 1\n");
    let o = env.run(&["build", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(server.requests().len(), 15, "aucun appel de la passe");
    let a21 = err
        .lines()
        .position(|l| l == "avertissement: sous-systèmes non nommés : 3 fichiers restent à enrichir ; relancez nephila update --subsystems")
        .unwrap_or_else(|| panic!("{err}"));
    assert!(a21 < err.lines().count() - 1, "{err}");
    assert!(
        last(&err).starts_with("erreur: envoi interrompu : panne réseau répétée"),
        "{err}"
    );
}

#[test]
fn req261_batch_mode_sends_the_pass_one_by_one() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    let server = FakeServer::start(vec![(200, valid_reply("Noyau"))]);
    let env = Env::server(&f, &server.base_url, "");
    let o = env.run(&["build", "--batch", "--subsystems"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(env.community_of(0).0, "Noyau");
    assert!(!err.contains("plafond"), "{err}");
}

// ---- AC-261-8 : plafond ----

#[test]
fn req261_ac8_max_calls_names_the_largest_first() {
    let env = Env::mock(&three_groups(), "");
    let o = env.run(&["build", "--subsystems", "--max-calls", "2"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(env.subsystem_calls(), 2);
    assert!(err.lines().any(|l| l == "avertissement: plafond de 2 appels atteint : 1 communautés laissées sans nom ; relancez nephila update --subsystems pour continuer ou augmentez max_calls"), "{err}");
    assert!(!err.contains("erreur:"), "{err}");
    assert!(env.community_of(0).1.is_some());
    assert!(env.community_of(1).1.is_some());
    assert_eq!(env.community_of(2), ("p2".to_string(), None));
}

// ---- AC-261-9, AC-262-2 : rien n'a changé, cache ----

#[test]
fn req261_ac9_update_without_change_runs_the_pass_then_writes_nothing() {
    let env = Env::mock(&three_groups(), "");
    assert_eq!(env.run(&["build"]).status.code(), Some(0));
    let o = env.run(&["update", "--subsystems"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(out.starts_with("graphe à jour\n"), "{out}");
    assert_eq!(env.subsystem_calls(), 3);
    assert_eq!(named_count(&env), 3);
    let before = env.state();
    let labels = env.labels();
    let o = env.run(&["update", "--subsystems"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    assert_eq!(
        text(&o).0,
        "graphe à jour\nsous-systèmes : 0 communautés nommées, 3 reprises sans appel, 0 laissées, 0 non retenues (moins de 10 membres ou de 3 résumés)\n"
    );
    assert_eq!(env.subsystem_calls(), 3, "aucun appel de plus");
    assert_eq!(env.labels(), labels);
    assert!(env.state() == before, "le second lancement a écrit");
}

#[test]
fn req262_ac2_build_update_build_reuse_the_names() {
    let env = Env::mock(&three_groups(), "");
    assert_eq!(env.run(&["build", "--subsystems"]).status.code(), Some(0));
    assert_eq!(env.subsystem_calls(), 3);
    let labels = env.labels();
    assert!(
        labels.iter().all(|l| l.starts_with("Sous-système")),
        "{labels:?}"
    );
    assert_eq!(env.run(&["update", "--subsystems"]).status.code(), Some(0));
    assert_eq!(env.subsystem_calls(), 3);
    assert_eq!(env.labels(), labels);
    assert_eq!(env.run(&["build", "--subsystems"]).status.code(), Some(0));
    assert_eq!(env.subsystem_calls(), 3);
    assert_eq!(env.labels(), labels);
    assert_eq!(named_count(&env), 3);
}

// ---- AC-262-3, AC-262-4 : modification d'une communauté ----

fn two_groups() -> Vec<(String, String)> {
    let mut f = Vec::new();
    group(&mut f, 0, 12, 5);
    group(&mut f, 1, 11, 5);
    f
}

/// Ajoute au groupe 1 une classe qui référence `C0`.
fn grow_group_1(env: &Env) {
    env.write(
        "src/p1/Extra.java",
        "package p1;\n/** Ajout. */\npublic class Extra {\n  private C0 a;\n  private C1 b;\n  private C2 c;\n}\n",
    );
}

#[test]
fn req262_ac3_one_changed_community_costs_one_call() {
    let env = Env::mock(&two_groups(), "");
    assert_eq!(env.run(&["build", "--subsystems"]).status.code(), Some(0));
    assert_eq!(env.subsystem_calls(), 2);
    let untouched = env.community_of(0);
    grow_group_1(&env);
    let o = env.run(&["update", "--subsystems"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    assert_eq!(env.subsystem_calls(), 3);
    assert_eq!(env.community_of(0), untouched);
    assert!(env.community_of(1).1.is_some());
}

#[test]
fn req262_ac4_no_llm_update_keeps_or_drops_names() {
    let env = Env::mock(&two_groups(), "");
    assert_eq!(env.run(&["build", "--subsystems"]).status.code(), Some(0));
    let untouched = env.community_of(0);
    grow_group_1(&env);
    let o = env.run(&["update", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    assert_eq!(env.subsystem_calls(), 2, "aucun appel");
    assert_eq!(env.community_of(0), untouched);
    assert_eq!(env.community_of(1), ("p1".to_string(), None));
}

// ---- AC-262-7 : réponses reçues avant un arrêt ----

#[test]
fn req262_ac7_answers_received_before_a_stop_are_not_asked_again() {
    let env = Env::mock(&three_groups(), "");
    // Premier appel valide, le deuxième refuse la clé : la passe s'arrête.
    let o = env.run_env(
        &["build", "--subsystems"],
        &[("NEPHILA_MOCK_KEY_REVOKED_AFTER", "1")],
    );
    assert_ne!(o.status.code(), Some(0));
    assert_eq!(env.subsystem_calls(), 2);
    let first = env.community_of(0);
    assert!(first.1.is_some());
    let o = env.run(&["update", "--subsystems"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    assert_eq!(env.subsystem_calls(), 4, "seules les deux autres repartent");
    assert_eq!(env.community_of(0), first);
    assert_eq!(named_count(&env), 3);
}

// ---- AC-261-14 : aide ----

#[test]
fn req261_ac14_help_lists_the_option_between_batch_and_exclude() {
    let env = Env::mock(&[], "");
    for cmd in ["build", "update"] {
        let o = env.run(&[cmd, "--help"]);
        let out = text(&o).0;
        let at = |needle: &str| {
            out.lines()
                .position(|l| l.trim_start().starts_with(needle))
                .unwrap_or_else(|| panic!("{needle} absent de {out}"))
        };
        let (batch, sub, exclude) = (at("--batch "), at("--subsystems"), at("--exclude <MOTIF>"));
        assert!(batch < sub && sub < exclude, "{out}");
        assert!(out.contains("Nomme chaque grande communauté par un appel LLM, à la fin de la commande ; le nom reste gardé tant que la communauté ne change pas (incompatible avec --no-llm et --dry-run)"), "{out}");
    }
    for cmd in ["status", "summary", "export"] {
        let out = text(&env.run(&[cmd, "--help"])).0;
        assert!(!out.contains("--subsystems"), "{cmd}");
    }
}

// ---- AC-261-11, AC-261-12, AC-261-15 : bilan, traces et progression ----

const NOT_RETAINED: &str = "2 non retenues (moins de 10 membres ou de 3 résumés)";

/// Dépôt d'AC-261-2 : une communauté retenue, deux qui ne le sont pas.
fn one_retained() -> Vec<(String, String)> {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    group(&mut f, 1, 8, 5);
    group(&mut f, 2, 11, 2);
    f
}

#[test]
fn req261_ac12_ac11_the_subsystems_line_follows_the_report() {
    let env = Env::mock(&one_retained(), "");
    let o = env.run(&["build", "--subsystems"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("build terminé"), "{out}");
    assert_eq!(
        lines[1],
        format!("sous-systèmes : 1 communautés nommées, 0 reprises sans appel, 0 laissées, {NOT_RETAINED}")
    );
    assert_eq!(lines.len(), 2, "{out}");
    let o = env.run(&["update", "--subsystems"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    assert_eq!(
        text(&o).0,
        format!("graphe à jour\nsous-systèmes : 0 communautés nommées, 1 reprises sans appel, 0 laissées, {NOT_RETAINED}\n")
    );
}

#[test]
fn req261_ac12_without_the_option_no_subsystems_line() {
    let env = Env::mock(&one_retained(), "");
    for args in [&["build"][..], &["update"], &["update", "--no-llm"]] {
        let (out, _) = text(&env.run(args));
        assert!(
            !out.lines().any(|l| l.starts_with("sous-systèmes")),
            "{out}"
        );
    }
}

#[test]
fn req261_ac12_ac11_files_left_to_enrich_count_every_community_as_left() {
    let mut f = one_retained();
    f.push(("doc.md".into(), "# Doc\n\nTexte.\n".into()));
    let env = Env::mock(&f, "");
    let o = env.run_env(
        &["build", "--subsystems"],
        &[("NEPHILA_MOCK_MALFORMED", "doc.md:9")],
    );
    let (out, _) = text(&o);
    assert!(
        out.lines().any(|l| l
            == "sous-systèmes : 0 communautés nommées, 0 reprises sans appel, 1 laissées, 3 non retenues (moins de 10 membres ou de 3 résumés)"),
        "{out}"
    );
}

#[test]
fn req261_ac15_verbose_traces_in_order_without_llm_text() {
    let env = Env::mock(&one_retained(), "");
    env.write("docs/a.md", "# Titre\n\nUn texte.\n");
    let o = env.run(&["build", "--subsystems", "--verbose"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let events: Vec<&str> = err
        .lines()
        .filter_map(|l| l.split_once("] ").map(|(_, e)| e))
        .filter(|e| {
            e.starts_with("sous-systèmes:")
                || e.starts_with("llm sous-système")
                || e.starts_with("appels:")
        })
        .collect();
    assert_eq!(events.len(), 6, "{events:?}");
    assert!(events[0].starts_with("appels: "), "{events:?}");
    assert_eq!(
        events[1],
        "sous-systèmes: début 1 communautés à nommer, 0 reprises sans appel, 1 fils"
    );
    assert!(
        events[2].starts_with("llm sous-système départ communauté 0 : ")
            && events[2].contains(" lignes, ")
            && events[2].ends_with(" car."),
        "{events:?}"
    );
    assert!(
        events[3].starts_with("llm sous-système fin communauté 0 ok "),
        "{events:?}"
    );
    assert!(
        events[4].starts_with("sous-systèmes: fin ")
            && events[4].ends_with(" ms, 1 nommées, 0 laissées"),
        "{events:?}"
    );
    assert!(events[5].starts_with("appels: 1 appels, "), "{events:?}");
    let enrich_starts: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("] llm enrich départ "))
        .collect();
    assert_eq!(enrich_starts.len(), 1, "{enrich_starts:?}");
    assert!(enrich_starts[0].contains("docs/a.md"), "{enrich_starts:?}");
    assert!(
        !err.contains("Sous-système") && !err.contains("Description du"),
        "{err}"
    );
    assert!(!err.contains('\u{1b}'));
}

#[test]
fn req261_ac15_progress_lines_after_the_enrichment_ones() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    group(&mut f, 1, 11, 5);
    let env = Env::mock(&f, "");
    env.write("docs/a.md", "# Titre\n\nUn texte.\n");
    let o = env.run(&["build", "--subsystems", "--show-progress"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let progress: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("progression: "))
        .collect();
    let n = progress.len();
    assert_eq!(
        &progress[n - 3..],
        [
            "progression: sous-systèmes 0 % (0/2 communautés)",
            "progression: sous-systèmes 50 % (1/2 communautés)",
            "progression: sous-systèmes 100 % (2/2 communautés)"
        ]
    );
    assert!(progress[..n - 3]
        .iter()
        .any(|l| l.starts_with("progression: enrichissement")));
    assert!(!err.contains('\u{1b}'));
    // Même sortie avec NO_COLOR=1, sur un dépôt identique.
    let other = Env::mock(&f, "");
    other.write("docs/a.md", "# Titre\n\nUn texte.\n");
    let o2 = other.run_env(
        &["build", "--subsystems", "--show-progress"],
        &[("NO_COLOR", "1")],
    );
    let err2 = text(&o2).1;
    let progress2: Vec<&str> = err2
        .lines()
        .filter(|l| l.starts_with("progression: "))
        .collect();
    assert_eq!(progress, progress2);
    assert!(!err2.contains('\u{1b}'));
    let again = env.run(&["update", "--subsystems", "--show-progress"]);
    assert!(!text(&again).1.contains("progression: sous-systèmes"));
}

// ---- Corrections de relecture ----

#[test]
fn req261_ac11_refused_key_during_enrichment_still_writes_the_line() {
    let mut f = three_groups();
    f.push(("doc.md".into(), "# Doc\n\nTexte.\n".into()));
    let env = Env::mock(&f, "");
    let o = env.run_env(
        &["build", "--subsystems"],
        &[("NEPHILA_MOCK_INVALID_KEY", "1")],
    );
    let (out, err) = text(&o);
    assert_ne!(o.status.code(), Some(0), "{err}");
    assert!(last(&err).starts_with("erreur: clé d'API refusée"), "{err}");
    assert_eq!(env.subsystem_calls(), 0);
    assert!(
        out.lines().any(|l| l
            == "sous-systèmes : 0 communautés nommées, 0 reprises sans appel, 3 laissées, 1 non retenues (moins de 10 membres ou de 3 résumés)"),
        "{out}"
    );
    assert!(!err.contains("fichiers restent à enrichir"), "{err}");
}

#[test]
fn req261_ac15_failed_call_trace_is_fixed_and_without_llm_text() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    let env = Env::mock(&f, "");
    fs::write(
        env.mock.join("_subsystem.json"),
        serde_json::json!({"name": format!("SECRET{}", "x".repeat(60)), "description": "SECRETDESC"})
            .to_string(),
    )
    .unwrap();
    let o = env.run(&["build", "--subsystems", "--verbose"]);
    let (_, err) = text(&o);
    assert!(
        err.lines().any(
            |l| l.contains("] llm sous-système fin communauté 0 erreur ")
                && l.ends_with(
                    ": réponse LLM non conforme : name vide ou de plus de 60 caractères"
                )
        ),
        "{err}"
    );
    assert!(!err.contains("SECRET"), "{err}");
}

#[test]
fn req261_ac15_truncated_trace_has_no_action() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    let cut = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "{\"name\":"}, "finish_reason": "length"}]
    })
    .to_string();
    let server = FakeServer::start_replies(vec![Reply::status(200, &cut)]);
    let env = Env::server(&f, &server.base_url, "");
    let o = env.run(&["build", "--subsystems", "--verbose"]);
    let (_, err) = text(&o);
    let fin = err
        .lines()
        .find(|l| l.contains("] llm sous-système fin communauté 0 "))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(fin.contains(" tronquée "), "{fin}");
    assert!(!fin.contains("augmentez"), "{fin}");
}

#[test]
fn req274_ac12_blocked_name_request_is_named_by_a23_and_others_are_named() {
    let mut f = Vec::new();
    group(&mut f, 0, 13, 5);
    group(&mut f, 1, 12, 5);
    let blocked = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "{\"name\":\"SECRET\",\"description\":\"SECRET\"}"}, "finish_reason": "content_filter"}]
    })
    .to_string();
    let server = FakeServer::start_replies(vec![
        Reply::status(200, &blocked),
        Reply::status(200, &valid_reply("Deux")),
    ]);
    let env = Env::server(&f, &server.base_url, "");
    let o = env.run(&["build", "--subsystems", "--verbose"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert_eq!(last(&err), "erreur: 1 communauté(s) à nommer");
    let warn = err
        .lines()
        .find(|l| l.starts_with("avertissement: communauté "))
        .unwrap_or_else(|| panic!("{err}"));
    let rest = warn.strip_prefix("avertissement: communauté ").unwrap();
    let (n, rest) = rest.split_once(' ').unwrap();
    assert!(rest.ends_with(" : réponse bloquée par le modèle"), "{warn}");
    assert!(
        !warn.contains("augmentez") && !warn.contains("tronquée"),
        "{warn}"
    );
    let fin = err
        .lines()
        .find(|l| l.contains(&format!("] llm sous-système fin communauté {n} bloquée ")))
        .unwrap_or_else(|| panic!("{err}"));
    assert!(fin.ends_with(" : réponse bloquée par le modèle"), "{fin}");
    assert!(!err.contains("SECRET"), "{err}");
    assert_eq!(server.requests().len(), 2, "{err}");
    assert!(out.contains("1 communautés nommées"), "{out}");
}

#[test]
fn req255_req261_ac15_wait_and_pause_traces_use_the_subsystem_words() {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    let server = FakeServer::start_replies(vec![
        Reply::with_header(429, "{\"error\":\"x\"}", "retry-after-ms", "20"),
        Reply::status(200, &valid_reply("Un")),
    ]);
    let env = Env::server(&f, &server.base_url, "");
    let o = env.run(&["build", "--subsystems", "--verbose"]);
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert!(
        err.lines()
            .any(|l| l.contains("] llm sous-système attente communauté 0 20 ms après HTTP 429")),
        "{err}"
    );
    assert!(
        err.lines()
            .any(|l| l.contains("] llm sous-système pause communauté 0 : aucun envoi pendant")),
        "{err}"
    );
    assert!(
        !err.contains("llm enrich attente") && !err.contains("llm enrich pause"),
        "{err}"
    );
}

// ---- REQ-263 : noms et descriptions dans les lectures ----

/// Graphe construit avec une communauté nommée (le groupe 0) ; `fixture` = `_subsystem.json`.
/// Retourne l'environnement, le nom, la description, le numéro et la taille de la communauté.
fn named_graph_with(fixture: Option<&str>) -> (Env, String, String, i64, i64) {
    let mut f = Vec::new();
    group(&mut f, 0, 11, 5);
    group(&mut f, 1, 8, 5);
    let env = Env::mock(&f, "");
    if let Some(j) = fixture {
        fs::write(env.mock.join("_subsystem.json"), j).unwrap();
    }
    let o = env.run(&["build", "--subsystems"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let (id, size, label, description): (i64, i64, String, Option<String>) = env
        .db()
        .query_row(
            "SELECT c.id, c.size, c.label, c.description FROM communities c
             JOIN node_communities m ON m.community = c.id
             JOIN nodes n ON n.id = m.node
             WHERE n.id LIKE '%p0.C0' ORDER BY c.size DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    (env, label, description.expect("description"), id, size)
}

fn named_graph() -> (Env, String, String, i64, i64) {
    named_graph_with(None)
}

#[test]
fn req263_ac3_ac019_ac8_summary_query_community_and_export() {
    let (env, label, description, id, size) = named_graph();
    // summary : texte et JSON, lignes entières.
    let (out, _) = text(&env.run(&["summary"]));
    let expected = format!("  {id} {label} ({size} nœuds) : {description}");
    assert!(out.lines().any(|l| l == expected), "{expected}\n{out}");
    let other = env.community_of(1).0;
    let (oid, osize): (i64, i64) = env
        .db()
        .query_row(
            "SELECT id, size FROM communities WHERE label = ?1",
            [&other],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let plain = format!("  {oid} {other} ({osize} nœuds)");
    assert!(out.lines().any(|l| l == plain), "{plain}\n{out}");
    let (json, _) = text(&env.run(&["summary", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let top = v["communities"]["top"].as_array().unwrap();
    let named = top.iter().find(|c| c["label"] == label.as_str()).unwrap();
    assert_eq!(named["description"], description.as_str());
    let keys: Vec<&String> = named.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["cohesion", "description", "id", "label", "size"]);
    let unnamed = top.iter().find(|c| c["label"] == other.as_str()).unwrap();
    assert!(unnamed.get("description").is_none());
    // query community : le nom, jamais la description (AC-148-10), sous les trois formes.
    let member: String = env
        .db()
        .query_row(
            "SELECT node FROM node_communities WHERE community = ?1 AND node LIKE '%p0.C0'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    let head = format!("{id} {label} ({size} nœuds, cohésion ");
    for args in [
        vec!["query", "community"],
        vec!["query", "community", &format!("{id}")],
        vec!["query", "community", "--node", &member],
    ] {
        let (out, _) = text(&env.run(&args));
        assert!(out.contains(&head), "{args:?}\n{out}");
        assert!(!out.contains(&description), "{args:?}\n{out}");
        let mut a = args.clone();
        a.push("--json");
        let (out, _) = text(&env.run(&a));
        assert!(!out.contains("description"), "{a:?}\n{out}");
        let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        let first = j["communities"]
            .get(0)
            .or_else(|| j.get("community"))
            .unwrap_or(&j)
            .clone();
        let obj = first.as_object().unwrap_or_else(|| panic!("{out}"));
        let keys: Vec<&String> = obj.keys().collect();
        assert_eq!(keys, ["cohesion", "id", "label", "pivot", "size"], "{out}");
    }
    // export : clé à sa place alphabétique, version 1.
    let (out, _) = text(&env.run(&["export", "--format", "json", "--output", "-"]));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["version"], 1);
    assert!(out.contains(&format!(
        "\"description\":{},\"id\":",
        serde_json::to_string(&description).unwrap()
    )));
    let comms = v["communities"].as_array().unwrap();
    let named = comms.iter().find(|c| c["label"] == label.as_str()).unwrap();
    assert_eq!(named["description"], description.as_str());
    let unnamed = comms.iter().find(|c| c["label"] == other.as_str()).unwrap();
    assert!(unnamed.get("description").is_none());
}

#[test]
fn req263_ac8_escape_in_summary_and_query_community() {
    let fixture = serde_json::json!({"name": "No\u{1b}[1mm", "description": "a\u{1b}[31mb\u{7}c"})
        .to_string();
    let (env, _, _, id, _) = named_graph_with(Some(&fixture));
    for no_color in [false, true] {
        let extra: &[(&str, &str)] = if no_color { &[("NO_COLOR", "1")] } else { &[] };
        let o = env.run_env(&["summary"], extra);
        assert!(!o.stdout.contains(&0x1b), "octet 0x1B");
        let out = text(&o).0;
        assert!(out.contains("No\\u{001B}[1mm"), "{out}");
        assert!(out.contains("a\\u{001B}[31mb\\u{0007}c"), "{out}");
        let o = env.run_env(&["query", "community", &format!("{id}")], extra);
        assert!(!o.stdout.contains(&0x1b) && !o.stderr.contains(&0x1b));
        assert!(text(&o).0.contains("No\\u{001B}[1mm"), "{}", text(&o).0);
    }
}

#[test]
fn req263_ac4_ac260_ac12_ask_without_seed_carries_name_and_description() {
    let (env, label, description, id, size) = named_graph();
    let reply = serde_json::json!({"found": true, "answer": "vue", "nodes": [label]}).to_string();
    let ok = (
        200,
        serde_json::json!({"content": [{"type": "text", "text": reply}]}).to_string(),
    );
    let server = FakeServer::start(vec![ok.clone(), ok]);
    fs::write(
        &env.config,
        format!(
            "provider = \"anthropic\"\nmodel = \"claude-x\"\napi_key_env = \"{KEY_VAR}\"\nbase_url = \"{}\"\n",
            server.base_url
        ),
    )
    .unwrap();
    let o = env.run(&["query", "ask", "zzzabsent qqqabsent ?"]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    let reqs = server.requests();
    let post = reqs.iter().find(|r| r.method == "POST").unwrap();
    assert!(post.body.contains("Résumé du graphe :"), "{}", post.body);
    let line = format!("- {label} ({size} nœuds) : {description}");
    assert!(post.body.contains(&line), "{line}\n{}", post.body);
    assert!(!out.contains(&description) && !err.contains(&description));
    assert!(!out.contains("Résumé du graphe") && !err.contains("Résumé du graphe"));
    assert_eq!(out, "vue\n");
    assert!(err.contains("réponse non vérifiable"), "{err}");
    let _ = id;
    let o = env.run(&["query", "ask", "zzzabsent qqqabsent ?", "--json"]);
    let (out, _) = text(&o);
    assert_eq!(o.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["nodes"], serde_json::json!([]));
}
