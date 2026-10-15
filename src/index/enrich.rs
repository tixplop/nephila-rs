//! Phase enrichissement (DEC-019, REQ-024) : résumés et concepts demandés au LLM pour les
//! fichiers « à enrichir », avec la politique d'échec de DEC-015 / DEC-022.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

use crate::chunk;
use crate::error::{warn, Error, Result};
use crate::index::build::BuildReport;
use crate::llm::{Enrichment, Llm};
use crate::model::{EdgeKind, FileRecord, Node, NodeKind, Ref};
use crate::store::Store;

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
    Done(FileRecord, Enrichment),
    Failed(String, Error),
}

/// Enrichit un fichier, en plusieurs appels s'il dépasse `max_chunk_chars` ; le premier échec
/// d'un morceau fait échouer le fichier entier (REQ-007).
fn enrich_chunks(
    llm: &dyn Llm,
    path: &str,
    text: &str,
    max_chunk_chars: usize,
    node_ids: &[String],
) -> Result<Enrichment> {
    let parts = chunk::split(text, max_chunk_chars);
    if parts.len() == 1 {
        return llm.enrich(path, 1, &parts[0], node_ids);
    }
    let mut results = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        results.push(llm.enrich(path, i + 1, part, node_ids)?);
    }
    Ok(chunk::merge_enrichments(results))
}

/// Identifiants des nœuds déclarés par chaque fichier, hors packages (REQ-024).
fn node_ids_by_file(store: &Store) -> Result<HashMap<String, Vec<String>>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (file, node) in store.declarations()? {
        if !node.starts_with("package:") {
            map.entry(file).or_default().push(node);
        }
    }
    Ok(map)
}

/// Nœuds concept et références `mentions` (section → concept) d'un enrichissement.
fn concept_records(e: &Enrichment) -> (Vec<Node>, Vec<Ref>) {
    let mut nodes = Vec::new();
    let mut refs = Vec::new();
    for c in &e.concepts {
        nodes.push(Node::new(
            NodeKind::Concept,
            &c.name,
            &c.name,
            None,
            None,
            c.summary.as_deref(),
        ));
        for s in &c.sections {
            refs.push(Ref {
                source: s.clone(),
                kind: EdgeKind::Mentions,
                target_name: c.name.clone(),
                context: None,
            });
        }
    }
    (nodes, refs)
}

/// Pool de threads LLM ; le thread courant écrit une transaction par fichier.
pub(crate) fn enrich_all(
    repo: &Path,
    files: &[FileRecord],
    concurrency: usize,
    max_chunk_chars: usize,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    let ids = node_ids_by_file(store)?;
    let queue: Mutex<VecDeque<&FileRecord>> = Mutex::new(files.iter().collect());
    let stop = AtomicBool::new(false);
    let policy = Mutex::new(Policy::default());
    let (tx, rx) = mpsc::channel::<Outcome>();
    let workers = concurrency.clamp(1, files.len().max(1));
    let mut write_error: Option<Error> = None;

    std::thread::scope(|scope| {
        let queue = &queue;
        let stop = &stop;
        let policy = &policy;
        let ids = &ids;
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
                let node_ids: &[String] = ids.get(&file.path).map_or(&[], Vec::as_slice);
                let outcome = match std::fs::read_to_string(repo.join(&file.path)) {
                    Err(e) => Outcome::Failed(
                        file.path.clone(),
                        Error::Message(format!("lecture de {} impossible : {e}", file.path)),
                    ),
                    Ok(text) => {
                        match enrich_chunks(llm, &file.path, &text, max_chunk_chars, node_ids) {
                            Ok(x) => Outcome::Done(file.clone(), x),
                            Err(e) => Outcome::Failed(file.path.clone(), e),
                        }
                    }
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
                Outcome::Done(file, x) => {
                    if write_error.is_some() {
                        continue;
                    }
                    if !x.rejected.is_empty() {
                        warn(&format!(
                            "{} : {} élément(s) d'enrichissement écarté(s) : {}",
                            file.path,
                            x.rejected.len(),
                            x.rejected.join(" ; ")
                        ));
                    }
                    let (concepts, refs) = concept_records(&x);
                    if let Err(e) = store.apply_enrichment(
                        &file.path,
                        &file.hash,
                        &x.summaries,
                        &concepts,
                        &refs,
                    ) {
                        stop.store(true, Ordering::SeqCst);
                        write_error = Some(e);
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
