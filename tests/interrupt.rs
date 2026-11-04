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

// ---- AC-227-7 : l'appel en vol à l'arrêt est conservé, la reprise n'envoie que les autres ----

#[test]
fn req227_ac7_reprise_apres_signal_ne_renvoie_que_les_appels_manquants() {
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
    assert_eq!(env.summarized_nodes("big.md"), 0);
    assert_eq!(d.lines("avertissement: "), vec![A12], "{}", d.stderr);
    assert_eq!(d.last(), e27(1), "{}", d.stderr);

    let fast = Server::start(|_| Act::Ok(0));
    env.set_url(&fast.base_url);
    let u = env.run(&["update"]);
    assert_eq!(u.code(), Some(0), "{}", u.stderr);
    assert_eq!(fast.requests(), 2, "morceaux 2 et 3 : {}", u.stderr);
    assert_eq!(env.enriched(&["big.md".to_string()]).len(), 1);
    assert_eq!(env.to_enrich(), 0);
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

// ---- REQ-190, REQ-191 : second Ctrl-C, annonce, trace, ordre de stderr, plafond, autre cause ----

const E28: &str =
    "erreur: envoi interrompu : second Ctrl-C reçu : relancez nephila update pour continuer";

const A10_1: &str = "avertissement: plafond de 1 appels atteint : 2 fichiers laissés à enrichir ; relancez nephila update pour continuer ou augmentez max_calls";

const E27_BARE: &str =
    "erreur: envoi interrompu : Ctrl-C reçu : relancez nephila update pour continuer";

const TRACE_STOP: &str = "politique: arrêt de l'envoi (Ctrl-C reçu), 4 fichiers non envoyés";

const FIX189: &str = "concurrency = 2\nbatch_files = 1\nformat_retries = 0\n";

/// Fixture d'AC-189-1 : 6 documents, serveur conforme à 1 s, SIGINT à 2 requêtes reçues.
fn fixture_189(args: &[&str], no_color: bool) -> (Env, Done) {
    let server = Server::start(|_| Act::Ok(1000));
    let env = Env::new(&docs(6), &server.base_url, FIX189);
    let mut c = env.command(args);
    if no_color {
        c.env("NO_COLOR", "1");
    }
    let mut run = Running::start(c);
    wait_until("2 requêtes reçues", || server.requests() >= 2);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    assert_eq!(server.requests(), 2, "{}", d.stderr);
    (env, d)
}

/// Fixture d'AC-190-1 : serveur à 5 s, 4 documents, deux SIGINT espacés de 300 ms.
fn fixture_190(args: &[&str]) -> (Env, Done) {
    let server = Server::start(|_| Act::Ok(5000));
    let env = Env::new(&docs(4), &server.base_url, FIX189);
    let mut run = env.spawn(args);
    wait_until("2 requêtes reçues", || server.requests() >= 2);
    run.sigint();
    std::thread::sleep(Duration::from_millis(300));
    run.sigint();
    (env, run.finish())
}

fn check_second_signal(d: &Done, verbose: bool) {
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    assert!(
        d.after_signal.unwrap() < Duration::from_secs(1),
        "{:?}",
        d.after_signal
    );
    assert_eq!(d.stdout, "", "stdout vide");
    assert_eq!(
        d.lines("avertissement: Ctrl-C reçu").len(),
        1,
        "{}",
        d.stderr
    );
    assert_eq!(
        d.stderr.lines().filter(|l| *l == A12).count(),
        1,
        "{}",
        d.stderr
    );
    assert_eq!(d.last(), E28, "{}", d.stderr);
    assert_eq!(d.stderr.lines().filter(|l| *l == E28).count(), 1);
    assert_eq!(d.lines("erreur:"), vec![E28], "{}", d.stderr);
    for bad in [
        "avertissement: plafond",
        "fichier(s) à enrichir",
        "panicked",
    ] {
        assert!(!d.stderr.contains(bad), "{bad} : {}", d.stderr);
    }
    assert!(
        !d.stderr.contains('\u{1b}') && !d.stderr.contains('\r'),
        "{:?}",
        d.stderr
    );
    if verbose {
        assert!(!d.stderr.contains("appels:"), "{}", d.stderr);
        assert!(!d.stderr.contains("enrichissement: fin"), "{}", d.stderr);
    }
}

// ---- AC-190-1, AC-190-3 ----

#[test]
fn req190_ac1_ac3_second_signal_sortie_immediate() {
    let (env, d) = fixture_190(&["build"]);
    check_second_signal(&d, false);
    assert_eq!(env.to_enrich(), 4);

    let fast = Server::start(|_| Act::Ok(0));
    env.set_url(&fast.base_url);
    let u = env.run(&["update"]);
    assert_eq!(u.code(), Some(0), "{}", u.stderr);
    assert!(
        !u.stderr.contains("verrou")
            && !u.stderr.contains("locked")
            && u.lines("erreur:").is_empty(),
        "{}",
        u.stderr
    );
    assert_eq!(env.enriched(&names(4)).len(), 4);
    assert_eq!(env.to_enrich(), 0);
}

#[test]
fn req190_ac3_second_signal_verbose() {
    let (_env, d) = fixture_190(&["build", "--verbose"]);
    check_second_signal(&d, true);
}

// ---- AC-190-2 : un seul signal, les appels en vol vont à leur terme ----

#[test]
fn req190_ac2_un_seul_signal_attend_les_appels_en_vol() {
    let server = Server::start(|_| Act::Ok(5000));
    let env = Env::new(&docs(4), &server.base_url, FIX189);
    let mut run = env.spawn(&["build"]);
    wait_until("2 requêtes reçues", || server.requests() >= 2);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    assert!(
        d.after_signal.unwrap() >= Duration::from_secs(4),
        "{:?}",
        d.after_signal
    );
    assert_eq!(env.enriched(&names(4)).len(), 2, "{}", d.stderr);
}

// ---- AC-191-1, AC-191-5, AC-191-6 ----

/// Bilan de stdout sans les chiffres (« aux comptes près »).
fn shape(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

#[test]
fn req191_ac1_ac5_ac6_annonce_et_ligne_de_fin() {
    let (_env, d) = fixture_189(&["build"], false);
    let lines: Vec<&str> = d.stderr.lines().collect();
    let announces: Vec<usize> = (0..lines.len()).filter(|i| lines[*i] == A12).collect();
    assert_eq!(announces.len(), 1, "{}", d.stderr);
    assert_eq!(d.last(), e27(4), "{}", d.stderr);
    assert_eq!(
        d.last(),
        "erreur: envoi interrompu : Ctrl-C reçu : relancez nephila update pour continuer ; 4 fichier(s) à enrichir"
    );
    assert!(announces[0] < lines.len() - 1, "{}", d.stderr);
    assert_eq!(d.lines("erreur:").len(), 1, "{}", d.stderr);
    assert!(d.lines("trace:").is_empty(), "{}", d.stderr);
    for l in &lines {
        assert!(
            l.starts_with("erreur: ")
                || l.starts_with("avertissement: ")
                || l.starts_with("trace: "),
            "{l}"
        );
    }
    assert!(!d.stderr.contains('\u{1b}'), "{:?}", d.stderr);

    let (_e2, nc) = fixture_189(&["build"], true);
    assert!(!nc.stderr.contains('\u{1b}'), "{:?}", nc.stderr);

    // Bilan identique, aux comptes près, à celui de la même fixture arrêtée par une clé refusée.
    let server = Server::start(|i| {
        if i == 0 {
            Act::Status(401, None)
        } else {
            Act::Ok(1000)
        }
    });
    let env = Env::new(&docs(6), &server.base_url, FIX189);
    let refused = env.run(&["build"]);
    assert_eq!(refused.code(), Some(1), "{}", refused.stderr);
    assert!(!refused.stdout.is_empty());
    assert_eq!(shape(&d.stdout), shape(&refused.stdout));
}

// ---- AC-191-2, AC-191-7 : --verbose et --show-progress ----

#[test]
fn req191_ac2_ac7_trace_verbose() {
    let (_env, d) = fixture_189(&["build", "--verbose"], false);
    let lines: Vec<&str> = d.stderr.lines().collect();
    let policy: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].contains("politique: arrêt de l'envoi"))
        .collect();
    assert_eq!(policy.len(), 1, "{}", d.stderr);
    let p = policy[0];
    assert!(lines[p].ends_with(TRACE_STOP), "{}", lines[p]);
    let a = lines.iter().position(|l| *l == A12).expect(&d.stderr);
    assert_eq!(p, a + 1, "{}", d.stderr);
    let starts: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].contains("llm enrich départ"))
        .collect();
    assert!(!starts.is_empty());
    assert!(starts.iter().all(|i| *i < p), "{}", d.stderr);
    let ends: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].contains("llm enrich fin"))
        .collect();
    assert_eq!(ends.len(), 2, "{}", d.stderr);
    assert!(ends.iter().all(|i| *i > p), "{}", d.stderr);
    let calls: Vec<&&str> = lines.iter().filter(|l| l.contains("appels:")).collect();
    assert_eq!(calls.len(), 1, "{}", d.stderr);
    assert!(calls[0].contains("appels: 2 appels,"), "{}", calls[0]);
    assert_eq!(d.last(), e27(4));
}

#[test]
fn req191_ac7_show_progress() {
    let (_env, d) = fixture_189(&["build", "--show-progress"], false);
    let lines: Vec<&str> = d.stderr.lines().collect();
    let zero = lines
        .iter()
        .position(|l| l.contains("progression: enrichissement 0 % (0/6 fichiers)"))
        .expect(&d.stderr);
    let a = lines.iter().position(|l| *l == A12).expect(&d.stderr);
    assert!(a > zero, "{}", d.stderr);
    assert!(
        !d.stderr.contains("progression: enrichissement 100 %"),
        "{}",
        d.stderr
    );
    assert!(d.lines("trace:").is_empty(), "{}", d.stderr);
    assert_eq!(d.last(), e27(4), "{}", d.stderr);
    let last_progress = lines
        .iter()
        .rposition(|l| l.contains("progression:"))
        .unwrap();
    assert!(last_progress < lines.len() - 1, "{}", d.stderr);
}

// ---- AC-191-3, AC-191-9 : plafond puis Ctrl-C ----

#[test]
fn req191_ac3_ac9_plafond_puis_signal() {
    let server = Server::start(|_| Act::Ok(1000));
    let env = Env::new(&docs(3), &server.base_url, FIX189);
    let mut run = env.spawn(&["build", "--max-calls", "1"]);
    wait_until("1 requête reçue", || server.requests() >= 1);
    run.sigint();
    let d = run.finish();
    assert_eq!(d.code(), Some(130), "{}", d.stderr);
    let lines: Vec<&str> = d.stderr.lines().collect();
    let n = lines.len();
    assert!(n >= 3, "{}", d.stderr);
    assert_eq!(lines[n - 2], A10_1, "{}", d.stderr);
    assert_eq!(lines[n - 1], E27_BARE, "{}", d.stderr);
    assert_eq!(
        lines.iter().filter(|l| **l == A12).count(),
        1,
        "{}",
        d.stderr
    );
    let a = lines.iter().position(|l| *l == A12).unwrap();
    assert!(a < n - 2, "{}", d.stderr);
}

// ---- AC-191-4, AC-191-8 : une autre cause d'abord ----

#[test]
fn plan_server_status_after_attend_la_requete_n() {
    use std::io::Write;
    use std::net::TcpStream;
    let server = Server::start(|i| {
        if i == 0 {
            Act::StatusAfter(401, 2)
        } else {
            Act::Ok(0)
        }
    });
    let addr = server.base_url.trim_start_matches("http://").to_string();
    let body = r#"{"messages":[{"role":"user","content":"x"}]}"#;
    let req = format!(
        "POST / HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut first = TcpStream::connect(&addr).unwrap();
    first.write_all(req.as_bytes()).unwrap();
    first
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut buf = [0u8; 64];
    assert!(first.read(&mut buf).is_err(), "réponse reçue trop tôt");
    let mut second = TcpStream::connect(&addr).unwrap();
    second.write_all(req.as_bytes()).unwrap();
    first
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let n = first.read(&mut buf).unwrap();
    assert!(
        String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 401"),
        "{:?}",
        &buf[..n]
    );
    wait_until("deux réponses servies", || server.served() >= 2);
}

fn key_refused_then_signal(verbose: bool) -> Done {
    let server = Server::start(|i| {
        if i == 0 {
            Act::StatusAfter(401, 2)
        } else {
            Act::Ok(2000)
        }
    });
    let env = Env::new(&docs(2), &server.base_url, FIX189);
    let mut run = env.spawn(if verbose {
        &["build", "--verbose"]
    } else {
        &["build"]
    });
    wait_until("401 servie et seconde requête reçue", || {
        server.requests() >= 2 && server.served() >= 1
    });
    run.sigint();
    run.finish()
}

#[test]
fn req191_ac4_ac8_autre_cause_d_abord() {
    let d = key_refused_then_signal(false);
    assert_eq!(d.code(), Some(1), "{}", d.stderr);
    assert_eq!(
        d.stderr.lines().filter(|l| *l == A12).count(),
        1,
        "{}",
        d.stderr
    );
    assert!(
        d.last().starts_with("erreur: clé d'API refusée par"),
        "{}",
        d.stderr
    );
    assert!(!d.stderr.contains("envoi interrompu"), "{}", d.stderr);
}

#[test]
fn req191_ac8_autre_cause_verbose() {
    let d = key_refused_then_signal(true);
    assert_eq!(d.code(), Some(1), "{}", d.stderr);
    assert!(
        !d.stderr.contains("arrêt de l'envoi (Ctrl-C reçu)"),
        "{}",
        d.stderr
    );
    assert!(
        d.last().starts_with("erreur: clé d'API refusée par"),
        "{}",
        d.stderr
    );
}
