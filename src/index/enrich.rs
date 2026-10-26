//! Phase enrichissement (DEC-019, REQ-024) : résumés et concepts demandés au LLM pour les
//! fichiers « à enrichir », avec la politique d'échec de DEC-015 / DEC-022.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::chunk;
use crate::error::{warn, Error, Result, TRUNCATED};
use crate::index::build::{BuildReport, IndexOptions};
use crate::llm::{with_format_retries, Enrichment, Llm, LotItem};
use crate::model::{EdgeKind, FileRecord, Node, NodeKind, Ref};
use crate::parse::markdown;
use crate::progress::{self, Phase};
use crate::store::{EnrichmentWrite, Store};
use crate::trace;

/// Seuil des erreurs réseau consécutives (saturations comprises) et des délais dépassés
/// consécutifs, chacun compté à part, qui arrête l'envoi (DEC-015, DEC-099).
const MAX_CONSECUTIVE_NETWORK_ERRORS: usize = 3;

/// Cause d'arrêt à seuil : libellé de la trace et cause de la ligne `erreur:` (une seule table).
struct ThresholdCause {
    label: &'static str,
    cause: &'static str,
}

const CAUSE_NETWORK: ThresholdCause = ThresholdCause {
    label: "panne réseau répétée (3 erreurs réseau consécutives)",
    cause: "panne réseau répétée (3 erreurs réseau consécutives)",
};
const CAUSE_SATURATED: ThresholdCause = ThresholdCause {
    label: "fournisseur saturé",
    cause: "fournisseur saturé : relancez nephila update plus tard ou réduisez concurrency",
};
const CAUSE_TIMEOUT: ThresholdCause = ThresholdCause {
    label: "délais dépassés répétés",
    cause: "délais dépassés répétés : augmentez timeout_secs ou vérifiez le fournisseur",
};

/// Politique d'échec partagée entre les threads de travail (DEC-015).
#[derive(Default)]
struct Policy {
    consecutive_network: usize,
    network_total: usize,
    consecutive_timeout: usize,
    timeout_total: usize,
    /// Cause de la ligne `erreur:` finale.
    stop_cause: Option<String>,
    /// Libellé de la cause dans la trace `politique: arrêt de l'envoi`.
    stop_label: Option<String>,
    /// Première erreur d'arrêt immédiat (clé refusée REQ-054, requête refusée REQ-091), si elle
    /// est la cause de l'arrêt.
    refused: Option<Error>,
}

/// Totaux des erreurs réseau et des délais dépassés constatés au départ d'un appel (DEC-022).
type Ticket = (usize, usize);

impl Policy {
    /// Prend un ticket au départ d'un appel (sous le verrou de la politique).
    fn dispatch(&self) -> Ticket {
        (self.network_total, self.timeout_total)
    }

    fn stop_with(&mut self, label: &str, cause: &str) {
        if self.stop_cause.is_none() {
            self.stop_label = Some(label.to_string());
            self.stop_cause = Some(cause.to_string());
        }
    }

    /// Met à jour la politique ; renvoie vrai si l'envoi de nouveaux fichiers doit s'arrêter.
    /// Une réussite n'efface que les échecs constatés avant le départ de son appel (DEC-022).
    fn record(&mut self, ticket: Ticket, result: Option<&Error>) -> bool {
        match result {
            None => {
                self.consecutive_network = self
                    .consecutive_network
                    .min(self.network_total.saturating_sub(ticket.0));
                self.consecutive_timeout = self
                    .consecutive_timeout
                    .min(self.timeout_total.saturating_sub(ticket.1));
            }
            Some(
                e @ (Error::InvalidKey(_) | Error::Rejected { .. } | Error::Redirected { .. }),
            ) => {
                if self.stop_cause.is_none() {
                    self.refused = Some(e.clone());
                    let cause = match e {
                        Error::InvalidKey(_) => "clé d'API refusée par le fournisseur",
                        Error::Redirected { .. } => "requête redirigée par le fournisseur",
                        _ => "requête refusée par le fournisseur",
                    };
                    self.stop_with(cause, cause);
                }
            }
            Some(e @ (Error::Network(_) | Error::Saturated(_))) => {
                self.consecutive_network += 1;
                self.network_total += 1;
                if self.consecutive_network >= MAX_CONSECUTIVE_NETWORK_ERRORS {
                    let c = if matches!(e, Error::Saturated(_)) {
                        CAUSE_SATURATED
                    } else {
                        CAUSE_NETWORK
                    };
                    self.stop_with(c.label, c.cause);
                }
            }
            Some(Error::Timeout(_)) => {
                self.consecutive_timeout += 1;
                self.timeout_total += 1;
                if self.consecutive_timeout >= MAX_CONSECUTIVE_NETWORK_ERRORS {
                    self.stop_with(CAUSE_TIMEOUT.label, CAUSE_TIMEOUT.cause);
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

/// Unité de travail d'un fil (DEC-063) : un lot de fichiers envoyés ensemble, un gros fichier
/// envoyé seul (REQ-027) qui se transforme en appels dans la file commune, ou un de ces appels
/// (morceau × groupe de nœuds, REQ-047).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unit<'a> {
    Lot(Vec<&'a FileRecord>),
    Big(&'a FileRecord),
    Call {
        file: &'a FileRecord,
        /// Rang de l'appel dans le plan, à partir de 1.
        index: usize,
        count: usize,
        part: Arc<str>,
        requested: Vec<String>,
        /// Liste complète des nœuds du fichier, qui filtre la réponse.
        known: Arc<[String]>,
    },
}

impl Unit<'_> {
    /// Fichiers pas encore commencés que cette unité représente.
    fn file_count(&self) -> usize {
        match self {
            Unit::Lot(v) => v.len(),
            Unit::Big(_) => 1,
            Unit::Call { .. } => 0,
        }
    }
}

/// Nombre maximal de nœuds demandés par appel d'enrichissement (DEC-058) : environ 40 jetons par
/// résumé, soit environ 6 000 jetons, sous la limite par défaut
/// (`DEFAULT_MAX_OUTPUT_TOKENS`) ; un `max_output_tokens` plus bas configuré peut tronquer la réponse.
const MAX_NODES_PER_CALL: usize = 150;

/// Nombre maximal d'appels d'enrichissement pour un fichier envoyé seul (DEC-059, non
/// configurable) : 50 appels = 7 500 nœuds d'un seul morceau. Au-delà, aucun appel n'est envoyé.
const MAX_CALLS_PER_FILE: usize = 50;

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

/// Suite d'une unité qui n'est pas un appel.
enum Step<'a> {
    /// Résultats par fichier, et l'erreur de l'appel (pour la politique) s'il y a eu un appel.
    Finished(Vec<Outcome>, Option<Option<Error>>),
    /// Un fichier envoyé seul : ses appels, dans l'ordre du plan, à mettre dans la file.
    Calls(Vec<Outcome>, Vec<Unit<'a>>),
}

/// Où lire les fichiers à enrichir : racine du dépôt et chemins réels déjà connus (DEC-070).
#[derive(Clone, Copy)]
pub(crate) struct Sources<'a> {
    pub(crate) repo: &'a Path,
    pub(crate) abs: &'a HashMap<String, PathBuf>,
}

impl Sources<'_> {
    /// Chemin réel d'un fichier : l'entrée d'`abs`, sinon la racine jointe au chemin.
    fn real_path(&self, path: &str) -> PathBuf {
        self.abs
            .get(path)
            .cloned()
            .unwrap_or_else(|| self.repo.join(path))
    }
}

/// Exécute une unité `Lot` ou `Big`. Un fichier illisible échoue seul et sort du lot.
fn run_unit<'a>(
    unit: &Unit<'a>,
    sources: Sources<'_>,
    max_chunk_chars: usize,
    llm: &dyn Llm,
    ids: &HashMap<String, Vec<(String, String)>>,
    retries: u32,
    stop: &AtomicBool,
) -> Step<'a> {
    let files: Vec<&'a FileRecord> = match unit {
        Unit::Lot(v) => v.clone(),
        Unit::Big(f) => vec![*f],
        Unit::Call { .. } => unreachable!("un appel ne passe pas par run_unit"),
    };
    let mut outcomes = Vec::new();
    let mut readable: Vec<(&'a FileRecord, String)> = Vec::new();
    for f in files {
        match std::fs::read_to_string(sources.real_path(&f.path)) {
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
        0 => Step::Finished(outcomes, None),
        1 => {
            let (f, text) = &readable[0];
            match plan_calls(f, text, max_chunk_chars, &nodes(f)) {
                Ok(calls) => Step::Calls(outcomes, calls),
                Err(e) => {
                    outcomes.push(Outcome::Failed(
                        f.path.clone(),
                        truncation_advice(e.clone()),
                    ));
                    Step::Finished(outcomes, Some(Some(e)))
                }
            }
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
                // Lot tronqué (REQ-096, DEC-101) : deux moitiés remises en tête de file ; aucune
                // issue pour le lot lui-même, ses fichiers en recevront par leurs moitiés.
                Err(Error::Truncated(_)) => {
                    let mut first: Vec<&'a FileRecord> = readable.iter().map(|(f, _)| *f).collect();
                    let second = first.split_off(first.len().div_ceil(2));
                    let name = |half: &[&FileRecord]| match half {
                        [only] => only.path.clone(),
                        [head, ..] => format!("lot {} +{}", head.path, half.len() - 1),
                        [] => String::new(),
                    };
                    trace!(
                        "llm enrich découpe lot {} +{} [1/1] en {} et {}",
                        items[0].file,
                        items.len() - 1,
                        name(&first),
                        name(&second)
                    );
                    Step::Calls(outcomes, vec![Unit::Lot(first), Unit::Lot(second)])
                }
                Err(e) => {
                    let per_file = truncation_advice(e.clone());
                    for (f, _) in &readable {
                        outcomes.push(Outcome::Failed(f.path.clone(), per_file.clone()));
                    }
                    Step::Finished(outcomes, Some(Some(e)))
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
                    Step::Finished(outcomes, Some(None))
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
        Error::Rejected { trace, .. } | Error::Redirected { trace, .. } => trace.clone(),
        other => other.to_string(),
    }
}

/// Erreur de chaque fichier dont l'appel a été tronqué : le libellé fixe et l'action (A-5).
fn truncation_advice(e: Error) -> Error {
    match e {
        Error::Truncated(_) => Error::Truncated(format!(
            "{TRUNCATED} ; réduisez max_chunk_chars ou batch_files, ou augmentez max_output_tokens"
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
    stop: &AtomicBool,
) -> Result<Enrichment> {
    if !trace::enabled() {
        let label = format!("{path} [{index}/{total}]");
        return llm.enrich_labeled(&label, path, index, text, requested, known, stop);
    }
    trace!(
        "llm enrich départ {path} [{index}/{total}] {} car.{}",
        text.chars().count(),
        attempt.mark()
    );
    let started = Instant::now();
    let label = format!("{path} [{index}/{total}]");
    let result = llm.enrich_labeled(&label, path, index, text, requested, known, stop);
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
        traced_lot_once(llm, items, Attempt { current, total }, stop)
    })
}

fn traced_lot_once(
    llm: &dyn Llm,
    items: &[LotItem],
    attempt: Attempt,
    stop: &AtomicBool,
) -> Result<Vec<(String, Result<Enrichment>)>> {
    let label = format!("lot {} +{}", items[0].file, items.len() - 1);
    if !trace::enabled() {
        return llm.enrich_lot_labeled(&format!("{label} [1/1]"), items, stop);
    }
    trace!(
        "llm enrich départ {label} [1/1] {} car.{}",
        items.iter().map(|i| i.text.chars().count()).sum::<usize>(),
        attempt.mark()
    );
    let started = Instant::now();
    let result = llm.enrich_lot_labeled(&format!("{label} [1/1]"), items, stop);
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

/// Plan d'appels d'un fichier (DEC-059, DEC-061) : couples (indice du morceau, liste de nœuds
/// demandée), dans l'ordre d'envoi. Un seul morceau : la liste complète, découpée par
/// `node_groups`. Sinon, un nœud présent dans `homes` (identifiant → morceau de son titre, pour
/// les sections Markdown) va dans la seule liste de ce morceau ; tout autre nœud est demandé aux
/// morceaux où son nom court figure (mot entier, ou, pour un nom qui n'est pas un mot,
/// occurrence aux bords contrôlés ; sans casse pour un nœud `table:`, le SQL ne distinguant pas
/// les majuscules) ; un nœud de nom vide ou absent de tous les morceaux va dans le morceau 1. Au
/// moins un appel par morceau, liste vide permise.
fn call_plan(
    parts: &[String],
    nodes: &[(String, String)],
    homes: &HashMap<String, usize>,
) -> Vec<(usize, Vec<String>)> {
    let ids: Vec<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
    if parts.len() <= 1 {
        return node_groups(&ids)
            .into_iter()
            .map(|g| (0, g.to_vec()))
            .collect();
    }
    let words: Vec<WordIndex> = parts.iter().map(|p| WordIndex::new(p)).collect();
    // Textes en minuscules et leurs index, calculés une fois, seulement si un nœud `table:` existe.
    let lowered: Vec<String> = if nodes.iter().any(|(id, _)| id.starts_with("table:")) {
        parts.iter().map(|p| p.to_lowercase()).collect()
    } else {
        Vec::new()
    };
    let lower_words: Vec<WordIndex> = lowered.iter().map(|p| WordIndex::new(p)).collect();
    let mut lists: Vec<Vec<String>> = vec![Vec::new(); parts.len()];
    for (id, short) in nodes {
        if let Some(&k) = homes.get(id) {
            lists[k.min(parts.len() - 1)].push(id.clone());
            continue;
        }
        let mut found = false;
        if !short.is_empty() {
            let table = id.starts_with("table:");
            let short_lower = table.then(|| short.to_lowercase());
            let name = short_lower.as_deref().unwrap_or(short);
            let simple = name.chars().all(is_ident);
            for k in 0..parts.len() {
                let index = if table { &lower_words[k] } else { &words[k] };
                let hit = if simple {
                    index.has_word(name)
                } else {
                    index.contains_bounded(name)
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

/// Index d'un texte (DEC-107) : mot maximal d'`is_ident` → positions en octets de ses
/// occurrences, par ordre croissant. Calculé en un seul parcours.
struct WordIndex<'a> {
    text: &'a str,
    positions: HashMap<&'a str, Vec<usize>>,
}

impl<'a> WordIndex<'a> {
    fn new(text: &'a str) -> Self {
        let mut positions: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut start: Option<usize> = None;
        for (i, c) in text.char_indices() {
            match (is_ident(c), start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    positions.entry(&text[s..i]).or_default().push(s);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            positions.entry(&text[s..]).or_default().push(s);
        }
        WordIndex { text, positions }
    }

    /// Vrai si `word` est un mot maximal du texte.
    fn has_word(&self, word: &str) -> bool {
        self.positions.contains_key(word)
    }

    /// Équivalent de `contains_bounded(text, name)`. Chaque mot maximal du nom est un mot
    /// maximal de toute occurrence acceptée : les candidats sont les positions de celui des
    /// mots du nom qui en compte le moins (le premier à égalité), vérifiées à son écart dans
    /// le nom ; un mot absent de l'index exclut le nom. Sans caractère d'identifiant dans le
    /// nom, recherche directe.
    fn contains_bounded(&self, name: &str) -> bool {
        let mut best: Option<(usize, &Vec<usize>)> = None;
        let mut from = 0;
        while let Some(rel) = name[from..].find(is_ident) {
            let start = from + rel;
            let end = name[start..]
                .find(|c| !is_ident(c))
                .map_or(name.len(), |e| start + e);
            let Some(list) = self.positions.get(&name[start..end]) else {
                return false;
            };
            if best.is_none_or(|(_, b)| list.len() < b.len()) {
                best = Some((start, list));
            }
            from = end;
        }
        let Some((offset, candidates)) = best else {
            return contains_bounded(self.text, name);
        };
        candidates.iter().any(|&p| {
            p.checked_sub(offset)
                .and_then(|i| self.text.get(i..))
                .is_some_and(|rest| {
                    rest.starts_with(name) && bounds_ok(self.text, p - offset, name)
                })
        })
    }
}

/// Bords de l'occurrence de `name` à l'octet `i` de `text` (règle de `contains_bounded`).
fn bounds_ok(text: &str, i: usize, name: &str) -> bool {
    let first_ident = name.chars().next().is_some_and(is_ident);
    let last_ident = name.chars().next_back().is_some_and(is_ident);
    let before_ok = !first_ident || !text[..i].chars().next_back().is_some_and(is_ident);
    let after_ok = !last_ident || !text[i + name.len()..].chars().next().is_some_and(is_ident);
    before_ok && after_ok
}

/// Vrai si `name` (non vide) figure dans `text` avec des bords qui ne prolongent pas un mot : le
/// caractère qui précède (resp. suit) ne doit pas être `is_ident` quand le nom commence (resp.
/// finit) par un caractère `is_ident`. Utilisé pour un nom sans caractère d'identifiant.
fn contains_bounded(text: &str, name: &str) -> bool {
    text.match_indices(name)
        .any(|(i, _)| bounds_ok(text, i, name))
}

/// Morceau du titre de chaque section Markdown (DEC-061) : identifiant du nœud `section:` →
/// morceau qui contient la ligne de son titre. Vide pour un seul morceau ou sans nœud `section:`
/// (aucune analyse Markdown dans ces cas).
fn section_homes(
    text: &str,
    spans: &[(String, usize)],
    nodes: &[(String, String)],
) -> HashMap<String, usize> {
    let mut homes = HashMap::new();
    if spans.len() < 2 {
        return homes;
    }
    let by_anchor: HashMap<&str, &str> = nodes
        .iter()
        .filter(|(id, _)| id.starts_with("section:"))
        .map(|(id, short)| (short.as_str(), id.as_str()))
        .collect();
    if by_anchor.is_empty() {
        return homes;
    }
    for (anchor, position) in markdown::section_offsets(text) {
        if let Some(id) = by_anchor.get(anchor.as_str()) {
            let k = spans.partition_point(|(_, s)| *s <= position) - 1;
            homes.insert((*id).to_string(), k);
        }
    }
    homes
}

/// Appels d'un fichier envoyé seul selon `call_plan`, numérotés 1..n dans l'ordre du plan. Le
/// plafond de `MAX_CALLS_PER_FILE` est vérifié ici, avant qu'un seul appel soit mis en file
/// (REQ-048).
fn plan_calls<'a>(
    file: &'a FileRecord,
    text: &str,
    max_chunk_chars: usize,
    nodes: &[(String, String)],
) -> Result<Vec<Unit<'a>>> {
    let known: Arc<[String]> = nodes.iter().map(|(id, _)| id.clone()).collect();
    let spans = chunk::split_spans(text, max_chunk_chars);
    let homes = section_homes(text, &spans, nodes);
    let parts: Vec<String> = spans.into_iter().map(|(part, _)| part).collect();
    let plan = call_plan(&parts, nodes, &homes);
    let count = plan.len();
    if count > MAX_CALLS_PER_FILE {
        return Err(Error::TooManyCalls {
            calls: count,
            max: MAX_CALLS_PER_FILE,
        });
    }
    let shared: Vec<Arc<str>> = parts.iter().map(|p| Arc::from(p.as_str())).collect();
    Ok(plan
        .into_iter()
        .enumerate()
        .map(|(i, (k, requested))| Unit::Call {
            file,
            index: i + 1,
            count,
            part: Arc::clone(&shared[k]),
            requested,
            known: Arc::clone(&known),
        })
        .collect())
}

/// Un appel d'enrichissement avec ses nouveaux essais de format (REQ-038) ; le compte d'essais
/// repart à 1 pour chaque appel et seul l'appel fautif est renvoyé.
#[allow(clippy::too_many_arguments)]
fn run_call(
    llm: &dyn Llm,
    path: &str,
    index: usize,
    count: usize,
    part: &str,
    requested: &[String],
    known: &[String],
    retries: u32,
    stop: &AtomicBool,
) -> Result<Enrichment> {
    let total = retries.saturating_add(1);
    with_format_retries(retries, stop, |current| {
        traced_enrich(
            llm,
            path,
            index,
            count,
            part,
            requested,
            known,
            Attempt { current, total },
            stop,
        )
    })
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

/// Ce que les fils envoient au fil écrivain.
enum Msg<'a> {
    /// Résultats par fichier d'un lot, ou échecs d'un fichier envoyé seul.
    Outcomes(Vec<Outcome>),
    /// Résultat d'un appel d'un fichier envoyé seul (`index` à partir de 1).
    Chunk {
        file: &'a FileRecord,
        index: usize,
        count: usize,
        result: Result<Enrichment>,
    },
}

/// Résultats rangés par indice des appels d'un fichier envoyé seul, côté fil écrivain.
struct Partial {
    results: Vec<Option<Enrichment>>,
    done: usize,
    failed: bool,
}

/// File commune des fils (DEC-063), sous un seul verrou.
struct Shared<'a> {
    queue: VecDeque<Unit<'a>>,
    /// Fichiers dont un appel a échoué : leurs appels non partis sont sautés.
    failed: HashSet<String>,
    /// Unités qui peuvent encore ajouter des appels à la file (lecture et plan d'un fichier).
    expanding: usize,
}

/// Délai de réveil d'un fil qui attend des appels, pour voir l'arrêt posé hors de la file.
const IDLE_WAKE: Duration = Duration::from_millis(20);

/// Prend l'unité suivante ; attend quand la file est vide mais qu'une unité en cours peut encore
/// y ajouter des appels. `None` : plus rien à faire ou arrêt demandé.
fn next_unit<'a>(
    shared: &Mutex<Shared<'a>>,
    wake: &Condvar,
    stop: &AtomicBool,
) -> Option<Unit<'a>> {
    let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        match guard.queue.pop_front() {
            Some(Unit::Call { file, .. }) if guard.failed.contains(&file.path) => continue,
            Some(unit) => {
                if !matches!(unit, Unit::Call { .. }) {
                    guard.expanding += 1;
                }
                return Some(unit);
            }
            None if guard.expanding == 0 => return None,
            None => {
                guard = wake
                    .wait_timeout(guard, IDLE_WAKE)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
            }
        }
    }
}

/// Fin de l'expansion d'une unité : ses appels passent en tête de file, dans l'ordre du plan.
fn finish_unit<'a>(
    shared: &Mutex<Shared<'a>>,
    wake: &Condvar,
    calls: Vec<Unit<'a>>,
    ran_unit: bool,
) {
    let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
    for call in calls.into_iter().rev() {
        guard.queue.push_front(call);
    }
    if ran_unit {
        guard.expanding -= 1;
    }
    drop(guard);
    wake.notify_all();
}

/// Garde de l'expansion d'une unité : si le fil panique avant `finish_unit`, le compteur
/// `expanding` est tout de même décrémenté, pour que les autres fils ne l'attendent pas.
struct ExpandGuard<'s, 'a> {
    shared: &'s Mutex<Shared<'a>>,
    wake: &'s Condvar,
    armed: bool,
}

impl Drop for ExpandGuard<'_, '_> {
    fn drop(&mut self) {
        if self.armed {
            let mut guard = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.expanding = guard.expanding.saturating_sub(1);
            drop(guard);
            self.wake.notify_all();
        }
    }
}

/// Prépare l'écriture de l'enrichissement complet d'un fichier : avertissement des éléments
/// écartés (texte rangé dans `notes`, émis avec la progression du message), puis ajout au groupe
/// `pending` (écrit en une transaction par `flush_group`).
fn queue_done(
    file: &FileRecord,
    x: &Enrichment,
    succeeded: &mut usize,
    write_error: &Option<Error>,
    pending: &mut Vec<EnrichmentWrite>,
    notes: &mut Vec<String>,
) {
    if write_error.is_some() {
        return;
    }
    *succeeded += 1;
    if !x.rejected.is_empty() {
        notes.push(format!(
            "{} : {} élément(s) d'enrichissement écarté(s) : {}",
            file.path,
            x.rejected.len(),
            x.rejected.join(" ; ")
        ));
    }
    let (concepts, refs) = concept_records(x);
    pending.push(EnrichmentWrite {
        path: file.path.clone(),
        hash: file.hash.clone(),
        summaries: x.summaries.clone(),
        concepts,
        refs,
    });
}

/// Écrit le groupe de fichiers enrichis en une seule transaction (DEC-074) ; une erreur annule le
/// groupe (fichiers « à enrichir »), arrête l'envoi et est mémorisée.
fn flush_group(
    store: &mut Store,
    model: &str,
    pending: &mut Vec<EnrichmentWrite>,
    stop: &AtomicBool,
    write_error: &mut Option<Error>,
) {
    if pending.is_empty() {
        return;
    }
    if let Err(e) = store.apply_enrichments(model, pending) {
        stop.store(true, Ordering::SeqCst);
        *write_error = Some(e);
    }
    pending.clear();
}

/// Références partagées par les fils d'enrichissement.
#[derive(Clone, Copy)]
struct Ctx<'s, 'a> {
    shared: &'s Mutex<Shared<'a>>,
    wake: &'s Condvar,
    policy: &'s Mutex<Policy>,
    ids: &'s HashMap<String, Vec<(String, String)>>,
    llm: &'s dyn Llm,
    sources: Sources<'s>,
    max_chunk_chars: usize,
    format_retries: u32,
    stop: &'s AtomicBool,
}

/// Corps d'un fil d'enrichissement : prend des unités jusqu'à l'arrêt ou la fin de la file.
fn worker<'a>(ctx: Ctx<'_, 'a>, tx: mpsc::Sender<Msg<'a>>) {
    let Ctx {
        shared,
        wake,
        policy,
        ids,
        llm,
        sources,
        max_chunk_chars,
        format_retries,
        stop,
    } = ctx;
    while let Some(unit) = next_unit(shared, wake, stop) {
        let mut expand = ExpandGuard {
            shared,
            wake,
            armed: !matches!(unit, Unit::Call { .. }),
        };
        let ticket = policy.lock().unwrap_or_else(|p| p.into_inner()).dispatch();
        // Résultat de l'unité : l'erreur de l'appel pour la politique s'il y en a eu un.
        let (call, message, calls, expanded) = match unit {
            Unit::Call {
                file,
                index,
                count,
                part,
                requested,
                known,
            } => {
                let result = run_call(
                    llm,
                    &file.path,
                    index,
                    count,
                    &part,
                    &requested,
                    &known,
                    format_retries,
                    stop,
                );
                let err = result.as_ref().err().cloned();
                // Toute cause d'échec fait sauter les appels non partis du fichier
                // (AC-051-5) ; la politique enregistre quand même cet appel.
                if err.is_some() {
                    shared
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .failed
                        .insert(file.path.clone());
                }
                let result = result.map_err(truncation_advice);
                (
                    Some(err),
                    Some(Msg::Chunk {
                        file,
                        index,
                        count,
                        result,
                    }),
                    Vec::new(),
                    false,
                )
            }
            unit => {
                let step = run_unit(
                    &unit,
                    sources,
                    max_chunk_chars,
                    llm,
                    ids,
                    format_retries,
                    stop,
                );
                match step {
                    Step::Finished(outcomes, call) => (
                        call,
                        (!outcomes.is_empty()).then_some(Msg::Outcomes(outcomes)),
                        Vec::new(),
                        true,
                    ),
                    Step::Calls(outcomes, calls) => (
                        None,
                        (!outcomes.is_empty()).then_some(Msg::Outcomes(outcomes)),
                        calls,
                        true,
                    ),
                }
            }
        };
        expand.armed = false;
        finish_unit(shared, wake, calls, expanded);
        let halt = record_policy(policy, shared, ticket, &call);
        if halt {
            stop.store(true, Ordering::SeqCst);
            // Réveille les fils qui attendent des appels.
            drop(shared.lock().unwrap_or_else(|p| p.into_inner()));
            wake.notify_all();
        }
        if let Some(m) = message {
            if tx.send(m).is_err() {
                break;
            }
        }
    }
}

/// Enregistre l'appel d'une unité dans la politique d'arrêt (traces `politique: …` comprises) ;
/// rend `true` si l'envoi doit s'arrêter.
fn record_policy(
    policy: &Mutex<Policy>,
    shared: &Mutex<Shared<'_>>,
    ticket: Ticket,
    call: &Option<Option<Error>>,
) -> bool {
    let error = call.as_ref().and_then(|e| e.as_ref());
    let mut guard = policy.lock().unwrap_or_else(|p| p.into_inner());
    let was_stopped = guard.stop_cause.is_some();
    let halt = match call {
        Some(_) => guard.record(ticket, error),
        None => guard.stop_cause.is_some(),
    };
    if trace::enabled() {
        match error {
            Some(Error::Network(_)) => trace!(
                "politique: erreurs réseau consécutives {}/{MAX_CONSECUTIVE_NETWORK_ERRORS}",
                guard.consecutive_network
            ),
            Some(Error::Saturated(_)) => trace!(
                "politique: erreurs réseau consécutives {}/{MAX_CONSECUTIVE_NETWORK_ERRORS} (fournisseur saturé)",
                guard.consecutive_network
            ),
            Some(Error::Timeout(_)) => trace!(
                "politique: délais dépassés consécutifs {}/{MAX_CONSECUTIVE_NETWORK_ERRORS}",
                guard.consecutive_timeout
            ),
            _ => {}
        }
        if halt && !was_stopped {
            let left: usize = shared
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .queue
                .iter()
                .map(Unit::file_count)
                .sum();
            trace!(
                "politique: arrêt de l'envoi ({}), {left} fichiers non envoyés",
                guard.stop_label.as_deref().unwrap_or("")
            );
        }
    }
    halt
}

/// Traite un fichier en échec : avertissement (sauf clé ou requête refusée, E-12 et E-19 : l'erreur
/// finale la dit, aucun fichier n'est nommé), décompte et mémorisation.
fn record_failure(
    report: &mut BuildReport,
    failures: &mut Vec<(String, Error)>,
    notes: &mut Vec<String>,
    path: String,
    e: Error,
) {
    if !matches!(
        e,
        Error::InvalidKey(_) | Error::Rejected { .. } | Error::Redirected { .. }
    ) {
        notes.push(format!("{path} : {e}"));
    }
    report.failed += 1;
    failures.push((path, e));
}

/// Pool de threads LLM ; le thread courant écrit une transaction par groupe de messages reçus.
pub(crate) fn enrich_all(
    sources: Sources<'_>,
    files: &[FileRecord],
    opts: &IndexOptions,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    enrich_with_stop(
        sources,
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
    sources: Sources<'_>,
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
        enrich_code: _,
        jobs: _,
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
    // Un gros fichier peut donner jusqu'à `MAX_CALLS_PER_FILE` appels : autant de fils utiles.
    let useful: usize = units
        .iter()
        .map(|u| match u {
            Unit::Big(_) => MAX_CALLS_PER_FILE,
            _ => 1,
        })
        .sum();
    let workers = concurrency.clamp(1, useful.max(1));
    let shared = Mutex::new(Shared {
        queue: units.into(),
        failed: HashSet::new(),
        expanding: 0,
    });
    let wake = Condvar::new();
    let policy = Mutex::new(Policy::default());
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut write_error: Option<Error> = None;
    let mut failures: Vec<(String, Error)> = Vec::new();
    let mut partials: HashMap<String, Partial> = HashMap::new();
    trace!(
        "enrichissement: début {} fichiers, {workers} fils",
        files.len()
    );
    let mut meter = progress::Meter::start(Phase::Enrichment, files.len());

    std::thread::scope(|scope| {
        let ctx = Ctx {
            shared: &shared,
            wake: &wake,
            policy: &policy,
            ids: &ids,
            llm,
            sources,
            max_chunk_chars,
            format_retries,
            stop,
        };
        for _ in 0..workers {
            let tx = tx.clone();
            scope.spawn(move || worker(ctx, tx));
        }
        drop(tx);
        // Un message reçu, puis tous ceux déjà arrivés (sans attente), sont écrits en une seule
        // transaction avant d'attendre le suivant : aucune transaction ouverte pendant un appel.
        let mut pending: Vec<EnrichmentWrite> = Vec::new();
        while let Ok(first) = rx.recv() {
            // Avertissements et progression de chaque message, émis après la validation du groupe.
            let mut advances: Vec<(Vec<String>, usize)> = Vec::new();
            let mut next = Some(first);
            while let Some(msg) = next.take() {
                let mut adv = 0usize;
                let mut notes: Vec<String> = Vec::new();
                match msg {
                    Msg::Outcomes(batch) => {
                        adv = batch.len();
                        for outcome in batch {
                            match outcome {
                                Outcome::Failed(path, e) => {
                                    record_failure(report, &mut failures, &mut notes, path, e);
                                }
                                Outcome::Done(file, x) => {
                                    queue_done(
                                        &file,
                                        &x,
                                        &mut succeeded,
                                        &write_error,
                                        &mut pending,
                                        &mut notes,
                                    );
                                }
                            }
                        }
                    }
                    Msg::Chunk {
                        file,
                        index,
                        count,
                        result,
                    } => {
                        let entry = partials
                            .entry(file.path.clone())
                            .or_insert_with(|| Partial {
                                results: vec![None; count],
                                done: 0,
                                failed: false,
                            });
                        // Résultat d'un fichier déjà en échec : ignoré.
                        if !entry.failed {
                            match result {
                                Err(e) => {
                                    entry.failed = true;
                                    record_failure(
                                        report,
                                        &mut failures,
                                        &mut notes,
                                        file.path.clone(),
                                        e,
                                    );
                                    adv = 1;
                                }
                                Ok(x) => {
                                    entry.results[index - 1] = Some(x);
                                    entry.done += 1;
                                    if entry.done == count {
                                        let parts: Vec<Enrichment> = entry
                                            .results
                                            .iter_mut()
                                            .filter_map(Option::take)
                                            .collect();
                                        // Un seul appel : résultat tel quel, comme avant DEC-063.
                                        let merged = if count == 1 {
                                            parts.into_iter().next().unwrap_or_default()
                                        } else {
                                            chunk::merge_enrichments(parts)
                                        };
                                        queue_done(
                                            file,
                                            &merged,
                                            &mut succeeded,
                                            &write_error,
                                            &mut pending,
                                            &mut notes,
                                        );
                                        adv = 1;
                                    }
                                }
                            }
                        }
                    }
                }
                advances.push((notes, adv));
                next = rx.try_recv().ok();
            }
            flush_group(store, llm.model(), &mut pending, stop, &mut write_error);
            // Avertissements et progression de chaque message, dans l'ordre d'arrivée.
            for (notes, adv) in advances {
                for note in notes {
                    warn(&note);
                }
                if let Some(m) = meter.as_mut() {
                    m.advance(adv);
                }
            }
        }
    });
    trace!(
        "enrichissement: fin {} ms, {succeeded} enrichis, {} en échec",
        started.elapsed().as_millis(),
        report.failed - failed_before
    );
    let policy = policy.into_inner().unwrap_or_else(|p| p.into_inner());
    report.stop_cause = policy.stop_cause;
    report.refused = policy.refused;
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
                Unit::Call { .. } => unreachable!(),
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
            call_plan(&parts, &nodes, &HashMap::new()),
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
        let plan = call_plan(&parts[2..], &many, &HashMap::new());
        let expected: Vec<(usize, Vec<String>)> = node_groups(&all)
            .into_iter()
            .map(|g| (0, g.to_vec()))
            .collect();
        assert_eq!(plan, expected);
        assert_eq!(plan.len(), 2);

        // 160 nœuds tous dans le morceau 1, aucun dans le morceau 2.
        let text1: String = (0..160).map(|i| format!("n{i} ")).collect();
        let two = vec![text1, "rien".to_string()];
        let plan = call_plan(&two, &many, &HashMap::new());
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
        let plan = call_plan(&parts, &nodes, &HashMap::new());
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
        let plan = call_plan(&parts, &nodes, &HashMap::new());
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
    fn req047_ac16_plan_sections_par_position() {
        let parts = strs(&["le guide ici\n", "autre\n", "fin\n"]);
        let nodes = pairs(&[
            ("document:d.md", "d.md"),
            ("section:d.md#guide", "guide"),
            ("section:d.md#autre", "autre"),
            ("section:d.md#fin", "fin"),
            ("section:d.md#orpheline", "orpheline"),
            ("method:x", "guide"),
        ]);
        // `guide` : titre au morceau 2 alors que le mot figure au morceau 1.
        let homes: HashMap<String, usize> = [
            ("section:d.md#guide", 1),
            ("section:d.md#autre", 2),
            ("section:d.md#fin", 0),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), *b))
        .collect();
        assert_eq!(
            call_plan(&parts, &nodes, &homes),
            vec![
                (
                    0,
                    strs(&[
                        "document:d.md",
                        "section:d.md#fin",
                        "section:d.md#orpheline",
                        "method:x"
                    ])
                ),
                (1, strs(&["section:d.md#guide"])),
                (2, strs(&["section:d.md#autre"])),
            ]
        );
    }

    #[test]
    fn req047_ac16_plan_tables_sans_casse() {
        let parts = strs(&[
            "CREATE TABLE ORDERS (id INT);\n",
            "create table Public.Customers (o INT REFERENCES orders_archive(id));\n",
        ]);
        let nodes = pairs(&[
            ("table:orders", "orders"),
            ("table:orders_archive", "orders_archive"),
            ("table:public.customers", "customers"),
        ]);
        assert_eq!(
            call_plan(&parts, &nodes, &HashMap::new()),
            vec![
                (0, strs(&["table:orders"])),
                (1, strs(&["table:orders_archive", "table:public.customers"])),
            ]
        );
        let nodes = pairs(&[("method:p.C.order", "order")]);
        let plan = call_plan(&parts, &nodes, &HashMap::new());
        assert_eq!(plan[0], (0, strs(&["method:p.C.order"])));
        let parts = strs(&["Order\n", "rien\n"]);
        let nodes = pairs(&[("method:p.C.order", "order"), ("table:order", "order")]);
        let plan = call_plan(&parts, &nodes, &HashMap::new());
        assert_eq!(plan[0], (0, strs(&["method:p.C.order", "table:order"])));
        assert_eq!(plan[1], (1, vec![]));
    }

    #[test]
    fn req047_ac17_plan_de_10000_sections_rapide() {
        let mut text = String::new();
        let mut nodes = Vec::new();
        for i in 0..10_000 {
            text.push_str(&format!("## Titre numéro {i}\n\ncorps\n\n"));
            nodes.push((
                format!("section:d.md#titre-numéro-{i}"),
                format!("titre-numéro-{i}"),
            ));
        }
        let spans = chunk::split_spans(&text, text.chars().count() / 5 + 100);
        assert_eq!(spans.len(), 5);
        let homes = section_homes(&text, &spans, &nodes);
        assert_eq!(homes.len(), 10_000);
        let parts: Vec<String> = spans.iter().map(|(p, _)| p.clone()).collect();
        let plan = call_plan(&parts, &nodes, &homes);
        let total: usize = plan.iter().map(|(_, l)| l.len()).sum();
        assert_eq!(total, 10_000);
        let mut seen = HashSet::new();
        assert!(plan.iter().flat_map(|(_, l)| l).all(|id| seen.insert(id)));
        let expected: usize = (0..5)
            .map(|k| {
                let n: usize = plan
                    .iter()
                    .filter(|(p, _)| *p == k)
                    .map(|(_, l)| l.len())
                    .sum();
                n.div_ceil(MAX_NODES_PER_CALL).max(1)
            })
            .sum();
        assert_eq!(plan.len(), expected);
        assert!(plan.len() <= 10_001usize.div_ceil(150) + 5);
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

    #[test]
    fn req027_ac13_form_units_properties_on_pseudo_random_sets() {
        // Générateur congruentiel linéaire (graine fixe), sans dépendance.
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |bound: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % bound
        };
        for set in 0..1000 {
            let count = 1 + next(40) as usize;
            let sizes_cap = [50u64, 600, 5_000, 80_000][next(4) as usize];
            let files: Vec<FileRecord> = (0..count)
                .map(|i| rec(&format!("f{i:03}"), 1 + next(sizes_cap)))
                .collect();
            let node_counts: Vec<usize> = (0..count)
                .map(|_| match next(10) {
                    0 => 100 + next(150) as usize,
                    _ => next(60) as usize,
                })
                .collect();
            let nodes = |f: &FileRecord| node_counts[f.path[1..].parse::<usize>().unwrap()];
            for batch in [1usize, 10, 100] {
                for max in [100usize, 1_000, 60_000] {
                    let units = form_units_by_nodes(&files, nodes, batch, max);
                    let ctx = format!("jeu {set}, batch {batch}, max {max}");
                    let mut seen: Vec<&str> = Vec::new();
                    for u in &units {
                        match u {
                            Unit::Lot(v) => {
                                assert!(!v.is_empty(), "{ctx}");
                                assert!(v.len() <= batch, "{ctx}");
                                assert!(
                                    v.iter().map(|f| f.size).sum::<u64>() <= max as u64,
                                    "{ctx}"
                                );
                                assert!(v.iter().map(|f| nodes(f)).sum::<usize>() <= 150, "{ctx}");
                                seen.extend(v.iter().map(|f| f.path.as_str()));
                            }
                            Unit::Big(f) => {
                                assert!(
                                    f.size > max as u64 || nodes(f) > 150,
                                    "{ctx} : {} seul sans raison",
                                    f.path
                                );
                                seen.push(f.path.as_str());
                            }
                            Unit::Call { .. } => unreachable!(),
                        }
                    }
                    let expected: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
                    assert_eq!(seen, expected, "{ctx}");
                }
            }
        }
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

    /// Faux `Llm` qui compte ses appels d'extraction.
    struct Counting(std::sync::atomic::AtomicUsize);
    impl Llm for Counting {
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Enrichment::default())
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    /// Enchaîne les appels d'un fichier dans l'ordre, comme un seul fil, et fusionne.
    fn enrich_chunks(
        llm: &dyn Llm,
        path: &str,
        text: &str,
        max_chunk_chars: usize,
        nodes: &[(String, String)],
        retries: u32,
        stop: &AtomicBool,
    ) -> Result<Enrichment> {
        let file = rec(path, text.len() as u64);
        let mut results = Vec::new();
        for unit in plan_calls(&file, text, max_chunk_chars, nodes)? {
            let Unit::Call {
                index,
                count,
                part,
                requested,
                known,
                ..
            } = unit
            else {
                unreachable!()
            };
            results.push(run_call(
                llm, path, index, count, &part, &requested, &known, retries, stop,
            )?);
        }
        if results.len() == 1 {
            return Ok(results.remove(0));
        }
        Ok(chunk::merge_enrichments(results))
    }

    #[test]
    fn req048_ac1_plafond_de_50_appels() {
        let stop = AtomicBool::new(false);
        let run = |lines: usize| {
            let text = "aaaaaaaaa\n".repeat(lines);
            assert_eq!(chunk::split(&text, 10).len(), lines);
            let llm = Counting(std::sync::atomic::AtomicUsize::new(0));
            let r = enrich_chunks(&llm, "x", &text, 10, &[], 0, &stop);
            (r, llm.0.load(Ordering::SeqCst))
        };
        let (r, calls) = run(51);
        assert_eq!(calls, 0);
        let e = r.unwrap_err();
        assert_eq!(e, Error::TooManyCalls { calls: 51, max: 50 });
        assert_eq!(
            format!("avertissement: x : {e}"),
            "avertissement: x : trop d'appels d'enrichissement (51 nécessaires, au plus 50 par fichier), aucun envoyé ; excluez ce fichier avec exclude dans .nephila.toml"
        );
        let (r, calls) = run(50);
        assert!(r.is_ok());
        assert_eq!(calls, 50);
    }

    #[test]
    fn req048_ac1_plafond_markdown() {
        let stop = AtomicBool::new(false);
        let text: String = (0..51).map(|i| format!("# t{i}\n")).collect();
        assert_eq!(chunk::split(&text, 8).len(), 51);
        let nodes: Vec<(String, String)> = (0..51)
            .map(|i| (format!("section:x.md#t{i}"), format!("t{i}")))
            .collect();
        let spans = chunk::split_spans(&text, 8);
        let homes = section_homes(&text, &spans, &nodes);
        let parts: Vec<String> = spans.into_iter().map(|(p, _)| p).collect();
        for i in 0..51 {
            assert_eq!(homes[&format!("section:x.md#t{i}")], i);
        }
        let plan = call_plan(&parts, &nodes, &homes);
        assert_eq!(plan.len(), 51);
        for (i, (_, list)) in plan.iter().enumerate() {
            assert_eq!(list, &vec![format!("section:x.md#t{i}")]);
        }
        let llm = Counting(std::sync::atomic::AtomicUsize::new(0));
        let r = enrich_chunks(&llm, "x.md", &text, 8, &nodes, 0, &stop);
        assert_eq!(r.unwrap_err(), Error::TooManyCalls { calls: 51, max: 50 });
        assert_eq!(llm.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn req048_too_many_calls_neither_counts_nor_resets() {
        let many = Error::TooManyCalls { calls: 51, max: 50 };
        let mut p = Policy::default();
        for r in [Some(&net()), Some(&net()), Some(&many)] {
            let t = p.dispatch();
            assert!(!p.record(t, r));
        }
        assert_eq!((p.consecutive_network, p.network_total), (2, 2));
        let t = p.dispatch();
        assert!(p.record(t, Some(&net())));

        let mut p = Policy::default();
        for _ in 0..5 {
            let t = p.dispatch();
            assert!(!p.record(t, Some(&many)));
        }
        assert!(p.stop_cause.is_none());
    }

    fn sat() -> Error {
        Error::Saturated("saturé".into())
    }

    fn to() -> Error {
        Error::Timeout("délai".into())
    }

    fn run(p: &mut Policy, errors: &[Option<Error>]) -> Vec<bool> {
        errors
            .iter()
            .map(|e| {
                let t = p.dispatch();
                p.record(t, e.as_ref())
            })
            .collect()
    }

    #[test]
    fn req093_ac2_cause_follows_the_error_reaching_the_threshold() {
        let mut p = Policy::default();
        assert_eq!(
            run(&mut p, &[Some(net()), Some(net()), Some(sat())]),
            [false, false, true]
        );
        assert!(p
            .stop_cause
            .as_deref()
            .unwrap()
            .starts_with("fournisseur saturé : relancez nephila update plus tard"));
        assert_eq!(p.stop_label.as_deref(), Some("fournisseur saturé"));

        let mut p = Policy::default();
        assert_eq!(
            run(&mut p, &[Some(sat()), Some(sat()), Some(net())]),
            [false, false, true]
        );
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("panne réseau répétée (3 erreurs réseau consécutives)")
        );

        // Une réussite partie après remet le compte à zéro dans les deux cas.
        for last in [sat(), net()] {
            let mut p = Policy::default();
            let r = run(
                &mut p,
                &[Some(sat()), Some(net()), None, Some(sat()), Some(last)],
            );
            assert!(r.iter().all(|h| !h), "{r:?}");
            assert!(p.stop_cause.is_none());
        }
    }

    #[test]
    fn req094_ac4_three_timeouts_stop_with_their_own_cause() {
        let mut p = Policy::default();
        assert_eq!(
            run(&mut p, &[Some(to()), Some(to()), Some(to())]),
            [false, false, true]
        );
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("délais dépassés répétés : augmentez timeout_secs ou vérifiez le fournisseur")
        );
        assert_eq!(p.stop_label.as_deref(), Some("délais dépassés répétés"));
    }

    #[test]
    fn req094_ac4_network_error_neither_counts_nor_resets_timeouts() {
        let mut p = Policy::default();
        assert_eq!(
            run(&mut p, &[Some(to()), Some(to()), Some(net()), Some(to())]),
            [false, false, false, true]
        );
        assert!(p.stop_cause.unwrap().starts_with("délais dépassés répétés"));
    }

    #[test]
    fn req094_ac4_success_after_the_timeouts_resets_them() {
        let mut p = Policy::default();
        assert_eq!(
            run(&mut p, &[Some(to()), Some(to()), None, Some(to())]),
            [false, false, false, false]
        );
        assert!(p.stop_cause.is_none());
    }

    #[test]
    fn req094_ac4_success_dispatched_before_timeouts_does_not_reset_them() {
        let mut p = Policy::default();
        let a = p.dispatch();
        let (b, c) = (p.dispatch(), p.dispatch());
        assert!(!p.record(b, Some(&to())));
        assert!(!p.record(c, Some(&to())));
        assert!(!p.record(a, None));
        let d = p.dispatch();
        assert!(p.record(d, Some(&to())));
    }

    #[test]
    fn req094_ac4_counts_are_not_added_up() {
        let mut p = Policy::default();
        let r = run(&mut p, &[Some(net()), Some(to()), Some(net()), Some(to())]);
        assert!(r.iter().all(|h| !h));
        assert_eq!((p.consecutive_network, p.consecutive_timeout), (2, 2));
        assert!(p.stop_cause.is_none());
    }

    #[test]
    fn req010_ac9_timeouts_distinct_count_and_saturation_is_a_network_error() {
        let mut p = Policy::default();
        run(&mut p, &[Some(net()), Some(sat())]);
        assert_eq!((p.consecutive_network, p.network_total), (2, 2));
        assert_eq!(p.consecutive_timeout, 0);
    }

    #[test]
    fn req010_ac9_invalid_key_stops_at_first() {
        let mut p = Policy::default();
        let t = p.dispatch();
        assert!(p.record(t, Some(&Error::InvalidKey("k".into()))));
    }

    fn rejected() -> Error {
        Error::Rejected {
            message: "requête refusée par openai (HTTP 400) sur u ; v".into(),
            trace: "requête refusée par openai (HTTP 400)".into(),
        }
    }

    #[test]
    fn req091_ac5_rejected_stops_at_first_and_keeps_network_count() {
        let mut p = Policy::default();
        for _ in 0..2 {
            let t = p.dispatch();
            assert!(!p.record(t, Some(&net())));
        }
        let t = p.dispatch();
        assert!(p.record(t, Some(&rejected())));
        assert_eq!(p.consecutive_network, 2);
        assert_eq!(p.network_total, 2);
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("requête refusée par le fournisseur")
        );
        assert_eq!(p.refused, Some(rejected()));

        let mut p = Policy::default();
        assert!(p.record((0, 0), Some(&rejected())));
        assert_eq!(p.consecutive_network, 0);
    }

    #[test]
    fn req091_ac5_first_immediate_stop_wins() {
        let key = Error::InvalidKey("clé".into());
        let mut p = Policy::default();
        assert!(p.record((0, 0), Some(&key)));
        assert!(p.record((0, 0), Some(&rejected())));
        assert_eq!(p.refused, Some(key.clone()));
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("clé d'API refusée par le fournisseur")
        );

        let mut p = Policy::default();
        assert!(p.record((0, 0), Some(&rejected())));
        assert!(p.record((0, 0), Some(&key)));
        assert_eq!(p.refused, Some(rejected()));
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("requête refusée par le fournisseur")
        );
    }

    #[test]
    fn req092_redirect_stops_at_first_with_its_own_cause() {
        let r = Error::Redirected {
            message: "openai redirige (HTTP 302) sans destination ; corrigez base_url".into(),
            trace: "openai redirige (HTTP 302)".into(),
        };
        let mut p = Policy::default();
        assert!(p.record((0, 0), Some(&r)));
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("requête redirigée par le fournisseur")
        );
        assert_eq!(p.refused, Some(r.clone()));
        assert_eq!(trace_detail(&r), "openai redirige (HTTP 302)");
        assert_eq!(crate::trace::classify(&r), "requête redirigée");
    }

    #[test]
    fn req091_trace_detail_is_the_fixed_label() {
        assert_eq!(
            trace_detail(&rejected()),
            "requête refusée par openai (HTTP 400)"
        );
        assert_eq!(crate::trace::classify(&rejected()), "requête refusée");
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
    /// A garde son erreur `Format` et n'est pas réessayé. Ordre imposé par deux canaux : A
    /// signale « parti » à l'entrée de son premier appel, B attend ce signal avant de rendre
    /// `InvalidKey`. Sans cela, B pouvait poser l'arrêt avant que `A#1` soit pris, et `A#1`
    /// sautait (comportement voulu, DEC-063), laissant `calls_a` à 0.
    #[test]
    fn req038_ac10_invalid_key_on_other_thread_stops_retries() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc::{Receiver, Sender};
        use std::time::Duration;

        struct Fake<'a> {
            stop: &'a AtomicBool,
            rx: Mutex<Receiver<()>>,
            tx: Mutex<Sender<()>>,
            a_started_tx: Mutex<Sender<()>>,
            a_started_rx: Mutex<Receiver<()>>,
            calls_a: AtomicUsize,
        }
        impl Llm for Fake<'_> {
            fn enrich(
                &self,
                path: &str,
                _: usize,
                _: &str,
                _: &[String],
                _: &[String],
            ) -> Result<Enrichment> {
                if path.ends_with("B.java") {
                    self.a_started_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .expect("premier appel de A jamais parti");
                    self.tx.lock().unwrap().send(()).unwrap();
                    return Err(Error::InvalidKey("clé refusée".into()));
                }
                if self.calls_a.fetch_add(1, Ordering::SeqCst) == 0 {
                    self.a_started_tx.lock().unwrap().send(()).unwrap();
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
            jobs: 1,
            concurrency: 2,
            max_chunk_chars: 1000,
            batch_files: 1,
            format_retries: u32::MAX,
            enrich_code: false,
        };
        let stop = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        let (a_tx, a_rx) = mpsc::channel();
        let llm = Fake {
            stop: &stop,
            rx: Mutex::new(rx),
            tx: Mutex::new(tx),
            a_started_tx: Mutex::new(a_tx),
            a_started_rx: Mutex::new(a_rx),
            calls_a: AtomicUsize::new(0),
        };
        let mut failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
            },
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
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files: 4,
            format_retries: 0,
            enrich_code: false,
        };
        progress::init(true);
        progress::take_captured();
        enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
            },
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

    /// AC-096-4 : un lot tronqué est coupé en deux moitiés remises en tête de file.
    #[test]
    fn req096_ac4_lot_tronque_coupe_en_moities_dans_l_ordre() {
        struct Cut(Mutex<Vec<String>>);
        impl Llm for Cut {
            fn enrich(
                &self,
                file: &str,
                _: usize,
                _: &str,
                _: &[String],
                _: &[String],
            ) -> Result<Enrichment> {
                self.0
                    .lock()
                    .unwrap()
                    .push(file.trim_end_matches(".txt").into());
                Ok(Enrichment::default())
            }
            fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
                let names: String = items
                    .iter()
                    .map(|i| i.file.trim_end_matches(".txt"))
                    .collect();
                self.0.lock().unwrap().push(names);
                Err(Error::Truncated("tronquée".into()))
            }
            fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
                Err(Error::Message("inutilisé".into()))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for n in ["a", "b", "c", "d", "e", "f"] {
            let name = format!("{n}.txt");
            std::fs::write(dir.path().join(&name), "texte\n").unwrap();
            files.push(rec(&name, 6));
        }
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(&files).unwrap();
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files: 5,
            format_retries: 0,
            enrich_code: false,
        };
        let llm = Cut(Mutex::new(Vec::new()));
        progress::init(true);
        progress::take_captured();
        let failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
            },
            &files,
            &opts,
            &llm,
            &mut store,
            &mut report,
            &AtomicBool::new(false),
        )
        .unwrap();
        let lines = progress::take_captured();
        progress::init(false);
        assert_eq!(
            *llm.0.lock().unwrap(),
            ["abcde", "abc", "ab", "a", "b", "c", "de", "d", "e", "f"]
        );
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(lines.len(), 7, "{lines:?}");
        assert_eq!(
            lines.last().unwrap(),
            "progression: enrichissement 100 % (6/6 fichiers)"
        );
    }

    /// Faux `Llm` qui identifie chaque appel par fichier et indice (`"A#2"`, ou `"lot:S0,S1"`
    /// pour un lot), mesure les appels simultanés et force les `rendezvous` premiers appels à
    /// se rencontrer (AC-051-3, AC-051-5).
    struct Probe {
        now: std::sync::atomic::AtomicUsize,
        max: std::sync::atomic::AtomicUsize,
        arrivals: std::sync::atomic::AtomicUsize,
        rendezvous: usize,
        barrier: std::sync::Barrier,
        calls: Mutex<Vec<String>>,
        /// Appels (`"fichier#indice"`) en erreur réseau.
        network: Vec<String>,
    }

    impl Probe {
        fn new(rendezvous: usize, network: &[&str]) -> Probe {
            Probe {
                now: Default::default(),
                max: Default::default(),
                arrivals: Default::default(),
                rendezvous,
                barrier: std::sync::Barrier::new(rendezvous.max(1)),
                calls: Mutex::new(Vec::new()),
                network: network.iter().map(|s| s.to_string()).collect(),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn measured<T>(&self, f: impl FnOnce() -> T) -> T {
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(n, Ordering::SeqCst);
            // Les premiers appels attendent les autres : ils sont donc simultanés.
            if self.arrivals.fetch_add(1, Ordering::SeqCst) < self.rendezvous {
                self.barrier.wait();
            }
            let r = f();
            self.now.fetch_sub(1, Ordering::SeqCst);
            r
        }
    }

    impl Llm for Probe {
        fn enrich(
            &self,
            file: &str,
            call: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            let id = format!("{file}#{call}");
            self.calls.lock().unwrap().push(id.clone());
            self.measured(|| {
                if self.network.contains(&id) {
                    return Err(net());
                }
                let mut x = Enrichment::default();
                if file == "Big.txt" {
                    x.concepts.push(crate::llm::enrich::Concept {
                        name: "c".into(),
                        summary: Some(format!("s{call}")),
                        sections: vec![format!("section:x#{call}")],
                    });
                }
                Ok(x)
            })
        }
        fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            let names: Vec<&str> = items.iter().map(|i| i.file.as_str()).collect();
            self.calls
                .lock()
                .unwrap()
                .push(format!("lot:{}", names.join(",")));
            self.measured(|| {
                Ok(items
                    .iter()
                    .map(|i| (i.file.clone(), Ok(Enrichment::default())))
                    .collect())
            })
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    /// Dépôt temporaire : chaque gros fichier `(nom, lignes)` (lignes de 10 octets, un morceau
    /// par ligne avec `max_chunk_chars = 10`), puis `small` petits fichiers de 2 octets.
    fn probe_repo(big: &[(&str, usize)], small: usize) -> (tempfile::TempDir, Vec<FileRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for (name, lines) in big {
            std::fs::write(dir.path().join(name), "aaaaaaaaa\n".repeat(*lines)).unwrap();
            files.push(rec(name, 10 * *lines as u64));
        }
        for i in 0..small {
            let name = format!("S{i}.txt");
            std::fs::write(dir.path().join(&name), "x\n").unwrap();
            files.push(rec(&name, 2));
        }
        (dir, files)
    }

    fn probe_run(
        dir: &tempfile::TempDir,
        files: &[FileRecord],
        llm: &dyn Llm,
        concurrency: usize,
        batch_files: usize,
    ) -> (Store, BuildReport, Vec<(String, Error)>) {
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(files).unwrap();
        // Les sections que citent les concepts du faux `Llm` sont déclarées par `Big.txt`.
        let sections: Vec<Node> = (1..=8)
            .map(|k| {
                let name = format!("x#{k}");
                Node::new(NodeKind::Section, &name, &name, Some("Big.txt"), None, None)
            })
            .collect();
        if files.iter().any(|f| f.path == "Big.txt") {
            store.replace_structure("Big.txt", &sections, &[]).unwrap();
        }
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency,
            max_chunk_chars: 10,
            batch_files,
            format_retries: 0,
            enrich_code: false,
        };
        let failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
            },
            files,
            &opts,
            llm,
            &mut store,
            &mut report,
            &AtomicBool::new(false),
        )
        .unwrap();
        (store, report, failures)
    }

    fn names(failures: &[(String, Error)]) -> Vec<&str> {
        failures.iter().map(|(p, _)| p.as_str()).collect()
    }

    #[test]
    fn req051_ac3_appels_simultanes_bornes_par_concurrency() {
        for concurrency in [3usize, 1] {
            let (dir, files) = probe_repo(&[("Big.txt", 8)], 4);
            let llm = Probe::new(concurrency, &[]);
            let (store, report, failures) = probe_run(&dir, &files, &llm, concurrency, 2);
            assert!(failures.is_empty(), "{failures:?}");
            assert_eq!(report.failed, 0);
            assert_eq!(
                llm.max.load(Ordering::SeqCst),
                concurrency,
                "concurrency = {concurrency}"
            );
            let calls = llm.calls();
            let (lots, chunks): (Vec<_>, Vec<_>) =
                calls.iter().partition(|c| c.starts_with("lot:"));
            assert_eq!(lots.len(), 2, "deux appels de lot : {calls:?}");
            let mut seen: Vec<&str> = lots
                .iter()
                .flat_map(|l| l.trim_start_matches("lot:").split(','))
                .collect();
            seen.sort_unstable();
            assert_eq!(seen, ["S0.txt", "S1.txt", "S2.txt", "S3.txt"]);
            let mut chunks: Vec<String> = chunks.into_iter().cloned().collect();
            if concurrency == 1 {
                let expected: Vec<String> = (1..=8).map(|k| format!("Big.txt#{k}")).collect();
                assert_eq!(chunks, expected);
            }
            chunks.sort_unstable();
            let expected: Vec<String> = (1..=8).map(|k| format!("Big.txt#{k}")).collect();
            assert_eq!(chunks, expected, "chaque appel une fois");
            for i in 0..4 {
                let name = format!("S{i}.txt");
                assert_eq!(
                    store.enriched_hash(&name).unwrap(),
                    Some(String::new()),
                    "{name}"
                );
            }
            // Fusion attendue : premier résumé du concept (appel 1), sections des 8 appels.
            let concept = store.node_by_id("concept:c").unwrap().unwrap();
            assert_eq!(concept.summary.as_deref(), Some("s1"));
            let mut sections: Vec<String> = store
                .refs()
                .unwrap()
                .into_iter()
                .filter(|r| r.kind == EdgeKind::Mentions)
                .map(|r| r.source)
                .collect();
            sections.sort();
            let expected: Vec<String> = (1..=8).map(|k| format!("section:x#{k}")).collect();
            assert_eq!(sections, expected);
            assert_eq!(store.enriched_hash("Big.txt").unwrap(), Some(String::new()));
        }
    }

    /// Sans petits fichiers : seuls les morceaux d'un même gros fichier peuvent franchir la
    /// barrière (3 rendez-vous), ce qui prouve leur parallélisme (maximum exactement 3).
    #[test]
    fn req051_ac3_morceaux_d_un_gros_fichier_en_parallele_bornes_par_concurrency() {
        let (dir, files) = probe_repo(&[("Big.txt", 8)], 0);
        let llm = Probe::new(3, &[]);
        let (_store, report, failures) = probe_run(&dir, &files, &llm, 3, 2);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(llm.max.load(Ordering::SeqCst), 3);
        let mut calls = llm.calls();
        assert!(calls.iter().all(|c| c.starts_with("Big.txt#")), "{calls:?}");
        calls.sort_unstable();
        let expected: Vec<String> = (1..=8).map(|k| format!("Big.txt#{k}")).collect();
        assert_eq!(calls, expected);
    }

    #[test]
    fn req051_ac5_a_trois_erreurs_reseau_arretent_l_envoi() {
        let (dir, files) = probe_repo(&[("A", 2), ("B", 2), ("C", 2), ("D", 2)], 0);
        let llm = Probe::new(0, &["A#1", "A#2", "B#1", "B#2", "C#1", "C#2", "D#1", "D#2"]);
        let (store, report, failures) = probe_run(&dir, &files, &llm, 1, 1);
        assert_eq!(llm.calls(), ["A#1", "B#1", "C#1"]);
        assert!(report
            .stop_cause
            .as_deref()
            .is_some_and(|c| c.contains("panne réseau répétée")));
        assert_eq!(names(&failures), ["A", "B", "C"], "chacun nommé une fois");
        assert!(failures.iter().all(|(_, e)| matches!(e, Error::Network(_))));
        assert_eq!(report.failed, 3);
        for f in ["A", "B", "C", "D"] {
            assert_eq!(store.enriched_hash(f).unwrap(), None, "{f} à enrichir");
        }
    }

    #[test]
    fn req051_ac5_b_une_reussite_remet_le_compte_a_zero() {
        let (dir, files) = probe_repo(&[("A", 2), ("B", 2), ("C", 2), ("D", 2)], 0);
        let llm = Probe::new(0, &["A#1", "B#2", "C#1"]);
        let (store, report, failures) = probe_run(&dir, &files, &llm, 1, 1);
        assert_eq!(llm.calls(), ["A#1", "B#1", "B#2", "C#1", "D#1", "D#2"]);
        assert!(report.stop_cause.is_none(), "{:?}", report.stop_cause);
        assert_eq!(names(&failures), ["A", "B", "C"], "chacun nommé une fois");
        for f in ["A", "B", "C"] {
            assert_eq!(store.enriched_hash(f).unwrap(), None, "{f} à enrichir");
        }
        assert_eq!(store.enriched_hash("D").unwrap(), Some(String::new()));
    }

    #[test]
    fn req051_ac5_c_une_erreur_reseau_isolee_ne_fait_partir_que_le_premier_appel() {
        let (dir, files) = probe_repo(&[("Big.txt", 5)], 0);
        let llm = Probe::new(0, &["Big.txt#1"]);
        let (store, report, failures) = probe_run(&dir, &files, &llm, 1, 1);
        assert_eq!(llm.calls(), ["Big.txt#1"]);
        assert!(report.stop_cause.is_none(), "{:?}", report.stop_cause);
        assert_eq!(names(&failures), ["Big.txt"]);
        assert_eq!(report.failed, 1);
        assert_eq!(store.enriched_hash("Big.txt").unwrap(), None);
    }

    #[test]
    fn req051_echec_non_reseau_fait_sauter_les_appels_restants() {
        struct FailSecond(Mutex<Vec<usize>>);
        impl Llm for FailSecond {
            fn enrich(
                &self,
                _: &str,
                call: usize,
                _: &str,
                _: &[String],
                _: &[String],
            ) -> Result<Enrichment> {
                self.0.lock().unwrap().push(call);
                if call == 2 {
                    return Err(Error::Format("non conforme".into()));
                }
                Ok(Enrichment::default())
            }
            fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
                Err(Error::Message("inutilisé".into()))
            }
            fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
                Err(Error::Message("inutilisé".into()))
            }
        }
        let (dir, files) = probe_repo(&[("Big.txt", 4)], 0);
        let llm = FailSecond(Mutex::new(Vec::new()));
        let (_, report, failures) = probe_run(&dir, &files, &llm, 1, 1);
        assert_eq!(*llm.0.lock().unwrap(), [1, 2]);
        assert_eq!(failures.len(), 1);
        assert_eq!(report.failed, 1);
    }
}

/// REQ-106 : le plan d'appels indexé par mots donne celui de l'oracle (code de la 0.24.0).
#[cfg(test)]
mod plan_index_tests {
    use super::*;

    fn oracle_ident_words(text: &str) -> HashSet<&str> {
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

    fn oracle_contains_bounded(text: &str, name: &str) -> bool {
        let first_ident = name.chars().next().is_some_and(is_ident);
        let last_ident = name.chars().next_back().is_some_and(is_ident);
        text.match_indices(name).any(|(i, m)| {
            let before_ok = !first_ident || !text[..i].chars().next_back().is_some_and(is_ident);
            let after_ok = !last_ident || !text[i + m.len()..].chars().next().is_some_and(is_ident);
            before_ok && after_ok
        })
    }

    fn oracle_call_plan(
        parts: &[String],
        nodes: &[(String, String)],
        homes: &HashMap<String, usize>,
    ) -> Vec<(usize, Vec<String>)> {
        let ids: Vec<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
        if parts.len() <= 1 {
            return node_groups(&ids)
                .into_iter()
                .map(|g| (0, g.to_vec()))
                .collect();
        }
        let words: Vec<HashSet<&str>> = parts.iter().map(|p| oracle_ident_words(p)).collect();
        let lowered: Vec<String> = if nodes.iter().any(|(id, _)| id.starts_with("table:")) {
            parts.iter().map(|p| p.to_lowercase()).collect()
        } else {
            Vec::new()
        };
        let lower_words: Vec<HashSet<&str>> =
            lowered.iter().map(|p| oracle_ident_words(p)).collect();
        let mut lists: Vec<Vec<String>> = vec![Vec::new(); parts.len()];
        for (id, short) in nodes {
            if let Some(&k) = homes.get(id) {
                lists[k.min(parts.len() - 1)].push(id.clone());
                continue;
            }
            let mut found = false;
            if !short.is_empty() {
                let table = id.starts_with("table:");
                let short_lower = table.then(|| short.to_lowercase());
                let name = short_lower.as_deref().unwrap_or(short);
                let simple = name.chars().all(is_ident);
                for (k, part) in parts.iter().enumerate() {
                    let hit = match (simple, table) {
                        (true, true) => lower_words[k].contains(name),
                        (true, false) => words[k].contains(name),
                        (false, true) => oracle_contains_bounded(&lowered[k], name),
                        (false, false) => oracle_contains_bounded(part, name),
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

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    const ALPHABET: &[&str] = &[
        "a", "b", "s", "l", "f", "4", "j", "A", "B", "É", "é", "ß", "中", "文", "😀", "ж", "0",
        "1", "_", "$", "-", ".", ":", "/", "@", "'", " ", "\n", "-", ".", "a", "b",
    ];
    const SEPARATORS: &[&str] = &["-", ".", ":", "/", "@", "'", " ", "\n"];

    fn random_string(rng: &mut Rng, max: usize) -> String {
        (0..rng.below(max) + 1)
            .map(|_| ALPHABET[rng.below(ALPHABET.len())])
            .collect()
    }

    fn random_name(rng: &mut Rng) -> String {
        match rng.below(5) {
            0 => random_string(rng, 4),
            1 => format!(
                "{}{}{}",
                random_string(rng, 3),
                SEPARATORS[rng.below(SEPARATORS.len())],
                random_string(rng, 3)
            ),
            2 => format!(
                "{}{}",
                SEPARATORS[rng.below(SEPARATORS.len())],
                random_string(rng, 3)
            ),
            3 => format!(
                "{}{}",
                random_string(rng, 3),
                SEPARATORS[rng.below(SEPARATORS.len())]
            ),
            _ => SEPARATORS[rng.below(SEPARATORS.len())].repeat(rng.below(2) + 1),
        }
    }

    #[test]
    fn req106_ac1_plan_equal_to_oracle() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut with_non_word_hit = 0;
        for case in 0..3000 {
            let n_parts = rng.below(4) + 1;
            let mut parts: Vec<String> =
                (0..n_parts).map(|_| random_string(&mut rng, 40)).collect();
            let n_nodes = rng.below(8) + 1;
            let mut nodes: Vec<(String, String)> = Vec::new();
            for i in 0..n_nodes {
                let mut name = random_name(&mut rng);
                if rng.below(3) > 0 {
                    // le nom est cité dans un morceau, entre deux caractères tirés au hasard
                    let k = rng.below(parts.len());
                    let (l, r) = (random_string(&mut rng, 2), random_string(&mut rng, 2));
                    parts[k] = format!("{l}{name}{r}{}", parts[k]);
                }
                if rng.below(6) == 0 {
                    name = name.to_uppercase();
                }
                let kind = ["class", "table", "dep", "section"][rng.below(4)];
                nodes.push((format!("{kind}:n{i}"), name));
                if rng.below(5) == 0 {
                    let dup = nodes[i].clone();
                    nodes.push((format!("{}d", dup.0), dup.1));
                }
            }
            let mut homes = HashMap::new();
            for (id, _) in &nodes {
                if id.starts_with("section:") && rng.below(2) == 0 {
                    homes.insert(id.clone(), rng.below(parts.len() + 1));
                }
            }
            if parts.len() > 1 {
                for (id, short) in &nodes {
                    let name = if id.starts_with("table:") {
                        short.to_lowercase()
                    } else {
                        short.clone()
                    };
                    if !name.is_empty()
                        && !name.chars().all(is_ident)
                        && name.chars().any(is_ident)
                        && parts.iter().any(|p| {
                            let p = if id.starts_with("table:") {
                                p.to_lowercase()
                            } else {
                                p.clone()
                            };
                            oracle_contains_bounded(&p, &name)
                        })
                    {
                        with_non_word_hit += 1;
                        break;
                    }
                }
            }
            assert_eq!(
                call_plan(&parts, &nodes, &homes),
                oracle_call_plan(&parts, &nodes, &homes),
                "cas {case} : {parts:?} {nodes:?}"
            );
        }
        assert!(
            with_non_word_hit >= 500,
            "seulement {with_non_word_hit} cas à nom non-mot présent"
        );
    }

    fn plan_for(parts: &[&str], nodes: &[(&str, &str)]) -> Vec<(usize, Vec<String>)> {
        let parts: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
        let nodes: Vec<(String, String)> = nodes
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let got = call_plan(&parts, &nodes, &HashMap::new());
        assert_eq!(got, oracle_call_plan(&parts, &nodes, &HashMap::new()));
        got
    }

    #[test]
    fn req106_ac2_written_cases() {
        let parts = [
            "<artifactId>slf4j-api</artifactId>",
            "slf4j-api-ext",
            "slf4j-api.jar",
            "xslf4j-api",
            "slf4j-api2",
            "\"@scope/pkg\":",
            "\"@scope/pkgx\"",
        ];
        let plan = plan_for(&parts, &[("dep:a", "slf4j-api"), ("dep:b", "@scope/pkg")]);
        let at = |k: usize| -> Vec<String> {
            plan.iter()
                .filter(|(i, _)| *i == k)
                .flat_map(|(_, l)| l.clone())
                .collect()
        };
        assert_eq!(at(0), ["dep:a"]);
        assert_eq!(at(1), ["dep:a"]);
        assert_eq!(at(2), ["dep:a"]);
        assert!(at(3).is_empty() && at(4).is_empty());
        assert_eq!(at(5), ["dep:b"]);
        assert!(at(6).is_empty());
        // nœud `table:` : texte en minuscules
        let plan = plan_for(
            &["x", "SELECT * FROM \"My-Table\" ;", "my-table2"],
            &[("table:t", "My-Table")],
        );
        assert_eq!(
            plan,
            vec![(0, vec![]), (1, vec!["table:t".to_string()]), (2, vec![])]
        );
    }

    #[test]
    fn req106_ac3_cost() {
        let nodes: Vec<(String, String)> = (0..20_000)
            .map(|i| (format!("dep:{i}"), format!("dep-{i}")))
            .collect();
        let parts: Vec<String> = (0..5)
            .map(|k| {
                let mut s = String::with_capacity(100_000);
                let mut i = k;
                while s.len() < 100_000 {
                    s.push_str(&format!("compile 'g:dep-{}:1.0' // x\n", i * 7 % 20_000));
                    i += 1;
                }
                s
            })
            .collect();
        let plan = call_plan(&parts, &nodes, &HashMap::new());
        let mut lists: Vec<usize> = vec![0; 5];
        for (k, l) in &plan {
            lists[*k] += l.len();
        }
        let expected: usize = lists.iter().map(|n| (*n).div_ceil(150).max(1)).sum();
        assert_eq!(plan.len(), expected);
        assert!(lists.iter().sum::<usize>() >= 20_000);
    }
}
