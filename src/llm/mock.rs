//! Fournisseur `mock` piloté par variables d'environnement (REQ-003).
//!
//! Variables : `NEPHILA_MOCK_DIR` (fixtures), `NEPHILA_MOCK_LOG` (journal d'appels),
//! `NEPHILA_MOCK_FAIL_AFTER`, `NEPHILA_MOCK_INVALID_KEY`, `NEPHILA_MOCK_DELAY_MS`,
//! `NEPHILA_MOCK_TIMEOUT_FILES`, `NEPHILA_MOCK_KEY_REVOKED_AFTER`,
//! `NEPHILA_MOCK_MALFORMED=<chemin>:<k>[,…]` (`_ask` pour `ask`) : les k premiers appels qui
//! portent ce fichier reçoivent une réponse non conforme (`Error::Format`), après journal,
//! `FAIL_AFTER`, `KEY_REVOKED_AFTER` et `TIMEOUT_FILES` ; un appel décompte tous les fichiers
//! qu'il porte.
//! Fixture d'un fichier : `<dir>/<chemin>.<n>.json` pour le morceau n (à partir de 1) si
//! elle existe, sinon `<dir>/<chemin>.json`, sinon réponse vide.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use super::ask::AskReply;
use super::enrich::parse_lot;
use super::{Enrichment, Llm, LotItem};
use crate::error::{Error, Result};
use serde_json::Value;

#[derive(Debug, Default, Clone)]
pub struct MockOptions {
    pub dir: Option<PathBuf>,
    pub log: Option<PathBuf>,
    pub fail_after: Option<usize>,
    pub invalid_key: bool,
    pub delay_ms: u64,
    /// Fichiers (chemins relatifs) dont l'extraction échoue en « délai dépassé ».
    pub timeout_files: Vec<String>,
    /// Les appels d'extraction au-delà du N-ième échouent en « clé refusée ».
    pub key_revoked_after: Option<usize>,
    /// Réponses non conformes restantes par fichier (`NEPHILA_MOCK_MALFORMED`).
    pub malformed: Vec<(String, usize)>,
}

#[derive(Debug)]
pub struct MockLlm {
    opts: MockOptions,
    calls: AtomicUsize,
    extractions: AtomicUsize,
    malformed: Mutex<HashMap<String, usize>>,
}

type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

fn env_nonempty(env: EnvFn, name: &str) -> Option<String> {
    env(name).filter(|v| !v.is_empty())
}

fn env_number<T: std::str::FromStr>(env: EnvFn, name: &str) -> Result<Option<T>> {
    match env_nonempty(env, name) {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| Error::Message(format!("{name} : entier attendu, reçu « {v} »"))),
    }
}

impl MockLlm {
    pub fn new(opts: MockOptions) -> MockLlm {
        let malformed = Mutex::new(opts.malformed.iter().cloned().collect());
        MockLlm {
            opts,
            malformed,
            calls: AtomicUsize::new(0),
            extractions: AtomicUsize::new(0),
        }
    }

    /// Lit les options dans l'environnement du processus.
    pub fn from_env() -> Result<MockLlm> {
        MockLlm::from_env_with(&|k| std::env::var(k).ok())
    }

    /// Lit les options via `env` ; une valeur numérique invalide est une erreur.
    pub fn from_env_with(env: EnvFn) -> Result<MockLlm> {
        Ok(MockLlm::new(MockOptions {
            dir: env_nonempty(env, "NEPHILA_MOCK_DIR").map(PathBuf::from),
            log: env_nonempty(env, "NEPHILA_MOCK_LOG").map(PathBuf::from),
            fail_after: env_number(env, "NEPHILA_MOCK_FAIL_AFTER")?,
            invalid_key: env_nonempty(env, "NEPHILA_MOCK_INVALID_KEY").is_some_and(|v| v != "0"),
            delay_ms: env_number(env, "NEPHILA_MOCK_DELAY_MS")?.unwrap_or(0),
            timeout_files: env_nonempty(env, "NEPHILA_MOCK_TIMEOUT_FILES")
                .map(|v| {
                    v.split(',')
                        .map(|f| f.trim().to_string())
                        .filter(|f| !f.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            key_revoked_after: env_number(env, "NEPHILA_MOCK_KEY_REVOKED_AFTER")?,
            malformed: parse_malformed(env_nonempty(env, "NEPHILA_MOCK_MALFORMED"))?,
        }))
    }

    /// Journalise, temporise, puis échoue si le quota d'appels est dépassé.
    fn begin_call(&self, file: &str, chunk: usize) -> Result<()> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(log) = &self.opts.log {
            let line = format!("{file}\t{chunk}\n");
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
                .map_err(|e| {
                    Error::Message(format!("journal mock {} illisible : {e}", log.display()))
                })?;
            // Un seul write sur un descripteur O_APPEND : ajout atomique.
            f.write_all(line.as_bytes())
                .map_err(|e| Error::Message(format!("écriture du journal mock : {e}")))?;
        }
        if self.opts.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.opts.delay_ms));
        }
        if let Some(limit) = self.opts.fail_after {
            if n >= limit {
                return Err(Error::Network(format!(
                    "erreur réseau (LLM simulé) : appel {} au-delà de NEPHILA_MOCK_FAIL_AFTER={limit}",
                    n + 1
                )));
            }
        }
        Ok(())
    }

    fn read_fixture(&self, rel: &str) -> Result<Option<String>> {
        let Some(dir) = &self.opts.dir else {
            return Ok(None);
        };
        match std::fs::read_to_string(dir.join(rel)) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Message(format!(
                "fixture mock {rel} illisible : {e}"
            ))),
        }
    }

    /// Journalise l'appel (`label`), temporise et applique les échecs simulés ; `files` = fichiers
    /// concernés par l'appel (délai dépassé si l'un est dans `NEPHILA_MOCK_TIMEOUT_FILES`).
    fn gate(&self, label: &str, chunk: usize, files: &[&str]) -> Result<()> {
        let n = self.extractions.fetch_add(1, Ordering::SeqCst);
        self.begin_call(label, chunk)?;
        if let Some(file) = files
            .iter()
            .find(|f| self.opts.timeout_files.iter().any(|t| t == *f))
        {
            return Err(Error::Timeout(format!(
                "délai dépassé (LLM simulé) pour {file} (NEPHILA_MOCK_TIMEOUT_FILES)"
            )));
        }
        if let Some(limit) = self.opts.key_revoked_after {
            if n >= limit {
                return Err(Error::InvalidKey(format!(
                    "clé d'API refusée par le fournisseur (LLM simulé) : appel {} \
                     au-delà de NEPHILA_MOCK_KEY_REVOKED_AFTER={limit}",
                    n + 1
                )));
            }
        }
        Ok(())
    }

    /// Si un fichier de `files` a encore un compte `NEPHILA_MOCK_MALFORMED` > 0, décompte tous
    /// ceux qui en ont un et rend une réponse non conforme.
    fn check_malformed(&self, files: &[&str]) -> Result<()> {
        let mut left = self.malformed.lock().unwrap_or_else(|e| e.into_inner());
        if !files.iter().any(|f| left.get(*f).is_some_and(|k| *k > 0)) {
            return Ok(());
        }
        for f in files {
            if let Some(k) = left.get_mut(*f) {
                *k = k.saturating_sub(1);
            }
        }
        Err(Error::Format(
            "réponse LLM non conforme au schéma : JSON illisible (NEPHILA_MOCK_MALFORMED)".into(),
        ))
    }

    /// Journalise l'appel, applique les échecs simulés et lit la fixture du morceau.
    fn fixture_json(&self, file: &str, call: usize) -> Result<Option<String>> {
        check_relative(file)?;
        self.gate(file, call, &[file])?;
        self.check_malformed(&[file])?;
        let by_chunk = self.read_fixture(&format!("{file}.{call}.json"))?;
        let json = match by_chunk {
            Some(j) => Some(j),
            None => self.read_fixture(&format!("{file}.json"))?,
        };
        Ok(json)
    }
}

/// Analyse `<chemin>:<k>[,…]` ; `k` entier strictement positif.
fn parse_malformed(v: Option<String>) -> Result<Vec<(String, usize)>> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let bad = |item: &str| {
        Error::Message(format!(
            "NEPHILA_MOCK_MALFORMED : « {item} » invalide (attendu : <chemin>:<k>, k entier > 0)"
        ))
    };
    let mut out = Vec::new();
    for item in v.split(',') {
        let item = item.trim();
        let (file, k) = item.rsplit_once(':').ok_or_else(|| bad(item))?;
        let k: usize = k.trim().parse().map_err(|_| bad(item))?;
        if file.trim().is_empty() || k == 0 {
            return Err(bad(item));
        }
        out.push((file.trim().to_string(), k));
    }
    Ok(out)
}

/// Refuse les chemins absolus ou sortant du dossier de fixtures.
fn check_relative(file: &str) -> Result<()> {
    let ok = !file.is_empty()
        && Path::new(file)
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if ok {
        Ok(())
    } else {
        Err(Error::Message(format!(
            "chemin de fichier invalide pour le LLM simulé : « {file} »"
        )))
    }
}

impl Llm for MockLlm {
    fn verify_key(&self) -> Result<()> {
        if self.opts.invalid_key {
            Err(Error::InvalidKey(
                "clé d'API refusée par le fournisseur (LLM simulé, NEPHILA_MOCK_INVALID_KEY)"
                    .into(),
            ))
        } else {
            Ok(())
        }
    }

    fn enrich(
        &self,
        file: &str,
        call: usize,
        _text: &str,
        _requested: &[String],
        known: &[String],
    ) -> Result<Enrichment> {
        match self.fixture_json(file, call)? {
            Some(j) => Enrichment::parse(&j, file, known),
            None => Ok(Enrichment::default()),
        }
    }

    fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
        for it in items {
            check_relative(&it.file)?;
        }
        let files: Vec<&str> = items.iter().map(|it| it.file.as_str()).collect();
        self.gate(&files.join(";"), 1, &files)?;
        self.check_malformed(&files)?;
        let mut entries = Vec::with_capacity(items.len());
        for it in items {
            let json = match self.read_fixture(&format!("{}.1.json", it.file))? {
                Some(j) => Some(j),
                None => self.read_fixture(&format!("{}.json", it.file))?,
            };
            let (summaries, concepts) = match json {
                None => (Value::Array(vec![]), Value::Array(vec![])),
                Some(j) => {
                    // Même analyse stricte que `enrich` (version 2, champs inconnus refusés).
                    Enrichment::parse(&j, &it.file, &it.node_ids)?;
                    let v: Value = serde_json::from_str(&j).map_err(|e| {
                        Error::Format(format!("fixture mock de {} illisible : {e}", it.file))
                    })?;
                    (
                        v.get("summaries").cloned().unwrap_or(Value::Null),
                        v.get("concepts").cloned().unwrap_or(Value::Null),
                    )
                }
            };
            entries.push(serde_json::json!({
                "file": it.file, "summaries": summaries, "concepts": concepts
            }));
        }
        let reply = serde_json::json!({ "version": 3, "files": entries }).to_string();
        Ok(parse_lot(&reply, items)?.into_results())
    }

    fn ask(&self, _question: &str, _context: &str) -> Result<AskReply> {
        self.begin_call("_ask", 1)?;
        self.check_malformed(&["_ask"])?;
        match self.read_fixture("_ask.json")? {
            Some(j) => AskReply::parse(&j),
            None => Ok(AskReply::NotFound),
        }
    }
}
