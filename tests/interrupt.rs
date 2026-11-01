//! Premier Ctrl-C pendant l'enrichissement (REQ-189, REQ-191) : appels en vol enregistrés, attente
//! et essais de format abandonnés, fichier envoyé seul, portée, signal ignoré. Le binaire est lancé
//! en sous-processus et reçoit SIGINT par `libc::kill` : le processus de test n'est jamais signalé.
//! L'instant d'envoi du signal est piloté par le compte de requêtes reçues du faux serveur, jamais
//! par un délai fixe ; aucune borne serrée.

use assert_cmd::cargo::cargo_bin;
use nephila::chunk::split;
use nephila::store::Store;
use std::fs;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/plan_server.rs"]
mod plan_server;
use plan_server::{Act, Server};

const KEY_VAR: &str = "NEPHILA_T280_KEY";

const A12: &str = "avertissement: Ctrl-C reçu : plus aucun appel ne part, ceux en cours vont à leur terme et seront enregistrés ; appuyez de nouveau sur Ctrl-C pour quitter sans les attendre";

fn e27(n: usize) -> String {
    format!("erreur: envoi interrompu : Ctrl-C reçu : relancez nephila update pour continuer ; {n} fichier(s) à enrichir")
}

fn small(i: usize) -> String {
    format!("# D{i}\n\nTexte du document {i}.\n")
}

fn docs(n: usize) -> Vec<(String, String)> {
    (1..=n).map(|i| (format!("d{i}.md"), small(i))).collect()
}

fn names(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("d{i}.md")).collect()
}

/// Attente active de 10 ms, plafonnée à 10 s.
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "délai dépassé : {what}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    settings: String,
}

impl Env {
    /// `settings` : lignes TOML de la configuration personnelle, après le fournisseur.
    fn new(files: &[(String, String)], url: &str, settings: &str) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        for (path, content) in files {
            let p = repo.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let env = Env {
            config: tmp.path().join("config.toml"),
            _tmp: tmp,
            repo,
            settings: settings.to_string(),
        };
        env.set_url(url);
        env
    }

    fn set_url(&self, url: &str) {
        fs::write(
            &self.config,
            format!(
                "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{url}\"\napi_key_env = \"{KEY_VAR}\"\n{}",
                self.settings
            ),
        )
        .unwrap();
    }

    fn project(&self, content: &str) {
        fs::write(self.repo.join(".nephila.toml"), content).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).args(args);
        c.env_remove("NO_COLOR")
            .env("NEPHILA_CONFIG", &self.config)
            .env(KEY_VAR, "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "10")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn spawn(&self, args: &[&str]) -> Running {
        Running::start(self.command(args))
    }

    /// Lance la commande jusqu'à sa fin, sans signal.
    fn run(&self, args: &[&str]) -> Done {
        self.spawn(args).finish()
    }

    /// Parmi `paths`, ceux qui ont une entrée d'enrichissement.
    fn enriched(&self, paths: &[String]) -> Vec<String> {
        let store = Store::open_readonly(&self.repo).unwrap();
        paths
            .iter()
            .filter(|p| store.enrichment_entry(p).unwrap().is_some())
            .cloned()
            .collect()
    }

    fn to_enrich(&self) -> u64 {
        let o = self.command(&["status", "--json"]).output().unwrap();
        assert_eq!(o.status.code(), Some(0));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        v["to_enrich"].as_u64().unwrap()
    }

    /// Nœuds du fichier `path` qui portent un résumé, lus par `nephila query search`.
    fn summaries_by_query(&self, path: &str) -> usize {
        let o = self
            .command(&["query", "search", path, "--json", "--limit", "100"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        v["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["file"] == path && n["summary"].as_str().is_some_and(|s| !s.is_empty()))
            .count()
    }

    /// Parmi `paths`, ceux dont `query` rend au moins un nœud résumé.
    fn summarized(&self, paths: &[String]) -> Vec<String> {
        paths
            .iter()
            .filter(|p| self.summaries_by_query(p) > 0)
            .cloned()
            .collect()
    }

    /// Nœuds du fichier `path` qui portent un résumé.
    fn summarized_nodes(&self, path: &str) -> usize {
        Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .iter()
            .filter(|n| n.file.as_deref() == Some(path) && n.summary.is_some())
            .count()
    }
}

/// Enfant `nephila` dont stdout et stderr sont lus par des fils.
struct Running {
    child: Child,
    out: Option<std::thread::JoinHandle<String>>,
    err: Option<std::thread::JoinHandle<String>>,
    signalled: Option<Instant>,
}

fn reader(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = r.read_to_end(&mut b);
        String::from_utf8_lossy(&b).into_owned()
    })
}

impl Drop for Running {
    /// Un test qui échoue avant `finish` ne laisse pas d'enfant en vie.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Running {
    fn start(mut c: Command) -> Running {
        let mut child = c.spawn().unwrap();
        let out = Some(reader(child.stdout.take().unwrap()));
        let err = Some(reader(child.stderr.take().unwrap()));
        Running {
            child,
            out,
            err,
            signalled: None,
        }
    }

    /// Envoie SIGINT à l'enfant ; rend l'instant d'envoi.
    fn sigint(&mut self) -> Instant {
        // SAFETY: le pid est celui d'un enfant vivant, non encore attendu.
        let r = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(r, 0);
        let t = Instant::now();
        self.signalled = Some(t);
        t
    }

    fn finish(mut self) -> Done {
        let start = Instant::now();
        let status = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s;
            }
            if start.elapsed() > Duration::from_secs(50) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("la commande ne finit pas");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let ended = Instant::now();
        Done {
            status,
            stdout: self.out.take().unwrap().join().unwrap(),
            stderr: self.err.take().unwrap().join().unwrap(),
            after_signal: self.signalled.map(|t| ended.duration_since(t)),
        }
    }
}

struct Done {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
    /// Durée entre le dernier signal envoyé et la fin du processus.
    after_signal: Option<Duration>,
}

impl Done {
    fn code(&self) -> Option<i32> {
        self.status.code()
    }

    fn last(&self) -> &str {
        self.stderr.lines().last().unwrap_or("")
    }

    fn lines(&self, prefix: &str) -> Vec<&str> {
        self.stderr
            .lines()
            .filter(|l| l.starts_with(prefix))
            .collect()
    }
}

fn big_doc() -> String {
    (1..=3)
        .map(|i| format!("# Section {i}\n\n{}\n\n", "mot ".repeat(40)))
        .collect()
}

const MAX_CHUNK: usize = 250;

// ---- AC-189-1, AC-189-3 (1) : appels en vol enregistrés, le reste à enrichir ----

#[test]
fn req189_ac1_appels_en_vol_enregistres_le_reste_a_enrichir() {
    let server = Server::start(|_| Act::Ok(1000));
    let env = Env::new(
        &docs(6),
        &server.base_url,
        "concurrency = 2\nbatch_files = 1\nformat_retries = 0\n",
    );
    let mut run = env.spawn(&["build"]);
    wait_until("2 requêtes reçues", || server.requests() >= 2);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    assert_eq!(server.requests(), 2, "{}", d.stderr);
    let done = env.enriched(&names(6));
    assert_eq!(done.len(), 2, "{}", d.stderr);
    assert_eq!(env.summarized(&names(6)), done, "résumés lus par query");
    assert_eq!(env.to_enrich(), 4);
    assert_eq!(d.stdout.lines().count(), 1, "{}", d.stdout);
    assert!(
        d.stdout.starts_with("build terminé : 6 fichiers traités, ")
            && d.stdout.contains(", 4 fichiers à enrichir"),
        "{}",
        d.stdout
    );

    let fast = Server::start(|_| Act::Ok(0));
    env.set_url(&fast.base_url);
    let u = env.run(&["update"]);
    assert_eq!(u.code(), Some(0), "{}", u.stderr);
    assert_eq!(fast.requests(), 4, "{}", u.stderr);
    assert_eq!(env.enriched(&names(6)).len(), 6);
    assert_eq!(env.summarized(&names(6)).len(), 6, "résumés lus par query");
    assert_eq!(env.to_enrich(), 0);
}

// ---- AC-189-2, AC-189-10 : attente de nouvel essai HTTP interrompue ----

#[test]
fn req189_ac2_ac10_attente_interrompue() {
    let server = Server::start(|i| {
        if i == 0 {
            Act::Status(429, Some("30"))
        } else {
            Act::Ok(0)
        }
    });
    let env = Env::new(
        &docs(2),
        &server.base_url,
        "concurrency = 1\nbatch_files = 1\nformat_retries = 0\n",
    );
    let mut run = env.spawn(&["build"]);
    wait_until("429 servie", || server.served() >= 1);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    assert!(
        d.after_signal.unwrap() < Duration::from_secs(2),
        "{:?}",
        d.after_signal
    );
    assert_eq!(server.requests(), 1, "{}", d.stderr);
    assert!(env.enriched(&names(2)).is_empty());

    let warnings = d.lines("avertissement: ");
    assert_eq!(warnings[0], A12, "{}", d.stderr);
    assert!(
        warnings.iter().any(|l| l.contains("(HTTP 429)")),
        "{}",
        d.stderr
    );
    for w in &warnings[1..] {
        assert!(w.contains("(HTTP 429)"), "{w}");
    }
    assert_eq!(d.last(), e27(2), "{}", d.stderr);
    for l in d.stderr.lines() {
        assert!(
            !l.contains("nouvel essai") && !l.contains("interrompue") && !l.contains("annulé"),
            "{}",
            d.stderr
        );
    }
}

// ---- AC-189-3, AC-189-11 : nouvel essai de format abandonné ----

#[test]
fn req189_ac3_ac11_essai_de_format_abandonne() {
    let server = Server::start(|_| Act::SlowNonConforming(1000));
    let env = Env::new(
        &docs(2),
        &server.base_url,
        "concurrency = 1\nbatch_files = 1\nformat_retries = 2\n",
    );
    let mut run = env.spawn(&["build"]);
    wait_until("1 requête reçue", || server.requests() >= 1);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    assert_eq!(server.requests(), 1, "{}", d.stderr);
    assert_eq!(env.to_enrich(), 2);
    assert!(env.enriched(&names(2)).is_empty());

    let lines: Vec<&str> = d.stderr.lines().collect();
    let announce = lines.iter().position(|l| *l == A12).expect(&d.stderr);
    let format: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("réponse LLM non conforme au schéma"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(format.len(), 1, "{}", d.stderr);
    let l = lines[format[0]];
    assert!(
        l.starts_with("avertissement: d1.md : réponse LLM non conforme au schéma : "),
        "{l}"
    );
    assert!(l.ends_with("(1 essais sur 3)"), "{l}");
    assert!(format[0] > announce, "{}", d.stderr);
    assert!(
        !d.lines("avertissement: ")
            .iter()
            .any(|l| l.contains("d2.md")),
        "{}",
        d.stderr
    );
    assert_eq!(d.last(), e27(2), "{}", d.stderr);
}

// ---- AC-189-4 : fichier envoyé seul, appels suivants abandonnés ----

#[test]
fn req189_ac4_fichier_envoye_seul() {
    assert_eq!(split(&big_doc(), MAX_CHUNK).len(), 3);
    let server = Server::start(|_| Act::Ok(1000));
    let env = Env::new(
        &[("big.md".to_string(), big_doc())],
        &server.base_url,
        "concurrency = 1\nbatch_files = 1\nformat_retries = 0\n",
    );
    env.project(&format!("max_chunk_chars = {MAX_CHUNK}\n"));
    let mut run = env.spawn(&["build"]);
    wait_until("1 requête reçue", || server.requests() >= 1);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    assert_eq!(server.requests(), 1, "{}", d.stderr);
    assert_eq!(env.to_enrich(), 1);
    assert!(env.enriched(&["big.md".to_string()]).is_empty());
    assert_eq!(env.summarized_nodes("big.md"), 0);
    assert_eq!(d.lines("avertissement: "), vec![A12], "{}", d.stderr);
    assert_eq!(d.last(), e27(1), "{}", d.stderr);
}

// ---- AC-189-5 : portée, `query ask` garde l'effet par défaut du signal ----

#[test]
fn req189_ac5_query_ask_tue_par_le_signal() {
    let server = Server::start(|_| Act::Ok(5000));
    let env = Env::new(
        &docs(2),
        &server.base_url,
        "concurrency = 1\nbatch_files = 1\nformat_retries = 0\n",
    );
    let b = env.run(&["build", "--no-llm"]);
    assert_eq!(b.code(), Some(0), "{}", b.stderr);
    let mut run = env.spawn(&["query", "ask", "d1.md"]);
    wait_until("requête reçue", || server.requests() >= 1);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.status.signal(), Some(libc::SIGINT), "{}", d.stderr);
    assert!(
        d.after_signal.unwrap() < Duration::from_secs(1),
        "{:?}",
        d.after_signal
    );
    assert!(d.lines("erreur:").is_empty(), "{}", d.stderr);
}

// ---- AC-189-6 : SIGINT ignoré à l'entrée, il le reste ----

#[test]
fn req189_ac6_signal_ignore_a_l_entree() {
    let server = Server::start(|_| Act::Ok(1000));
    let env = Env::new(
        &docs(3),
        &server.base_url,
        "concurrency = 1\nbatch_files = 1\nformat_retries = 0\n",
    );
    let mut c = env.command(&["build"]);
    // SAFETY: `signal` est sûr entre `fork` et `exec`.
    unsafe {
        c.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut run = Running::start(c);
    wait_until("1 requête reçue", || server.requests() >= 1);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(0), "{}", d.stderr);
    assert_eq!(server.requests(), 3, "{}", d.stderr);
    assert_eq!(env.enriched(&names(3)).len(), 3);
    assert!(!d.stderr.contains("Ctrl-C"), "{}", d.stderr);
}
