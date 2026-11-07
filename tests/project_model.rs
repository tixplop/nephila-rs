//! Modèle imposé par `.nephila.toml` et `model_override` (REQ-195, REQ-196) : `deny`, traces,
//! erreur de valeur, `warn` sans message, option `--accept-project-model`.

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;

use assert_cmd::Command;
use fake_server::FakeServer;
use std::fs;
use std::path::PathBuf;
use std::process::Output;

const KEY_VAR: &str = "NEPHILA_T286_KEY";
const ASK_A: &str = r#"{"found":true,"answer":"réponse","nodes":["type:p.Alpha"]}"#;

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn lines(b: &[u8]) -> Vec<String> {
    text(b).lines().map(String::from).collect()
}

/// Dépôt de `n` documents `docs/dNN.md` (et leurs fixtures du mock) et d'une classe `Alpha`.
struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    /// `personal` : réglages de la configuration personnelle, après `provider = "mock"`.
    fn new(n: usize, personal: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(repo.join("docs")).unwrap();
        fs::create_dir_all(mock.join("docs")).unwrap();
        fs::write(
            repo.join("Alpha.java"),
            "package p;\npublic class Alpha {}\n",
        )
        .unwrap();
        for i in 0..n {
            fs::write(
                repo.join(format!("docs/d{i:02}.md")),
                format!("# D{i:02}\n\ntexte {i}\n"),
            )
            .unwrap();
            fs::write(
                mock.join(format!("docs/d{i:02}.md.json")),
                format!(
                    r#"{{"version":2,"summaries":[{{"node":"document:docs/d{i:02}.md","summary":"résumé {i}"}}],"concepts":[]}}"#
                ),
            )
            .unwrap();
        }
        let e = Env {
            config: tmp.path().join("config.toml"),
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
        };
        e.personal(personal);
        e
    }

    fn personal(&self, extra: &str) {
        fs::write(&self.config, format!("provider = \"mock\"\n{extra}")).unwrap();
    }

    fn project(&self, content: &str) {
        fs::write(self.repo.join(".nephila.toml"), content).unwrap();
    }

    fn command(&self) -> Command {
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env(KEY_VAR, "sk-test")
            .env_remove("NEPHILA_MOCK_MALFORMED");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    /// Appels reçus par le mock depuis le dernier effacement du journal.
    fn calls(&self) -> usize {
        fs::read_to_string(&self.log)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn clear_log(&self) {
        let _ = fs::remove_file(&self.log);
    }

    /// `SELECT DISTINCT model FROM enrichments`, trié.
    fn models(&self) -> Vec<String> {
        let db = rusqlite::Connection::open(self.repo.join(".nephila/graph.db")).unwrap();
        let mut st = db
            .prepare("SELECT DISTINCT model FROM (SELECT model, ROW_NUMBER() OVER (PARTITION BY path ORDER BY used_at DESC) AS rn FROM enrichments) WHERE rn = 1 ORDER BY model")
            .unwrap();
        st.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// `SELECT * FROM enrichments ORDER BY 1`, chaque ligne en texte.
    fn enrichment_rows(&self) -> Vec<String> {
        let db = rusqlite::Connection::open(self.repo.join(".nephila/graph.db")).unwrap();
        let mut st = db.prepare("SELECT * FROM enrichments ORDER BY 1").unwrap();
        let n = st.column_count();
        st.query_map([], |r| {
            Ok((0..n)
                .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Nombre d'entrées d'enrichissement du modèle donné.
    fn count_model(&self, model: &str) -> i64 {
        let db = rusqlite::Connection::open(self.repo.join(".nephila/graph.db")).unwrap();
        db.query_row(
            "SELECT COUNT(*) FROM enrichments WHERE model = ?1",
            [model],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// `query ask` contre un faux serveur `openai` (configuration personnelle réécrite).
    fn ask(&self, extra: &str, args: &[&str]) -> (Output, Vec<serde_json::Value>) {
        let reply =
            serde_json::json!({"choices":[{"message":{"role":"assistant","content":ASK_A}}]})
                .to_string();
        let server = FakeServer::start(vec![(200, reply)]);
        fs::write(
            &self.config,
            format!(
                "provider = \"openai\"\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\n{extra}",
                server.base_url
            ),
        )
        .unwrap();
        let mut a = vec!["query", "ask", "Alpha"];
        a.extend_from_slice(args);
        let o = self.run(&a);
        let bodies = server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST")
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect();
        (o, bodies)
    }
}

/// Texte d'une ligne de trace sans son préfixe `trace: [<s>.<mmm>s] `.
fn trace_text(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("trace: [")?;
    let (stamp, msg) = rest.split_once("s] ")?;
    let (s, ms) = stamp.split_once('.')?;
    (!s.is_empty()
        && s.bytes().all(|b| b.is_ascii_digit())
        && ms.len() == 3
        && ms.bytes().all(|b| b.is_ascii_digit()))
    .then_some(msg)
}

fn position(err: &[String], needle: &str) -> usize {
    err.iter()
        .position(|l| trace_text(l).is_some_and(|t| t.starts_with(needle)))
        .unwrap_or_else(|| panic!("pas de trace `{needle}` : {err:#?}"))
}

fn ignored_lines(err: &[String]) -> Vec<&String> {
    err.iter()
        .filter(|l| l.contains("ignoré (model_override"))
        .collect()
}

const DENY: &str = "model = \"m-perso\"\nmodel_override = \"deny\"\n";

// ---- REQ-195 : deny ----

#[test]
fn req195_ac4_deny_enrichissement_au_modele_personnel() {
    let e = Env::new(3, DENY);
    e.project("model = \"m-projet\"\n");
    let o = e.run(&["build"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(
        !text(&o.stderr).contains("avertissement:"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(e.models(), vec!["m-perso"]);
}

#[test]
fn req195_ac4_deny_corps_de_query_ask() {
    let e = Env::new(1, DENY);
    e.project("model = \"m-projet\"\n[ask]\nmodel = \"m-ask-projet\"\n");
    assert!(e.run(&["build"]).status.success());
    let (o, bodies) = e.ask("model = \"m-perso\"\nmodel_override = \"deny\"\n", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["model"], "m-perso");
    let (o, bodies) = e.ask(
        "model = \"m-perso\"\nmodel_override = \"deny\"\n[ask]\nmodel = \"m-ask-perso\"\n",
        &[],
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(bodies[0]["model"], "m-ask-perso");
}

#[test]
fn req195_ac4_deny_dry_run_comme_sans_modele_de_depot() {
    let with = Env::new(3, DENY);
    with.project("model = \"m-projet\"\n");
    let without = Env::new(3, DENY);
    for args in [&["build", "--dry-run"][..], &["update", "--dry-run"][..]] {
        let a = with.run(args);
        let b = without.run(args);
        assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
        assert!(
            text(&a.stdout).contains("estimation"),
            "{}",
            text(&a.stdout)
        );
        assert_eq!(text(&a.stdout), text(&b.stdout));
        assert_eq!(text(&a.stderr), text(&b.stderr));
    }
}

// ---- REQ-195 : traces ----

#[test]
fn req195_ac5_ac8_trace_du_modele_ignore_apres_config() {
    for (project, mark) in [
        ("model = \"m-projet\"\n", None),
        (
            "model = \"m-projet\"\nconcurrency = 2\n",
            Some(" (.nephila.toml : concurrency)"),
        ),
    ] {
        let e = Env::new(2, DENY);
        e.project(project);
        let o = e.run(&["build", "--verbose"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let err = lines(&o.stderr);
        let at = position(&err, "config:");
        let config = trace_text(&err[at]).unwrap();
        assert!(config.contains(" model=m-perso "), "{config}");
        match mark {
            Some(m) => assert!(config.ends_with(m), "{config}"),
            None => assert!(!config.contains("(.nephila.toml"), "{config}"),
        }
        assert_eq!(
            trace_text(&err[at + 1]),
            Some("modèle: m-projet de .nephila.toml ignoré (model_override = \"deny\")"),
            "{err:#?}"
        );
        assert_eq!(ignored_lines(&err).len(), 1);
    }
}

#[test]
fn req195_ac8_trace_de_query_ask_entre_config_et_appels() {
    let e = Env::new(1, DENY);
    e.project("model = \"m-projet\"\n[ask]\nmodel = \"m-ask-projet\"\n");
    assert!(e.run(&["build"]).status.success());
    let (o, _) = e.ask(DENY, &["--verbose"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = lines(&o.stderr);
    let with_label: Vec<&String> = err.iter().filter(|l| l.contains("modèle: ")).collect();
    assert_eq!(with_label.len(), 1, "{err:#?}");
    let at = position(&err, "config:");
    assert!(trace_text(&err[at]).unwrap().contains(" model=m-perso "));
    assert_eq!(
        trace_text(&err[at + 1]),
        Some("modèle: m-ask-projet de .nephila.toml ignoré (model_override = \"deny\")")
    );
    assert!(position(&err, "appels:") > at + 1);
}

#[test]
fn req195_ac8_pas_de_trace_sous_allow_warn_ni_deny_sans_modele() {
    for (personal, project) in [
        (
            "model = \"m-perso\"\nmodel_override = \"allow\"\n",
            "model = \"m-projet\"\n",
        ),
        (
            "model = \"m-perso\"\nmodel_override = \"warn\"\n",
            "model = \"m-projet\"\n",
        ),
        ("model = \"m-perso\"\n", "model = \"m-projet\"\n"),
        (DENY, "concurrency = 2\n"),
    ] {
        let e = Env::new(2, personal);
        e.project(project);
        let o = e.run(&["build", "--verbose"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            ignored_lines(&lines(&o.stderr)).is_empty(),
            "{}",
            text(&o.stderr)
        );
        // query ask, avec `[ask] model` ou sans modèle du tout.
        for p in [
            format!("{project}[ask]\nmodel = \"m-ask-projet\"\n"),
            project.to_string(),
        ] {
            e.project(&p);
            let (o, _) = e.ask(personal, &["--verbose"]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            let err = lines(&o.stderr);
            let denied_with_ask = personal == DENY && p.contains("[ask]");
            assert_eq!(ignored_lines(&err).is_empty(), !denied_with_ask, "{err:#?}");
        }
    }
}

#[test]
fn req195_ac8_deny_sans_modele_de_depot_pour_ask_aucune_trace() {
    let e = Env::new(1, DENY);
    e.project("concurrency = 2\n");
    assert!(e.run(&["build"]).status.success());
    let (o, _) = e.ask(DENY, &["--verbose"]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(ignored_lines(&lines(&o.stderr)).is_empty());
    assert!(!text(&o.stderr).contains("modèle: "));
}

// ---- REQ-195 : valeur invalide, stderr sans trace ----

fn invalid(e: &Env) -> String {
    format!(
        "erreur: configuration invalide ({}) : clé « model_override » doit être \"allow\", \"warn\" ou \"deny\" (reçu : \"oui\")",
        e.config.display()
    )
}

#[test]
fn req195_ac6_valeur_invalide_refusee_sans_ecriture() {
    let e = Env::new(2, "model = \"m-perso\"\nmodel_override = \"oui\"\n");
    e.project("model = \"m-projet\"\n");
    let o = e.run(&["build"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(lines(&o.stderr).last().unwrap(), &invalid(&e));
    assert!(!e.repo.join(".nephila").exists());
    // query ask : même erreur.
    let (o, bodies) = e.ask("model = \"m-perso\"\nmodel_override = \"oui\"\n", &[]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(lines(&o.stderr).last().unwrap(), &invalid(&e));
    assert!(bodies.is_empty());
    // --no-llm : la configuration n'est pas lue.
    let o = e.run(&["build", "--no-llm"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(!text(&o.stderr).contains("erreur:"), "{}", text(&o.stderr));
}

#[test]
fn req195_ac9_stderr_identique_sans_modele_de_depot() {
    let with = Env::new(2, DENY);
    with.project("model = \"m-projet\"\n[ask]\nmodel = \"m-ask-projet\"\n");
    let without = Env::new(2, DENY);
    let a = with.run(&["build"]);
    let b = without.run(&["build"]);
    assert_eq!(a.status.code(), Some(0));
    assert_eq!(text(&a.stderr), text(&b.stderr));
    assert_eq!(text(&a.stdout), text(&b.stdout));
    let (a, _) = with.ask(DENY, &[]);
    let (b, _) = without.ask(DENY, &[]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert_eq!(text(&a.stderr), text(&b.stderr));
    assert_eq!(text(&a.stdout), text(&b.stdout));
    assert!(!text(&a.stderr).contains("m-projet"));
    assert!(!text(&a.stderr).contains("model_override"));
}

#[test]
fn req195_ac9_build_deny_stderr_sans_m_projet_ni_model_override() {
    let e = Env::new(2, DENY);
    e.project("model = \"m-projet\"\n");
    let o = e.run(&["build"]);
    let err = text(&o.stderr);
    assert!(
        !err.contains("m-projet") && !err.contains("model_override"),
        "{err}"
    );
}

// ---- REQ-195 : allow = warn ----

#[test]
fn req195_ac7_allow_identique_a_warn() {
    let run = |policy: &str| {
        let e = Env::new(
            3,
            &format!("model = \"m-perso\"\nmodel_override = \"{policy}\"\n"),
        );
        e.project("model = \"m-projet\"\nconcurrency = 2\n");
        let mut out = Vec::new();
        for args in [
            &["build", "--dry-run"][..],
            &["build"][..],
            &["update"][..],
            &["status"][..],
            &["query", "search", "Alpha"][..],
        ] {
            let o = e.run(args);
            assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o.stderr));
            out.push((o.stdout, o.stderr));
        }
        assert_eq!(e.models(), vec!["m-projet"]);
        out
    };
    assert_eq!(run("allow"), run("warn"));
}

// ---- REQ-196 : cas sans message ----

fn no_message(o: &Output) {
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(
        !err.contains("avertissement:") && !err.contains("erreur:"),
        "{err}"
    );
}

#[test]
fn req196_ac5_premier_build_sans_message() {
    let e = Env::new(3, "model = \"m-perso\"\n");
    e.project("model = \"m-projet\"\n");
    no_message(&e.run(&["build"]));
    assert_eq!(e.models(), vec!["m-projet"]);
}

#[test]
fn req196_ac5_meme_modele_dans_le_projet_et_la_configuration() {
    // 51 fichiers : le seuil ne se déclenche pas, le redo vient de l'utilisateur.
    let e = Env::new(51, "model = \"m-old\"\n");
    no_message(&e.run(&["build"]));
    assert_eq!(e.models(), vec!["m-old"]);
    e.personal("model = \"m-projet\"\n");
    e.project("model = \"m-projet\"\n");
    e.clear_log();
    no_message(&e.run(&["update"]));
    assert_eq!(e.models(), vec!["m-projet"]);
    assert!(e.calls() > 0);
}

#[test]
fn req196_ac5_no_llm_sans_message() {
    let e = Env::new(51, "model = \"m-perso\"\n");
    no_message(&e.run(&["build"]));
    e.project("model = \"m-projet\"\n");
    e.clear_log();
    no_message(&e.run(&["update", "--no-llm"]));
    no_message(&e.run(&["build", "--no-llm"]));
    assert_eq!(e.calls(), 0);
    assert_eq!(e.models(), vec!["m-perso"]);
}

#[test]
fn req196_ac5_aucune_configuration_sans_message() {
    let e = Env::new(51, "model = \"m-perso\"\n");
    no_message(&e.run(&["build"]));
    e.project("model = \"m-projet\"\n");
    let missing = e.config.with_file_name("absente.toml");
    for args in [
        &["update"][..],
        &["build"][..],
        &["update", "--dry-run"][..],
    ] {
        let o = e
            .command()
            .env("NEPHILA_CONFIG", &missing)
            .arg("-C")
            .arg(&e.repo)
            .args(args)
            .output()
            .unwrap();
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {err}");
        assert!(
            !err.contains("avertissement:")
                && !err.contains("erreur:")
                && !err.contains("m-projet"),
            "{args:?}: {err}"
        );
        assert!(!err.contains("--accept-project-model"), "{args:?}: {err}");
    }
    // Aucune configuration : aucun LLM, le graphe enrichi n'a pas bougé.
    assert_eq!(e.models(), vec!["m-perso"]);
}

// ---- REQ-196 : --accept-project-model sans effet ----

type Setup = fn() -> Env;

fn plain() -> Env {
    Env::new(3, "model = \"m-perso\"\n")
}
fn without_project_file() -> Env {
    let e = plain();
    assert!(e.run(&["build"]).status.success());
    e
}
fn policy(p: &str) -> Env {
    let e = Env::new(
        3,
        &format!("model = \"m-perso\"\nmodel_override = \"{p}\"\n"),
    );
    assert!(e.run(&["build"]).status.success());
    e.project("model = \"m-projet\"\n");
    e
}
fn allow() -> Env {
    policy("allow")
}
/// Politique par défaut (`warn`), graphe enrichi avec `m-perso`, puis `m-projet` imposé.
fn warn_default() -> Env {
    let e = Env::new(3, "model = \"m-perso\"\n");
    assert!(e.run(&["build"]).status.success());
    e.project("model = \"m-projet\"\n");
    e
}
fn deny() -> Env {
    policy("deny")
}
fn project_file_only() -> Env {
    let e = without_project_file();
    e.project("concurrency = 2\n");
    e
}

#[test]
fn req196_ac6_option_sans_effet() {
    let cases: [(&str, Setup, &[&str]); 8] = [
        ("sans .nephila.toml", without_project_file, &["update"]),
        ("sans .nephila.toml", plain, &["build"]),
        ("fichier sans model", project_file_only, &["update"]),
        ("allow", allow, &["update"]),
        ("deny", deny, &["update"]),
        ("deny", deny, &["build"]),
        ("deny", deny, &["update", "--dry-run"]),
        ("no-llm", warn_default, &["update", "--no-llm"]),
    ];
    for (name, setup, args) in cases {
        let mut with: Vec<&str> = args.to_vec();
        with.push("--accept-project-model");
        let a = setup();
        let b = setup();
        let (oa, ob) = (a.run(&with), b.run(args));
        assert_eq!(oa.status.code(), ob.status.code(), "{name} {args:?}");
        assert_eq!(oa.status.code(), Some(0), "{name}: {}", text(&oa.stderr));
        assert_eq!(text(&oa.stdout), text(&ob.stdout), "{name} {args:?}");
        assert_eq!(text(&oa.stderr), text(&ob.stderr), "{name} {args:?}");
        assert_eq!(a.models(), b.models(), "{name} {args:?}");
    }
}

#[test]
fn req196_ac6_option_refusee_par_status_et_query_ask() {
    let e = plain();
    for args in [
        &["status", "--accept-project-model"][..],
        &["query", "ask", "x", "--accept-project-model"][..],
    ] {
        let o = e.run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o.stderr));
        assert!(text(&o.stderr).contains("--accept-project-model"));
    }
}

// ---- REQ-196 : A-13, E-29, seuil, --dry-run (T-287) ----

const PERSO: &str = "model = \"m-perso\"\nbatch_files = 10\n";

/// `n` documents enrichis avec `m-perso`, puis `model` imposé par `.nephila.toml`.
fn imposed(n: usize, personal: &str, project_model_toml: &str) -> Env {
    let e = Env::new(n, personal);
    assert!(e.run(&["build"]).status.success());
    e.project(&format!("model = {project_model_toml}\n"));
    e.clear_log();
    e
}

fn warn_line(head: &str, n: usize, project: &str) -> String {
    format!(
        "avertissement: {head} : modèle `{project}` au lieu de `m-perso`, {n} fichiers déjà enrichis seront réenrichis ; écrivez model_override = \"deny\" dans la configuration personnelle pour garder `m-perso`"
    )
}

fn refusal_line(head: &str, n: usize, project: &str) -> String {
    format!(
        "erreur: {head} : modèle `{project}` au lieu de `m-perso`, {n} fichiers déjà enrichis seraient réenrichis (au plus 50 sans confirmation) ; relancez avec --accept-project-model ou écrivez model_override = \"deny\" dans la configuration personnelle pour garder `m-perso`"
    )
}

fn head(e: &Env) -> String {
    format!("{}/.nephila.toml", e.repo.display())
}

fn db_bytes(e: &Env) -> Vec<u8> {
    fs::read(e.repo.join(".nephila/graph.db")).unwrap()
}

fn status_json(e: &Env) -> String {
    text(&e.run(&["status", "--json"]).stdout)
}

fn first_with(err: &[String], needle: &str) -> usize {
    err.iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("pas de `{needle}` : {err:#?}"))
}

#[test]
fn req196_ac2_ac8_avertissement_en_premiere_ligne_puis_plus_rien() {
    let e = imposed(3, PERSO, "\"m-projet\"");
    let o = e.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = lines(&o.stderr);
    assert_eq!(err[0], warn_line(&head(&e), 3, "m-projet"));
    assert_eq!(err.iter().filter(|l| l.contains("au lieu de")).count(), 1);
    // 3 fichiers, `batch_files` 10 : un seul appel ; les entrées portent le modèle du dépôt.
    assert_eq!(e.calls(), 1);
    assert_eq!(e.count_model("m-projet"), 3);
    assert_eq!(e.models(), vec!["m-projet"]);
    let second = e.run(&["update"]);
    assert_eq!(second.status.code(), Some(0));
    assert!(!text(&second.stderr).contains("au lieu de"));
}

#[test]
fn req196_ac8_sans_c_la_tete_est_dot_slash() {
    let e = imposed(3, PERSO, "\"m-projet\"");
    let o = e
        .command()
        .current_dir(&e.repo)
        .arg("update")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(
        lines(&o.stderr)[0],
        warn_line("./.nephila.toml", 3, "m-projet")
    );
}

#[test]
fn req196_ac2_ac8_place_sous_verbose() {
    let e = imposed(3, PERSO, "\"m-projet\"");
    let o = e.run(&["update", "--verbose"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = lines(&o.stderr);
    let warn = first_with(&err, "avertissement:");
    assert_eq!(err[warn], warn_line(&head(&e), 3, "m-projet"));
    assert!(first_with(&err, "config:") < warn, "{err:#?}");
    assert!(warn < first_with(&err, "parcours:"), "{err:#?}");
}

#[test]
fn req196_ac2_ac8_place_sous_show_progress() {
    let e = imposed(3, PERSO, "\"m-projet\"");
    let o = e.run(&["update", "--show-progress"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = lines(&o.stderr);
    let warn = first_with(&err, "avertissement:");
    assert_eq!(warn, 0, "{err:#?}");
    assert!(warn < first_with(&err, "progression:"), "{err:#?}");
}

#[test]
fn req196_ac3_ac9_refus_a_51_fichiers_pour_update_et_build() {
    let e = imposed(51, PERSO, "\"m-projet\"");
    let db = db_bytes(&e);
    let status = status_json(&e);
    for args in [
        &["update"][..],
        &["build"][..],
        &["update", "--show-progress"][..],
        &["update", "--verbose"][..],
    ] {
        let o = e.run(args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(o.stdout.is_empty(), "{args:?}: {}", text(&o.stdout));
        let err = lines(&o.stderr);
        assert_eq!(
            err.last().unwrap(),
            &refusal_line(&head(&e), 51, "m-projet")
        );
        if args.len() == 1 {
            assert_eq!(err.len(), 1, "{err:#?}");
        }
        for l in &err {
            assert!(
                !l.starts_with("progression:")
                    && !l.contains("parcours:")
                    && !l.contains("structure:")
                    && !l.contains("llm enrich"),
                "{args:?}: {l}"
            );
        }
        assert_eq!(e.calls(), 0);
        assert_eq!(db_bytes(&e), db, "{args:?}");
        assert_eq!(status_json(&e), status, "{args:?}");
    }
}

#[test]
fn req196_ac3_accept_project_model_leve_le_refus() {
    let e = imposed(51, PERSO, "\"m-projet\"");
    let o = e.run(&["update", "--accept-project-model"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(lines(&o.stderr)[0], warn_line(&head(&e), 51, "m-projet"));
    assert_eq!(e.models(), vec!["m-projet"]);
    let db = rusqlite::Connection::open(e.repo.join(".nephila/graph.db")).unwrap();
    let n: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM enrichments WHERE model = 'm-projet'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 51);
}

#[test]
fn req196_ac3_cinquante_fichiers_avertissent_sans_refuser() {
    let e = imposed(50, PERSO, "\"m-projet\"");
    let o = e.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(lines(&o.stderr)[0], warn_line(&head(&e), 50, "m-projet"));
    assert!(!text(&o.stderr).contains("erreur:"));
    assert_eq!(e.models(), vec!["m-projet"]);
}

#[test]
fn req196_ac4_dry_run_avertit_estime_et_ne_modifie_rien() {
    let e = imposed(51, PERSO, "\"m-projet\"");
    let before = e.enrichment_rows();
    assert_eq!(before.len(), 51);
    let o = e.run(&["update", "--dry-run"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = lines(&o.stderr);
    assert_eq!(err[0], warn_line(&head(&e), 51, "m-projet"));
    assert!(!text(&o.stderr).contains("erreur:"));
    let out = lines(&o.stdout);
    let est: Vec<&String> = out.iter().filter(|l| l.starts_with("estimation")).collect();
    assert!(!est.is_empty(), "{out:#?}");
    assert!(
        est.iter().any(|l| l.contains("51 fichiers à enrichir")),
        "{est:#?}"
    );
    assert_eq!(e.calls(), 0);
    assert_eq!(e.enrichment_rows(), before);
    assert_eq!(e.models(), vec!["m-perso"]);
}

#[test]
fn req196_ac4_allow_ni_avertissement_ni_refus() {
    let e = imposed(
        51,
        "model = \"m-perso\"\nbatch_files = 10\nmodel_override = \"allow\"\n",
        "\"m-projet\"",
    );
    let o = e.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(
        !err.contains("avertissement:") && !err.contains("erreur:"),
        "{err}"
    );
    assert_eq!(e.models(), vec!["m-projet"]);
}

#[test]
fn req196_ac4_deny_graphe_a_jour_sans_appel() {
    let e = imposed(
        51,
        "model = \"m-perso\"\nbatch_files = 10\nmodel_override = \"deny\"\n",
        "\"m-projet\"",
    );
    let o = e.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(
        !err.contains("avertissement:") && !err.contains("erreur:"),
        "{err}"
    );
    assert!(
        text(&o.stdout).contains("graphe à jour"),
        "{}",
        text(&o.stdout)
    );
    assert_eq!(e.calls(), 0);
    assert_eq!(e.models(), vec!["m-perso"]);
}

#[test]
fn req196_ac10_echappement_de_l_avertissement_et_du_refus() {
    let raw = "\"m\\u001b]0;x\\u0007\"";
    let shown = "m\\u{001B}]0;x\\u{0007}";
    let e = imposed(3, PERSO, raw);
    let o = e.run(&["update"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(e.calls(), 1);
    assert_eq!(lines(&o.stderr)[0], warn_line(&head(&e), 3, shown));
    assert!(!o.stderr.contains(&0x1B) && !o.stderr.contains(&0x07));

    let e = imposed(51, PERSO, raw);
    let o = e.run(&["update"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        text(&o.stderr).trim_end(),
        refusal_line(&head(&e), 51, shown)
    );
    assert!(!o.stderr.contains(&0x1B) && !o.stderr.contains(&0x07));
}
