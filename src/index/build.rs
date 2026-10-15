//! Commande `build` : reconstruction complète du graphe (REQ-009, REQ-006).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

use crate::chunk;
use crate::config::Config;
use crate::error::{warn, Error, Result};
use crate::index::lock::RepoLock;
use crate::llm::{build_llm, Extraction, Llm};
use crate::model::{FileRecord, FileState, Node, NodeKind, Ref};
use crate::resolve;
use crate::scan::{self, ScannedFile};
use crate::store::{db_path, Store};

/// Bilan d'un build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildReport {
    pub processed: usize,
    pub pending: usize,
    pub nodes: usize,
    pub edges: usize,
    pub unresolved: usize,
    /// Nombre de fichiers dont l'extraction a échoué.
    pub failed: usize,
    /// Cause de l'arrêt de l'envoi de nouveaux fichiers (clé refusée, panne réseau répétée).
    pub stop_cause: Option<String>,
}

impl BuildReport {
    pub fn line(&self) -> String {
        self.line_for("build")
    }

    pub fn line_for(&self, verb: &str) -> String {
        format!(
            "{verb} terminé : {} fichiers traités, {} nœuds, {} liens, {} références non résolues, {} fichiers en attente",
            self.processed, self.nodes, self.edges, self.unresolved, self.pending
        )
    }
}

/// Point d'entrée de `rkg build` : valide le dépôt, la configuration et la clé, puis construit.
pub fn run(repo: &Path) -> Result<()> {
    check_repo(repo)?;
    let config = Config::load()?;
    let llm = build_llm(&config)?;
    llm.verify_key()?;
    // Le verrou précède toute écriture, mais suit les contrôles qui ne doivent rien créer.
    let lock = RepoLock::acquire(repo)?;
    let report = build(repo, &config, llm.as_ref(), &lock)?;
    finish(&report, "build")
}

/// Affiche le bilan ; un fichier resté en attente ou un arrêt rend une erreur (code non nul).
pub(crate) fn finish(report: &BuildReport, verb: &str) -> Result<()> {
    println!("{}", report.line_for(verb));
    match &report.stop_cause {
        Some(cause) => Err(Error::Message(format!(
            "envoi interrompu : {cause} ; {} fichier(s) en attente",
            report.pending
        ))),
        None if report.pending > 0 => Err(Error::Message(format!(
            "{} fichier(s) en attente",
            report.pending
        ))),
        None => Ok(()),
    }
}

pub(crate) fn check_repo(repo: &Path) -> Result<()> {
    if !repo.is_dir() {
        return Err(Error::Message(format!(
            "{} n'est pas un dossier lisible",
            repo.display()
        )));
    }
    std::fs::read_dir(repo).map_err(|e| {
        Error::Message(format!(
            "{} n'est pas un dossier lisible : {e}",
            repo.display()
        ))
    })?;
    Ok(())
}

/// Construit le graphe de `repo` avec le fournisseur donné (config et clé déjà validées). Le
/// verrou d'écriture, pris par l'appelant, prouve l'exclusivité avant toute écriture.
pub fn build(repo: &Path, config: &Config, llm: &dyn Llm, _lock: &RepoLock) -> Result<BuildReport> {
    check_repo(repo)?;
    let mut store = Store::open_or_create(repo)?;
    let known = if db_path(repo).is_file() {
        store.files()?
    } else {
        Vec::new()
    };
    let scanned = scan::scan(repo, &known)?;
    let records: Vec<FileRecord> = scanned
        .files
        .iter()
        .map(|f| FileRecord {
            path: f.path.clone(),
            hash: f.hash.clone(),
            size: f.size,
            mtime: f.mtime,
            recorded_at: 0,
            state: FileState::Pending,
        })
        .collect();
    store.reset_with_pending_files(&records)?;
    if records.is_empty() {
        warn("aucun fichier pris en charge dans le dépôt : graphe vide");
    }

    let mut report = BuildReport::default();
    extract_all(
        repo,
        &scanned.files,
        config.concurrency,
        config.max_chunk_chars,
        llm,
        &mut store,
        &mut report,
    )?;

    let stats = resolve::resolve(&mut store)?;
    report.nodes = store.nodes()?.len();
    report.edges = stats.edges;
    report.unresolved = stats.unresolved;
    report.pending = store
        .files()?
        .iter()
        .filter(|f| f.state == FileState::Pending)
        .count();
    crate::summary::refresh(&mut store)?;
    Ok(report)
}

/// Extrait un fichier, en plusieurs appels s'il dépasse `max_chunk_chars` ; le premier échec
/// d'un morceau fait échouer le fichier entier (REQ-007).
fn extract_chunks(
    llm: &dyn Llm,
    path: &str,
    text: &str,
    max_chunk_chars: usize,
) -> Result<Extraction> {
    let parts = chunk::split(text, max_chunk_chars);
    if parts.len() == 1 {
        return llm.extract(path, 1, &parts[0]);
    }
    let mut results = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        results.push(llm.extract(path, i + 1, part)?);
    }
    Ok(chunk::merge(results))
}

/// Nombre d'erreurs réseau consécutives qui arrête l'envoi (DEC-015).
const MAX_CONSECUTIVE_NETWORK_ERRORS: usize = 3;

/// Politique d'échec partagée entre les threads de travail (DEC-015).
#[derive(Default)]
struct Policy {
    consecutive_network: usize,
    network_total: usize,
    stop_cause: Option<String>,
}

/// Total des erreurs réseau constatées au départ d'un appel (DEC-022).
type Ticket = usize;

impl Policy {
    /// Prend un ticket au départ d'un appel (sous le verrou de la politique).
    fn dispatch(&self) -> Ticket {
        self.network_total
    }

    /// Met à jour la politique ; renvoie vrai si l'envoi de nouveaux fichiers doit s'arrêter.
    /// Une réussite n'efface que les erreurs réseau constatées avant le départ de son appel (DEC-022).
    fn record(&mut self, ticket: Ticket, result: Option<&Error>) -> bool {
        match result {
            None => {
                self.consecutive_network = self
                    .consecutive_network
                    .min(self.network_total.saturating_sub(ticket));
            }
            Some(Error::InvalidKey(_)) => {
                self.stop_cause
                    .get_or_insert_with(|| "clé d'API refusée par le fournisseur".to_string());
            }
            Some(Error::Network(_)) => {
                self.consecutive_network += 1;
                self.network_total += 1;
                if self.consecutive_network >= MAX_CONSECUTIVE_NETWORK_ERRORS {
                    self.stop_cause.get_or_insert_with(|| {
                        format!(
                            "panne réseau répétée ({MAX_CONSECUTIVE_NETWORK_ERRORS} erreurs réseau consécutives)"
                        )
                    });
                }
            }
            Some(_) => {}
        }
        self.stop_cause.is_some()
    }
}

enum Outcome {
    Done(String, Extraction),
    Failed(String, Error),
}

/// Pool de threads LLM ; le thread courant écrit une transaction par fichier.
pub(crate) fn extract_all(
    repo: &Path,
    files: &[ScannedFile],
    concurrency: usize,
    max_chunk_chars: usize,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    let queue: Mutex<VecDeque<&ScannedFile>> = Mutex::new(files.iter().collect());
    let stop = AtomicBool::new(false);
    let policy = Mutex::new(Policy::default());
    let (tx, rx) = mpsc::channel::<Outcome>();
    let workers = concurrency.clamp(1, files.len().max(1));
    let mut write_error: Option<Error> = None;

    std::thread::scope(|scope| {
        let queue = &queue;
        let stop = &stop;
        let policy = &policy;
        for _ in 0..workers {
            let tx = tx.clone();
            scope.spawn(move || loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Some(file) = queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front() else {
                    break;
                };
                let ticket = policy.lock().unwrap_or_else(|p| p.into_inner()).dispatch();
                let outcome = match std::fs::read_to_string(repo.join(&file.path)) {
                    Err(e) => Outcome::Failed(
                        file.path.clone(),
                        Error::Message(format!("lecture de {} impossible : {e}", file.path)),
                    ),
                    Ok(text) => match extract_chunks(llm, &file.path, &text, max_chunk_chars) {
                        Ok(x) => Outcome::Done(file.path.clone(), x),
                        Err(e) => Outcome::Failed(file.path.clone(), e),
                    },
                };
                let error = match &outcome {
                    Outcome::Failed(_, e) => Some(e),
                    Outcome::Done(..) => None,
                };
                let halt = policy
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .record(ticket, error);
                if halt {
                    stop.store(true, Ordering::SeqCst);
                }
                if tx.send(outcome).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for outcome in rx {
            match outcome {
                Outcome::Failed(path, e) => {
                    warn(&format!("{path} : {e}"));
                    report.failed += 1;
                }
                Outcome::Done(path, x) => {
                    if write_error.is_some() {
                        continue;
                    }
                    if !x.rejected.is_empty() {
                        warn(&format!(
                            "{path} : {} référence(s) écartée(s), genre de lien non autorisé : {}",
                            x.rejected.len(),
                            x.rejected.join(" ; ")
                        ));
                    }
                    let (nodes, refs, dropped) = to_records(&path, x);
                    if dropped > 0 {
                        warn(&format!(
                            "{path} : {dropped} référence(s) ignorée(s), source non déclarée"
                        ));
                    }
                    match store.replace_file(&path, &nodes, &refs) {
                        Ok(()) => report.processed += 1,
                        Err(e) => {
                            stop.store(true, Ordering::SeqCst);
                            write_error = Some(e);
                        }
                    }
                }
            }
        }
    });
    report.stop_cause = policy
        .into_inner()
        .unwrap_or_else(|p| p.into_inner())
        .stop_cause;
    match write_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Nom court d'un nœud, selon son genre.
fn short_name(kind: NodeKind, qualified: &str) -> &str {
    let after = |seps: &[char]| qualified.rsplit(seps).next().unwrap_or(qualified);
    match kind {
        NodeKind::Document | NodeKind::Module => after(&['/']),
        NodeKind::Section => after(&['#']),
        NodeKind::Dependency => after(&[':']),
        _ => after(&['.']),
    }
}

/// Convertit une extraction en nœuds et références rattachés au fichier d'origine. Les références
/// dont la source n'est pas déclarée sont écartées (leur nombre est renvoyé).
fn to_records(path: &str, x: Extraction) -> (Vec<Node>, Vec<Ref>, usize) {
    let mut nodes: Vec<Node> = Vec::new();
    for d in &x.declares {
        let id = d.id();
        if nodes.iter().any(|n| n.id == id) {
            continue;
        }
        // Dépendances externes et concepts n'ont pas de fichier propre.
        let file = match d.kind {
            NodeKind::Dependency | NodeKind::Concept => None,
            _ => Some(path),
        };
        nodes.push(Node::new(
            d.kind,
            &d.qualified,
            short_name(d.kind, &d.qualified),
            file,
            d.subkind,
            d.summary.as_deref(),
        ));
    }
    let mut refs = Vec::new();
    let mut dropped = 0;
    for r in x.references {
        if !nodes.iter().any(|n| n.id == r.source) {
            dropped += 1;
            continue;
        }
        refs.push(Ref {
            source: r.source,
            kind: r.kind,
            target_name: r.target,
            context: x.context.clone(),
        });
    }
    (nodes, refs, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net() -> Error {
        Error::Network("réseau".into())
    }

    #[test]
    fn req010_ac9_success_dispatched_before_errors_keeps_them_counted() {
        let mut p = Policy::default();
        let a = p.dispatch();
        assert!(!p.record(a, None));
        let (b, c, d) = (p.dispatch(), p.dispatch(), p.dispatch());
        assert!(!p.record(c, Some(&net())));
        assert!(!p.record(d, Some(&net())));
        assert!(!p.record(b, None));
        let e = p.dispatch();
        assert!(p.record(e, Some(&net())));
        assert!(p.stop_cause.unwrap().contains("panne réseau répétée"));
    }

    #[test]
    fn req010_ac9_sequential_success_resets_count() {
        let mut p = Policy::default();
        for _ in 0..2 {
            let t = p.dispatch();
            assert!(!p.record(t, Some(&net())));
        }
        let t = p.dispatch();
        assert!(!p.record(t, None));
        for _ in 0..2 {
            let t = p.dispatch();
            assert!(!p.record(t, Some(&net())));
        }
        let t = p.dispatch();
        assert!(p.record(t, Some(&net())));
    }

    #[test]
    fn req010_ac9_timeout_and_invalid_neither_count_nor_reset() {
        let mut p = Policy::default();
        for _ in 0..2 {
            let t = p.dispatch();
            p.record(t, Some(&net()));
        }
        for e in [Error::Timeout("d".into()), Error::Message("x".into())] {
            let t = p.dispatch();
            assert!(!p.record(t, Some(&e)));
        }
        let t = p.dispatch();
        assert!(p.record(t, Some(&net())));
    }

    #[test]
    fn req010_ac9_invalid_key_stops_at_first() {
        let mut p = Policy::default();
        let t = p.dispatch();
        assert!(p.record(t, Some(&Error::InvalidKey("k".into()))));
    }
}
