//! `build --dry-run` et `update --dry-run` (REQ-184) : aucun envoi, graphe de `--no-llm`,
//! exactitude de l'estimation contre un faux serveur, fichiers estimés, sans configuration,
//! codes de sortie et texte.

#![allow(dead_code)]

#[path = "support/fake_server.rs"]
mod fake_server;

use assert_cmd::cargo::cargo_bin;
use fake_server::FakeServer;
use nephila::store::Store;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const KEY_VAR: &str = "NEPHILA_T274_KEY";

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn out(o: &Output) -> String {
    text(&o.stdout)
}

fn err(o: &Output) -> String {
    text(&o.stderr)
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("code de sortie")
}

/// Lignes de stdout qui commencent par `estimation`.
fn estimation_lines(o: &Output) -> Vec<String> {
    out(o)
        .lines()
        .filter(|l| l.starts_with("estimation"))
        .map(str::to_string)
        .collect()
}

/// Classe Java de `k` méthodes `m000()` …
fn java(class: &str, k: usize) -> String {
    let mut s = format!("public class {class} {{\n");
    for i in 0..k {
        s.push_str(&format!("    void m{i:03}() {{}}\n"));
    }
    s.push_str("}\n");
    s
}

/// Document Markdown de `k` sections de 240 octets de texte.
fn sections(prefix: &str, k: usize) -> String {
    (0..k)
        .map(|i| format!("# {prefix}{i}\n\n{}\n\n", "mot ".repeat(60)))
        .collect()
}

/// Fixture de REQ-184 avec `max_chunk_chars = 1500` : `docs/a.md` et `docs/b.md` forment un
/// lot, `docs/c.md` est seul dans le sien, `docs/d.md` (plusieurs sections, plusieurs morceaux)
/// et `src/Big.java` sont envoyés seuls, `web/src/side.js` n'a aucun nœud à demander.
fn write_fixture(repo: &Path) {
    let files: Vec<(&str, String)> = vec![
        (
            "docs/a.md",
            format!("# A\n\n{}\n", "alpha ".repeat(65).trim_end()),
        ),
        (
            "docs/b.md",
            format!("# B\n\n{}\n", "beta ".repeat(75).trim_end()),
        ),
        (
            "docs/c.md",
            format!("# C\n\n{}\n", "gamma ".repeat(150).trim_end()),
        ),
        ("docs/d.md", sections("S", 8)),
        ("src/Big.java", java("Big", 200)),
        ("web/src/side.js", "console.log(1);\n".to_string()),
        (
            ".nephila.toml",
            "max_chunk_chars = 1500\nformat_retries = 0\nconcurrency = 1\n".to_string(),
        ),
    ];
    for (rel, content) in files {
        let p = repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
}

impl Env {
    /// Dépôt vide, configuration `mock` (modèle `mock`).
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env {
            repo: tmp.path().join("repo"),
            config: tmp.path().join("config.toml"),
            mock: tmp.path().join("mock"),
            _tmp: tmp,
        };
        fs::create_dir_all(&env.repo).unwrap();
        fs::create_dir_all(&env.mock).unwrap();
        env.config_text("provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\n");
        env
    }

    fn with_fixture() -> Env {
        let env = Env::new();
        write_fixture(&env.repo);
        env
    }

    fn config_text(&self, content: &str) {
        fs::write(&self.config, content).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).args(args);
        for v in [
            "NEPHILA_MOCK_FAIL_AFTER",
            "NEPHILA_MOCK_DELAY_MS",
            "NEPHILA_MOCK_INVALID_KEY",
            "NEPHILA_MOCK_TIMEOUT_FILES",
            "NEPHILA_MOCK_KEY_REVOKED_AFTER",
            "NEPHILA_MOCK_MALFORMED",
            "NEPHILA_MOCK_LOG",
            "NO_COLOR",
            KEY_VAR,
        ] {
            c.env_remove(v);
        }
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn store(&self) -> Store {
        Store::open_readonly(&self.repo).unwrap()
    }

    fn export(&self) -> String {
        let o = self.run(&["export", "--format", "json", "--output", "-"]);
        assert_eq!(code(&o), 0, "{}", err(&o));
        out(&o)
    }

    /// Chemins ayant une entrée d'enrichissement.
    fn enriched(&self, paths: &[&str]) -> Vec<String> {
        let store = self.store();
        paths
            .iter()
            .filter(|p| store.enrichment_entry(p).unwrap().is_some())
            .map(|p| p.to_string())
            .collect()
    }
}

const ALL: [&str; 6] = [
    "docs/a.md",
    "docs/b.md",
    "docs/c.md",
    "docs/d.md",
    "src/Big.java",
    "web/src/side.js",
];

// ---- AC-184-2 : aucun envoi, variable de clé jamais lue ----

fn no_send(provider: &str) {
    let server = FakeServer::start(vec![]);
    let env = Env::with_fixture();
    env.config_text(&format!(
        "provider = \"{provider}\"\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\n",
        server.base_url
    ));
    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(code(&o), 0, "{provider} : {}", err(&o));
    let lines = estimation_lines(&o);
    assert_eq!(lines.len(), 3, "{provider} : {}", out(&o));
    assert!(
        lines[0].starts_with("estimation : 6 fichiers à enrichir"),
        "{lines:?}"
    );
    assert_eq!(server.connections(), 0, "{provider}");
    assert!(server.requests().is_empty(), "{provider}");

    // Témoin : sans --dry-run, la variable absente est une erreur de configuration.
    let env2 = Env::with_fixture();
    env2.config_text(&format!(
        "provider = \"{provider}\"\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\n",
        server.base_url
    ));
    let o = env2.run(&["build", "--enrich-code"]);
    assert_eq!(code(&o), 1, "{provider} : {}", err(&o));
    assert!(err(&o).contains(KEY_VAR), "{}", err(&o));
    assert!(err(&o).contains("absente ou vide"), "{}", err(&o));
    assert_eq!(server.connections(), 0, "{provider}");
}

#[test]
fn req184_ac2_openai_dry_run_sans_cle_ni_envoi() {
    no_send("openai");
}

#[test]
fn req184_ac2_anthropic_dry_run_sans_cle_ni_envoi() {
    no_send("anthropic");
}

// ---- AC-184-3 : le graphe est celui de --no-llm ----

#[test]
fn req184_ac3_graphe_identique_a_no_llm_sans_enrichissement() {
    let a = Env::with_fixture();
    let b = Env::with_fixture();
    let o = a.run(&["build", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = b.run(&["build", "--no-llm"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let (ga, gb) = (a.export(), b.export());
    assert!(ga.len() > 100, "{ga}");
    assert_eq!(ga, gb);
    assert!(a.enriched(&ALL).is_empty());
    assert!(b.enriched(&ALL).is_empty());

    // Sous --verbose : structure: fin sans enrichissement conservé, aucune phase d'enrichissement.
    let c = Env::with_fixture();
    let o = c.run(&["build", "--dry-run", "--verbose"]);
    let e = err(&o);
    assert!(e.contains("0 enrichissements conservés"), "{e}");
    assert!(!e.contains("enrichissement: début"), "{e}");
    assert!(c.enriched(&ALL).is_empty());
    let s = text(&c.run(&["status"]).stdout);
    assert!(s.contains("fichiers à enrichir"), "{s}");
}

#[test]
fn req184_ac3_modele_change_meta_et_enrichissements_inchanges() {
    let env = Env::with_fixture();
    let o = env.run(&["build"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(env.enriched(&ALL).len(), ALL.len());
    let before_status = out(&env.run(&["status"]));
    let before_model = env.store().enrich_model().unwrap();
    assert_eq!(before_model.as_deref(), Some("mock"));
    let before_graph = env.export();

    env.config_text("provider = \"mock\"\nmodel = \"autre\"\nenrich_code = true\n");
    let o = env.run(&["build", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    // Avec le nouveau modèle, tout est à enrichir : c'est l'estimation qui le dit.
    assert!(
        estimation_lines(&o)[0].starts_with("estimation : 6 fichiers à enrichir"),
        "{}",
        out(&o)
    );
    let store = env.store();
    assert_eq!(store.enrich_model().unwrap(), before_model);
    drop(store);
    assert_eq!(env.enriched(&ALL).len(), ALL.len());
    let store = env.store();
    for p in ALL {
        // Entrée de l'ancien modèle toujours présente.
        assert!(store.enrichment_entry(p).unwrap().is_some(), "{p}");
    }
    drop(store);
    assert_eq!(env.export(), before_graph);
    assert_eq!(out(&env.run(&["status"])), before_status);
}

// ---- Faux serveur openai conforme à chaque requête ----

#[derive(Default)]
struct Seen {
    /// Octets des messages `system` et `user`, requête par requête.
    bytes: Vec<usize>,
    /// Nœuds listés dans chaque requête.
    nodes: usize,
    /// Rang (à partir de 1) de la requête qui reçoit une réponse non conforme ; 0 = aucune.
    fail_at: usize,
}

struct ConformantServer {
    base_url: String,
    seen: Arc<Mutex<Seen>>,
    connections: Arc<AtomicUsize>,
}

impl ConformantServer {
    fn start() -> ConformantServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Seen::default()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (s, c) = (seen.clone(), connections.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                c.fetch_add(1, Ordering::SeqCst);
                let s = s.clone();
                std::thread::spawn(move || serve(stream, &s));
            }
        });
        ConformantServer {
            base_url,
            seen,
            connections,
        }
    }

    fn requests(&self) -> usize {
        self.seen.lock().unwrap().bytes.len()
    }

    fn bytes(&self) -> usize {
        self.seen.lock().unwrap().bytes.iter().sum()
    }

    fn nodes(&self) -> usize {
        self.seen.lock().unwrap().nodes
    }
}

/// Lit des requêtes HTTP (keep-alive) jusqu'à la fermeture de la connexion.
fn serve(mut stream: std::net::TcpStream, seen: &Mutex<Seen>) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut chunk = [0u8; 8192];
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let mut chunk = [0u8; 8192];
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let body: Vec<u8> = buf[head_end..head_end + len].to_vec();
        buf.drain(..head_end + len);
        let reply = answer(&body, seen);
        let payload =
            serde_json::json!({"choices":[{"message":{"role":"assistant","content":reply}}]})
                .to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{payload}",
            payload.len()
        );
        if stream.write_all(resp.as_bytes()).is_err() {
            return;
        }
    }
}

/// Réponse conforme construite depuis la liste de nœuds de la requête.
fn answer(body: &[u8], seen: &Mutex<Seen>) -> String {
    let v: serde_json::Value = serde_json::from_slice(body).unwrap();
    let mut bytes = 0;
    let mut user = String::new();
    for m in v["messages"].as_array().unwrap() {
        let c = m["content"].as_str().unwrap();
        match m["role"].as_str().unwrap() {
            "system" => bytes += c.len(),
            "user" => {
                bytes += c.len();
                user = c.to_string();
            }
            r => panic!("rôle inattendu {r}"),
        }
    }
    let mut files: Vec<(String, Vec<String>)> = Vec::new();
    let mut in_list = false;
    for line in user.lines() {
        if let Some(f) = line
            .strip_prefix("=== FICHIER [")
            .and_then(|l| l.split_once("] : "))
            .and_then(|(_, l)| l.strip_suffix(" ==="))
        {
            files.push((f.to_string(), vec![]));
        } else if line == "Nœuds du fichier :" {
            if files.is_empty() {
                files.push((String::new(), vec![]));
            }
            in_list = true;
        } else if in_list {
            match line
                .strip_prefix('[')
                .and_then(|r| r.split_once(" — "))
                .map(|(_, id)| id)
            {
                Some(id) => files.last_mut().unwrap().1.push(id.to_string()),
                None => in_list = false,
            }
        }
    }
    let nodes: usize = files.iter().map(|(_, n)| n.len()).sum();
    {
        let mut s = seen.lock().unwrap();
        s.bytes.push(bytes);
        s.nodes += nodes;
        if s.fail_at == s.bytes.len() {
            return "pas du json".to_string();
        }
    }
    let summaries = |ids: &[String]| -> Vec<serde_json::Value> {
        ids.iter()
            .map(|id| serde_json::json!({"node": id, "summary": "résumé"}))
            .collect()
    };
    if user.starts_with("=== FICHIER [") {
        let list: Vec<_> = files
            .iter()
            .map(|(f, ids)| serde_json::json!({"file": f, "summaries": summaries(ids), "concepts": []}))
            .collect();
        serde_json::json!({"version": 3, "files": list}).to_string()
    } else {
        serde_json::json!({"version": 2, "summaries": summaries(&files[0].1), "concepts": []})
            .to_string()
    }
}

fn server_config(env: &Env, server: &ConformantServer) {
    env.config_text(&format!(
        "provider = \"openai\"\nmodel = \"m\"\nenrich_code = true\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\n",
        server.base_url
    ));
}

/// Nombre après `prefix` dans `line`, jusqu'au mot `suffix`.
fn number(line: &str, before: &str, after: &str) -> usize {
    let rest = line
        .split(before)
        .nth(1)
        .unwrap_or_else(|| panic!("{line}"));
    rest.split(after).next().unwrap().trim().parse().unwrap()
}

// ---- AC-184-4 : exactitude contre la commande réelle ----

#[test]
fn req184_ac4_estimation_egale_a_la_commande_reelle() {
    let server = ConformantServer::start();
    let env = Env::with_fixture();
    server_config(&env, &server);

    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    let lines = estimation_lines(&o);
    assert_eq!(lines.len(), 3, "{}", out(&o));
    let files = number(&lines[0], "estimation : ", " fichiers");
    let alone = number(&lines[0], "dont ", " envoyés seuls");
    let calls = number(&lines[1], "minimale : ", " appels");
    let bytes = number(&lines[1], "appels, ", " octets");

    let mut real = env.command(&["build", "--enrich-code", "--verbose"]);
    real.env(KEY_VAR, "sk-test");
    let r = real.output().unwrap();
    assert_eq!(code(&r), 0, "{}", err(&r));
    assert_eq!(calls, server.requests());
    assert_eq!(bytes, server.bytes());

    let e = err(&r);
    let starts: Vec<&str> = e
        .lines()
        .filter(|l| l.contains("llm enrich départ "))
        .collect();
    let mut lot_files = 0;
    let mut alone_paths: Vec<&str> = vec![];
    for l in &starts {
        let rest = l.split("llm enrich départ ").nth(1).unwrap();
        if let Some(lot) = rest.strip_prefix("lot ") {
            let extra: usize = lot
                .split(" +")
                .nth(1)
                .unwrap()
                .split(' ')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            lot_files += 1 + extra;
        } else {
            // `[k/n]` : n > 1 morceaux = fichier envoyé seul ; n = 1 = lot d'un seul fichier
            // (la fixture n'a aucun fichier seul à un seul appel).
            let path = rest.split(' ').next().unwrap();
            let total: usize = rest
                .split(" [")
                .nth(1)
                .unwrap()
                .split('/')
                .nth(1)
                .unwrap()
                .split(']')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            if total == 1 {
                lot_files += 1;
            } else if !alone_paths.contains(&path) {
                alone_paths.push(path);
            }
        }
    }
    let fin = e
        .lines()
        .find(|l| l.contains("enrichissement: fin "))
        .unwrap();
    let enriched: usize = fin
        .split(" enrichis,")
        .next()
        .unwrap()
        .rsplit(", ")
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(files, enriched, "{e}");
    assert_eq!(alone, alone_paths.len(), "{alone_paths:?}");
    assert_eq!(lot_files + alone_paths.len() + 1, files);
    // Fixture : un lot de deux et un lot d'un (3 fichiers), deux fichiers seuls, un sans appel.
    assert_eq!((files, alone, lot_files), (6, 2, 3));
    assert_eq!(alone_paths, ["docs/d.md", "src/Big.java"]);
    assert!(calls > 4, "{calls}");
    assert_eq!(
        number(&lines[1], "jetons d'entrée, ", " jetons de sortie"),
        server.nodes() * 40
    );
    assert_eq!(
        number(&lines[1], "octets, ", " jetons d'entrée"),
        bytes.div_ceil(4)
    );
}

// ---- AC-184-6 : fichiers estimés par update --dry-run ----

/// Premier nombre de la ligne `estimation : <f> fichiers à enrichir dont <s> envoyés seuls et
/// <z> sans appel`, en `(f, s, z)`.
fn counts(o: &Output) -> (usize, usize, usize) {
    let l = &estimation_lines(o)[0];
    (
        number(l, "estimation : ", " fichiers"),
        number(l, "dont ", " envoyés seuls"),
        number(l, "seuls et ", " sans appel"),
    )
}

fn built_with_mock() -> Env {
    let env = Env::with_fixture();
    let o = env.run(&["build"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(env.enriched(&ALL).len(), ALL.len());
    env
}

#[test]
fn req184_ac6_graphe_a_jour_puis_trois_lignes_a_zero() {
    let env = built_with_mock();
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "graphe à jour\n\
         estimation : 0 fichiers à enrichir dont 0 envoyés seuls et 0 sans appel\n\
         estimation minimale : 0 appels, 0 octets, 0 jetons d'entrée, 0 jetons de sortie (un essai par appel)\n\
         estimation maximale : 0 appels, 0 octets, 0 jetons d'entrée, 0 jetons de sortie (1 essais par appel)\n"
    );
}

#[test]
fn req184_ac6_modele_ou_langue_change_tous_les_fichiers_de_la_portee() {
    let env = built_with_mock();
    env.config_text("provider = \"mock\"\nmodel = \"autre\"\nenrich_code = true\n");
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).starts_with("graphe à jour\n"), "{}", out(&o));
    assert_eq!(counts(&o), (6, 2, 1));

    env.config_text(
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\nsummary_language = \"English\"\n",
    );
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(counts(&o), (6, 2, 1));
    // Le graphe n'a pas bougé : une entrée par fichier, modèle d'origine.
    assert_eq!(env.enriched(&ALL).len(), ALL.len());
    assert_eq!(env.store().enrich_model().unwrap().as_deref(), Some("mock"));
}

#[test]
fn req184_ac6_enrich_code_decide_des_fichiers_de_code() {
    let env = built_with_mock();
    // Modèle changé, enrich_code faux : seuls les documents sont dans la portée.
    env.config_text("provider = \"mock\"\nmodel = \"autre\"\n");
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(counts(&o), (4, 1, 0));
    // --enrich-code : src/Big.java (envoyé seul) et web/src/side.js (sans appel) s'ajoutent.
    let o = env.run(&["update", "--dry-run", "--enrich-code"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(counts(&o), (6, 2, 1));
}

#[test]
fn req184_ac6_fichier_modifie_seul_estime_et_dans_le_graphe() {
    let env = built_with_mock();
    fs::write(
        env.repo.join("docs/a.md"),
        "# A\n\nnouveau texte\n\n## Neuvieme\n\nautre texte\n",
    )
    .unwrap();
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let stdout = out(&o);
    assert!(
        stdout.starts_with("update terminé : 1 fichiers traités"),
        "{stdout}"
    );
    assert!(
        stdout.contains("enrichissement : estimé sans envoi"),
        "{stdout}"
    );
    // Seul docs/a.md est estimé : un fichier, dans un lot d'un fichier (1 appel).
    assert_eq!(counts(&o), (1, 0, 0));
    let min = &estimation_lines(&o)[1];
    assert!(min.starts_with("estimation minimale : 1 appels, "), "{min}");
    // Son nouveau contenu est dans le graphe.
    let q = env.run(&["query", "search", "neuvieme"]);
    assert_eq!(code(&q), 0, "{}", err(&q));
    assert!(out(&q).contains("neuvieme"), "{}", out(&q));
    assert!(env.enriched(&["docs/a.md"]).is_empty());
    assert_eq!(env.enriched(&ALL[1..]).len(), ALL.len() - 1);
}

// ---- AC-184-7 : sans configuration ----

#[test]
fn req184_ac7_build_sans_configuration() {
    let env = Env::with_fixture();
    let absent = env.config.with_file_name("absente.toml");
    fs::remove_file(env.repo.join(".nephila.toml")).unwrap();
    let mut c = env.command(&["build", "--dry-run", "--enrich-code"]);
    c.env("NEPHILA_CONFIG", &absent);
    let o = c.output().unwrap();
    assert_eq!(code(&o), 0, "{}", err(&o));
    // Réglages par défaut, tous les fichiers de la portée comptés.
    assert_eq!(counts(&o).0, 6);
    let last = out(&o).lines().last().unwrap().to_string();
    assert_eq!(
        last,
        format!(
            "estimation sans configuration : réglages par défaut ; nephila n'enrichit rien tant que {} n'existe pas",
            absent.display()
        )
    );
    // Segment de fin du bilan : même place, même texte sans configuration.
    assert!(
        out(&o).contains(", enrichissement : estimé sans envoi\n"),
        "{}",
        out(&o)
    );
    assert!(env.enriched(&ALL).is_empty());
}

#[test]
fn req184_ac7_update_sans_graphe_fait_le_build() {
    let env = Env::with_fixture();
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(
        err(&o).contains("aucun graphe : build complet"),
        "{}",
        err(&o)
    );
    let stdout = out(&o);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("build terminé : "), "{stdout}");
    assert!(lines[1].starts_with("estimation : "), "{stdout}");
    assert_eq!(lines.len(), 4, "{stdout}");
    assert!(env.store().nodes().unwrap().len() > 100);
}

// ---- AC-184-8 : codes de sortie ----

/// Dépôt dont `docs/a.md`, d'horodatage ancien (empreinte réutilisée sans relecture), devient
/// illisible après un premier `build --no-llm` : le fichier est « en attente » (REQ-010).
/// `None` si les droits ne protègent rien (administrateur).
#[cfg(unix)]
fn with_pending_file() -> Option<Env> {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::with_fixture();
    let path = env.repo.join("docs/a.md");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        eprintln!(
            "test non applicable : les droits ne protègent rien (exécution en administrateur)"
        );
        return None;
    }
    Some(env)
}

#[cfg(unix)]
#[test]
fn req184_ac8_fichier_en_attente_meme_code_et_meme_erreur_que_no_llm() {
    use std::os::unix::fs::PermissionsExt;
    let (Some(a), Some(b)) = (with_pending_file(), with_pending_file()) else {
        return;
    };
    let dry = a.run(&["build", "--dry-run"]);
    let no_llm = b.run(&["build", "--no-llm"]);
    for env in [&a, &b] {
        fs::set_permissions(
            env.repo.join("docs/a.md"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    assert_eq!(code(&no_llm), 1, "{}", err(&no_llm));
    assert_eq!(code(&dry), code(&no_llm), "{}", err(&dry));
    let errors = |o: &Output| -> Vec<String> {
        err(o)
            .lines()
            .filter(|l| l.starts_with("erreur:"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(errors(&no_llm), ["erreur: 1 fichier(s) en attente"]);
    assert_eq!(errors(&dry), errors(&no_llm));
    // Le bilan et l'estimation sont tout de même écrits.
    assert!(out(&dry).contains("1 fichiers en attente"), "{}", out(&dry));
    assert_eq!(estimation_lines(&dry).len(), 3, "{}", out(&dry));
}

#[test]
fn req184_ac8_fichier_au_plafond_code_zero_sans_avertissement() {
    let env = Env::new();
    for (rel, content) in [
        ("src/Big.java", java("Big", 800)),
        ("src/Small.java", java("Small", 1)),
        (
            ".nephila.toml",
            "max_chunk_chars = 200\nconcurrency = 1\n".into(),
        ),
    ] {
        let p = env.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let stdout = out(&o);
    let cap = stdout
        .lines()
        .find(|l| l.starts_with("estimation au plafond : src/Big.java ("))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(
        cap.ends_with(" appels prévus, au plus 50 par fichier, aucun compté)"),
        "{cap}"
    );
    let planned = number(cap, "Big.java (", " appels prévus");
    assert!(planned > 50, "{cap}");
    assert_eq!(counts(&o), (2, 1, 0));
    let e = err(&o);
    assert!(!e.contains("trop d'appels d'enrichissement"), "{e}");
    assert!(!e.contains("avertissement"), "{e}");
    // La commande réelle, elle, avertit (A-6) : le témoin que l'avertissement existe bien.
    let real = Env::new();
    for (rel, content) in [
        ("src/Big.java", java("Big", 800)),
        (
            ".nephila.toml",
            "max_chunk_chars = 200\nconcurrency = 1\n".into(),
        ),
    ] {
        let p = real.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    let r = real.run(&["build"]);
    assert!(
        err(&r).contains("trop d'appels d'enrichissement"),
        "{}",
        err(&r)
    );
}

// ---- AC-184-9 : texte ----

const BILAN: &str = "build terminé : 6 fichiers traités, 217 nœuds, 211 liens, 0 références non résolues, 0 fichiers en attente, 4 fichiers à enrichir, enrichissement : estimé sans envoi";

#[test]
fn req184_ac9_stdout_complete_octet_pour_octet() {
    let server = ConformantServer::start();
    let env = Env::with_fixture();
    server_config(&env, &server);
    let o = env.run(&[
        "build",
        "--dry-run",
        "--enrich-code",
        "--format-retries",
        "2",
    ]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    // Minimum mesuré par la commande réelle (format_retries 0 du fichier de projet).
    let mut real = env.command(&["build", "--enrich-code"]);
    real.env(KEY_VAR, "sk-test");
    let r = real.output().unwrap();
    assert_eq!(code(&r), 0, "{}", err(&r));
    let (calls, bytes, nodes) = (server.requests(), server.bytes(), server.nodes());
    let (tin, tout) = (bytes.div_ceil(4), 40 * nodes);
    let expected = format!(
        "{BILAN}\n\
         estimation : 6 fichiers à enrichir dont 2 envoyés seuls et 1 sans appel\n\
         estimation minimale : {calls} appels, {bytes} octets, {tin} jetons d'entrée, {tout} jetons de sortie (un essai par appel)\n\
         estimation maximale : {} appels, {} octets, {} jetons d'entrée, {} jetons de sortie (3 essais par appel)\n",
        3 * calls,
        3 * bytes,
        3 * tin,
        3 * tout
    );
    assert_eq!(out(&o), expected);
}

#[test]
fn req184_ac9_traces_estimation_sous_verbose() {
    let env = Env::with_fixture();
    let o = env.run(&["build", "--dry-run", "--enrich-code", "--verbose"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let e = err(&o);
    let lines: Vec<&str> = e.lines().collect();
    let comm = lines
        .iter()
        .position(|l| l.contains("communautés:"))
        .unwrap_or_else(|| panic!("{e}"));
    let est: Vec<(usize, &&str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains("] estimation "))
        .collect();
    assert_eq!(est.len(), 2, "{e}");
    assert!(est.iter().all(|(i, _)| *i > comm), "{e}");
    // Triées par chemin, uniquement les fichiers envoyés seuls.
    assert!(
        est[0].1.contains("] estimation docs/d.md : 2 appels, "),
        "{e}"
    );
    assert!(
        est[1].1.contains("] estimation src/Big.java : 3 appels, "),
        "{e}"
    );
    for (_, l) in &est {
        assert!(l.starts_with("trace: ["), "{l}");
        assert!(l.ends_with(" octets"), "{l}");
    }
    let total: usize = est
        .iter()
        .map(|(_, l)| number(l, " appels, ", " octets"))
        .sum();
    assert!(total < number(&estimation_lines(&o)[1], "appels, ", " octets"));
    for forbidden in ["enrichissement:", "llm enrich", "appels: "] {
        assert!(
            lines.iter().all(|l| !l.contains(forbidden)),
            "{forbidden} : {e}"
        );
    }
}

#[test]
fn req184_ac9_show_progress_sans_enrichissement() {
    let env = Env::with_fixture();
    let o = env.run(&["build", "--dry-run", "--enrich-code", "--show-progress"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let e = err(&o);
    let progress: Vec<&str> = e
        .lines()
        .filter(|l| l.starts_with("progression:"))
        .collect();
    assert!(!progress.is_empty(), "{e}");
    assert!(
        progress
            .iter()
            .all(|l| l.starts_with("progression: extraction")),
        "{e}"
    );
    assert!(!e.contains("progression: enrichissement"), "{e}");
}

#[cfg(unix)]
#[test]
fn req184_ac9_chemin_a_caractere_de_controle_echappe() {
    let env = Env::new();
    for (rel, content) in [
        ("a/\u{1b}Big.java", java("Big", 800)),
        ("b/Big.java", java("Big", 800)),
        (
            ".nephila.toml",
            "max_chunk_chars = 200\nconcurrency = 1\n".into(),
        ),
    ] {
        let p = env.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!o.stdout.contains(&0x1b), "{:?}", out(&o));
    let caps: Vec<String> = out(&o)
        .lines()
        .filter(|l| l.starts_with("estimation au plafond"))
        .map(str::to_string)
        .collect();
    assert_eq!(caps.len(), 2, "{}", out(&o));
    assert!(
        caps[0].starts_with("estimation au plafond : a/\\x1BBig.java ("),
        "{caps:?}"
    );
    assert!(
        caps[1].starts_with("estimation au plafond : b/Big.java ("),
        "{caps:?}"
    );
    assert!(
        !err(&o).contains("trop d'appels d'enrichissement"),
        "{}",
        err(&o)
    );
}

// ---- AC-228 : appels conservés ----

/// Dépôt d'un seul fichier envoyé seul (plusieurs appels), faux serveur conforme.
fn kept_env(server: &ConformantServer) -> Env {
    let env = Env::new();
    for (rel, content) in [
        ("docs/d.md", sections("S", 16)),
        (
            ".nephila.toml",
            "max_chunk_chars = 1500\nformat_retries = 0\nconcurrency = 1\n".to_string(),
        ),
    ] {
        let p = env.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    server_config(&env, server);
    env
}

fn real(env: &Env, args: &[&str]) -> Output {
    let mut c = env.command(args);
    c.env(KEY_VAR, "sk-test");
    c.output().unwrap()
}

fn kept_rows(env: &Env) -> usize {
    env.store().call_results().unwrap().len()
}

#[test]
fn req228_ac1_estimation_ne_compte_que_les_appels_manquants() {
    let server = ConformantServer::start();
    let env = kept_env(&server);
    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let full = number(&estimation_lines(&o)[1], "minimale : ", " appels");
    assert!(full >= 3, "{full}");

    server.seen.lock().unwrap().fail_at = 3;
    let r = real(&env, &["build", "--enrich-code"]);
    assert_eq!(code(&r), 1, "{}", err(&r));
    assert_eq!(server.requests(), 3);
    assert_eq!(kept_rows(&env), 2);

    let before = server.requests();
    let o = env.run(&["update", "--dry-run", "--verbose"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let lines = estimation_lines(&o);
    let calls = number(&lines[1], "minimale : ", " appels");
    let bytes = number(&lines[1], "appels, ", " octets");
    assert_eq!(calls, full - 2, "{}", out(&o));
    let trace = format!("estimation docs/d.md : {calls} appels, {bytes} octets");
    assert!(err(&o).contains(&trace), "{}", err(&o));
    assert_eq!(kept_rows(&env), 2);
    assert_eq!(server.requests(), before);

    // Le modèle change : plus rien n'est réutilisé.
    env.config_text(&format!(
        "provider = \"openai\"\nmodel = \"autre\"\nenrich_code = true\nbase_url = \"{}\"\napi_key_env = \"{KEY_VAR}\"\n",
        server.base_url
    ));
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(
        number(&estimation_lines(&o)[1], "minimale : ", " appels"),
        full
    );
    assert_eq!(kept_rows(&env), 2);
    server_config(&env, &server);

    server.seen.lock().unwrap().fail_at = 0;
    let (b0, n0) = (server.bytes(), server.requests());
    let r = real(&env, &["update"]);
    assert_eq!(code(&r), 0, "{}", err(&r));
    assert_eq!(server.requests() - n0, calls);
    assert_eq!(server.bytes() - b0, bytes);
}

#[test]
fn req228_ac4_tous_les_appels_conserves_zero_appel() {
    let server = ConformantServer::start();
    let env = kept_env(&server);
    let r = real(&env, &["build"]);
    assert_eq!(code(&r), 0, "{}", err(&r));
    // Un déclencheur garde les lignes à la réussite ; le fichier redevient « à enrichir ».
    let db = env.repo.join(".nephila/graph.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER keep BEFORE DELETE ON enrich_calls BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    drop(conn);
    fs::write(env.repo.join("docs/d.md"), sections("S", 16) + "\n").unwrap();
    let r = real(&env, &["update"]);
    assert_eq!(code(&r), 0, "{}", err(&r));
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("DROP TRIGGER keep;").unwrap();
    conn.execute("DELETE FROM enrichments WHERE path = 'docs/d.md'", [])
        .unwrap();
    drop(conn);
    assert!(kept_rows(&env) >= 3);

    let o = env.run(&["update", "--dry-run", "--verbose"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let stdout = out(&o);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "graphe à jour", "{stdout}");
    assert_eq!(
        lines[1],
        "estimation : 1 fichiers à enrichir dont 1 envoyés seuls et 0 sans appel"
    );
    assert_eq!(
        lines[2],
        "estimation minimale : 0 appels, 0 octets, 0 jetons d'entrée, 0 jetons de sortie (un essai par appel)"
    );
    assert!(
        err(&o)
            .lines()
            .any(|l| l.ends_with("estimation docs/d.md : 0 appels, 0 octets")),
        "{}",
        err(&o)
    );
}

#[test]
fn req228_ac4_plafond_compte_le_plan_entier_malgre_trois_appels_conserves() {
    let env = Env::new();
    fs::write(env.repo.join("big.md"), sections("S", 52)).unwrap();
    fs::write(env.repo.join(".nephila.toml"), "max_chunk_chars = 300\n").unwrap();
    let o = env.run(&["build", "--no-llm"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let conn = rusqlite::Connection::open(env.repo.join(".nephila/graph.db")).unwrap();
    for k in ["k1", "k2", "k3"] {
        conn.execute(
            "INSERT INTO enrich_calls(path, call_key, payload) VALUES ('big.md', ?1, '{\"v\":1,\"summaries\":[],\"concepts\":[],\"rejected\":[]}')",
            [k],
        )
        .unwrap();
    }
    drop(conn);
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let stdout = out(&o);
    assert!(
        stdout.lines().any(|l| l
            == "estimation au plafond : big.md (52 appels prévus, au plus 50 par fichier, aucun compté)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("estimation minimale : 0 appels, 0 octets"),
        "{stdout}"
    );
    assert_eq!(kept_rows(&env), 3);
}

// ---- AC-241-4 : un gros JSON est estimé sur son extrait ----

#[test]
fn req241_ac4_gros_json_estime_sur_son_extrait() {
    let server = ConformantServer::start();
    let env = Env::new();
    server_config(&env, &server);
    fs::create_dir_all(env.repo.join("config")).unwrap();
    let mut json = String::from("{\"cle\":[");
    while json.len() < 3_200_000 - 20 {
        json.push_str("{\"n\":1},");
    }
    json.push_str("{\"n\":2}]}");
    fs::write(env.repo.join("config/big.json"), &json).unwrap();
    fs::write(
        env.repo.join(".nephila.toml"),
        "batch_files = 1\nconcurrency = 1\n",
    )
    .unwrap();

    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!out(&o).contains("estimation au plafond"), "{}", out(&o));
    let lines = estimation_lines(&o);
    assert_eq!(lines.len(), 3, "{}", out(&o));
    assert_eq!(number(&lines[1], "minimale : ", " appels"), 1);
    let bytes = number(&lines[1], "appels, ", " octets");
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    assert!(bytes < 30_000, "{bytes}");

    let mut real = env.command(&["build", "--enrich-code"]);
    real.env(KEY_VAR, "sk-test");
    let r = real.output().unwrap();
    assert_eq!(code(&r), 0, "{}", err(&r));
    assert_eq!(server.requests(), 1);
    assert_eq!(server.bytes(), bytes);
}

// ---- REQ-271 / AC-271-6 : l'estimation suit la borne dérivée de la limite de sortie ----

#[test]
fn req271_ac6_estimation_egale_a_la_commande_reelle_avec_petite_limite() {
    let env = Env::new();
    env.config_text(
        "provider = \"mock\"\nmodel = \"mock\"\nenrich_code = true\nmax_output_tokens = 4096\n",
    );
    fs::create_dir_all(env.repo.join("src")).unwrap();
    fs::write(env.repo.join("src/Big.java"), java("Big", 400)).unwrap();

    let o = env.run(&["build", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let min = &estimation_lines(&o)[1];
    assert_eq!(number(min, "minimale : ", " appels"), 6, "{min}");

    let log = env.repo.parent().unwrap().join("mock.log");
    let mut real = env.command(&["build"]);
    real.env("NEPHILA_MOCK_LOG", &log);
    let r = real.output().unwrap();
    assert_eq!(code(&r), 0, "{}", err(&r));
    let lines = fs::read_to_string(&log).unwrap();
    let n = lines.lines().filter(|l| l.contains("src/Big.java")).count();
    assert_eq!(n, 6, "{lines}");
}

#[test]
#[allow(non_snake_case)]
fn REQ277_AC8_equivalent_name_estimates_no_call() {
    let env = built_with_mock();
    env.config_text("provider = \"mock\"\nmodel = \"mock-daté\"\nenrich_code = true\n");
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(counts(&o), (6, 2, 1), "sans la table");

    env.config_text(
        "provider = \"mock\"\nmodel = \"mock-daté\"\nenrich_code = true\n[model_aliases]\n\"mock\" = \"mock-daté\"\n",
    );
    let o = env.run(&["update", "--dry-run"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(counts(&o), (0, 0, 0), "{}", out(&o));
    assert!(
        out(&o).contains("estimation minimale : 0 appels"),
        "{}",
        out(&o)
    );
}
