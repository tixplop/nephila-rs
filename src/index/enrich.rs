//! Phase enrichissement (DEC-019, REQ-024) : résumés et concepts demandés au LLM pour les
//! fichiers « à enrichir », avec la politique d'échec de DEC-015 / DEC-022.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;

use crate::chunk;
use crate::error::{warn, Error, Result, TRUNCATED};
use crate::index::build::{BuildReport, IndexOptions};
use crate::llm::{with_format_retries, Enrichment, Llm, LotItem};
use crate::model::{EdgeKind, FileRecord, Node, NodeKind, Ref};
use crate::progress::{self, Phase};
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

/// Nombre maximal de nœuds demandés par appel d'enrichissement (DEC-058) : environ 40 jetons par
/// résumé, ce qui reste sous `max_tokens = 8192` ; non configurable.
const MAX_NODES_PER_CALL: usize = 150;

/// Découpe la liste des nœuds d'un fichier en groupes d'au plus `MAX_NODES_PER_CALL` ; au moins
/// un groupe, vide si la liste l'est.
fn node_groups(ids: &[String]) -> Vec<&[String]> {
    if ids.is_empty() {
        return vec![ids];
    }
    ids.chunks(MAX_NODES_PER_CALL).collect()
}

/// Forme les unités avec 0 nœud par fichier (tests de la forme par taille et par nombre).
#[cfg(test)]
pub(crate) fn form_units(
    files: &[FileRecord],
    batch_files: usize,
    max_chunk_chars: usize,
) -> Vec<Unit<'_>> {
    form_units_by_nodes(files, |_| 0, batch_files, max_chunk_chars)
}

/// Forme les unités dans l'ordre des fichiers : un lot compte au plus `batch_files` fichiers, au
/// plus `max_chunk_chars` octets et au plus `MAX_NODES_PER_CALL` nœuds au total ; un fichier plus
/// gros que `max_chunk_chars` ou de plus de `MAX_NODES_PER_CALL` nœuds forme une unité `Big` et
/// coupe le lot en cours.
pub(crate) fn form_units_by_nodes<'a>(
    files: &'a [FileRecord],
    nodes: impl Fn(&FileRecord) -> usize,
    batch_files: usize,
    max_chunk_chars: usize,
) -> Vec<Unit<'a>> {
    let batch_files = batch_files.max(1);
    let mut units = Vec::new();
    let mut current: Vec<&FileRecord> = Vec::new();
    let mut total = 0u64;
    let mut total_nodes = 0usize;
    for f in files {
        let n = nodes(f);
        if f.size > max_chunk_chars as u64 || n > MAX_NODES_PER_CALL {
            if !current.is_empty() {
                units.push(Unit::Lot(std::mem::take(&mut current)));
                total = 0;
                total_nodes = 0;
            }
            units.push(Unit::Big(f));
            continue;
        }
        if !current.is_empty()
            && (current.len() >= batch_files
                || total + f.size > max_chunk_chars as u64
                || total_nodes + n > MAX_NODES_PER_CALL)
        {
            units.push(Unit::Lot(std::mem::take(&mut current)));
            total = 0;
            total_nodes = 0;
        }
        total += f.size;
        total_nodes += n;
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
    ids: &HashMap<String, Vec<(String, String)>>,
    retries: u32,
    stop: &AtomicBool,
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
    let nodes =
        |f: &FileRecord| -> Vec<(String, String)> { ids.get(&f.path).cloned().unwrap_or_default() };
    let node_ids = |f: &FileRecord| -> Vec<String> {
        ids.get(&f.path)
            .map(|v| v.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    };
    match readable.len() {
        0 => (outcomes, None),
        1 => {
            let (f, text) = &readable[0];
            let r = enrich_chunks(
                llm,
                &f.path,
                text,
                max_chunk_chars,
                &nodes(f),
                retries,
                stop,
            );
            let err = r.as_ref().err().cloned();
            let r = r.map_err(truncation_advice);
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
            match traced_lot(llm, &items, retries, stop) {
                Err(e) => {
                    let per_file = truncation_advice(e.clone());
                    for (f, _) in &readable {
                        outcomes.push(Outcome::Failed(f.path.clone(), per_file.clone()));
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
        Error::Message(m) | Error::Format(m) => m.split(" : ").next().unwrap_or("").to_string(),
        Error::Truncated(_) => TRUNCATED.to_string(),
        other => other.to_string(),
    }
}

/// Erreur de chaque fichier dont l'appel a été tronqué : le libellé fixe et l'action (A-5).
fn truncation_advice(e: Error) -> Error {
    match e {
        Error::Truncated(_) => Error::Truncated(format!(
            "{TRUNCATED} ; réduisez max_chunk_chars ou batch_files"
        )),
        other => other,
    }
}

/// Numéro d'essai `current` sur `total` d'un même appel (REQ-038).
#[derive(Clone, Copy)]
struct Attempt {
    current: u32,
    total: u32,
}

impl Attempt {
    /// Marque de la ligne de départ d'un essai à partir du deuxième.
    fn mark(self) -> String {
        if self.current >= 2 {
            format!(" (essai {}/{})", self.current, self.total)
        } else {
            String::new()
        }
    }
}

/// Un appel `llm.enrich` encadré de ses traces de départ et de fin (jamais de contenu).
#[allow(clippy::too_many_arguments)]
fn traced_enrich(
    llm: &dyn Llm,
    path: &str,
    index: usize,
    total: usize,
    text: &str,
    requested: &[String],
    known: &[String],
    attempt: Attempt,
) -> Result<Enrichment> {
    if !trace::enabled() {
        return llm.enrich(path, index, text, requested, known);
    }
    trace!(
        "llm enrich départ {path} [{index}/{total}] {} car.{}",
        text.chars().count(),
        attempt.mark()
    );
    let started = Instant::now();
    let result = llm.enrich(path, index, text, requested, known);
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
fn traced_lot(
    llm: &dyn Llm,
    items: &[LotItem],
    retries: u32,
    stop: &AtomicBool,
) -> Result<Vec<(String, Result<Enrichment>)>> {
    let total = retries.saturating_add(1);
    with_format_retries(retries, stop, |current| {
        traced_lot_once(llm, items, Attempt { current, total })
    })
}

fn traced_lot_once(
    llm: &dyn Llm,
    items: &[LotItem],
    attempt: Attempt,
) -> Result<Vec<(String, Result<Enrichment>)>> {
    if !trace::enabled() {
        return llm.enrich_lot(items);
    }
    let label = format!("lot {} +{}", items[0].file, items.len() - 1);
    trace!(
        "llm enrich départ {label} [1/1] {} car.{}",
        items.iter().map(|i| i.text.chars().count()).sum::<usize>(),
        attempt.mark()
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

/// Plan d'appels d'un fichier (DEC-059) : couples (indice du morceau, liste de nœuds demandée),
/// dans l'ordre d'envoi. Un seul morceau : la liste complète, découpée par `node_groups`. Sinon,
/// chaque morceau ne demande que les nœuds dont le nom court y figure (mot entier, ou, pour un
/// nom qui n'est pas un mot, occurrence aux bords contrôlés) ; un nœud de nom vide ou absent de
/// tous les morceaux va dans le morceau 1. Au moins un appel par morceau, liste vide permise.
fn call_plan(parts: &[String], nodes: &[(String, String)]) -> Vec<(usize, Vec<String>)> {
    let ids: Vec<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
    if parts.len() <= 1 {
        return node_groups(&ids)
            .into_iter()
            .map(|g| (0, g.to_vec()))
            .collect();
    }
    let words: Vec<HashSet<&str>> = parts.iter().map(|p| ident_words(p)).collect();
    let mut lists: Vec<Vec<String>> = vec![Vec::new(); parts.len()];
    for (id, short) in nodes {
        let mut found = false;
        if !short.is_empty() {
            let simple = short.chars().all(is_ident);
            for (k, part) in parts.iter().enumerate() {
                let hit = if simple {
                    words[k].contains(short.as_str())
                } else {
                    contains_bounded(part, short)
                };
                if hit {
                    lists[k].push(id.clone());
                    found = true;
                }
            }
        }
        if !found {
            lists[0].push(id.clone());
        }
    }
    let mut plan = Vec::new();
    for (k, list) in lists.iter().enumerate() {
        for g in node_groups(list) {
            plan.push((k, g.to_vec()));
        }
    }
    plan
}

/// Caractère d'un identifiant : lettre, chiffre, `_` ou `$` (REQ-047).
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Mots maximaux d'`is_ident` d'un texte, en un seul parcours.
fn ident_words(text: &str) -> HashSet<&str> {
    let mut set = HashSet::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (is_ident(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                set.insert(&text[s..i]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        set.insert(&text[s..]);
    }
    set
}

/// Vrai si `name` (non vide) figure dans `text` avec des bords qui ne prolongent pas un mot : le
/// caractère qui précède (resp. suit) ne doit pas être `is_ident` quand le nom commence (resp.
/// finit) par un caractère `is_ident`.
fn contains_bounded(text: &str, name: &str) -> bool {
    let first_ident = name.chars().next().is_some_and(is_ident);
    let last_ident = name.chars().next_back().is_some_and(is_ident);
    text.match_indices(name).any(|(i, m)| {
        let before_ok = !first_ident || !text[..i].chars().next_back().is_some_and(is_ident);
        let after_ok = !last_ident || !text[i + m.len()..].chars().next().is_some_and(is_ident);
        before_ok && after_ok
    })
}

/// Enrichit un fichier selon `call_plan` : un appel par morceau et par groupe de la liste propre
/// à ce morceau, numérotés 1..n dans l'ordre du plan ; le premier échec d'un appel fait échouer
/// le fichier entier (REQ-007).
fn enrich_chunks(
    llm: &dyn Llm,
    path: &str,
    text: &str,
    max_chunk_chars: usize,
    nodes: &[(String, String)],
    retries: u32,
    stop: &AtomicBool,
) -> Result<Enrichment> {
    let total = retries.saturating_add(1);
    let known: Vec<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
    // Le compte d'essais repart à 1 pour chaque appel ; seul l'appel fautif est renvoyé.
    let one = |index: usize, count: usize, part: &str, requested: &[String]| {
        with_format_retries(retries, stop, |current| {
            traced_enrich(
                llm,
                path,
                index,
                count,
                part,
                requested,
                &known,
                Attempt { current, total },
            )
        })
    };
    let parts = chunk::split(text, max_chunk_chars);
    let plan = call_plan(&parts, nodes);
    let count = plan.len();
    if count == 1 {
        return one(1, 1, &parts[plan[0].0], &plan[0].1);
    }
    let mut results = Vec::with_capacity(count);
    for (k, list) in &plan {
        results.push(one(results.len() + 1, count, &parts[*k], list)?);
    }
    Ok(chunk::merge_enrichments(results))
}

/// Nœuds déclarés par chaque fichier (identifiant, nom court), hors packages et clés de
/// configuration (REQ-024).
fn nodes_by_file(store: &Store) -> Result<HashMap<String, Vec<(String, String)>>> {
    let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (file, node, short) in store.declared_short_names()? {
        if !node.starts_with("package:") && !node.starts_with("key:") {
            map.entry(file).or_default().push((node, short));
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
    enrich_with_stop(
        repo,
        files,
        opts,
        llm,
        store,
        report,
        &AtomicBool::new(false),
    )
    .map(|_| ())
}

/// Corps d'`enrich_all` avec le drapeau d'arrêt injecté (DEC-047) ; rend les fichiers en échec.
pub(crate) fn enrich_with_stop(
    repo: &Path,
    files: &[FileRecord],
    opts: &IndexOptions,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
    stop: &AtomicBool,
) -> Result<Vec<(String, Error)>> {
    let IndexOptions {
        concurrency,
        max_chunk_chars,
        batch_files,
        format_retries,
    } = *opts;
    let ids = nodes_by_file(store)?;
    let started = Instant::now();
    let failed_before = report.failed;
    let mut succeeded = 0usize;
    let units = form_units_by_nodes(
        files,
        |f| ids.get(&f.path).map_or(0, Vec::len),
        batch_files,
        max_chunk_chars,
    );
    let queue: Mutex<VecDeque<Unit>> = Mutex::new(units.into());
    let policy = Mutex::new(Policy::default());
    let (tx, rx) = mpsc::channel::<Vec<Outcome>>();
    let workers = concurrency.clamp(1, queue_len(&queue).max(1));
    let mut write_error: Option<Error> = None;
    let mut failures: Vec<(String, Error)> = Vec::new();
    trace!(
        "enrichissement: début {} fichiers, {workers} fils",
        files.len()
    );
    let mut meter = progress::Meter::start(Phase::Enrichment, files.len());

    std::thread::scope(|scope| {
        let queue = &queue;
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
                let (outcomes, call) = run_unit(
                    &unit,
                    repo,
                    max_chunk_chars,
                    llm,
                    ids,
                    format_retries,
                    stop,
                );
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
                if !outcomes.is_empty() && tx.send(outcomes).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for batch in rx {
            let count = batch.len();
            for outcome in batch {
                match outcome {
                    Outcome::Failed(path, e) => {
                        warn(&format!("{path} : {e}"));
                        report.failed += 1;
                        failures.push((path, e));
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
            if let Some(m) = meter.as_mut() {
                m.advance(count);
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
        None => Ok(failures),
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

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("n{i}")).collect()
    }

    #[test]
    fn req047_ac2_node_groups_bounds() {
        assert_eq!(node_groups(&[]).len(), 1);
        assert!(node_groups(&[])[0].is_empty());
        let sizes =
            |n: usize| -> Vec<usize> { node_groups(&ids(n)).iter().map(|g| g.len()).collect() };
        assert_eq!(sizes(150), [150]);
        assert_eq!(sizes(151), [150, 1]);
        assert_eq!(sizes(401), [150, 150, 101]);
        let all = ids(401);
        let joined: Vec<String> = node_groups(&all).concat();
        assert_eq!(joined, all);
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn req047_ac12_plan_par_nom_court() {
        let parts = strs(&[
            "class A {\n  void foo() {}\n  void m1() {}\n",
            "class A {\n  void m12() {}\n  void bar() { foo(); }\n",
            "// rien\n",
        ]);
        let nodes = pairs(&[
            ("A", "A"),
            ("bar", "bar"),
            ("foo", "foo"),
            ("m1", "m1"),
            ("m12", "m12"),
            ("qux", "qux"),
            ("vide", ""),
        ]);
        assert_eq!(
            call_plan(&parts, &nodes),
            vec![
                (0, strs(&["A", "foo", "m1", "qux", "vide"])),
                (1, strs(&["A", "bar", "foo", "m12"])),
                (2, vec![]),
            ]
        );

        // Un seul morceau : la liste complète, comme en 0.17.4.
        let many: Vec<(String, String)> = (0..160)
            .map(|i| (format!("id{i}"), format!("n{i}")))
            .collect();
        let all: Vec<String> = many.iter().map(|(a, _)| a.clone()).collect();
        let plan = call_plan(&parts[2..], &many);
        let expected: Vec<(usize, Vec<String>)> = node_groups(&all)
            .into_iter()
            .map(|g| (0, g.to_vec()))
            .collect();
        assert_eq!(plan, expected);
        assert_eq!(plan.len(), 2);

        // 160 nœuds tous dans le morceau 1, aucun dans le morceau 2.
        let text1: String = (0..160).map(|i| format!("n{i} ")).collect();
        let two = vec![text1, "rien".to_string()];
        let plan = call_plan(&two, &many);
        let sizes: Vec<(usize, usize)> = plan.iter().map(|(k, l)| (*k, l.len())).collect();
        assert_eq!(sizes, [(0, 150), (0, 10), (1, 0)]);
    }

    #[test]
    fn req047_ac12_noms_non_mots_et_bords() {
        let parts = strs(&["a operator+ b\n", "x+= y\n foo$bar\n"]);
        let nodes = pairs(&[
            ("plus", "operator+"),
            ("bords", "+="),
            ("dollar", "foo$bar"),
            ("prefixe", "foo"),
        ]);
        let plan = call_plan(&parts, &nodes);
        assert_eq!(plan[0], (0, strs(&["plus", "prefixe"])));
        assert_eq!(plan[1], (1, strs(&["bords", "dollar"])));
    }

    #[test]
    fn req047_ac13_plan_de_20000_noeuds_rapide() {
        let nodes: Vec<(String, String)> = (0..20_000)
            .map(|i| (format!("method:p.C.m{i}"), format!("m{i}")))
            .collect();
        // Le morceau k cite les 4 000 noms de sa tranche, puis du remplissage jusqu'à 100 000.
        let parts: Vec<String> = (0..5)
            .map(|k| {
                let mut s = String::new();
                for i in k * 4_000..(k + 1) * 4_000 {
                    s.push_str(&format!("void m{i}();\n"));
                }
                while s.len() < 100_000 {
                    s.push_str("// remplissage\n");
                }
                s
            })
            .collect();
        let plan = call_plan(&parts, &nodes);
        let expected: usize = (0..5)
            .map(|k| {
                let n = plan
                    .iter()
                    .filter(|(p, _)| *p == k)
                    .map(|(_, l)| l.len())
                    .sum::<usize>();
                n.div_ceil(MAX_NODES_PER_CALL).max(1)
            })
            .sum();
        assert_eq!(plan.len(), expected);
        assert!(
            plan.len() <= 20_000usize.div_ceil(150) + 10,
            "{}",
            plan.len()
        );
        assert!(plan.iter().all(|(_, l)| l.len() <= MAX_NODES_PER_CALL));
    }

    #[test]
    fn req047_ac1_form_units_by_nodes() {
        let counts = [100usize, 60, 40, 200, 10];
        let files: Vec<_> = (0..5).map(|i| rec(&format!("f{i}"), 1)).collect();
        let nodes = |f: &FileRecord| counts[f.path[1..].parse::<usize>().unwrap()];
        let u = form_units_by_nodes(&files, nodes, 8, 1000);
        assert_eq!(shape(&u), ["f0", "f1,f2", "BIG:f3", "f4"]);
        let files = vec![rec("a", 1)];
        let u = form_units_by_nodes(&files, |_| 150, 8, 1000);
        assert_eq!(shape(&u), ["a"]);
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
    fn req046_ac6_truncated_neither_counts_nor_resets() {
        let trunc = Error::Truncated("tronquée".into());
        let mut p = Policy::default();
        for r in [Some(&net()), Some(&net()), Some(&trunc)] {
            let t = p.dispatch();
            assert!(!p.record(t, r));
        }
        assert_eq!((p.consecutive_network, p.network_total), (2, 2));
        let t = p.dispatch();
        assert!(p.record(t, Some(&net())));

        let mut p = Policy::default();
        for _ in 0..5 {
            let t = p.dispatch();
            assert!(!p.record(t, Some(&trunc)));
        }
        assert!(p.stop_cause.is_none());
    }

    #[test]
    fn req010_ac9_invalid_key_stops_at_first() {
        let mut p = Policy::default();
        let t = p.dispatch();
        assert!(p.record(t, Some(&Error::InvalidKey("k".into()))));
    }

    #[test]
    fn req038_ac7_final_format_result_does_not_touch_network_count() {
        let mut p = Policy::default();
        for _ in 0..2 {
            let t = p.dispatch();
            assert!(!p.record(t, Some(&net())));
        }
        let fmt = Error::Format("non conforme (3 essais sur 3)".into());
        let t = p.dispatch();
        assert!(!p.record(t, Some(&fmt)));
        assert_eq!((p.consecutive_network, p.network_total), (2, 2));
        // Une suite d'essais terminée par une erreur réseau compte pour une seule erreur.
        let t = p.dispatch();
        let r: Result<()> = with_format_retries(2, &AtomicBool::new(false), |n| {
            if n < 3 {
                Err(Error::Format("f".into()))
            } else {
                Err(net())
            }
        });
        assert!(matches!(r, Err(Error::Network(_))));
        assert!(p.record(t, r.as_ref().err()));
        assert_eq!((p.consecutive_network, p.network_total), (3, 3));
    }

    /// Une clé invalide sur un autre fil pose l'arrêt pendant que A boucle sur ses essais :
    /// A garde son erreur `Format` et n'est pas réessayé.
    #[test]
    fn req038_ac10_invalid_key_on_other_thread_stops_retries() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc::{Receiver, Sender};
        use std::time::Duration;

        struct Fake<'a> {
            stop: &'a AtomicBool,
            rx: Mutex<Receiver<()>>,
            tx: Mutex<Sender<()>>,
            calls_a: AtomicUsize,
        }
        impl Llm for Fake<'_> {
            fn verify_key(&self) -> Result<()> {
                Ok(())
            }
            fn enrich(
                &self,
                path: &str,
                _: usize,
                _: &str,
                _: &[String],
                _: &[String],
            ) -> Result<Enrichment> {
                if path.ends_with("B.java") {
                    self.tx.lock().unwrap().send(()).unwrap();
                    return Err(Error::InvalidKey("clé refusée".into()));
                }
                if self.calls_a.fetch_add(1, Ordering::SeqCst) == 0 {
                    self.rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .expect("B jamais envoyé pendant l'appel de A");
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !self.stop.load(Ordering::SeqCst) {
                        assert!(Instant::now() < deadline, "arrêt jamais posé après B");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                Err(Error::Format("réponse non conforme".into()))
            }
            fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
                Err(Error::Message("inutilisé".into()))
            }
            fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
                Err(Error::Message("inutilisé".into()))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        for n in ["A.java", "B.java"] {
            std::fs::write(dir.path().join(n), "package p;\npublic class X {}\n").unwrap();
        }
        let files = [rec("A.java", 30), rec("B.java", 30)];
        let mut store = Store::open_or_create(dir.path()).unwrap();
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            concurrency: 2,
            max_chunk_chars: 1000,
            batch_files: 1,
            format_retries: u32::MAX,
        };
        let stop = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        let llm = Fake {
            stop: &stop,
            rx: Mutex::new(rx),
            tx: Mutex::new(tx),
            calls_a: AtomicUsize::new(0),
        };
        let mut failures = enrich_with_stop(
            dir.path(),
            &files,
            &opts,
            &llm,
            &mut store,
            &mut report,
            &stop,
        )
        .unwrap();
        failures.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(llm.calls_a.load(Ordering::SeqCst), 1);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].0, "A.java");
        match &failures[0].1 {
            Error::Format(m) => assert!(m.contains("non conforme"), "{m}"),
            e => panic!("attendu Format, obtenu {e:?}"),
        }
        assert_eq!(failures[1].0, "B.java");
        assert!(matches!(failures[1].1, Error::InvalidKey(_)));
        assert!(report.stop_cause.is_some());
    }

    /// Un lot de 4 fichiers avance le compte d'un bloc (AC-043-5).
    #[test]
    fn req043_ac5_batch_advances_as_one_block() {
        struct Lots;
        impl Llm for Lots {
            fn verify_key(&self) -> Result<()> {
                Ok(())
            }
            fn enrich(
                &self,
                _: &str,
                _: usize,
                _: &str,
                _: &[String],
                _: &[String],
            ) -> Result<Enrichment> {
                Ok(Enrichment::default())
            }
            fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
                Ok(items
                    .iter()
                    .map(|i| (i.file.clone(), Ok(Enrichment::default())))
                    .collect())
            }
            fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
                Err(Error::Message("inutilisé".into()))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for i in 0..10 {
            let name = format!("C{i}.java");
            std::fs::write(dir.path().join(&name), "package p;\npublic class X {}\n").unwrap();
            files.push(rec(&name, 30));
        }
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(&files).unwrap();
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files: 4,
            format_retries: 0,
        };
        progress::init(true);
        progress::take_captured();
        enrich_with_stop(
            dir.path(),
            &files,
            &opts,
            &Lots,
            &mut store,
            &mut report,
            &AtomicBool::new(false),
        )
        .unwrap();
        let lines = progress::take_captured();
        progress::init(false);
        assert_eq!(
            lines,
            [
                "progression: enrichissement 0 % (0/10 fichiers)",
                "progression: enrichissement 40 % (4/10 fichiers)",
                "progression: enrichissement 80 % (8/10 fichiers)",
                "progression: enrichissement 100 % (10/10 fichiers)",
            ]
        );
    }
}
