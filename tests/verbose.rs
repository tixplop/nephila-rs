//! Option `--verbose` de build et update (REQ-026) : configuration, parcours, phases.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

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

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new(config: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let repo = tmp.path().join("repo");
        copy_dir(&fx.join("maven2"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("mock"), &mock);
        let cfg = tmp.path().join("config.toml");
        fs::write(&cfg, config).unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            config: cfg,
            mock,
        }
    }

    fn mock() -> Env {
        Env::new("provider = \"mock\"\nenrich_code = true\nbatch_files = 1\nmodel = \"mock\"\nconcurrency = 4\n")
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        self.cmd_with(args, &[])
    }

    fn cmd_with(&self, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
        let mut c = Command::cargo_bin("nephila").unwrap();
        for k in [
            "NEPHILA_MOCK_FAIL_AFTER",
            "NEPHILA_MOCK_INVALID_KEY",
            "NEPHILA_MOCK_DELAY_MS",
            "NEPHILA_MOCK_TIMEOUT_FILES",
            "NEPHILA_MOCK_KEY_REVOKED_AFTER",
        ] {
            c.env_remove(k);
        }
        // Attente de nouvel essai HTTP minimale : un faux serveur à file épuisée ne ralentit pas la suite.
        c.env("NEPHILA_RETRY_BASE_MS", "1");
        c.envs(envs.iter().copied());
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }
}

fn stderr(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn trace_lines(o: &std::process::Output) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.starts_with("trace:"))
        .map(String::from)
        .collect()
}

fn has(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

fn well_formed(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("trace: [") else {
        return false;
    };
    let Some((time, _)) = rest.split_once("s] ") else {
        return false;
    };
    let Some((s, ms)) = time.split_once('.') else {
        return false;
    };
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_digit())
        && ms.len() == 3
        && ms.chars().all(|c| c.is_ascii_digit())
}

#[test]
fn req026_ac1_help_lists_verbose_and_status_rejects_it() {
    let env = Env::mock();
    for sub in ["build", "update"] {
        let o = env.cmd(&[sub, "--help"]);
        assert!(String::from_utf8_lossy(&o.stdout).contains("--verbose"));
    }
    let o = env.cmd(&["status", "--verbose"]);
    assert!(!o.status.success());
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn req026_ac2_stdout_identical_and_trace_format() {
    let plain = Env::mock();
    let o1 = plain.cmd(&["build"]);
    assert!(
        !stderr(&o1).lines().any(|l| l.starts_with("trace:")),
        "{}",
        stderr(&o1)
    );
    let verbose = Env::mock();
    let o2 = verbose.cmd(&["build", "--verbose"]);
    assert_eq!(o1.stdout, o2.stdout);
    assert_eq!(o1.status.code(), o2.status.code());
    let lines = trace_lines(&o2);
    assert!(!lines.is_empty());
    for l in &lines {
        assert!(well_formed(l), "{l}");
    }
}

#[test]
fn req026_ac3_config_parcours_and_phases() {
    let env = Env::mock();
    let o = env.cmd(&["build", "-v"]);
    let lines = trace_lines(&o);
    assert!(
        lines.iter().any(|l| l.contains("config")
            && l.contains("mock")
            && l.contains("concurrency=4")
            && l.contains("batch_files=1")
            && l.contains("timeout_secs=")),
        "{lines:?}"
    );
    assert!(!has(&lines, "verify_key"));
    assert!(has(&lines, "parcours"));
    assert!(has(&lines, "structure: début"));
    assert!(has(&lines, "structure: fin"));
    assert!(has(&lines, "résolution:"));
    let pos = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(pos("config") < pos("parcours"));
    assert!(pos("parcours") < pos("structure: début"));
    assert!(pos("structure: fin") < pos("résolution:"));
    for l in stderr(&o).lines() {
        assert!(
            l.starts_with("trace:") || l.starts_with("avertissement:") || l.starts_with("erreur:"),
            "{l}"
        );
    }
}

#[test]
fn req026_ac7_no_llm_and_no_config() {
    let env = Env::mock();
    let o = env.cmd(&["build", "--no-llm", "--verbose"]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "llm inactif (--no-llm)"));
    assert!(!has(&lines, "llm enrich"));
    assert!(!has(&lines, "verify_key"));

    let env = Env::mock();
    fs::remove_file(&env.config).unwrap();
    let o = env.cmd(&["build", "-v"]);
    assert!(has(&trace_lines(&o), "llm inactif (aucune configuration)"));
}

#[test]
fn req026_ac7_update_without_change_and_without_graph() {
    let env = Env::mock();
    // Sans graphe : build complet tracé.
    let o = env.cmd(&["update", "-v"]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "structure: début"), "{lines:?}");
    assert!(has(&lines, "parcours"));
    let env2 = Env::mock();
    assert!(env2.cmd(&["build"]).status.success());
    let o = env2.cmd(&["update", "--verbose"]);
    assert!(o.status.success());
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "graphe à jour");
    let lines = trace_lines(&o);
    assert!(has(&lines, "config"));
    assert!(has(
        &lines,
        "parcours: 0 ajoutés, 0 modifiés, 0 supprimés, 0 en attente"
    ));
}

fn enrich_ends(lines: &[String]) -> Vec<&String> {
    lines
        .iter()
        .filter(|l| l.contains("llm enrich fin"))
        .collect()
}

#[test]
fn req026_ac3_enrich_calls_paired_and_match_mock_log() {
    let env = Env::mock();
    let o = env.cmd(&["build", "-v"]);
    let lines = trace_lines(&o);
    let ends = enrich_ends(&lines);
    let log = fs::read_to_string(&env.log).unwrap();
    assert!(!ends.is_empty());
    assert_eq!(ends.len(), log.lines().count(), "{lines:?}");
    assert!(has(&lines, "enrichissement: début"));
    assert!(has(&lines, ", 4 fils"), "{lines:?}");
    assert!(has(&lines, "enrichissement: fin"));
    for (i, l) in lines.iter().enumerate() {
        let Some(rest) = l.split("llm enrich fin ").nth(1) else {
            continue;
        };
        let key: Vec<&str> = rest.split_whitespace().take(2).collect();
        let start = format!("llm enrich départ {} {}", key[0], key[1]);
        assert!(lines[..i].iter().any(|p| p.contains(&start)), "{l}");
        assert!(l.contains(" ms"), "{l}");
        assert!(l.contains("ok ("), "{l}");
    }
    for l in stderr(&o).lines() {
        assert!(
            l.starts_with("trace:") || l.starts_with("avertissement:") || l.starts_with("erreur:"),
            "{l}"
        );
    }
}

#[test]
fn req026_ac4_timeout_file() {
    let env = Env::mock();
    let o = env.cmd_with(
        &["build", "-v"],
        &[("NEPHILA_MOCK_TIMEOUT_FILES", "core/pom.xml")],
    );
    let lines = trace_lines(&o);
    let timeouts: Vec<_> = enrich_ends(&lines)
        .into_iter()
        .filter(|l| l.contains("délai dépassé"))
        .collect();
    assert_eq!(timeouts.len(), 1, "{lines:?}");
    assert!(timeouts[0].contains("core/pom.xml"));
}

#[test]
fn req026_ac4_network_policy_trace() {
    let env =
        Env::new("provider = \"mock\"\nenrich_code = true\nbatch_files = 1\nmodel = \"mock\"\nconcurrency = 1\n");
    let o = env.cmd_with(&["build", "-v"], &[("NEPHILA_MOCK_FAIL_AFTER", "2")]);
    let lines = trace_lines(&o);
    let pos = |n: &str| {
        lines
            .iter()
            .position(|l| l.contains(n))
            .unwrap_or_else(|| panic!("{n} absent : {lines:?}"))
    };
    let (a, b, c, d) = (
        pos("politique: erreurs réseau consécutives 1/3"),
        pos("politique: erreurs réseau consécutives 2/3"),
        pos("politique: erreurs réseau consécutives 3/3"),
        pos("politique: arrêt de l'envoi"),
    );
    assert!(a < b && b < c && c < d);
    assert!(lines[d].contains("fichiers non envoyés"));
    assert!(has(&lines, "réseau"));
    assert!(has(&lines, "enrichissement: fin"));
}

#[test]
fn req026_ac4_key_revoked() {
    let env =
        Env::new("provider = \"mock\"\nenrich_code = true\nbatch_files = 1\nmodel = \"mock\"\nconcurrency = 1\n");
    let o = env.cmd_with(&["build", "-v"], &[("NEPHILA_MOCK_KEY_REVOKED_AFTER", "2")]);
    let lines = trace_lines(&o);
    let r = lines
        .iter()
        .position(|l| l.contains("llm enrich fin") && l.contains("clé refusée"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    let s = lines
        .iter()
        .position(|l| l.contains("politique: arrêt de l'envoi"))
        .unwrap_or_else(|| panic!("{lines:?}"));
    assert!(r < s);
}

fn openai_env(server: &FakeServer, timeout: u32, source: &str) -> Env {
    let env = Env::new(&format!(
        "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T049_KEY\"\n\
         concurrency = 1\ntimeout_secs = {timeout}\n",
        server.base_url
    ));
    fs::remove_dir_all(&env.repo).unwrap();
    fs::create_dir_all(&env.repo).unwrap();
    fs::write(env.repo.join("A.java"), source).unwrap();
    env
}

fn openai_reply(text: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":text}}]}).to_string()
}

#[test]
fn req026_ac5_openai_timeout_duration() {
    let server = FakeServer::start_with_delays(vec![(200, openai_reply("{}"), 2000)]);
    let env = openai_env(&server, 1, "class A {}\n");
    let o = env.cmd_with(&["build", "-v"], &[("NEPHILA_T049_KEY", "sk-x")]);
    let lines = trace_lines(&o);
    let ends = enrich_ends(&lines);
    assert_eq!(ends.len(), 1, "{lines:?}");
    assert!(ends[0].contains("délai dépassé"), "{}", ends[0]);
    let ms: u64 = ends[0]
        .split(" ms")
        .next()
        .unwrap()
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!((900..=2000).contains(&ms), "{ms}");
}

#[test]
fn req026_ac5_openai_key_refused_at_first_call() {
    let server = FakeServer::start(vec![(401, "{}".into())]);
    let env = openai_env(&server, 5, "class A {}\n");
    let o = env.cmd_with(&["build", "-v"], &[("NEPHILA_T049_KEY", "sk-x")]);
    assert!(!o.status.success());
    let lines = trace_lines(&o);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("llm enrich fin") && l.contains("clé refusée")),
        "{lines:?}"
    );
    assert_eq!(o.status.code(), Some(1));
    let errs: Vec<String> = stderr(&o)
        .lines()
        .filter(|l| l.starts_with("erreur:"))
        .map(String::from)
        .collect();
    assert_eq!(
        errs,
        vec!["erreur: clé d'API refusée par openai (HTTP 401) ; vérifiez la variable désignée par api_key_env".to_string()],
        "{}",
        stderr(&o)
    );
}

fn assert_no_secret_in_failed_call(reply_text: &str) {
    let server = FakeServer::start(vec![(200, openai_reply(reply_text))]);
    let env = openai_env(&server, 5, "class A {}\n");
    let o = env.cmd_with(&["build", "-v"], &[("NEPHILA_T049_KEY", "sk-test-SECRET")]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "llm enrich fin"), "{lines:?}");
    for l in &lines {
        assert!(!l.contains("SECRET"), "{l}");
    }
}

#[test]
fn req026_ac6_no_secret_from_nonconforming_value() {
    assert_no_secret_in_failed_call(
        r#"{"version":1,"summaries":"SECRET_REPONSE_42 abc","concepts":[]}"#,
    );
}

#[test]
fn req026_ac6_no_secret_from_unknown_field() {
    assert_no_secret_in_failed_call(
        r#"{"version":1,"summaries":[],"concepts":[],"SECRET_CHAMP_42":1}"#,
    );
}

#[test]
fn req026_ac6_no_secret_in_traces() {
    let long = format!("SECRET_REPONSE_42 {}", "x".repeat(5000));
    let server = FakeServer::start(vec![(200, openai_reply(&long))]);
    let env = openai_env(
        &server,
        5,
        "class A { String s = \"SECRET_CONTENU_42\"; }\n",
    );
    let o = env.cmd_with(&["build", "-v"], &[("NEPHILA_T049_KEY", "sk-test-SECRET")]);
    let lines = trace_lines(&o);
    assert!(has(&lines, "llm enrich fin"), "{lines:?}");
    for l in &lines {
        for m in ["SECRET_CONTENU_42", "SECRET_REPONSE_42", "sk-test-SECRET"] {
            assert!(!l.contains(m), "{l}");
        }
        assert!(l.chars().count() <= 300, "{l}");
    }
}

// ---- REQ-042 : marque de source de la ligne `config` ----

fn config_line(o: &std::process::Output) -> String {
    trace_lines(o)
        .into_iter()
        .find(|l| l.contains("] config: "))
        .unwrap_or_else(|| panic!("{}", stderr(o)))
}

impl Env {
    fn project(&self, content: &str) {
        fs::write(self.repo.join(".nephila.toml"), content).unwrap();
    }
}

#[test]
fn req026_ac12_marque_de_source() {
    // modèle et format_retries, dans cet ordre dans le fichier
    let env = Env::mock();
    env.project("format_retries = 1\nmodel = \"m-projet\"\n");
    let o = env.cmd(&["build", "--verbose"]);
    let line = config_line(&o);
    assert!(line.contains(" model=m-projet "), "{line}");
    assert!(
        line.ends_with(" format_retries=1 (.nephila.toml : model,format_retries)"),
        "{line}"
    );
    // update sans changement : même fin de ligne
    let o = env.cmd(&["update", "--verbose"]);
    assert!(
        config_line(&o).ends_with(" format_retries=1 (.nephila.toml : model,format_retries)"),
        "{}",
        stderr(&o)
    );

    // l'option remplace batch_files, qui n'est plus nommé
    let env = Env::mock();
    env.project("batch_files = 2\nconcurrency = 3\n");
    let o = env.cmd(&["build", "--verbose", "--batch-files", "1"]);
    let line = config_line(&o);
    assert!(
        line.ends_with(" batch_files=1 format_retries=2 (.nephila.toml : concurrency)"),
        "{line}"
    );

    // réglages TLS avant la marque
    let env =
        Env::new("provider = \"mock\"\nenrich_code = true\nbatch_files = 1\nmodel = \"mock\"\nconcurrency = 4\ntls_insecure = true\n");
    env.project("batch_files = 2\nconcurrency = 3\n");
    let o = env.cmd(&["build", "--verbose", "--batch-files", "1"]);
    let line = config_line(&o);
    assert!(
        line.ends_with(" format_retries=2 tls_insecure (.nephila.toml : concurrency)"),
        "{line}"
    );

    // valeur égale à celle de la configuration personnelle : nommée quand même
    let env = Env::mock();
    env.project("concurrency = 4\n");
    let o = env.cmd(&["build", "--verbose"]);
    assert!(config_line(&o).ends_with(" (.nephila.toml : concurrency)"));

    // filtres seuls : aucune marque
    let env = Env::mock();
    env.project("exclude_types = [\"json\"]\n");
    let o = env.cmd(&["build", "--verbose"]);
    assert!(!config_line(&o).contains(".nephila.toml"));

    // LLM inactif : ligne exacte, aucune marque
    let env = Env::mock();
    fs::remove_file(&env.config).unwrap();
    env.project(
        "model = \"x\"\nmax_chunk_chars = 1000\nconcurrency = 2\ntimeout_secs = 5\nbatch_files = 2\nformat_retries = 1\n",
    );
    let o = env.cmd(&["build", "--verbose"]);
    assert!(config_line(&o).ends_with("] config: llm inactif (aucune configuration)"));
    assert!(!stderr(&o).contains("(.nephila.toml :"), "{}", stderr(&o));
}

#[test]
fn req042_ac10_aucune_trace_ne_contient_la_cle_d_api() {
    let secret = "sk-SECRET-VALEUR-T087-reconnaissable";
    let server = FakeServer::start(vec![(
        200,
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":
                r#"{"version":2,"summaries":[],"concepts":[]}"#}}]})
        .to_string(),
    )]);
    let env = openai_env(&server, 30, "class A {}\n");
    env.project("model = \"m-projet\"\n");
    for sub in ["build", "update"] {
        let o = env.cmd_with(&[sub, "--verbose"], &[("NEPHILA_T049_KEY", secret)]);
        let all = format!("{}{}", stderr(&o), String::from_utf8_lossy(&o.stdout));
        assert!(!all.contains(secret), "{sub}: {all}");
        assert!(all.contains("(.nephila.toml : model)"), "{sub}: {all}");
    }
}

/// Lignes de stderr qui contiennent ` non résolues`.
fn pass_lines(o: &std::process::Output) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.contains(" non résolues"))
        .map(String::from)
        .collect()
}

/// Forme d'AC-090-4 : `^trace: [..] résolution: \d+ ms, \d+ liens, \d+ non résolues (…)$`.
fn pass_kind(line: &str) -> String {
    assert!(well_formed(line), "{line}");
    let rest = line.split("] résolution: ").nth(1).expect(line);
    let (counts, kind) = rest.split_once(" non résolues (").expect(line);
    let nums: Vec<&str> = counts.split(", ").collect();
    assert_eq!(nums.len(), 3, "{line}");
    assert!(
        nums[0].ends_with(" ms") && nums[1].ends_with(" liens"),
        "{line}"
    );
    assert!(
        nums.iter().all(|n| n.as_bytes()[0].is_ascii_digit()),
        "{line}"
    );
    kind.strip_suffix(')').expect(line).to_string()
}

fn refs_in_graph(env: &Env) -> usize {
    let c = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    c.query_row("SELECT COUNT(*) FROM refs", [], |r| r.get::<_, i64>(0))
        .unwrap() as usize
}

#[test]
fn req090_ac1_full_pass_after_no_llm_build() {
    let env = Env::mock();
    let o = env.cmd(&["build", "--no-llm", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = pass_lines(&o);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with(" non résolues (complète)"),
        "{}",
        lines[0]
    );
    assert_eq!(pass_kind(&lines[0]), "complète");
}

#[test]
fn req090_ac1_incremental_pass_after_a_modified_file() {
    let env = Env::mock();
    assert!(env.cmd(&["build", "--no-llm"]).status.success());
    let java = env
        .repo
        .join("core/src/main/java/com/acme/core/OrderService.java");
    let mut text = fs::read_to_string(&java).unwrap();
    text.push_str("\n// modifié\n");
    fs::write(&java, text).unwrap();
    let o = env.cmd(&["update", "--no-llm", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = pass_lines(&o);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains(" non résolues (incrémentale, "),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].ends_with(" références réévaluées)"),
        "{}",
        lines[0]
    );
    let kind = pass_kind(&lines[0]);
    let n: usize = kind
        .strip_prefix("incrémentale, ")
        .and_then(|k| k.strip_suffix(" références réévaluées"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(n > 0 && n < refs_in_graph(&env), "n = {n}");
}

#[test]
fn req090_ac1_mock_build_has_a_full_then_an_incremental_pass() {
    let env = Env::mock();
    let o = env.cmd(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let all: Vec<String> = stderr(&o).lines().map(String::from).collect();
    let lines = pass_lines(&o);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(pass_kind(&lines[0]), "complète");
    assert!(
        pass_kind(&lines[1]).starts_with("incrémentale, ")
            && pass_kind(&lines[1]).ends_with(" références réévaluées"),
        "{}",
        lines[1]
    );
    // AC-090-4 : place des deux lignes par rapport aux appels d'enrichissement.
    let pos = |pred: &dyn Fn(&String) -> bool| all.iter().position(pred);
    let rpos = |pred: &dyn Fn(&String) -> bool| all.iter().rposition(pred);
    let first_start = pos(&|l| l.contains("llm enrich départ")).expect("départ");
    let last_end = rpos(&|l| l.contains("llm enrich fin")).expect("fin");
    let full = pos(&|l| l.contains("(complète)")).unwrap();
    let incr = pos(&|l| l.contains("(incrémentale, ")).unwrap();
    assert!(full < first_start && incr > last_end, "{all:?}");
}

#[test]
fn req090_ac1_unresolved_graph_gets_a_full_pass_without_any_change() {
    let env = Env::mock();
    assert!(env.cmd(&["build", "--no-llm"]).status.success());
    rusqlite::Connection::open(env.repo.join(".nephila/graph.db"))
        .unwrap()
        .execute(
            "UPDATE meta SET value = 'à résoudre' WHERE key = 'resolution'",
            [],
        )
        .unwrap();
    let o = env.cmd(&["update", "--no-llm", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = pass_lines(&o);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(pass_kind(&lines[0]), "complète");
}

// ---- REQ-147 : trace des communautés (AC-147-8, AC-147-9) ----

fn communities_in_graph(env: &Env) -> usize {
    let c = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    c.query_row("SELECT COUNT(*) FROM communities", [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap() as usize
}

/// Lignes de stderr dont le texte (après le préfixe horodaté) commence par `communautés:`.
fn community_lines(o: &std::process::Output) -> Vec<String> {
    stderr(o)
        .lines()
        .filter(|l| l.contains("] communautés: "))
        .map(String::from)
        .collect()
}

/// Forme d'AC-147-9 : `trace: [<s>.<mmm>s] communautés: <ms> ms, <n> communautés` ; rend `<n>`.
fn community_count(line: &str) -> usize {
    assert!(well_formed(line), "{line}");
    let rest = line.split("] communautés: ").nth(1).expect(line);
    let (ms, n) = rest.split_once(" ms, ").expect(line);
    assert!(
        !ms.is_empty() && ms.chars().all(|c| c.is_ascii_digit()),
        "{line}"
    );
    let n = n.strip_suffix(" communautés").expect(line);
    assert!(
        !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()),
        "{line}"
    );
    n.parse().unwrap()
}

/// Chaque ligne `communautés:` suit immédiatement une ligne `résolution:` de la même commande.
fn assert_each_follows_a_resolution(o: &std::process::Output) {
    let text = stderr(o);
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if l.contains("] communautés: ") {
            assert!(
                i > 0 && lines[i - 1].contains("] résolution: "),
                "ligne {i} : {l}\nprécédente : {:?}",
                i.checked_sub(1).map(|j| lines[j])
            );
        }
    }
}

#[test]
fn req147_ac8_ac9_no_llm_build_traces_one_communities_line_after_the_resolution() {
    let env = Env::mock();
    let o = env.cmd(&["build", "--no-llm", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = community_lines(&o);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let n = community_count(&lines[0]);
    assert!(n > 0);
    assert_eq!(n, communities_in_graph(&env));
    assert_each_follows_a_resolution(&o);
    assert!(pass_lines(&o)[0].ends_with("(complète)"));
}

#[test]
fn req147_ac9_mock_build_traces_one_line_after_each_of_the_two_passes() {
    let env = Env::mock();
    let o = env.cmd(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = community_lines(&o);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_each_follows_a_resolution(&o);
    let all: Vec<String> = stderr(&o).lines().map(String::from).collect();
    let at = |needle: &str| all.iter().position(|l| l.contains(needle)).unwrap();
    // La première suit la passe complète, la seconde l'incrémentale.
    assert!(all[at("(complète)") + 1].contains("] communautés: "));
    assert!(all[at("(incrémentale, ") + 1].contains("] communautés: "));
    // Le nombre de la dernière ligne est celui des tables ; l'enrichissement ajoute des concepts.
    assert_eq!(community_count(&lines[1]), communities_in_graph(&env));
}

#[test]
fn req147_ac9_update_after_a_change_traces_one_line_and_without_change_none() {
    let env = Env::mock();
    assert!(env.cmd(&["build", "--no-llm"]).status.success());
    let java = env
        .repo
        .join("core/src/main/java/com/acme/core/OrderService.java");
    let mut text = fs::read_to_string(&java).unwrap();
    text.push_str("\n// modifié\n");
    fs::write(&java, text).unwrap();
    let o = env.cmd(&["update", "--no-llm", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let lines = community_lines(&o);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(community_count(&lines[0]), communities_in_graph(&env));
    assert_each_follows_a_resolution(&o);
    assert!(pass_lines(&o)[0].contains("(incrémentale, "));
    // Sans changement : ni résolution ni calcul.
    let o = env.cmd(&["update", "--no-llm", "--verbose"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(community_lines(&o).is_empty(), "{}", stderr(&o));
    assert!(pass_lines(&o).is_empty());
}

#[test]
fn req147_ac9_without_verbose_no_stderr_line_mentions_communities() {
    let env = Env::mock();
    for args in [&["build", "--no-llm"][..], &["build"][..]] {
        let o = env.cmd(args);
        assert!(o.status.success(), "{}", stderr(&o));
        assert!(!stderr(&o).contains("communautés"), "{}", stderr(&o));
    }
}
