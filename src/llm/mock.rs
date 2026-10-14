//! Fournisseur `mock` piloté par variables d'environnement (REQ-003).
//!
//! Variables : `RKG_MOCK_DIR` (fixtures), `RKG_MOCK_LOG` (journal d'appels),
//! `RKG_MOCK_FAIL_AFTER`, `RKG_MOCK_INVALID_KEY`, `RKG_MOCK_DELAY_MS`,
//! `RKG_MOCK_TIMEOUT_FILES`, `RKG_MOCK_KEY_REVOKED_AFTER`.
//! Fixture d'un fichier : `<dir>/<chemin>.<n>.json` pour le morceau n (à partir de 1) si
//! elle existe, sinon `<dir>/<chemin>.json`, sinon réponse vide.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::extract::{AskReply, Extraction};
use super::Llm;
use crate::error::{Error, Result};

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
}

#[derive(Debug)]
pub struct MockLlm {
    opts: MockOptions,
    calls: AtomicUsize,
    extractions: AtomicUsize,
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
        MockLlm {
            opts,
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
            dir: env_nonempty(env, "RKG_MOCK_DIR").map(PathBuf::from),
            log: env_nonempty(env, "RKG_MOCK_LOG").map(PathBuf::from),
            fail_after: env_number(env, "RKG_MOCK_FAIL_AFTER")?,
            invalid_key: env_nonempty(env, "RKG_MOCK_INVALID_KEY").is_some_and(|v| v != "0"),
            delay_ms: env_number(env, "RKG_MOCK_DELAY_MS")?.unwrap_or(0),
            timeout_files: env_nonempty(env, "RKG_MOCK_TIMEOUT_FILES")
                .map(|v| {
                    v.split(',')
                        .map(|f| f.trim().to_string())
                        .filter(|f| !f.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            key_revoked_after: env_number(env, "RKG_MOCK_KEY_REVOKED_AFTER")?,
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
                    "erreur réseau (LLM simulé) : appel {} au-delà de RKG_MOCK_FAIL_AFTER={limit}",
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
                "clé d'API refusée par le fournisseur (LLM simulé, RKG_MOCK_INVALID_KEY)".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn extract(&self, file: &str, chunk: usize, _text: &str) -> Result<Extraction> {
        check_relative(file)?;
        let n = self.extractions.fetch_add(1, Ordering::SeqCst);
        self.begin_call(file, chunk)?;
        if self.opts.timeout_files.iter().any(|f| f == file) {
            return Err(Error::Timeout(format!(
                "délai dépassé (LLM simulé) pour {file} (RKG_MOCK_TIMEOUT_FILES)"
            )));
        }
        if let Some(limit) = self.opts.key_revoked_after {
            if n >= limit {
                return Err(Error::InvalidKey(format!(
                    "clé d'API refusée par le fournisseur (LLM simulé) : appel d'extraction {} \
                     au-delà de RKG_MOCK_KEY_REVOKED_AFTER={limit}",
                    n + 1
                )));
            }
        }
        let by_chunk = self.read_fixture(&format!("{file}.{chunk}.json"))?;
        let json = match by_chunk {
            Some(j) => Some(j),
            None => self.read_fixture(&format!("{file}.json"))?,
        };
        match json {
            Some(j) => Extraction::parse(&j),
            None => Ok(Extraction::default()),
        }
    }

    fn ask(&self, _question: &str, _context: &str) -> Result<AskReply> {
        self.begin_call("_ask", 1)?;
        match self.read_fixture("_ask.json")? {
            Some(j) => AskReply::parse(&j),
            None => Ok(AskReply::NotFound),
        }
    }
}
