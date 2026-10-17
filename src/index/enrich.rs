//! Phase enrichissement (DEC-019, REQ-024) : résumés et concepts demandés au LLM pour les
//! fichiers « à enrichir », avec la politique d'échec de DEC-015 / DEC-022.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;

use crate::chunk;
use crate::error::{warn, Error, Result};
use crate::index::build::{BuildReport, IndexOptions};
use crate::llm::{Enrichment, Llm, LotItem};
use crate::model::{EdgeKind, FileRecord, Node, NodeKind, Ref};
use crate::store::Store;
use crate::trace;

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

/// Unité de travail d'un fil : un lot de fichiers envoyés ensemble, ou un gros fichier envoyé
/// seul en morceaux (REQ-027).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unit<'a> {
    Lot(Vec<&'a FileRecord>),
    Big(&'a FileRecord),
}

impl Unit<'_> {
    fn file_count(&self) -> usize {
        match self {
            Unit::Lot(v) => v.len(),
            Unit::Big(_) => 1,
        }
    }
}

/// Forme les unités dans l'ordre des fichiers : un lot compte au plus `batch_files` fichiers et
/// au plus `max_chunk_chars` octets au total ; un fichier plus gros que `max_chunk_chars` forme
/// une unité `Big` et coupe le lot en cours.
pub(crate) fn form_units(
    files: &[FileRecord],
    batch_files: usize,
    max_chunk_chars: usize,
) -> Vec<Unit<'_>> {
    let batch_files = batch_files.max(1);
    let mut units = Vec::new();
    let mut current: Vec<&FileRecord> = Vec::new();
    let mut total = 0u64;
    for f in files {
        if f.size > max_chunk_chars as u64 {
            if !current.is_empty() {
                units.push(Unit::Lot(std::mem::take(&mut current)));
                total = 0;
            }
            units.push(Unit::Big(f));
            continue;
        }
        if !current.is_empty()
            && (current.len() >= batch_files || total + f.size > max_chunk_chars as u64)
        {
            units.push(Unit::Lot(std::mem::take(&mut current)));
            total = 0;
        }
        total += f.size;
        current.push(f);
    }
    if !current.is_empty() {
        units.push(Unit::Lot(current));
    }
    units
}

/// Enrichit une unité : un résultat par fichier, et l'erreur de l'appel (pour la politique) s'il
/// y a eu un appel. Un fichier illisible échoue seul et sort du lot.
fn run_unit(
    unit: &Unit<'_>,
    repo: &Path,
    max_chunk_chars: usize,
    llm: &dyn Llm,
    ids: &HashMap<String, Vec<String>>,
) -> (Vec<Outcome>, Option<Option<Error>>) {
    let files: Vec<&FileRecord> = match unit {
        Unit::Lot(v) => v.clone(),
        Unit::Big(f) => vec![*f],
    };
    let mut outcomes = Vec::new();
    let mut readable: Vec<(&FileRecord, String)> = Vec::new();
    for f in files {
        match std::fs::read_to_string(repo.join(&f.path)) {
            Ok(t) => readable.push((f, t)),
            Err(e) => outcomes.push(Outcome::Failed(
                f.path.clone(),
                Error::Message(format!("lecture de {} impossible : {e}", f.path)),
            )),
        }
    }
    let node_ids =
        |f: &FileRecord| -> Vec<String> { ids.get(&f.path).cloned().unwrap_or_default() };
    match readable.len() {
        0 => (outcomes, None),
        1 => {
            let (f, text) = &readable[0];
            let r = enrich_chunks(llm, &f.path, text, max_chunk_chars, &node_ids(f));
            let err = r.as_ref().err().cloned();
            outcomes.push(match r {
                Ok(x) => Outcome::Done((*f).clone(), x),
                Err(e) => Outcome::Failed(f.path.clone(), e),
            });
            (outcomes, Some(err))
        }
        _ => {
            let items: Vec<LotItem> = readable
                .iter()
                .map(|(f, t)| LotItem {
                    file: f.path.clone(),
                    text: t.clone(),
                    node_ids: node_ids(f),
                })
                .collect();
            match traced_lot(llm, &items) {
                Err(e) => {
                    for (f, _) in &readable {
                        outcomes.push(Outcome::Failed(f.path.clone(), e.clone()));
                    }
                    (outcomes, Some(Some(e)))
                }
                Ok(results) => {
                    let mut by_path: HashMap<String, Result<Enrichment>> =
                        results.into_iter().collect();
                    for (f, _) in &readable {
                        outcomes.push(match by_path.remove(&f.path) {
                            Some(Ok(x)) => Outcome::Done((*f).clone(), x),
                            Some(Err(e)) => Outcome::Failed(f.path.clone(), e),
                            None => Outcome::Failed(
                                f.path.clone(),
                                Error::Message(format!("{} : absent de la réponse du lot", f.path)),
                            ),
                        });
                    }
                    (outcomes, Some(None))
                }
            }
        }
    }
}

/// Détail d'un échec pour la trace. Pour `Error::Message`, seul le libellé fixe (avant le
/// premier « : ») est gardé : le détail serde cite des valeurs et des noms de champs de la réponse.
fn trace_detail(e: &Error) -> String {
    match e {
        Error::Message(m) => m.split(" : ").next().unwrap_or("").to_string(),
        other => other.to_string(),
    }
}

/// Un appel `llm.enrich` encadré de ses traces de départ et de fin (jamais de contenu).
fn traced_enrich(
    llm: &dyn Llm,
    path: &str,
    index: usize,
    total: usize,
    text: &str,
    node_ids: &[String],
) -> Result<Enrichment> {
    if !trace::enabled() {
        return llm.enrich(path, index, text, node_ids);
    }
    trace!(
        "llm enrich départ {path} [{index}/{total}] {} car.",
        text.chars().count()
    );
    let started = Instant::now();
    let result = llm.enrich(path, index, text, node_ids);
    let ms = started.elapsed().as_millis();
    match &result {
        Ok(x) => trace!(
            "llm enrich fin {path} [{index}/{total}] ok ({} résumés, {} concepts) {ms} ms",
            x.summaries.len(),
            x.concepts.len()
        ),
        Err(e) => trace!(
            "llm enrich fin {path} [{index}/{total}] {} {ms} ms : {}",
            trace::classify(e),
            trace::short(&trace_detail(e), trace::MAX_MESSAGE_CHARS)
        ),
    }
    result
}

/// Un appel `llm.enrich_lot` encadré de ses traces de départ et de fin (jamais de contenu).
fn traced_lot(llm: &dyn Llm, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
    if !trace::enabled() {
        return llm.enrich_lot(items);
    }
    let label = format!("lot {} +{}", items[0].file, items.len() - 1);
    trace!(
        "llm enrich départ {label} [1/1] {} car.",
        items.iter().map(|i| i.text.chars().count()).sum::<usize>()
    );
    let started = Instant::now();
    let result = llm.enrich_lot(items);
    let ms = started.elapsed().as_millis();
    match &result {
        Ok(results) => {
            let (r, k) = results
                .iter()
                .filter_map(|(_, x)| x.as_ref().ok())
                .fold((0, 0), |(r, k), x| {
                    (r + x.summaries.len(), k + x.concepts.len())
                });
            trace!("llm enrich fin {label} [1/1] ok ({r} résumés, {k} concepts) {ms} ms")
        }
        Err(e) => trace!(
            "llm enrich fin {label} [1/1] {} {ms} ms : {}",
            trace::classify(e),
            trace::short(&trace_detail(e), trace::MAX_MESSAGE_CHARS)
        ),
    }
    result
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
        return traced_enrich(llm, path, 1, 1, &parts[0], node_ids);
    }
    let mut results = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        results.push(traced_enrich(
            llm,
            path,
            i + 1,
            parts.len(),
            part,
            node_ids,
        )?);
    }
    Ok(chunk::merge_enrichments(results))
}

/// Identifiants des nœuds déclarés par chaque fichier, hors packages et clés de configuration
/// (REQ-024).
fn node_ids_by_file(store: &Store) -> Result<HashMap<String, Vec<String>>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (file, node) in store.declarations()? {
        if !node.starts_with("package:") && !node.starts_with("key:") {
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

fn queue_len(queue: &Mutex<VecDeque<Unit>>) -> usize {
    queue.lock().unwrap_or_else(|p| p.into_inner()).len()
}

/// Pool de threads LLM ; le thread courant écrit une transaction par fichier.
pub(crate) fn enrich_all(
    repo: &Path,
    files: &[FileRecord],
    opts: &IndexOptions,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    let IndexOptions {
        concurrency,
        max_chunk_chars,
        batch_files,
    } = *opts;
    let ids = node_ids_by_file(store)?;
    let started = Instant::now();
    let failed_before = report.failed;
    let mut succeeded = 0usize;
    let units = form_units(files, batch_files, max_chunk_chars);
    let queue: Mutex<VecDeque<Unit>> = Mutex::new(units.into());
    let stop = AtomicBool::new(false);
    let policy = Mutex::new(Policy::default());
    let (tx, rx) = mpsc::channel::<Outcome>();
    let workers = concurrency.clamp(1, queue_len(&queue).max(1));
    let mut write_error: Option<Error> = None;
    trace!(
        "enrichissement: début {} fichiers, {workers} fils",
        files.len()
    );

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
                let Some(unit) = queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front() else {
                    break;
                };
                let ticket = policy.lock().unwrap_or_else(|p| p.into_inner()).dispatch();
                let (outcomes, call) = run_unit(&unit, repo, max_chunk_chars, llm, ids);
                let error = call.as_ref().and_then(|e| e.as_ref());
                let halt = {
                    let mut guard = policy.lock().unwrap_or_else(|p| p.into_inner());
                    let was_stopped = guard.stop_cause.is_some();
                    let halt = match call {
                        Some(_) => guard.record(ticket, error),
                        None => guard.stop_cause.is_some(),
                    };
                    if trace::enabled() {
                        if matches!(error, Some(Error::Network(_))) {
                            trace!(
                                "politique: erreurs réseau consécutives {}/{MAX_CONSECUTIVE_NETWORK_ERRORS}",
                                guard.consecutive_network
                            );
                        }
                        if halt && !was_stopped {
                            let left: usize = queue
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .iter()
                                .map(Unit::file_count)
                                .sum();
                            trace!(
                                "politique: arrêt de l'envoi ({}), {left} fichiers non envoyés",
                                guard.stop_cause.as_deref().unwrap_or("")
                            );
                        }
                    }
                    halt
                };
                if halt {
                    stop.store(true, Ordering::SeqCst);
                }
                if outcomes.into_iter().any(|o| tx.send(o).is_err()) {
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
                    succeeded += 1;
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
    trace!(
        "enrichissement: fin {} ms, {succeeded} enrichis, {} en échec",
        started.elapsed().as_millis(),
        report.failed - failed_before
    );
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

    fn rec(path: &str, size: u64) -> FileRecord {
        FileRecord {
            path: path.into(),
            hash: String::new(),
            size,
            mtime: 0,
            recorded_at: 0,
            state: crate::model::FileState::Indexed,
        }
    }

    fn shape(units: &[Unit]) -> Vec<String> {
        units
            .iter()
            .map(|u| match u {
                Unit::Lot(v) => v
                    .iter()
                    .map(|f| f.path.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                Unit::Big(f) => format!("BIG:{}", f.path),
            })
            .collect()
    }

    #[test]
    fn req027_form_units_count_bound_and_order() {
        let files: Vec<_> = (0..10).map(|i| rec(&format!("f{i}"), 10)).collect();
        let u = form_units(&files, 4, 1000);
        assert_eq!(shape(&u), ["f0,f1,f2,f3", "f4,f5,f6,f7", "f8,f9"]);
    }

    #[test]
    fn req027_form_units_size_bound_and_big_isolated() {
        let files = vec![
            rec("a", 40),
            rec("b", 40),
            rec("c", 40),
            rec("big", 500),
            rec("d", 5),
        ];
        let u = form_units(&files, 8, 100);
        assert_eq!(shape(&u), ["a,b", "c", "BIG:big", "d"]);
    }

    #[test]
    fn req027_form_units_batch_one_is_one_file_per_unit() {
        let files: Vec<_> = (0..3).map(|i| rec(&format!("f{i}"), 1)).collect();
        assert_eq!(shape(&form_units(&files, 1, 100)), ["f0", "f1", "f2"]);
        assert!(form_units(&[], 4, 100).is_empty());
    }

    #[test]
    fn req027_form_units_exact_fit() {
        let files = vec![rec("a", 50), rec("b", 50), rec("c", 100)];
        assert_eq!(shape(&form_units(&files, 4, 100)), ["a,b", "c"]);
    }

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
