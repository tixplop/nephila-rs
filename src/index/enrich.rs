//! Phase enrichissement (DEC-019, REQ-024) : résumés et concepts demandés au LLM pour les
//! fichiers « à enrichir », avec la politique d'échec de DEC-015 / DEC-022.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::chunk;
use crate::error::{warn, Error, Result, TRUNCATED};
use crate::excerpt;
use crate::index::build::{coverage_segment, BuildReport, IndexOptions};
use crate::index::estimate::{AloneCase, AloneFile, Estimate};
use crate::llm::enrich::PromptVersions;
use crate::llm::enrich::{
    enrichment_system_prompt, enrichment_user_prompt, lot_system_prompt, lot_user_prompt,
    parse_lot_partial,
};
use crate::llm::{
    calls_line, tokens_segment, with_format_retries, CallStats, Enrichment, Excerpt, FormatFault,
    Llm, LlmTotals, LotItem,
};
use crate::model::{EdgeKind, FileRecord, Node, NodeKind, Ref};
use crate::parse::markdown;
use crate::progress::{self, Phase};
use crate::store::{call_key, CallKeyParts, CallWrite, EnrichmentWrite, Store};
use crate::trace;

/// Seuil des erreurs réseau consécutives (saturations comprises) et des délais dépassés
/// consécutifs, chacun compté à part, qui arrête l'envoi (DEC-015, DEC-099).
pub(crate) const MAX_CONSECUTIVE_NETWORK_ERRORS: usize = 3;

/// Cause d'arrêt à seuil : libellé de la trace et cause de la ligne `erreur:` (une seule table).
pub(crate) struct ThresholdCause {
    pub(crate) label: &'static str,
    pub(crate) cause: &'static str,
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
/// Arrêt demandé par Ctrl-C (REQ-189, REQ-191) : seule source du libellé et de la cause.
pub(crate) const CAUSE_INTERRUPT: ThresholdCause = ThresholdCause {
    label: "Ctrl-C reçu",
    cause: "Ctrl-C reçu : relancez nephila update pour continuer",
};

/// Annonce A-12 (sans le préfixe `avertissement: `) : seule source du texte.
pub(crate) const ANNOUNCE: &str = "Ctrl-C reçu : plus aucun appel ne part, ceux en cours vont à leur terme et seront enregistrés ; appuyez de nouveau sur Ctrl-C pour quitter sans les attendre";

/// Politique d'échec partagée entre les threads de travail (DEC-015).
#[derive(Default)]
pub(crate) struct Policy {
    consecutive_network: usize,
    network_total: usize,
    consecutive_timeout: usize,
    timeout_total: usize,
    /// Cause de la ligne `erreur:` finale.
    pub(crate) stop_cause: Option<String>,
    /// Libellé de la cause dans la trace `politique: arrêt de l'envoi`.
    pub(crate) stop_label: Option<String>,
    /// Première erreur d'arrêt immédiat (clé refusée REQ-054, requête refusée REQ-091), si elle
    /// est la cause de l'arrêt.
    pub(crate) refused: Option<Error>,
    /// Appels (essais) encore permis par le plafond de la commande (REQ-186) ; `None` sans plafond.
    budget: Option<usize>,
    /// Valeur du plafond, pour son libellé ; `None` sans plafond.
    pub(crate) limit: Option<usize>,
    /// Vrai dès qu'une prise a été refusée faute de compte. N'écrit jamais `stop_cause`,
    /// `stop_label` ni `refused` : le plafond n'est pas une erreur (REQ-187).
    pub(crate) capped: bool,
    /// Vrai si l'arrêt demandé par Ctrl-C est la cause de l'arrêt (REQ-191).
    pub(crate) interrupted: bool,
    /// Vrai pour la passe des sous-systèmes : la commande à relancer porte `--subsystems`.
    pub(crate) subsystems: bool,
}

/// Libellé du plafond atteint : seule source du texte, pour la trace `politique:` et pour A-10.
pub(crate) fn cap_label(max: usize) -> String {
    format!("plafond de {max} appels atteint")
}

/// Totaux des erreurs réseau et des délais dépassés constatés au départ d'un appel (DEC-022).
pub(crate) type Ticket = (usize, usize);

impl Policy {
    /// Politique d'une commande dont le plafond d'appels est `max_calls`.
    pub(crate) fn with_cap(max_calls: Option<usize>) -> Policy {
        Policy {
            budget: max_calls,
            limit: max_calls,
            ..Policy::default()
        }
    }

    /// Prend `n` appels sur le plafond (sous le verrou de la politique). Sans plafond : vrai.
    /// Reste suffisant : le décompte est fait. Sinon le plafond est marqué atteint, le reste
    /// n'est pas touché et la prise est refusée.
    pub(crate) fn take(&mut self, n: usize) -> bool {
        match self.budget {
            None => true,
            Some(left) if left >= n => {
                self.budget = Some(left - n);
                true
            }
            Some(_) => {
                self.capped = true;
                false
            }
        }
    }

    /// Prend un ticket au départ d'un appel (sous le verrou de la politique).
    pub(crate) fn dispatch(&self) -> Ticket {
        (self.network_total, self.timeout_total)
    }

    fn stop_with(&mut self, label: &str, cause: &str) {
        if self.stop_cause.is_none() {
            self.stop_label = Some(label.to_string());
            self.stop_cause = Some(if self.subsystems {
                cause.replace("nephila update", "nephila update --subsystems")
            } else {
                cause.to_string()
            });
        }
    }

    /// Arrêt demandé par Ctrl-C : pose la cause si aucune n'est posée et rend vrai ; sinon ne
    /// change rien et rend faux. N'écrit ni `capped` ni `refused`.
    pub(crate) fn request_stop(&mut self) -> bool {
        if self.stop_cause.is_some() {
            return false;
        }
        self.stop_with(CAUSE_INTERRUPT.label, CAUSE_INTERRUPT.cause);
        self.interrupted = true;
        true
    }

    /// Met à jour la politique ; renvoie vrai si l'envoi de nouveaux fichiers doit s'arrêter.
    /// Une réussite n'efface que les échecs constatés avant le départ de son appel (DEC-022).
    pub(crate) fn record(&mut self, ticket: Ticket, result: Option<&Error>) -> bool {
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

/// Passage d'un lot (DEC-161) : un fichier absent d'une réponse conforme n'est renvoyé qu'une
/// fois, par un lot de second passage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    First,
    Second,
}

/// Unité de travail d'un fil (DEC-063) : un lot de fichiers envoyés ensemble, un gros fichier
/// envoyé seul (REQ-027) qui se transforme en appels dans la file commune, ou un de ces appels
/// (morceau × groupe de nœuds, REQ-047).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unit<'a> {
    Lot(Vec<&'a FileRecord>, Pass),
    Big(&'a FileRecord),
    Call {
        file: &'a FileRecord,
        /// Rang de l'appel dans le plan, à partir de 1.
        index: usize,
        count: usize,
        /// Morceau envoyé : `None` quand le fichier tient en un seul morceau (REQ-169).
        excerpt: Option<Excerpt>,
        part: Arc<str>,
        requested: Vec<String>,
        /// Liste complète des nœuds du fichier, qui filtre la réponse.
        known: Arc<[String]>,
        /// Rangs (à partir de 1) des coupes successives qui ont donné ce sous-appel ; vide pour
        /// un appel du plan (REQ-230).
        path: Vec<usize>,
        /// Le contenu du morceau porte `cache_control` (REQ-268).
        cache: bool,
        /// Appels marqués du même morceau, qui partent à la fin de celui-ci (REQ-269) ; vide
        /// pour tout autre appel.
        followers: Vec<Unit<'a>>,
    },
}

impl Unit<'_> {
    /// Fichiers pas encore commencés que cette unité représente.
    fn file_count(&self) -> usize {
        match self {
            Unit::Lot(v, _) => v.len(),
            Unit::Big(_) => 1,
            Unit::Call { .. } => 0,
        }
    }
}

/// Nombre maximal de nœuds demandés par appel d'enrichissement (DEC-058) : environ 40 jetons par
/// résumé, soit environ 6 000 jetons, sous la limite par défaut
/// (`DEFAULT_MAX_OUTPUT_TOKENS`) ; un `max_output_tokens` plus bas configuré peut tronquer la réponse.
const MAX_NODES_PER_CALL: usize = 150;

/// Profondeur maximale de la découpe d'un appel tronqué (REQ-230, non configurable) : au pire
/// 4 appels d'affilée.
const MAX_SPLIT_DEPTH: usize = 3;

/// Taille minimale, en caractères, du texte d'un appel tronqué pour qu'il soit coupé (REQ-230,
/// non configurable) : en dessous, le texte n'explique pas la troncature.
const MIN_SPLIT_CHARS: usize = 1000;

/// Issue d'un appel tronqué dont on a lu les éléments complets (REQ-232).
#[derive(Debug)]
enum Recovery<'a> {
    /// Tout ce qui était demandé est reçu : le résultat de l'appel.
    All(Enrichment),
    /// Résumés reçus, et sous-appel pour les nœuds restants.
    Rest(Enrichment, Unit<'a>),
    /// Rien d'utile : la découpe de REQ-230 s'applique.
    Nothing,
}

/// Garde les éléments complets `kept` d'un appel tronqué et réduit l'appel au reste (REQ-232,
/// DEC-175). Liste demandée vide : `All` avec au moins un concept, sinon `Nothing`. Aucun nœud
/// demandé reçu : `Nothing`.
fn recover_call<'a>(unit: &Unit<'a>, kept: Enrichment) -> Recovery<'a> {
    let Unit::Call {
        file,
        index,
        count,
        excerpt,
        part,
        requested,
        known,
        path,
        cache,
        ..
    } = unit
    else {
        return Recovery::Nothing;
    };
    if requested.is_empty() {
        return if kept.concepts.is_empty() {
            Recovery::Nothing
        } else {
            Recovery::All(kept)
        };
    }
    let got: HashSet<&str> = kept.summaries.iter().map(|(n, _)| n.as_str()).collect();
    let rest: Vec<String> = requested
        .iter()
        .filter(|r| !got.contains(r.as_str()))
        .cloned()
        .collect();
    if rest.len() == requested.len() {
        return Recovery::Nothing;
    }
    if rest.is_empty() {
        return Recovery::All(kept);
    }
    let mut path = path.clone();
    path.push(1);
    let sub = Unit::Call {
        file,
        index: *index,
        count: *count,
        excerpt: *excerpt,
        part: Arc::clone(part),
        requested: rest,
        known: Arc::clone(known),
        path,
        cache: *cache,
        followers: Vec::new(),
    };
    Recovery::Rest(kept, sub)
}

/// Découpe un appel tronqué (REQ-230). Non Markdown : la liste de nœuds en deux (les ⌈n/2⌉
/// premiers puis les autres) sur le même texte pour 2 nœuds ou plus, sinon le texte. Markdown :
/// le texte, sinon la liste. `nodes` : `(identifiant, nom court)` des nœuds demandés. `None` si
/// aucune coupe n'est possible ou à la profondeur maximale.
fn split_call<'a>(unit: &Unit<'a>, nodes: &[(String, String)]) -> Option<Vec<Unit<'a>>> {
    let Unit::Call {
        file,
        index,
        count,
        excerpt,
        part,
        requested,
        known,
        path,
        cache,
        ..
    } = unit
    else {
        return None;
    };
    if path.len() >= MAX_SPLIT_DEPTH {
        return None;
    }
    let sub = |i: usize,
               excerpt: Option<Excerpt>,
               part: Arc<str>,
               requested: Vec<String>,
               cache: bool| {
        let mut path = path.clone();
        path.push(i + 1);
        Unit::Call {
            file,
            index: *index,
            count: *count,
            excerpt,
            part,
            requested,
            known: Arc::clone(known),
            path,
            cache,
            followers: Vec::new(),
        }
    };
    let by_list = || {
        let (first, second) = requested.split_at(requested.len().div_ceil(2));
        Some(
            [first, second]
                .into_iter()
                .enumerate()
                .map(|(i, list)| sub(i, *excerpt, Arc::clone(part), list.to_vec(), *cache))
                .collect(),
        )
    };
    let by_text = || {
        let c = part.chars().count();
        if c < MIN_SPLIT_CHARS {
            return None;
        }
        let spans = chunk::split_spans_for(&file.path, part, c.div_ceil(2));
        if spans.len() < 2 {
            return None;
        }
        let homes = section_homes(part, &spans, nodes);
        let excerpts = excerpts_of(part, &spans);
        let parts: Vec<String> = spans.into_iter().map(|(p, _)| p).collect();
        let mut plan = call_plan(&parts, nodes, &homes);
        retain_calls(&mut plan, &file.path, known);
        if plan.is_empty() {
            return None;
        }
        // En-tête répété du parent : lignes du texte hors lignes réelles de son extrait.
        let (base, header) = match excerpt {
            Some(e) => (
                e.first_line,
                part.lines()
                    .count()
                    .saturating_sub(e.last_line - e.first_line + 1),
            ),
            None => (1, 0),
        };
        let real = |rel: usize| (base + rel).saturating_sub(1 + header).max(base);
        plan.into_iter()
            .enumerate()
            .map(|(i, (k, list))| {
                let own = excerpts[k]?;
                let (rank, total) = excerpt.map_or((own.rank, own.total), |e| (e.rank, e.total));
                let first_line = real(own.first_line);
                let e = Excerpt {
                    rank,
                    total,
                    first_line,
                    last_line: real(own.last_line).max(first_line),
                };
                Some(sub(i, Some(e), Arc::from(parts[k].as_str()), list, false))
            })
            .collect::<Option<Vec<_>>>()
    };
    let markdown = crate::llm::enrich::is_markdown(&file.path);
    let list_ok = requested.len() >= 2;
    if markdown {
        by_text().or_else(|| if list_ok { by_list() } else { None })
    } else if list_ok {
        by_list()
    } else {
        by_text()
    }
}

/// Retire d'un plan d'appels ceux dont la liste de nœuds est vide, sauf pour un Markdown dont le
/// fichier a au moins une section (`known` : tous les nœuds du fichier, REQ-159) : un morceau de
/// suite peut porter des concepts citant des sections connues.
fn retain_calls(plan: &mut Vec<(usize, Vec<String>)>, path: &str, known: &[String]) {
    let keep_empty =
        crate::llm::enrich::is_markdown(path) && known.iter().any(|id| id.starts_with("section:"));
    if !keep_empty {
        plan.retain(|(_, list)| !list.is_empty());
    }
}

/// Nombre maximal d'appels d'enrichissement pour un fichier envoyé seul (DEC-059, non
/// configurable) : 50 appels = 7 500 nœuds d'un seul morceau. Au-delà, aucun appel n'est envoyé.
pub(crate) const MAX_CALLS_PER_FILE: usize = 50;

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
                units.push(Unit::Lot(std::mem::take(&mut current), Pass::First));
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
            units.push(Unit::Lot(std::mem::take(&mut current), Pass::First));
            total = 0;
            total_nodes = 0;
        }
        total += f.size;
        total_nodes += n;
        current.push(f);
    }
    if !current.is_empty() {
        units.push(Unit::Lot(current, Pass::First));
    }
    units
}

/// Prise sur le plafond d'appels : `take(n, files)` réserve `n` appels ; `files` est le nombre de
/// fichiers de l'unité en cours qui ne sont pas encore partis (compté dans la trace du refus).
type Take<'t> = &'t dyn Fn(usize, usize) -> bool;

/// Appels conservés lus au début de la passe (REQ-226) : partagés en lecture seule par les fils,
/// qui n'ont pas accès au `Store`.
pub(crate) struct KeptCalls {
    pub(crate) results: HashMap<String, Enrichment>,
    pub(crate) model: String,
    pub(crate) language: String,
    pub(crate) versions: PromptVersions,
}

impl KeptCalls {
    /// Clé d'un appel du plan d'un fichier envoyé seul.
    #[allow(clippy::too_many_arguments)]
    fn key(
        &self,
        path: &str,
        index: usize,
        count: usize,
        excerpt: Option<Excerpt>,
        part: &str,
        requested: &[String],
        known: &[String],
    ) -> String {
        call_key(&CallKeyParts {
            model: &self.model,
            language: &self.language,
            prompt_version: self.versions.of_path(path),
            path,
            rank: index,
            total: count,
            lines: excerpt.map(|e| (e.first_line, e.last_line)),
            text: part,
            requested,
            known,
        })
    }
}

/// Suite d'une unité qui n'est pas un appel.
enum Step<'a> {
    /// Fichier envoyé seul dont des appels sont conservés : le message des résultats conservés,
    /// puis les appels à envoyer (peut-être aucun).
    Resumed(Msg<'a>, Vec<Unit<'a>>),
    /// Résultats par fichier, et l'erreur de l'appel (pour la politique) s'il y a eu un appel.
    Finished(Vec<Outcome>, Option<Option<Error>>),
    /// Des unités à mettre en tête de la file (appels d'un fichier envoyé seul, moitiés d'un lot
    /// tronqué, fichiers renvoyés), et l'erreur de l'appel pour la politique s'il y a eu un appel.
    Calls(Vec<Outcome>, Vec<Unit<'a>>, Option<Option<Error>>),
}

/// Où lire les fichiers à enrichir : racine du dépôt et chemins réels déjà connus (DEC-070).
#[derive(Clone, Copy)]
pub(crate) struct Sources<'a> {
    pub(crate) repo: &'a Path,
    pub(crate) abs: &'a HashMap<String, PathBuf>,
    /// Décompte du masquage (REQ-264, REQ-266) ; `None` : texte envoyé tel quel.
    pub(crate) redact: Option<&'a crate::llm::redact::RedactTally>,
}

impl Sources<'_> {
    /// Texte d'un fichier à envoyer au LLM : lu, puis masqué et compté si `redact` est présent.
    fn read_for_llm(&self, path: &str) -> std::io::Result<String> {
        let text = std::fs::read_to_string(self.real_path(path))?;
        let Some(tally) = self.redact else {
            return Ok(text);
        };
        if !crate::filter::FileType::of(path).is_some_and(crate::llm::redact::is_targeted) {
            return Ok(text);
        }
        let (masked, n) = crate::llm::redact::redact(path, &text);
        tally.record(path, n);
        Ok(masked.into_owned())
    }

    /// Chemin réel d'un fichier : l'entrée d'`abs`, sinon la racine jointe au chemin.
    fn real_path(&self, path: &str) -> PathBuf {
        self.abs
            .get(path)
            .cloned()
            .unwrap_or_else(|| self.repo.join(path))
    }
}

/// Exécute une unité `Lot` ou `Big`. Un fichier illisible échoue seul et sort du lot.
#[allow(clippy::too_many_arguments)]
fn run_unit<'a>(
    unit: &Unit<'a>,
    sources: Sources<'_>,
    max_chunk_chars: usize,
    llm: &dyn Llm,
    ids: &HashMap<String, Vec<(String, String)>>,
    retries: u32,
    stop: &AtomicBool,
    totals: &Mutex<LlmTotals>,
    max_calls: Option<usize>,
    take: Take<'_>,
    kept: &KeptCalls,
) -> Step<'a> {
    let pass = match unit {
        Unit::Lot(_, pass) => *pass,
        _ => Pass::First,
    };
    let files: Vec<&'a FileRecord> = match unit {
        Unit::Lot(v, _) => v.clone(),
        Unit::Big(f) => vec![*f],
        Unit::Call { .. } => unreachable!("un appel ne passe pas par run_unit"),
    };
    let mut outcomes = Vec::new();
    let mut readable: Vec<(&'a FileRecord, String)> = Vec::new();
    for f in files {
        match sources.read_for_llm(&f.path) {
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
                Ok(calls) if calls.is_empty() => {
                    outcomes.push(Outcome::Done((*f).clone(), Enrichment::default()));
                    Step::Finished(outcomes, None)
                }
                Ok(calls) => {
                    let (mut calls, held) = split_kept(calls, kept);
                    mark_rereads(&mut calls, llm.cache_floor());
                    let k = calls.len();
                    // Appels conservés : ils ne prennent rien sur le plafond (REQ-227).
                    let kept_msg = (!held.is_empty()).then(|| Msg::Kept {
                        file: f,
                        count: k + held.len(),
                        results: held,
                    });
                    if k > 0 && max_calls.is_some_and(|m| k > m) {
                        // A-11 : plus d'appels à envoyer que le plafond de la commande, aucun
                        // compte pris.
                        let e = Error::CallsOverCap {
                            calls: k,
                            max: max_calls.unwrap_or(0),
                        };
                        outcomes.push(Outcome::Failed(f.path.clone(), e.clone()));
                        Step::Finished(outcomes, Some(Some(e)))
                    } else if k == 0 || take(k, 1) {
                        let calls = chain_followers(calls);
                        match kept_msg {
                            Some(msg) => Step::Resumed(msg, calls),
                            None => Step::Calls(outcomes, calls, None),
                        }
                    } else {
                        // Réservation refusée : aucune issue, aucun appel mis en file.
                        Step::Finished(outcomes, None)
                    }
                }
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
                    text: excerpt::for_llm(&f.path, t, max_chunk_chars).into_owned(),
                    node_ids: node_ids(f),
                })
                .collect();
            let result = traced_lot(llm, &items, retries, stop, totals, take);
            // Lot tronqué avec au moins une entrée complète : ces fichiers sont gardés et seul le
            // reste repart en un lot réduit, de même passage (REQ-232, DEC-175). Sans entrée
            // complète, la réponse est oubliée et les moitiés ci-dessous s'appliquent.
            if let Err(Error::Truncated { partial, .. }) = &result {
                let results = parse_lot_partial(partial, &items).into_results();
                if results.iter().any(|(_, r)| r.is_ok()) {
                    let mut rest: Vec<&'a FileRecord> = Vec::new();
                    for ((f, _), (_, r)) in readable.iter().zip(results) {
                        match r {
                            Ok(x) => outcomes.push(Outcome::Done((*f).clone(), x)),
                            Err(_) => rest.push(f),
                        }
                    }
                    let suite = if rest.is_empty() {
                        "rien ne repart".to_string()
                    } else {
                        format!("repart {}", unit_name(&rest))
                    };
                    trace!(
                        "llm enrich récupération lot {} +{} [1/1] : {} fichiers gardés, {suite}",
                        items[0].file,
                        items.len() - 1,
                        readable.len() - rest.len()
                    );
                    return if rest.is_empty() {
                        Step::Finished(outcomes, None)
                    } else {
                        Step::Calls(outcomes, vec![Unit::Lot(rest, pass)], None)
                    };
                }
            }
            match result {
                // Lot tronqué (REQ-096, DEC-101), ou de 4 fichiers ou plus encore non conforme au
                // premier passage (REQ-200, DEC-161 (4)) : deux moitiés remises en tête de file ;
                // aucune issue pour le lot lui-même, ses fichiers en recevront par leurs moitiés.
                // Les moitiés d'un lot non conforme sont de second passage.
                Err(e)
                    if matches!(e, Error::Truncated { .. })
                        || (matches!(e, Error::Format(_))
                            && pass == Pass::First
                            && readable.len() >= 4) =>
                {
                    let next = if matches!(e, Error::Truncated { .. }) {
                        pass
                    } else {
                        Pass::Second
                    };
                    let mut first: Vec<&'a FileRecord> = readable.iter().map(|(f, _)| *f).collect();
                    let second = first.split_off(first.len().div_ceil(2));
                    trace!(
                        "llm enrich découpe lot {} +{} [1/1] en {} et {}",
                        items[0].file,
                        items.len() - 1,
                        unit_name(&first),
                        unit_name(&second)
                    );
                    Step::Calls(
                        outcomes,
                        vec![Unit::Lot(first, next), Unit::Lot(second, next)],
                        None,
                    )
                }
                // Essai refusé par le plafond, ou requête mise de côté par le collecteur du mode
                // lot : aucune issue pour les fichiers du lot.
                Err(Error::Capped | Error::Deferred | Error::Unsent) => {
                    Step::Finished(outcomes, None)
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
                    let mut absent: Vec<&'a FileRecord> = Vec::new();
                    for (f, _) in &readable {
                        match by_path.remove(&f.path) {
                            Some(Ok(x)) => outcomes.push(Outcome::Done((*f).clone(), x)),
                            // Sans entrée dans la réponse : renvoyé une fois au premier passage.
                            _ if pass == Pass::First => absent.push(f),
                            Some(Err(e)) => outcomes.push(Outcome::Failed(f.path.clone(), e)),
                            None => outcomes.push(Outcome::Failed(
                                f.path.clone(),
                                Error::Message(format!("{} : absent de la réponse du lot", f.path)),
                            )),
                        }
                    }
                    if absent.is_empty() {
                        Step::Finished(outcomes, Some(None))
                    } else {
                        trace!(
                            "llm enrich renvoi {} : sans entrée dans la réponse de {} [1/1]",
                            unit_name(&absent),
                            unit_name(&readable.iter().map(|(f, _)| *f).collect::<Vec<_>>())
                        );
                        Step::Calls(outcomes, vec![Unit::Lot(absent, Pass::Second)], Some(None))
                    }
                }
            }
        }
    }
}

/// Nom d'une unité dans les traces : le chemin d'un fichier seul, sinon `lot <premier> +<n−1>`.
fn unit_name(files: &[&FileRecord]) -> String {
    match files {
        [only] => only.path.clone(),
        [head, ..] => format!("lot {} +{}", head.path, files.len() - 1),
        [] => String::new(),
    }
}

/// Détail d'un échec pour la trace. Pour `Error::Message`, seul le libellé fixe (avant le
/// premier « : ») est gardé : le détail serde cite des valeurs et des noms de champs de la réponse.
fn trace_detail(e: &Error) -> String {
    match e {
        Error::Message(m) | Error::Format(m) => m.split(" : ").next().unwrap_or("").to_string(),
        Error::Truncated { .. } => TRUNCATED.to_string(),
        Error::Rejected { trace, .. } | Error::Redirected { trace, .. } => trace.clone(),
        // Un essai refusé par le plafond part avant toute trace d'appel : jamais détaillé ici.
        // Texte fixe : la cause du fournisseur ne va pas dans la trace (design.md, A-17).
        Error::BatchItem { .. } => "requête du lot du fournisseur en échec".to_string(),
        Error::Capped => unreachable!("un essai refusé par le plafond n'a pas de détail de trace"),
        Error::Deferred => unreachable!("une requête mise de côté n'a pas de détail de trace"),
        Error::Unsent => unreachable!("un appel sans envoi n'a pas de détail de trace"),
        other => other.to_string(),
    }
}

/// Erreur de chaque fichier dont l'appel a été tronqué : le libellé fixe et l'action (A-5).
fn truncation_advice(e: Error) -> Error {
    match e {
        Error::Truncated { .. } => Error::truncated(format!(
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
    excerpt: Option<Excerpt>,
    text: &str,
    requested: &[String],
    known: &[String],
    attempt: Attempt,
    fault: Option<FormatFault>,
    stop: &AtomicBool,
    totals: &Mutex<LlmTotals>,
    sub: &[usize],
    cached: bool,
) -> Result<Enrichment> {
    let mark = sub_mark(sub);
    let stats = CallStats::with_mark(mark.clone());
    let label = format!("{path} [{index}/{total}]");
    if !trace::enabled() {
        let result = llm.enrich_labeled(
            &label, path, index, excerpt, text, requested, known, stop, &stats, fault, cached,
        );
        add_totals(totals, &stats, attempt);
        return result;
    }
    trace!(
        "llm enrich départ {path} [{index}/{total}] {} car.{}{mark}",
        text.chars().count(),
        attempt.mark()
    );
    let started = Instant::now();
    let result = llm.enrich_labeled(
        &label, path, index, excerpt, text, requested, known, stop, &stats, fault, cached,
    );
    let ms = started.elapsed().as_millis();
    add_totals(totals, &stats, attempt);
    let tokens = tokens_suffix(&stats);
    match &result {
        Ok(x) => {
            let n = x
                .summaries
                .iter()
                .filter(|(id, _)| requested.contains(id))
                .count();
            trace!(
                "llm enrich fin {path} [{index}/{total}] ok ({} résumés, {} concepts, {}) {ms} ms{tokens}{mark}",
                x.summaries.len(),
                x.concepts.len(),
                coverage_segment(n, requested.len())
            )
        }
        // Essai refusé par le plafond ou requête mise de côté (mode lot) : ni appel ni fin à tracer.
        Err(Error::Capped | Error::Deferred | Error::Unsent) => {}
        Err(e) => trace!(
            "llm enrich fin {path} [{index}/{total}] {} {ms} ms : {}{tokens}{mark}",
            trace::classify(e),
            trace::short(&trace_detail(e), trace::MAX_MESSAGE_CHARS)
        ),
    }
    result
}

/// Marque d'un sous-appel (REQ-231) : ` (sous-appel <rang>)`, rangs de `path` joints par `.` ;
/// vide pour un appel du plan.
fn sub_mark(path: &[usize]) -> String {
    if path.is_empty() {
        return String::new();
    }
    let ranks: Vec<String> = path.iter().map(usize::to_string).collect();
    format!(" (sous-appel {})", ranks.join("."))
}

/// Ajoute le relevé d'un appel aux totaux de la phase.
fn add_totals(totals: &Mutex<LlmTotals>, stats: &CallStats, attempt: Attempt) {
    totals
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .add(stats, attempt.current >= 2);
}

/// Fin de la ligne `llm enrich fin` : ` (<segment de jetons>)`, ou rien sans compte de jetons.
pub(crate) fn tokens_suffix(stats: &CallStats) -> String {
    stats
        .usage()
        .map(|u| format!(" ({})", tokens_segment(&u)))
        .unwrap_or_default()
}

/// Un appel `llm.enrich_lot` encadré de ses traces de départ et de fin (jamais de contenu).
fn traced_lot(
    llm: &dyn Llm,
    items: &[LotItem],
    retries: u32,
    stop: &AtomicBool,
    totals: &Mutex<LlmTotals>,
    take: Take<'_>,
) -> Result<Vec<(String, Result<Enrichment>)>> {
    let total = retries.saturating_add(1);
    with_format_retries(retries, stop, |current, fault| {
        // Chaque essai, le premier compris, est pris sur le plafond avant tout envoi.
        if !take(1, if current == 1 { items.len() } else { 0 }) {
            return Err(Error::Capped);
        }
        traced_lot_once(llm, items, Attempt { current, total }, fault, stop, totals)
    })
}

fn traced_lot_once(
    llm: &dyn Llm,
    items: &[LotItem],
    attempt: Attempt,
    fault: Option<FormatFault>,
    stop: &AtomicBool,
    totals: &Mutex<LlmTotals>,
) -> Result<Vec<(String, Result<Enrichment>)>> {
    let label = format!("lot {} +{}", items[0].file, items.len() - 1);
    let stats = CallStats::default();
    if !trace::enabled() {
        let result = llm.enrich_lot_labeled(&format!("{label} [1/1]"), items, stop, &stats, fault);
        add_totals(totals, &stats, attempt);
        return result;
    }
    trace!(
        "llm enrich départ {label} [1/1] {} car.{}",
        items.iter().map(|i| i.text.chars().count()).sum::<usize>(),
        attempt.mark()
    );
    let started = Instant::now();
    let result = llm.enrich_lot_labeled(&format!("{label} [1/1]"), items, stop, &stats, fault);
    let ms = started.elapsed().as_millis();
    add_totals(totals, &stats, attempt);
    let tokens = tokens_suffix(&stats);
    match &result {
        Ok(results) => {
            let (r, k) = results
                .iter()
                .filter_map(|(_, x)| x.as_ref().ok())
                .fold((0, 0), |(r, k), x| {
                    (r + x.summaries.len(), k + x.concepts.len())
                });
            let m: usize = items.iter().map(|i| i.node_ids.len()).sum();
            trace!(
                "llm enrich fin {label} [1/1] ok ({r} résumés, {k} concepts, {}) {ms} ms{tokens}",
                coverage_segment(r, m)
            )
        }
        // Essai refusé par le plafond ou requête mise de côté (mode lot) : ni appel ni fin à tracer.
        Err(Error::Capped | Error::Deferred | Error::Unsent) => {}
        Err(e) => trace!(
            "llm enrich fin {label} [1/1] {} {ms} ms : {}{tokens}",
            trace::classify(e),
            trace::short(&trace_detail(e), trace::MAX_MESSAGE_CHARS)
        ),
    }
    result
}

/// Plan d'appels d'un fichier (DEC-059, DEC-061) : couples (indice du morceau, liste de nœuds
/// demandée), dans l'ordre d'envoi (DEC-118 : un nœud n'est demandé qu'une fois). Un seul morceau : la liste complète, découpée par
/// `node_groups`. Sinon, un nœud présent dans `homes` (identifiant → morceau de son titre, pour
/// les sections Markdown) va dans la seule liste de ce morceau ; tout autre nœud est demandé au
/// premier morceau où son nom court figure (mot entier, ou, pour un nom qui n'est pas un mot,
/// occurrence aux bords contrôlés ; sans casse pour un nœud `table:`, le SQL ne distinguant pas
/// les majuscules) ; un nœud de nom vide ou absent de tous les morceaux va dans le morceau 1. Au
/// moins un appel par morceau, liste vide permise : le filtrage des appels vides se fait dans
/// `plan_calls`.
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
                    break;
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

/// Extrait de chaque morceau (REQ-169) : `None` pour un seul morceau, sinon rang, nombre de
/// morceaux et lignes du corps (numérotées à partir de 1), d'après les débuts de corps de
/// `spans`. Un seul passage sur le texte : les sauts de ligne sont comptés de façon cumulative.
fn excerpts_of(text: &str, spans: &[(String, usize)]) -> Vec<Option<Excerpt>> {
    let total = spans.len();
    if total <= 1 {
        return vec![None; total];
    }
    let bytes = text.as_bytes();
    // Lignes terminées avant l'octet `pos` ; `done` est le nombre de sauts de ligne déjà comptés.
    let mut counted = 0usize;
    let mut done = 0usize;
    let mut lines_before = |pos: usize| {
        let pos = pos.min(bytes.len());
        if pos > counted {
            done += bytes[counted..pos].iter().filter(|&&b| b == b'\n').count();
            counted = pos;
        }
        done
    };
    let starts: Vec<usize> = spans.iter().map(|(_, s)| *s).collect();
    let mut out = Vec::with_capacity(total);
    for k in 0..total {
        let first_line = lines_before(starts[k]) + 1;
        let last_line = match starts.get(k + 1) {
            Some(&next) => lines_before(next),
            None => {
                lines_before(bytes.len()) + usize::from(!bytes.is_empty() && !text.ends_with('\n'))
            }
        };
        out.push(Some(Excerpt {
            rank: k + 1,
            total,
            first_line,
            last_line: last_line.max(first_line),
        }));
    }
    out
}

/// Appels d'un fichier envoyé seul selon `call_plan`, privé de ses appels à liste vide (sauf
/// Markdown ayant au moins une section, REQ-159), numérotés 1..n dans l'ordre du plan. Le
/// plafond de `MAX_CALLS_PER_FILE` est vérifié ici sur le plan filtré, avant qu'un seul appel
/// soit mis en file (REQ-048).
fn plan_calls<'a>(
    file: &'a FileRecord,
    text: &str,
    max_chunk_chars: usize,
    nodes: &[(String, String)],
) -> Result<Vec<Unit<'a>>> {
    let known: Arc<[String]> = nodes.iter().map(|(id, _)| id.clone()).collect();
    let text = excerpt::for_llm(&file.path, text, max_chunk_chars);
    let text: &str = &text;
    let spans = if excerpt::never_split(&file.path) {
        vec![(text.to_string(), 0)]
    } else {
        chunk::split_spans_for(&file.path, text, max_chunk_chars)
    };
    let homes = section_homes(text, &spans, nodes);
    let excerpts = excerpts_of(text, &spans);
    let parts: Vec<String> = spans.into_iter().map(|(part, _)| part).collect();
    let mut plan = call_plan(&parts, nodes, &homes);
    retain_calls(&mut plan, &file.path, &known);
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
            excerpt: excerpts[k],
            part: Arc::clone(&shared[k]),
            requested,
            known: Arc::clone(&known),
            path: Vec::new(),
            cache: false,
            followers: Vec::new(),
        })
        .collect())
}

/// Marque `cache` les appels d'un même morceau (même `Arc`) quand ils sont au moins deux et que
/// le morceau dépasse `floor` jetons, à raison de 4 caractères par jeton (REQ-268). Sans `floor`,
/// aucun appel n'est marqué.
fn mark_rereads(calls: &mut [Unit<'_>], floor: Option<usize>) {
    let Some(floor) = floor else {
        return;
    };
    let shared: Vec<Option<Arc<str>>> = calls
        .iter()
        .map(|c| match c {
            Unit::Call { part, .. } => Some(Arc::clone(part)),
            _ => None,
        })
        .collect();
    for (i, c) in calls.iter_mut().enumerate() {
        let (Some(me), Unit::Call { cache, .. }) = (&shared[i], c) else {
            continue;
        };
        let same = shared
            .iter()
            .flatten()
            .filter(|p| Arc::ptr_eq(p, me))
            .count();
        *cache = same >= 2 && me.chars().count() / 4 > floor;
    }
}

/// Sépare les appels du plan dont la réponse est conservée (rang à partir de 1 et réponse) des
/// appels à envoyer, dans l'ordre du plan (REQ-227).
fn split_kept<'a>(
    calls: Vec<Unit<'a>>,
    kept: &KeptCalls,
) -> (Vec<Unit<'a>>, Vec<(usize, Enrichment)>) {
    if kept.results.is_empty() {
        return (calls, Vec::new());
    }
    let mut send = Vec::new();
    let mut held = Vec::new();
    for call in calls {
        let found = match &call {
            Unit::Call {
                file,
                index,
                count,
                excerpt,
                part,
                requested,
                known,
                ..
            } => kept
                .results
                .get(&kept.key(&file.path, *index, *count, *excerpt, part, requested, known))
                .map(|x| (*index, x.clone())),
            _ => None,
        };
        match found {
            Some(h) => held.push(h),
            None => send.push(call),
        }
    }
    (send, held)
}

/// Un appel d'enrichissement avec ses nouveaux essais de format (REQ-038) ; le compte d'essais
/// repart à 1 pour chaque appel et seul l'appel fautif est renvoyé.
#[allow(clippy::too_many_arguments)]
fn run_call(
    llm: &dyn Llm,
    path: &str,
    index: usize,
    count: usize,
    excerpt: Option<Excerpt>,
    part: &str,
    requested: &[String],
    known: &[String],
    retries: u32,
    stop: &AtomicBool,
    totals: &Mutex<LlmTotals>,
    take: Take<'_>,
    sub: &[usize],
    cache: bool,
) -> Result<Enrichment> {
    let total = retries.saturating_add(1);
    with_format_retries(retries, stop, |current, fault| {
        // Le premier essai d'un appel d'un fichier envoyé seul est déjà réservé (`run_unit`).
        if current >= 2 && !take(1, 0) {
            return Err(Error::Capped);
        }
        traced_enrich(
            llm,
            path,
            index,
            count,
            excerpt,
            part,
            requested,
            known,
            Attempt { current, total },
            fault,
            stop,
            totals,
            sub,
            cache,
        )
    })
}

/// Sépare les fichiers ayant des nœuds à demander de ceux qui n'en ont aucun (REQ-159).
fn partition_by_nodes(
    files: &[FileRecord],
    ids: &HashMap<String, Vec<(String, String)>>,
) -> (Vec<FileRecord>, Vec<FileRecord>) {
    files
        .iter()
        .cloned()
        .partition(|f| !ids.get(&f.path).is_none_or(Vec::is_empty))
}

/// Taille comptée d'un fichier d'un lot, sans lecture (REQ-241) : un JSON, YAML ou XML est réduit
/// à L caractères au plus ; tout autre fichier, SQL compris (majorant), compte sa taille.
fn sent_size(f: &FileRecord, max_chunk_chars: usize) -> usize {
    let size = f.size as usize;
    if excerpt::never_split(&f.path) {
        size.min(excerpt::data_limit(max_chunk_chars))
    } else {
        size
    }
}

/// Estimation du plan d'enrichissement de `files` sans appel ni écriture (REQ-184, DEC-148) : même
/// partition et mêmes unités que `enrich_with_stop`. Un lot ne lit aucun fichier (ses prompts
/// valent ceux à texte vide plus la somme des tailles), sauf, pour le seul décompte du masquage
/// (REQ-266), les fichiers visés par REQ-264, lus sans effet sur l'estimation ; un fichier envoyé seul est lu en série,
/// puis `plan_calls` donne ses appels. Un fichier illisible ou au plafond ne compte aucun appel.
pub(crate) fn estimate(
    sources: Sources<'_>,
    files: &[FileRecord],
    ids: &HashMap<String, Vec<(String, String)>>,
    language: &str,
    batch_files: usize,
    max_chunk_chars: usize,
    kept: &KeptCalls,
) -> Estimate {
    let (with_nodes, without_nodes) = partition_by_nodes(files, ids);
    let units = form_units_by_nodes(
        &with_nodes,
        |f| ids.get(&f.path).map_or(0, Vec::len),
        batch_files,
        max_chunk_chars,
    );
    let mut est = Estimate {
        files: files.len(),
        without_call: without_nodes.len(),
        ..Estimate::default()
    };
    let node_ids = |f: &FileRecord| -> Vec<String> {
        ids.get(&f.path)
            .map(|v| v.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    };
    for unit in &units {
        // Pour le seul décompte du masquage : un lot n'est pas lu pour son estimation (REQ-266).
        if let (Unit::Lot(v, _), Some(_)) = (unit, sources.redact) {
            for f in v.iter().filter(|f| {
                crate::filter::FileType::of(&f.path).is_some_and(crate::llm::redact::is_targeted)
            }) {
                let _ = sources.read_for_llm(&f.path);
            }
        }
        match unit {
            Unit::Lot(v, _) if v.len() >= 2 => {
                let items: Vec<LotItem> = v
                    .iter()
                    .map(|f| LotItem {
                        file: f.path.clone(),
                        text: String::new(),
                        node_ids: node_ids(f),
                    })
                    .collect();
                est.calls += 1;
                est.nodes += items.iter().map(|i| i.node_ids.len()).sum::<usize>();
                est.bytes += lot_system_prompt(language).len()
                    + lot_user_prompt(&items).len()
                    + v.iter()
                        .map(|f| sent_size(f, max_chunk_chars))
                        .sum::<usize>();
            }
            Unit::Lot(v, _) => {
                for f in v {
                    let requested = node_ids(f);
                    est.calls += 1;
                    est.nodes += requested.len();
                    est.bytes += enrichment_system_prompt(language).len()
                        + enrichment_user_prompt(&f.path, None, "", &requested).len()
                        + sent_size(f, max_chunk_chars);
                }
            }
            Unit::Big(f) => {
                est.sent_alone += 1;
                est.alone.push(estimate_alone(
                    f,
                    sources,
                    ids,
                    language,
                    max_chunk_chars,
                    kept,
                    &mut est.nodes,
                ));
                let last = est.alone.last().expect("fichier seul ajouté");
                est.calls += last.calls;
                est.bytes += last.bytes;
                match last.case {
                    AloneCase::Counted => {}
                    AloneCase::OverCap(planned) => est.capped.push((last.path.clone(), planned)),
                    AloneCase::Unreadable => est.unreadable.push(last.path.clone()),
                }
            }
            Unit::Call { .. } => unreachable!("un plan de fichiers ne contient pas d'appel"),
        }
    }
    est
}

/// Estimation d'un fichier envoyé seul ; ajoute ses nœuds demandés à `nodes`. Le texte lu est
/// libéré au retour.
fn estimate_alone(
    f: &FileRecord,
    sources: Sources<'_>,
    ids: &HashMap<String, Vec<(String, String)>>,
    language: &str,
    max_chunk_chars: usize,
    kept: &KeptCalls,
    nodes: &mut usize,
) -> AloneFile {
    let mut out = AloneFile {
        path: f.path.clone(),
        calls: 0,
        bytes: 0,
        case: AloneCase::Counted,
    };
    let Ok(text) = sources.read_for_llm(&f.path) else {
        out.case = AloneCase::Unreadable;
        return out;
    };
    let file_nodes = ids.get(&f.path).cloned().unwrap_or_default();
    match plan_calls(f, &text, max_chunk_chars, &file_nodes) {
        Ok(calls) => {
            let system = enrichment_system_prompt(language).len();
            // Les appels dont la réponse est conservée ne partent pas (REQ-228).
            let (send, _) = split_kept(calls, kept);
            for call in send {
                if let Unit::Call {
                    excerpt,
                    part,
                    requested,
                    ..
                } = call
                {
                    out.calls += 1;
                    *nodes += requested.len();
                    out.bytes +=
                        system + enrichment_user_prompt(&f.path, excerpt, &part, &requested).len();
                }
            }
        }
        Err(Error::TooManyCalls { calls, .. }) => out.case = AloneCase::OverCap(calls),
        Err(_) => out.case = AloneCase::Unreadable,
    }
    out
}

/// Nœuds à demander pour chaque fichier (identifiant, nom court) : déclarés sans doc (DEC-180),
/// hors packages et clés de configuration (REQ-024).
pub(crate) fn nodes_by_file(store: &Store) -> Result<HashMap<String, Vec<(String, String)>>> {
    let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (file, node, short) in store.undocumented_short_names()? {
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
        /// Rangs de la découpe du sous-appel (REQ-230) ; vide pour un appel du plan.
        path: Vec<usize>,
        count: usize,
        result: Result<Enrichment>,
        /// Clé de l'appel réussi d'un plan de 2 appels ou plus (REQ-226) ; calculée dans le fil
        /// d'appel.
        key: Option<String>,
    },
    /// Un appel tronqué d'un fichier envoyé seul a été remplacé par `added + 1` sous-appels.
    Split {
        file: &'a FileRecord,
        count: usize,
        added: usize,
    },
    /// Appels d'un fichier envoyé seul dont la réponse était conservée (rang à partir de 1).
    Kept {
        file: &'a FileRecord,
        count: usize,
        results: Vec<(usize, Enrichment)>,
    },
}

/// Résultats rangés par indice des appels d'un fichier envoyé seul, côté fil écrivain.
struct Partial {
    /// Clé `[rang de l'appel, rangs de découpe…]` : l'ordre des clés est celui de la fusion.
    results: BTreeMap<Vec<usize>, Enrichment>,
    /// Résultats attendus : les appels du plan, plus un par sous-appel de plus qu'une coupe ajoute.
    expected: usize,
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
    /// Sous-appels réservés par fichier, pour la borne de `MAX_CALLS_PER_FILE`.
    split_calls: HashMap<String, usize>,
}

/// Délai de réveil d'un fil qui attend des appels, pour voir l'arrêt posé hors de la file.
const IDLE_WAKE: Duration = Duration::from_millis(20);

/// Prend l'unité suivante ; attend quand la file est vide mais qu'une unité en cours peut encore
/// y ajouter des appels. `None` : plus rien à faire ou arrêt demandé. Le plafond atteint
/// (`capped`, jamais le drapeau `stop`) ferme la file aux lots et aux fichiers envoyés seuls ;
/// les appels déjà réservés d'un fichier envoyé seul vont à leur terme.
fn next_unit<'a>(
    shared: &Mutex<Shared<'a>>,
    wake: &Condvar,
    stop: &AtomicBool,
    capped: &AtomicBool,
) -> Option<Unit<'a>> {
    let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        let closed = capped.load(Ordering::SeqCst);
        if closed {
            // File fermée : seuls les appels réservés (hors fichier en échec) partent, où qu'ils
            // soient dans la file ; les lots et fichiers envoyés seuls restent « laissés ».
            let state = &*guard;
            let pos = state.queue.iter().position(
                |u| matches!(u, Unit::Call { file, .. } if !state.failed.contains(&file.path)),
            );
            if let Some(i) = pos {
                let unit = guard.queue.remove(i);
                if unit.as_ref().is_some_and(has_followers) {
                    guard.expanding += 1;
                }
                return unit;
            }
            if guard.expanding == 0 {
                return None;
            }
            guard = wake
                .wait_timeout(guard, IDLE_WAKE)
                .unwrap_or_else(|p| p.into_inner())
                .0;
            continue;
        }
        match guard.queue.pop_front() {
            Some(Unit::Call { file, .. }) if guard.failed.contains(&file.path) => continue,
            Some(unit) => {
                if !matches!(unit, Unit::Call { .. }) || has_followers(&unit) {
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

/// Un appel qui garde ses suivants : il compte parmi les unités qui peuvent encore ajouter des
/// appels à la file (REQ-269).
fn has_followers(unit: &Unit<'_>) -> bool {
    matches!(unit, Unit::Call { followers, .. } if !followers.is_empty())
}

/// Le premier appel marqué de chaque morceau prend les suivants du même morceau (REQ-269) ; les
/// autres appels gardent leur place et leur ordre.
fn chain_followers(calls: Vec<Unit<'_>>) -> Vec<Unit<'_>> {
    let mut out: Vec<Unit<'_>> = Vec::new();
    // Position, dans `out`, du premier appel marqué de chaque morceau.
    let mut heads: Vec<(Arc<str>, usize)> = Vec::new();
    for call in calls {
        let head = match &call {
            Unit::Call {
                cache: true, part, ..
            } => heads
                .iter()
                .find(|(p, _)| Arc::ptr_eq(p, part))
                .map(|h| h.1),
            _ => None,
        };
        match (head, &call) {
            (Some(i), _) => {
                if let Unit::Call { followers, .. } = &mut out[i] {
                    followers.push(call);
                }
            }
            (
                None,
                Unit::Call {
                    cache: true, part, ..
                },
            ) => {
                heads.push((Arc::clone(part), out.len()));
                out.push(call);
            }
            _ => out.push(call),
        }
    }
    out
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
/// La garde ne s'exécute que si la panique se déroule (profils `dev` et `test`) ; en `release`,
/// `panic = "abort"` (REQ-081) arrête le processus à la panique.
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
    language: &str,
    pending_calls: &mut Vec<CallWrite>,
    pending: &mut Vec<EnrichmentWrite>,
    stop: &AtomicBool,
    write_error: &mut Option<Error>,
) {
    if pending_calls.is_empty() && pending.is_empty() {
        return;
    }
    if let Err(e) = store.apply_group(model, language, pending_calls, pending) {
        stop.store(true, Ordering::SeqCst);
        *write_error = Some(e);
    }
    pending_calls.clear();
    pending.clear();
}

/// Entrée de `partials` d'un fichier envoyé seul, créée au premier message.
fn partial_of<'p>(
    partials: &'p mut HashMap<String, Partial>,
    file: &FileRecord,
    count: usize,
) -> &'p mut Partial {
    partials
        .entry(file.path.clone())
        .or_insert_with(|| Partial {
            results: BTreeMap::new(),
            expected: count,
            done: 0,
            failed: false,
        })
}

/// Fin commune des messages d'un fichier envoyé seul : quand tous les appels ont rendu, fusion
/// dans l'ordre du plan et mise en file de l'écriture. Rend l'avance de la progression (1 si le
/// fichier est complet).
fn complete_file(
    entry: &mut Partial,
    file: &FileRecord,
    succeeded: &mut usize,
    write_error: &Option<Error>,
    pending: &mut Vec<EnrichmentWrite>,
    notes: &mut Vec<String>,
) -> usize {
    if entry.done != entry.expected {
        return 0;
    }
    let parts: Vec<Enrichment> = std::mem::take(&mut entry.results).into_values().collect();
    // Une seule partie : résultat tel quel, comme avant DEC-063.
    let merged = if parts.len() == 1 {
        parts.into_iter().next().unwrap_or_default()
    } else {
        chunk::merge_enrichments(parts)
    };
    queue_done(file, &merged, succeeded, write_error, pending, notes);
    1
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
    totals: &'s Mutex<LlmTotals>,
    /// Plafond d'appels de la commande (REQ-186).
    max_calls: Option<usize>,
    /// Posé quand une prise sur le plafond est refusée : plus aucune unité nouvelle.
    capped: &'s AtomicBool,
    /// Appels conservés au début de la passe.
    kept: &'s KeptCalls,
    /// Dernière passe du mode lot : toutes les réponses sont déjà payées, aucun appel n'est
    /// sauté après un échec, pour que celles des autres appels du fichier soient conservées
    /// (REQ-228).
    answers_known: bool,
}

/// Prise de `n` appels sur le plafond, sous le verrou de la politique. Au premier refus, écrit la
/// trace `politique: arrêt de l'envoi (plafond …), <q> fichiers non envoyés` (`q` : fichiers de la
/// file plus les `unsent` de l'unité en cours) et réveille les fils qui attendent.
fn take_calls(ctx: &Ctx<'_, '_>, n: usize, unsent: usize) -> bool {
    let mut guard = ctx.policy.lock().unwrap_or_else(|p| p.into_inner());
    if guard.take(n) {
        return true;
    }
    let first = !ctx.capped.swap(true, Ordering::SeqCst);
    if first && trace::enabled() {
        let queued: usize = ctx
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .queue
            .iter()
            .map(Unit::file_count)
            .sum();
        trace!(
            "politique: arrêt de l'envoi ({}), {} fichiers non envoyés",
            cap_label(guard.limit.unwrap_or(0)),
            queued + unsent
        );
    }
    drop(guard);
    // Réveille les fils qui attendent des appels.
    drop(ctx.shared.lock().unwrap_or_else(|p| p.into_inner()));
    ctx.wake.notify_all();
    false
}

/// Réserve `k` sous-appels pour un fichier : refusée si, avec les `count` appels du plan et les
/// sous-appels déjà réservés, le total dépasse `MAX_CALLS_PER_FILE` (sous le verrou de `Shared`).
fn reserve_split(shared: &Mutex<Shared<'_>>, path: &str, count: usize, k: usize) -> bool {
    let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
    let used = guard.split_calls.entry(path.to_string()).or_insert(0);
    if count + *used + k > MAX_CALLS_PER_FILE {
        return false;
    }
    *used += k;
    true
}

/// Corps d'un fil d'enrichissement : prend des unités jusqu'à l'arrêt ou la fin de la file.
fn worker<'a>(ctx: Ctx<'_, 'a>, tx: mpsc::Sender<Msg<'a>>) {
    let take = |n: usize, unsent: usize| take_calls(&ctx, n, unsent);
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
        totals,
        max_calls,
        capped,
        kept,
        answers_known,
    } = ctx;
    while let Some(mut unit) = next_unit(shared, wake, stop, capped) {
        // Suivants d'un appel marqué : ils repartent à la fin de l'appel, sur tous ses chemins
        // de sortie (REQ-269), une fois l'échec éventuel du fichier posé.
        let mut followers = match &mut unit {
            Unit::Call { followers, .. } => std::mem::take(followers),
            _ => Vec::new(),
        };
        let ran = !followers.is_empty();
        let mut expand = ExpandGuard {
            shared,
            wake,
            armed: !matches!(unit, Unit::Call { .. }) || ran,
        };
        let ticket = policy.lock().unwrap_or_else(|p| p.into_inner()).dispatch();
        // Résultat de l'unité : l'erreur de l'appel pour la politique s'il y en a eu un.
        let (call, message, calls, expanded) = match &unit {
            Unit::Call {
                file,
                index,
                count,
                excerpt,
                part,
                requested,
                known,
                path,
                cache,
                ..
            } => {
                let (file, index, count, excerpt) = (*file, *index, *count, *excerpt);
                let mut result = run_call(
                    llm,
                    &file.path,
                    index,
                    count,
                    excerpt,
                    part,
                    requested,
                    known,
                    format_retries,
                    stop,
                    totals,
                    &take,
                    path,
                    *cache,
                );
                // Appel tronqué : sa liste de nœuds ou son texte est coupé et les sous-appels
                // passent en tête de file (REQ-230). Pas de coupe possible, total au-delà de la borne : l'échec
                // ordinaire (A-5). Réservation refusée par le plafond : comme un essai refusé.
                let recovery = match &result {
                    Err(Error::Truncated { partial, .. }) => recover_call(
                        &unit,
                        Enrichment::parse_partial(partial, &file.path, requested, known),
                    ),
                    _ => Recovery::Nothing,
                };
                // Appel tronqué avec des éléments complets (REQ-232) : tout reçu, le résultat est
                // celui de l'appel, sans effet sur la politique ; sinon les résumés reçus sont
                // écrits et seul le reste repart, en un sous-appel.
                let mut recovered = false;
                let mut split_allowed = true;
                match recovery {
                    Recovery::All(e) => {
                        trace!(
                            "llm enrich récupération {} [{index}/{count}] : {} résumés, {} concepts gardés, rien ne repart{}",
                            file.path,
                            e.summaries.len(),
                            e.concepts.len(),
                            sub_mark(path)
                        );
                        result = Ok(e);
                        recovered = true;
                    }
                    Recovery::Rest(kept_part, sub) => {
                        split_allowed = false;
                        if path.len() < MAX_SPLIT_DEPTH
                            && reserve_split(shared, &file.path, count, 1)
                        {
                            if take(1, 0) {
                                if let Unit::Call {
                                    requested: rest, ..
                                } = &sub
                                {
                                    trace!(
                                        "llm enrich récupération {} [{index}/{count}] : {} résumés, {} concepts gardés, repart {} nœuds{}",
                                        file.path,
                                        kept_part.summaries.len(),
                                        kept_part.concepts.len(),
                                        rest.len(),
                                        sub_mark(path)
                                    );
                                }
                                let mut first = path.clone();
                                first.push(0);
                                let split = tx.send(Msg::Split {
                                    file,
                                    count,
                                    added: 1,
                                });
                                let chunk = tx.send(Msg::Chunk {
                                    file,
                                    index,
                                    path: first,
                                    count,
                                    result: Ok(kept_part),
                                    key: None,
                                });
                                expand.armed = false;
                                let mut next = vec![sub];
                                next.append(&mut followers);
                                finish_unit(shared, wake, next, ran);
                                if split.is_err() || chunk.is_err() {
                                    break;
                                }
                                if record_policy(policy, shared, ticket, &None) {
                                    stop.store(true, Ordering::SeqCst);
                                    drop(shared.lock().unwrap_or_else(|p| p.into_inner()));
                                    wake.notify_all();
                                }
                                continue;
                            }
                            result = Err(Error::Capped);
                        }
                    }
                    Recovery::Nothing => {}
                }
                if split_allowed && matches!(result, Err(Error::Truncated { .. })) {
                    let nodes: Vec<(String, String)> = {
                        let all = ids.get(&file.path).map(Vec::as_slice).unwrap_or_default();
                        requested
                            .iter()
                            .map(|r| {
                                all.iter()
                                    .find(|(id, _)| id == r)
                                    .cloned()
                                    .unwrap_or_else(|| (r.clone(), String::new()))
                            })
                            .collect()
                    };
                    if let Some(subs) = split_call(&unit, &nodes) {
                        if reserve_split(shared, &file.path, count, subs.len()) {
                            if take(subs.len(), 0) {
                                trace!(
                                    "llm enrich découpe {} [{index}/{count}] en {} sous-appels{}",
                                    file.path,
                                    subs.len(),
                                    sub_mark(path)
                                );
                                let added = subs.len() - 1;
                                let sent = tx.send(Msg::Split { file, count, added });
                                expand.armed = false;
                                let mut next = subs;
                                next.append(&mut followers);
                                finish_unit(shared, wake, next, ran);
                                if sent.is_err() {
                                    break;
                                }
                                if record_policy(policy, shared, ticket, &None) {
                                    stop.store(true, Ordering::SeqCst);
                                    drop(shared.lock().unwrap_or_else(|p| p.into_inner()));
                                    wake.notify_all();
                                }
                                continue;
                            }
                            result = Err(Error::Capped);
                        }
                    }
                }
                // Requête mise de côté (mode lot) : aucune issue, mais le fichier n'est pas en
                // échec, pour que les autres appels d'un gros fichier soient relevés (AC-214-4).
                if matches!(result, Err(Error::Deferred)) {
                    expand.armed = false;
                    finish_unit(shared, wake, followers, ran);
                    continue;
                }
                // Nouvel essai refusé par le plafond : ni issue, ni échec, ni compte ; les appels
                // non partis du fichier sont sautés.
                if matches!(result, Err(Error::Capped | Error::Unsent)) {
                    shared
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .failed
                        .insert(file.path.clone());
                    expand.armed = false;
                    finish_unit(shared, wake, followers, ran);
                    continue;
                }
                let err = result.as_ref().err().cloned();
                // Toute cause d'échec fait sauter les appels non partis du fichier
                // (AC-051-5) ; la politique enregistre quand même cet appel.
                if err.is_some() && !answers_known {
                    shared
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .failed
                        .insert(file.path.clone());
                }
                let key = (result.is_ok() && count >= 2 && path.is_empty())
                    .then(|| kept.key(&file.path, index, count, excerpt, part, requested, known));
                let result = result.map_err(truncation_advice);
                (
                    (!recovered).then_some(err),
                    Some(Msg::Chunk {
                        file,
                        index,
                        path: path.clone(),
                        count,
                        result,
                        key,
                    }),
                    std::mem::take(&mut followers),
                    ran,
                )
            }
            _ => {
                let step = run_unit(
                    &unit,
                    sources,
                    max_chunk_chars,
                    llm,
                    ids,
                    format_retries,
                    stop,
                    totals,
                    max_calls,
                    &take,
                    kept,
                );
                match step {
                    Step::Resumed(msg, calls) => (None, Some(msg), calls, true),
                    Step::Finished(outcomes, call) => (
                        call,
                        (!outcomes.is_empty()).then_some(Msg::Outcomes(outcomes)),
                        Vec::new(),
                        true,
                    ),
                    Step::Calls(outcomes, calls, call) => (
                        call,
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

/// Fil de veille : au premier SIGINT vu, demande l'arrêt à la politique, pose le drapeau d'arrêt,
/// réveille les fils, écrit A-12 (une fois) puis, si l'arrêt demandé est la cause, la trace
/// `politique:`. Sans garde installée le compteur reste à 0 : le fil ne fait que dormir.
fn watch_interrupt(
    policy: &Mutex<Policy>,
    shared: &Mutex<Shared<'_>>,
    wake: &Condvar,
    stop: &AtomicBool,
    workers_done: &AtomicBool,
) {
    loop {
        let done = workers_done.load(Ordering::SeqCst);
        if crate::interrupt::received() >= 1 {
            on_interrupt(policy, shared, wake, stop);
            return;
        }
        if done {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn on_interrupt(
    policy: &Mutex<Policy>,
    shared: &Mutex<Shared<'_>>,
    wake: &Condvar,
    stop: &AtomicBool,
) {
    let mut guard = policy.lock().unwrap_or_else(|p| p.into_inner());
    let posed = guard.request_stop();
    stop.store(true, Ordering::SeqCst);
    let queued: usize = shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .queue
        .iter()
        .map(Unit::file_count)
        .sum();
    drop(guard);
    // Réveille les fils qui attendent des appels.
    drop(shared.lock().unwrap_or_else(|p| p.into_inner()));
    wake.notify_all();
    warn(ANNOUNCE);
    if posed && trace::enabled() {
        trace!(
            "politique: arrêt de l'envoi ({}), {queued} fichiers non envoyés",
            CAUSE_INTERRUPT.label
        );
    }
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
    let _guard = crate::interrupt::Guard::install();
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
    enrich_pass(
        sources,
        files,
        opts,
        llm,
        store,
        report,
        stop,
        PassMode::Normal,
    )
}

/// Passe de l'enrichissement (REQ-214, DEC-168). Le pilote du mode lot pose `Collect` et `Last` ;
/// ni l'un ni l'autre ne veille sur Ctrl-C, c'est le pilote qui le fait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassMode {
    /// Passe ordinaire du mode synchrone.
    Normal,
    /// Passe de collecte : les requêtes sans réponse sont mises de côté par le collecteur ; rien
    /// n'est écrit, aucun avertissement ni compteur de progression.
    Collect,
    /// Dernière passe : comme `Normal`, sans plafond d'appels (les réponses sont connues) ; avec
    /// `capped`, le plafond de la commande est constaté à la fin (A-10).
    Last { capped: Option<usize> },
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn enrich_pass(
    sources: Sources<'_>,
    files: &[FileRecord],
    opts: &IndexOptions,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
    stop: &AtomicBool,
    mode: PassMode,
) -> Result<Vec<(String, Error)>> {
    #[cfg(test)]
    let _shared = crate::interrupt::test_shared();
    let max_calls = match mode {
        PassMode::Normal => opts.max_calls,
        _ => None,
    };
    let (concurrency, max_chunk_chars, batch_files, format_retries) = (
        opts.concurrency,
        opts.max_chunk_chars,
        opts.batch_files,
        opts.format_retries,
    );
    let ids = nodes_by_file(store)?;
    let kept = KeptCalls {
        results: store.call_results()?,
        model: llm.model().to_string(),
        language: llm.summary_language().to_string(),
        versions: store.prompt_versions(),
    };
    let started = Instant::now();
    let failed_before = report.failed;
    let mut succeeded = 0usize;
    // REQ-159 : un fichier sans nœud à demander est enregistré vide, sans lecture ni appel.
    let (with_nodes, without_nodes) = partition_by_nodes(files, &ids);
    let units = form_units_by_nodes(
        &with_nodes,
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
        split_calls: HashMap::new(),
    });
    let wake = Condvar::new();
    let policy = Mutex::new(Policy::with_cap(max_calls));
    let capped = AtomicBool::new(false);
    let workers_done = AtomicBool::new(false);
    let totals = Mutex::new(LlmTotals::default());
    let (tx, rx) = mpsc::channel::<Msg>();
    if !without_nodes.is_empty() {
        let empty = without_nodes
            .iter()
            .map(|f| Outcome::Done(f.clone(), Enrichment::default()))
            .collect();
        // Le récepteur est encore là : l'envoi ne peut pas échouer.
        let _ = tx.send(Msg::Outcomes(empty));
    }
    let mut write_error: Option<Error> = None;
    let mut failures: Vec<(String, Error)> = Vec::new();
    let mut partials: HashMap<String, Partial> = HashMap::new();
    trace!(
        "enrichissement: début {} fichiers, {workers} fils",
        files.len()
    );
    let mut meter = match mode {
        PassMode::Collect => None,
        _ => progress::Meter::start(Phase::Enrichment, files.len()),
    };

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
            totals: &totals,
            max_calls,
            capped: &capped,
            kept: &kept,
            answers_known: matches!(mode, PassMode::Last { .. }),
        };
        for _ in 0..workers {
            let tx = tx.clone();
            scope.spawn(move || {
                crate::interrupt::block_in_this_thread();
                worker(ctx, tx)
            });
        }
        drop(tx);
        if mode == PassMode::Normal {
            scope.spawn(|| watch_interrupt(&policy, &shared, &wake, stop, &workers_done));
        }
        // Un message reçu, puis tous ceux déjà arrivés (sans attente), sont écrits en une seule
        // transaction avant d'attendre le suivant : aucune transaction ouverte pendant un appel.
        let mut pending: Vec<EnrichmentWrite> = Vec::new();
        let mut pending_calls: Vec<CallWrite> = Vec::new();
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
                        path,
                        count,
                        result,
                        key,
                    } => {
                        let entry = partial_of(&mut partials, file, count);
                        match result {
                            Ok(x) => {
                                // Un résultat valide est conservé même si le fichier est déjà en
                                // échec (REQ-227), sauf erreur d'écriture.
                                if let (Some(key), None) = (key, &write_error) {
                                    pending_calls.push(CallWrite {
                                        path: file.path.clone(),
                                        key,
                                        result: x.clone(),
                                    });
                                }
                                if !entry.failed {
                                    let mut rank = vec![index];
                                    rank.extend(path);
                                    entry.results.insert(rank, x);
                                    entry.done += 1;
                                    adv = complete_file(
                                        entry,
                                        file,
                                        &mut succeeded,
                                        &write_error,
                                        &mut pending,
                                        &mut notes,
                                    );
                                }
                            }
                            // Résultat d'un fichier déjà en échec : ignoré.
                            Err(e) if !entry.failed => {
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
                            Err(_) => {}
                        }
                    }
                    Msg::Split { file, count, added } => {
                        partial_of(&mut partials, file, count).expected += added;
                    }
                    Msg::Kept {
                        file,
                        count,
                        results,
                    } => {
                        let entry = partial_of(&mut partials, file, count);
                        if !entry.failed {
                            for (index, x) in results {
                                entry.results.insert(vec![index], x);
                                entry.done += 1;
                            }
                            adv = complete_file(
                                entry,
                                file,
                                &mut succeeded,
                                &write_error,
                                &mut pending,
                                &mut notes,
                            );
                        }
                    }
                }
                advances.push((notes, adv));
                next = rx.try_recv().ok();
            }
            if mode == PassMode::Collect {
                pending.clear();
                pending_calls.clear();
                continue;
            }
            for w in &pending {
                report.summarized += w.summaries.len();
                report.requested += ids.get(&w.path).map_or(0, Vec::len);
            }
            flush_group(
                store,
                llm.model(),
                llm.summary_language(),
                &mut pending_calls,
                &mut pending,
                stop,
                &mut write_error,
            );
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
        // Tous les fils de travail ont rendu (leurs émetteurs sont fermés).
        workers_done.store(true, Ordering::SeqCst);
    });
    let failed_now = report.failed - failed_before;
    trace!(
        "enrichissement: fin {} ms, {succeeded} enrichis, {failed_now} en échec",
        started.elapsed().as_millis()
    );
    let totals = totals.into_inner().unwrap_or_else(|p| p.into_inner());
    trace!("{}", calls_line(&totals));
    if let Some(line) = sources.redact.and_then(|t| t.line()) {
        trace!("{line}");
    }
    report.llm = totals;
    let policy = policy.into_inner().unwrap_or_else(|p| p.into_inner());
    report.stop_cause = policy.stop_cause;
    report.refused = policy.refused;
    report.interrupted = policy.interrupted;
    // Plafond atteint : les fichiers sans issue (ni enrichis ni en échec) restent à enrichir.
    if policy.capped {
        report.capped = policy.limit;
        report.left_by_cap = files.len().saturating_sub(succeeded + failed_now);
    }
    // Dernière passe d'un lot : le plafond a été constaté avant l'envoi, la politique n'en a pas.
    if let PassMode::Last { capped: Some(m) } = mode {
        let left = files.len().saturating_sub(succeeded + failed_now);
        report.capped = Some(m);
        report.left_by_cap = left;
        trace!(
            "politique: arrêt de l'envoi ({}), {left} fichiers non envoyés",
            cap_label(m)
        );
    }
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
                Unit::Lot(v, _) => v
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
                (1, strs(&["bar", "m12"])),
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
    fn req144_ac3_noeud_demande_au_premier_morceau_seulement() {
        let text = |n: usize| -> String { (0..n).map(|i| format!("n{i} ")).collect() };
        let parts = vec![text(160), text(170), "rien".to_string()];
        let nodes: Vec<(String, String)> = (0..170)
            .map(|i| (format!("id{i}"), format!("n{i}")))
            .collect();
        let plan = call_plan(&parts, &nodes, &HashMap::new());
        let sizes: Vec<(usize, usize)> = plan.iter().map(|(k, l)| (*k, l.len())).collect();
        assert_eq!(sizes, [(0, 150), (0, 10), (1, 10), (2, 0)]);
    }

    #[test]
    fn req144_ac4_table_et_section_au_premier_morceau() {
        let parts = strs(&[
            "rien
 guide
",
            "ORDERS
",
            "orders
",
        ]);
        let nodes = pairs(&[("table:orders", "orders"), ("section:d.md#guide", "guide")]);
        let mut homes = HashMap::new();
        homes.insert("section:d.md#guide".to_string(), 2usize);
        assert_eq!(
            call_plan(&parts, &nodes, &homes),
            vec![
                (0, vec![]),
                (1, strs(&["table:orders"])),
                (2, strs(&["section:d.md#guide"])),
            ]
        );
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
        let total: usize = plan.iter().map(|(_, l)| l.len()).sum();
        assert_eq!(total, 20_000);
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
            plan.len() <= 20_000usize.div_ceil(150) + 5,
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
                            Unit::Lot(v, _) => {
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
        let trunc = Error::truncated("tronquée");
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

    /// Déclare un nœud `method` par fichier : sans nœud à demander, un fichier n'est pas envoyé
    /// (REQ-159).
    fn declare_one_node(store: &mut Store, files: &[FileRecord]) {
        for f in files {
            let node = Node::new(
                NodeKind::Method,
                &format!("{}.m", f.path),
                "m",
                Some(&f.path),
                None,
                None,
            );
            store.replace_structure(&f.path, &[node], &[]).unwrap();
        }
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
        let totals = Mutex::new(LlmTotals::default());
        let mut results = Vec::new();
        for unit in plan_calls(&file, text, max_chunk_chars, nodes)? {
            let Unit::Call {
                index,
                count,
                excerpt,
                part,
                requested,
                known,
                ..
            } = unit
            else {
                unreachable!()
            };
            results.push(run_call(
                llm,
                path,
                index,
                count,
                excerpt,
                &part,
                &requested,
                &known,
                retries,
                stop,
                &totals,
                &|_, _| true,
                &[],
                false,
            )?);
        }
        match results.len() {
            0 => Ok(Enrichment::default()),
            1 => Ok(results.remove(0)),
            _ => Ok(chunk::merge_enrichments(results)),
        }
    }

    /// Appels de `plan_calls` : `(index, count, morceau, liste, connus)`.
    type PlannedCall = (usize, usize, String, Vec<String>, Vec<String>);

    fn planned(path: &str, text: &str, max: usize, nodes: &[(String, String)]) -> Vec<PlannedCall> {
        let file = rec(path, text.len() as u64);
        plan_calls(&file, text, max, nodes)
            .unwrap()
            .into_iter()
            .map(|u| match u {
                Unit::Call {
                    index,
                    count,
                    part,
                    requested,
                    known,
                    ..
                } => (
                    index,
                    count,
                    part.to_string(),
                    requested,
                    known.iter().cloned().collect(),
                ),
                _ => unreachable!(),
            })
            .collect()
    }

    fn node(id: &str, short: &str) -> (String, String) {
        (id.to_string(), short.to_string())
    }

    #[test]
    fn req159_ac1_appels_a_liste_vide_retires() {
        // JSON : un seul nœud, au morceau 1 ; les morceaux 2 et 3 n'ont rien à demander.
        let text = "aaaaaaaaa\n".repeat(3);
        assert_eq!(chunk::split_spans(&text, 10).len(), 3);
        let nodes = [node("document:data/big.json", "")];
        let calls = planned("data/big.json", &text, 10, &nodes);
        assert_eq!(calls.len(), 1);
        let (index, count, part, requested, known) = &calls[0];
        assert_eq!((*index, *count), (1, 1));
        assert_eq!(part, &*excerpt::for_llm("data/big.json", &text, 10));
        assert_eq!(requested, &vec!["document:data/big.json".to_string()]);
        assert_eq!(known, &vec!["document:data/big.json".to_string()]);

        // Java : nœuds aux morceaux 1 et 3 seulement.
        let text = "alpha xxx\nbbbbbbbbb\ngamma xxx\n";
        let spans = chunk::split_spans(text, 10);
        assert_eq!(spans.len(), 3);
        let nodes = [node("class:A", "alpha"), node("method:A.g", "gamma")];
        let parts: Vec<String> = spans.iter().map(|(p, _)| p.clone()).collect();
        let homes = section_homes(text, &spans, &nodes);
        let plan = call_plan(&parts, &nodes, &homes);
        assert_eq!(plan.len(), 3);
        assert!(plan[1].1.is_empty());
        let calls = planned("src/A.java", text, 10, &nodes);
        assert_eq!(calls.len(), 2);
        let known: Vec<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
        for (n, k) in [(0usize, 0usize), (1, 2)] {
            let (index, count, part, requested, kn) = &calls[n];
            assert_eq!((*index, *count), (n + 1, 2));
            assert_eq!(part, &parts[k]);
            assert_eq!(requested, &plan[k].1);
            assert_eq!(kn, &known);
        }
    }

    /// AC-239-6
    #[test]
    fn req239_ac6_donnees_un_seul_appel() {
        let nodes_of = |p: &str| [node(&format!("document:{p}"), "")];
        for path in ["data/big.json", "d.yaml", "d.xml"] {
            let text = match path {
                "d.xml" => format!("<r>{}</r>", "<a>x</a>".repeat(71_000)),
                "d.yaml" => "k: v\n".repeat(100_000),
                _ => format!("[{}0]", "1,".repeat(250_000)),
            };
            assert!(text.chars().count() >= 500_000, "{path}");
            for max in [60_000, 10] {
                let file = rec(path, text.len() as u64);
                let units = plan_calls(&file, &text, max, &nodes_of(path)).unwrap();
                assert_eq!(units.len(), 1, "{path} {max}");
                match &units[0] {
                    Unit::Call {
                        index,
                        count,
                        excerpt,
                        part,
                        ..
                    } => {
                        assert_eq!((*index, *count), (1, 1));
                        assert!(excerpt.is_none());
                        assert_eq!(&**part, &*excerpt::for_llm(path, &text, max));
                        assert!(part.starts_with("[extrait tronqué"), "{path} {max}");
                    }
                    _ => unreachable!(),
                }
            }
        }
        // Fichier de build : même plan qu'avant.
        let text = "x\n".repeat(250_000);
        let calls = planned("pom.xml", &text, 60_000, &nodes_of("pom.xml"));
        assert_eq!(calls.len(), 1);
        assert!(calls[0].2.len() <= 60_000 && !calls[0].2.starts_with("[extrait"));
    }

    #[test]
    fn req169_ac5_excerpt_of_each_call() {
        // Cinq lignes de 10 octets, limite 20 : trois morceaux (lignes 1-2, 3-4, 5).
        let text = "alpha xxx\nalpha yyy\nbbbbbbbbb\nbbbbbbbbb\ngamma xxx\n";
        let spans = chunk::split_spans_for("src/A.java", text, 20);
        assert_eq!(spans.len(), 3);
        let line_of = |pos: usize| text[..pos].matches('\n').count();
        let first = |k: usize| line_of(spans[k].1) + 1;
        let last = |k: usize| {
            spans
                .get(k + 1)
                .map_or(text.lines().count(), |s| line_of(s.1))
        };
        assert_eq!((first(0), last(0)), (1, 2));
        assert_eq!((first(2), last(2)), (5, 5));
        // 160 nœuds au morceau 1 (2 groupes), aucun au morceau 2, un au morceau 3.
        let mut nodes: Vec<(String, String)> = (0..160)
            .map(|i| node(&format!("method:A.m{i}"), "alpha"))
            .collect();
        nodes.push(node("method:A.g", "gamma"));
        let file = rec("src/A.java", text.len() as u64);
        let units = plan_calls(&file, text, 20, &nodes).unwrap();
        let got: Vec<(usize, usize, Option<Excerpt>)> = units
            .iter()
            .map(|u| match u {
                Unit::Call {
                    index,
                    count,
                    excerpt,
                    ..
                } => (*index, *count, *excerpt),
                _ => unreachable!(),
            })
            .collect();
        let ex = |rank: usize, k: usize| {
            Some(Excerpt {
                rank,
                total: 3,
                first_line: first(k),
                last_line: last(k),
            })
        };
        assert_eq!(
            got,
            vec![(1, 3, ex(1, 0)), (2, 3, ex(1, 0)), (3, 3, ex(3, 2))]
        );
        // Un seul morceau, trois groupes : aucun extrait.
        let many: Vec<(String, String)> = (0..400)
            .map(|i| node(&format!("method:A.m{i}"), "alpha"))
            .collect();
        let units = plan_calls(&file, text, 1000, &many).unwrap();
        assert_eq!(units.len(), 3);
        for u in &units {
            let Unit::Call { excerpt, .. } = u else {
                unreachable!()
            };
            assert_eq!(*excerpt, None);
        }
    }

    #[test]
    fn req159_ac2_markdown_garde_les_appels_a_liste_vide() {
        let text = "# t0\nxxxxxxx\n# t2\n";
        let spans = chunk::split_spans(text, 8);
        assert_eq!(spans.len(), 3);
        let nodes = [
            node("document:docs/g.md", ""),
            node("section:docs/g.md#t0", "t0"),
            node("section:docs/g.md#t2", "t2"),
        ];
        let homes = section_homes(text, &spans, &nodes);
        let parts: Vec<String> = spans.into_iter().map(|(p, _)| p).collect();
        let plan = call_plan(&parts, &nodes, &homes);
        assert_eq!(plan.len(), 3);
        assert!(plan[1].1.is_empty());
        let calls = planned("docs/g.md", text, 8, &nodes);
        assert_eq!(calls.len(), 3);
        for (i, c) in calls.iter().enumerate() {
            assert_eq!((c.0, c.1), (i + 1, 3));
            assert_eq!(c.2, parts[plan[i].0]);
            assert_eq!(c.3, plan[i].1);
        }
        // Sans titre : seul le nœud `document:` existe, un seul appel.
        let text = "xxxxxxx\n".repeat(3);
        assert_eq!(chunk::split_spans(&text, 8).len(), 3);
        let calls = planned("docs/n.md", &text, 8, &[node("document:docs/n.md", "")]);
        assert_eq!(calls.len(), 1);
        assert_eq!((calls[0].0, calls[0].1), (1, 1));
    }

    #[test]
    fn req159_ac3_plafond_apres_filtrage() {
        let stop = AtomicBool::new(false);
        let run = |lines: usize, with_node: usize| {
            let text: String = (0..lines).map(|i| format!("n{i:02} xxxxx\n")).collect();
            assert_eq!(chunk::split(&text, 10).len(), lines);
            let nodes: Vec<(String, String)> = (0..with_node)
                .map(|i| (format!("id{i}"), format!("n{i:02}")))
                .collect();
            let llm = Counting(std::sync::atomic::AtomicUsize::new(0));
            let r = enrich_chunks(&llm, "x", &text, 10, &nodes, 0, &stop);
            (r, llm.0.load(Ordering::SeqCst))
        };
        let (r, calls) = run(60, 50);
        assert!(r.is_ok());
        assert_eq!(calls, 50);
        let (r, calls) = run(61, 51);
        assert_eq!(calls, 0);
        let e = r.unwrap_err();
        assert_eq!(e, Error::TooManyCalls { calls: 51, max: 50 });
        assert_eq!(
            format!("avertissement: x : {e}"),
            "avertissement: x : trop d'appels d'enrichissement (51 nécessaires, au plus 50 par fichier), aucun envoyé ; excluez ce fichier avec exclude dans .nephila.toml"
        );
        let (r, calls) = run(51, 0);
        assert_eq!(r.unwrap(), Enrichment::default());
        assert_eq!(calls, 0);
    }

    #[test]
    fn req048_ac1_plafond_de_50_appels() {
        let stop = AtomicBool::new(false);
        let run = |lines: usize| {
            let text: String = (0..lines).map(|i| format!("n{i:02} xxxxx\n")).collect();
            assert_eq!(chunk::split(&text, 10).len(), lines);
            let nodes: Vec<(String, String)> = (0..lines)
                .map(|i| (format!("id{i}"), format!("n{i:02}")))
                .collect();
            let llm = Counting(std::sync::atomic::AtomicUsize::new(0));
            let r = enrich_chunks(&llm, "x", &text, 10, &nodes, 0, &stop);
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
            code: 400,
            detail: None,
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
    fn req214_trace_detail_batch_item_ne_recopie_pas_le_fournisseur() {
        let e = Error::BatchItem {
            cause: "api_error : secret du fournisseur".into(),
            refused: None,
        };
        assert_eq!(trace_detail(&e), "requête du lot du fournisseur en échec");
    }

    struct Failing(fn() -> Error);
    impl Llm for Failing {
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            Err((self.0)())
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err((self.0)())
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err((self.0)())
        }
    }

    #[test]
    fn req214_verbose_capped_et_deferred_ne_paniquent_pas_et_ne_tracent_pas_de_fin() {
        let _t = crate::interrupt::test_lock();
        let stop = AtomicBool::new(false);
        let totals = Mutex::new(LlmTotals::default());
        let attempt = Attempt {
            current: 1,
            total: 1,
        };
        let items = [LotItem {
            file: "a.md".into(),
            text: "t".into(),
            node_ids: vec![],
        }];
        for make in [(|| Error::Capped) as fn() -> Error, || Error::Deferred] {
            let llm = Failing(make);
            let (results, events) = crate::trace::capture_all(true, || {
                (
                    traced_enrich(
                        &llm,
                        "a.md",
                        1,
                        1,
                        None,
                        "t",
                        &[],
                        &[],
                        attempt,
                        None,
                        &stop,
                        &totals,
                        &[],
                        false,
                    ),
                    traced_lot_once(&llm, &items, attempt, None, &stop, &totals),
                )
            });
            assert_eq!(results.0.unwrap_err(), make());
            assert_eq!(results.1.unwrap_err(), make());
            assert_eq!(events.len(), 2, "{events:?}");
            assert!(events.iter().all(|e| e.starts_with("llm enrich départ")));
        }
        // Une requête du lot en échec garde sa ligne de fin, au texte fixe.
        let llm = Failing(|| Error::BatchItem {
            cause: "api_error : secret".into(),
            refused: None,
        });
        let (_, events) = crate::trace::capture_all(true, || {
            traced_lot_once(&llm, &items, attempt, None, &stop, &totals)
        });
        let fin = events.last().unwrap();
        assert!(fin.contains("erreur") && fin.contains("requête du lot du fournisseur en échec"));
        assert!(!fin.contains("secret"), "{fin}");
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
        let r: Result<()> = with_format_retries(2, &AtomicBool::new(false), |n, _| {
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
        store.insert_files(&files).unwrap();
        declare_one_node(&mut store, &files);
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 2,
            max_chunk_chars: 1000,
            batch_files: 1,
            format_retries: u32::MAX,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
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
                redact: None,
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
        declare_one_node(&mut store, &files);
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files: 4,
            format_retries: 0,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
        };
        progress::init(true);
        progress::take_captured();
        enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
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
                Err(Error::truncated("tronquée"))
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
        declare_one_node(&mut store, &files);
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files: 5,
            format_retries: 0,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
        };
        let llm = Cut(Mutex::new(Vec::new()));
        progress::init(true);
        progress::take_captured();
        let failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
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

    /// Faux `Llm` du second passage (DEC-161) : un lot qui ne contient aucun fichier déjà omis est
    /// de premier passage et omet son rang `omit` (à partir de 0, ceux qui existent) ; un lot
    /// qui en contient un est de second passage et omet `omit_again`. `truncate` : le lot de ce
    /// nom est tronqué. Les appels sont notés dans l'ordre (`"abcd"`, `"b"`…).
    struct Passes {
        calls: Mutex<Vec<String>>,
        omitted: Mutex<Vec<String>>,
        omit: Vec<usize>,
        omit_again: Vec<String>,
        truncate: Option<&'static str>,
    }

    impl Passes {
        fn new(omit: &[usize], omit_again: &[&str], truncate: Option<&'static str>) -> Passes {
            Passes {
                calls: Mutex::new(Vec::new()),
                omitted: Mutex::new(Vec::new()),
                omit: omit.to_vec(),
                omit_again: omit_again.iter().map(|s| s.to_string()).collect(),
                truncate,
            }
        }
    }

    impl Llm for Passes {
        fn enrich(
            &self,
            file: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls
                .lock()
                .unwrap()
                .push(file.trim_end_matches(".txt").into());
            Ok(Enrichment::default())
        }
        fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            let names: Vec<&str> = items
                .iter()
                .map(|i| i.file.trim_end_matches(".txt"))
                .collect();
            self.calls.lock().unwrap().push(names.concat());
            if self.truncate == Some(names.concat().as_str()) {
                return Err(Error::truncated("tronquée"));
            }
            let mut omitted = self.omitted.lock().unwrap();
            let second = names.iter().any(|n| omitted.iter().any(|o| o == n));
            let mut out = Vec::new();
            for (rank, it) in items.iter().enumerate() {
                let name = names[rank];
                let drop_it = if second {
                    self.omit_again.iter().any(|o| o == name)
                } else {
                    self.omit.contains(&rank)
                };
                if drop_it {
                    omitted.push(name.to_string());
                    out.push((
                        it.file.clone(),
                        Err(Error::Message(format!(
                            "{} : absent de la réponse du lot",
                            it.file
                        ))),
                    ));
                } else {
                    out.push((it.file.clone(), Ok(Enrichment::default())));
                }
            }
            Ok(out)
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    type PassesRun = (Vec<String>, Vec<(String, Error)>, BuildReport, Vec<String>);

    /// Fichiers `<nom>.txt` d'un dépôt temporaire, enrichis avec `llm` (`concurrency = 1`) ;
    /// rend les appels, les échecs, le rapport et les lignes de progression.
    fn run_passes(names: &[&str], batch_files: usize, llm: &Passes) -> PassesRun {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for n in names {
            let name = format!("{n}.txt");
            std::fs::write(dir.path().join(&name), "texte\n").unwrap();
            files.push(rec(&name, 6));
        }
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(&files).unwrap();
        declare_one_node(&mut store, &files);
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files,
            format_retries: 0,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
        };
        progress::init(true);
        progress::take_captured();
        let failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            &files,
            &opts,
            llm,
            &mut store,
            &mut report,
            &AtomicBool::new(false),
        )
        .unwrap();
        let lines = progress::take_captured();
        progress::init(false);
        let calls = llm.calls.lock().unwrap().clone();
        (calls, failures, report, lines)
    }

    /// AC-199-4 : les fichiers omis d'un lot de premier passage sont renvoyés une fois, en tête
    /// de file, et avancent une seule fois dans la progression.
    #[test]
    fn req199_ac4_absent_files_are_resent_once_in_order() {
        let llm = Passes::new(&[1, 3], &[], None);
        let (calls, failures, report, lines) = run_passes(&["a", "b", "c", "d", "e"], 4, &llm);
        assert_eq!(calls, ["abcd", "bd", "e"]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(
            lines,
            [
                "progression: enrichissement 0 % (0/5 fichiers)",
                "progression: enrichissement 40 % (2/5 fichiers)",
                "progression: enrichissement 80 % (4/5 fichiers)",
                "progression: enrichissement 100 % (5/5 fichiers)",
            ]
        );
    }

    #[test]
    fn req199_ac4_truncated_lot_halves_keep_resending_their_absent_files() {
        let llm = Passes::new(&[1], &[], Some("abcd"));
        let (calls, failures, report, lines) = run_passes(&["a", "b", "c", "d"], 4, &llm);
        assert_eq!(calls, ["abcd", "ab", "b", "cd", "d"]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(
            lines.last().unwrap(),
            "progression: enrichissement 100 % (4/4 fichiers)"
        );
    }

    /// Faux `Llm` d'AC-200-4 : un lot est non conforme (`Format`), sauf si `only` nomme un autre
    /// lot, qui est alors tronqué ; un fichier seul est conforme.
    struct Bad {
        only: Option<&'static str>,
        calls: Mutex<Vec<String>>,
    }

    impl Llm for Bad {
        fn enrich(
            &self,
            file: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls
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
            self.calls.lock().unwrap().push(names.clone());
            match self.only {
                Some(o) if o != names => Err(Error::truncated("tronquée")),
                _ => Err(Error::Format("réponse non conforme".into())),
            }
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    fn run_bad(names: &[&str], batch_files: usize, llm: &Bad) -> (Vec<(String, Error)>, usize) {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for n in names {
            let name = format!("{n}.txt");
            std::fs::write(dir.path().join(&name), "texte\n").unwrap();
            files.push(rec(&name, 6));
        }
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(&files).unwrap();
        declare_one_node(&mut store, &files);
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files,
            format_retries: 0,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
        };
        let failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            &files,
            &opts,
            llm,
            &mut store,
            &mut report,
            &AtomicBool::new(false),
        )
        .unwrap();
        (failures, report.failed)
    }

    /// AC-200-4 : un lot de 4 fichiers ou plus non conforme est découpé une fois ; ses moitiés,
    /// de second passage, ne sont plus découpées ; chaque fichier échoue une seule fois.
    #[test]
    fn req200_ac4_non_conforming_lot_is_split_once_in_order() {
        let llm = Bad {
            only: None,
            calls: Mutex::new(Vec::new()),
        };
        let (failures, failed) = run_bad(&["a", "b", "c", "d", "e", "f", "g"], 5, &llm);
        assert_eq!(*llm.calls.lock().unwrap(), ["abcde", "abc", "de", "fg"]);
        let mut paths: Vec<&str> = failures.iter().map(|f| f.0.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt", "f.txt", "g.txt"]
        );
        assert_eq!(failed, 7);
    }

    #[test]
    fn req200_ac4_halves_of_the_split_keep_their_own_course() {
        let llm = Bad {
            only: Some("abcd"),
            calls: Mutex::new(Vec::new()),
        };
        let (failures, failed) = run_bad(&["a", "b", "c", "d"], 4, &llm);
        assert_eq!(
            *llm.calls.lock().unwrap(),
            ["abcd", "ab", "a", "b", "cd", "c", "d"]
        );
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(failed, 0);
    }

    /// DEC-161 (3) : un fichier encore absent de la réponse du lot renvoyé échoue, sans
    /// troisième envoi.
    #[test]
    fn req199_second_pass_still_absent_file_fails_without_third_send() {
        let llm = Passes::new(&[1, 2], &["c"], None);
        let (calls, failures, report, _) = run_passes(&["a", "b", "c"], 3, &llm);
        assert_eq!(calls, ["abc", "bc"]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].0, "c.txt");
        assert!(
            failures[0]
                .1
                .to_string()
                .contains("absent de la réponse du lot"),
            "{}",
            failures[0].1
        );
        assert_eq!(report.failed, 1);
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
            // Une ligne de 10 octets par morceau (`max_chunk_chars = 10`), de mot unique `w<i>`.
            let text: String = (0..*lines).map(|i| format!("w{i} xxxxxxx\n")).collect();
            std::fs::write(dir.path().join(name), text).unwrap();
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
        probe_run_capped(dir, files, llm, concurrency, batch_files, None, 0)
    }

    fn probe_run_capped(
        dir: &tempfile::TempDir,
        files: &[FileRecord],
        llm: &dyn Llm,
        concurrency: usize,
        batch_files: usize,
        max_calls: Option<usize>,
        format_retries: u32,
    ) -> (Store, BuildReport, Vec<(String, Error)>) {
        probe_run_mode(
            dir,
            files,
            llm,
            concurrency,
            batch_files,
            max_calls,
            format_retries,
            PassMode::Normal,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn probe_run_mode(
        dir: &tempfile::TempDir,
        files: &[FileRecord],
        llm: &dyn Llm,
        concurrency: usize,
        batch_files: usize,
        max_calls: Option<usize>,
        format_retries: u32,
        mode: PassMode,
    ) -> (Store, BuildReport, Vec<(String, Error)>) {
        probe_run_extra(
            dir,
            files,
            llm,
            concurrency,
            batch_files,
            max_calls,
            format_retries,
            mode,
            &[],
        )
    }

    /// Comme `probe_run_mode`, avec des nœuds en plus : `(fichier, nom court)`, déclarés comme
    /// méthodes `<fichier>.<nom>`.
    #[allow(clippy::too_many_arguments)]
    fn probe_run_extra(
        dir: &tempfile::TempDir,
        files: &[FileRecord],
        llm: &dyn Llm,
        concurrency: usize,
        batch_files: usize,
        max_calls: Option<usize>,
        format_retries: u32,
        mode: PassMode,
        extra: &[(&str, &str)],
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
        // Chaque morceau d'un fichier porte un nœud (REQ-159) : le mot de sa première ligne.
        for f in files {
            let text = std::fs::read_to_string(dir.path().join(&f.path)).unwrap();
            let mut nodes: Vec<Node> = text
                .lines()
                .map(|l| {
                    let word = l.split(' ').next().unwrap();
                    let qualified = format!("{}.{word}", f.path);
                    Node::new(
                        NodeKind::Method,
                        &qualified,
                        word,
                        Some(&f.path),
                        None,
                        None,
                    )
                })
                .collect();
            if f.path == "Big.txt" {
                nodes.extend(sections.iter().cloned());
            }
            for (file, name) in extra {
                if *file == f.path {
                    let qualified = format!("{file}.{name}");
                    nodes.push(Node::new(
                        NodeKind::Method,
                        &qualified,
                        name,
                        Some(file),
                        None,
                        None,
                    ));
                }
            }
            store.replace_structure(&f.path, &nodes, &[]).unwrap();
        }
        let mut report = BuildReport::default();
        let opts = IndexOptions {
            jobs: 1,
            concurrency,
            max_chunk_chars: 10,
            batch_files,
            format_retries,
            enrich_code: false,
            max_calls,
            ..IndexOptions::default()
        };
        let failures = enrich_pass(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            files,
            &opts,
            llm,
            &mut store,
            &mut report,
            &AtomicBool::new(false),
            mode,
        )
        .unwrap();
        (store, report, failures)
    }

    #[test]
    fn req214_collect_pass_writes_nothing_and_keeps_files_to_enrich() {
        let (dir, files) = probe_repo(&[], 3);
        let llm = Probe::new(1, &[]);
        let (store, report, failures) =
            probe_run_mode(&dir, &files, &llm, 1, 1, None, 0, PassMode::Collect);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert!(store.enrichment_entry("S0.txt").unwrap().is_none());
        assert_eq!(left_paths(store).len(), 3);
        // Requêtes mises de côté : rien non plus, et aucun échec.
        let llm = Failing(|| Error::Deferred);
        let (store, report, failures) =
            probe_run_mode(&dir, &files, &llm, 1, 1, None, 0, PassMode::Collect);
        assert!(failures.is_empty() && report.failed == 0);
        assert_eq!(left_paths(store).len(), 3);
    }

    #[test]
    fn req235_collect_pass_counts_no_coverage() {
        let (dir, files) = probe_repo(&[], 3);
        let llm = Probe::new(1, &[]);
        let (_, report, _) = probe_run_mode(&dir, &files, &llm, 1, 1, None, 0, PassMode::Collect);
        assert_eq!((report.summarized, report.requested), (0, 0));
        // Témoin : une passe normale compte les nœuds demandés.
        let (dir, files) = probe_repo(&[], 3);
        let (_, report, _) = probe_run_mode(&dir, &files, &llm, 1, 1, None, 0, PassMode::Normal);
        assert!(report.requested > 0, "{report:?}");
    }

    #[test]
    fn req214_collect_pass_ignores_the_call_cap() {
        let (dir, files) = probe_repo(&[], 3);
        let llm = Probe::new(1, &[]);
        let (_, report, failures) =
            probe_run_mode(&dir, &files, &llm, 1, 1, Some(1), 0, PassMode::Collect);
        assert_eq!(llm.calls().len(), 3, "{:?}", llm.calls());
        assert_eq!(report.capped, None);
        assert!(failures.is_empty() && report.failed == 0);
    }

    #[test]
    fn req214_last_pass_ignores_the_call_cap() {
        let (dir, files) = probe_repo(&[], 3);
        let llm = Probe::new(1, &[]);
        let (store, report, _) = probe_run_mode(
            &dir,
            &files,
            &llm,
            1,
            1,
            Some(1),
            0,
            PassMode::Last { capped: None },
        );
        assert_eq!(left_paths(store).len(), 0);
        assert_eq!(report.capped, None);
        assert_eq!(report.left_by_cap, 0);
    }

    #[test]
    fn req216_last_pass_with_cap_reports_the_files_left() {
        let (dir, files) = probe_repo(&[], 3);
        // Collecteur fermé : aucune requête inconnue ne part, aucun fichier n'a d'issue.
        let llm = Failing(|| Error::Capped);
        let (events, (store, report, failures)) = {
            let (r, e) = crate::trace::capture_all(true, || {
                probe_run_mode(
                    &dir,
                    &files,
                    &llm,
                    1,
                    1,
                    Some(1),
                    0,
                    PassMode::Last { capped: Some(2) },
                )
            });
            (e, r)
        };
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.capped, Some(2));
        assert_eq!(report.left_by_cap, 3);
        assert_eq!(left_paths(store).len(), 3);
        assert!(
            events
                .iter()
                .any(|e| e == "politique: arrêt de l'envoi (plafond de 2 appels atteint), 3 fichiers non envoyés"),
            "{events:?}"
        );
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

    /// Faux `Llm` de 6 appels à 3 fils : l'appel 1 échoue pendant que les appels 2 et 3 sont en
    /// vol ; ceux-ci ne répondent qu'après l'échec (AC-227-2). Une barrière les réunit, aucune
    /// attente n'est fixée en temps.
    struct FailFirstWhileOthersFly {
        barrier: std::sync::Barrier,
        failed: AtomicBool,
        calls: Mutex<Vec<usize>>,
    }

    impl Llm for FailFirstWhileOthersFly {
        fn enrich(
            &self,
            _: &str,
            call: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls.lock().unwrap().push(call);
            if call > 3 {
                return Ok(Enrichment::default());
            }
            self.barrier.wait();
            if call == 1 {
                self.failed.store(true, Ordering::SeqCst);
                return Err(Error::Format("réponse non conforme".into()));
            }
            while !self.failed.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            // Laisse le fil de l'appel 1 poser l'échec du fichier avant de prendre un appel.
            for _ in 0..5000 {
                std::thread::yield_now();
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

    #[test]
    fn req227_ac2_appels_en_vol_apres_l_echec_conserves_les_suivants_ne_partent_pas() {
        let (dir, files) = probe_repo(&[("Big.txt", 6)], 0);
        let llm = FailFirstWhileOthersFly {
            barrier: std::sync::Barrier::new(3),
            failed: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        };
        let (store, report, failures) = probe_run(&dir, &files, &llm, 3, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(names(&failures), ["Big.txt"]);
        let mut sent = llm.calls.lock().unwrap().clone();
        sent.sort_unstable();
        assert_eq!(sent, [1, 2, 3], "l'appel 4 et les suivants ne partent pas");
        assert_eq!(store.enriched_hash("Big.txt").unwrap(), None);
        assert_eq!(
            store.call_results().unwrap().len(),
            2,
            "appels 2 et 3 conservés"
        );
        drop(store);

        let next = Probe::new(0, &[]);
        let (store, report, failures) = probe_run(&dir, &files, &next, 3, 1);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        let mut calls = next.calls();
        calls.sort_unstable();
        assert_eq!(calls, ["Big.txt#1", "Big.txt#4", "Big.txt#5", "Big.txt#6"]);
        assert!(store.enriched_hash("Big.txt").unwrap().is_some());
        assert!(store.call_results().unwrap().is_empty());
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

    #[test]
    fn req214_ac4_requete_mise_de_cote_ne_fait_pas_sauter_les_appels_du_fichier() {
        struct DeferSecond(Mutex<Vec<usize>>);
        impl Llm for DeferSecond {
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
                    return Err(Error::Deferred);
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
        let llm = DeferSecond(Mutex::new(Vec::new()));
        let (_, report, failures) = probe_run(&dir, &files, &llm, 1, 1);
        assert_eq!(*llm.0.lock().unwrap(), [1, 2, 3, 4]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert!(report.stop_cause.is_none());
    }

    // ---- Arrêt demandé par Ctrl-C (REQ-189 à REQ-191) ----

    #[test]
    fn req189_ac7_request_stop_without_cause_sets_it() {
        let mut p = Policy::default();
        assert!(p.request_stop());
        assert!(p.interrupted);
        assert_eq!(p.stop_cause.as_deref(), Some(CAUSE_INTERRUPT.cause));
        assert_eq!(p.stop_label.as_deref(), Some("Ctrl-C reçu"));
        assert!(p.refused.is_none() && !p.capped);
        // Une seconde demande ne change rien.
        assert!(!p.request_stop());
        assert!(p.interrupted);
    }

    #[test]
    fn req189_ac7_request_stop_after_refused_key_changes_nothing() {
        let mut p = Policy::default();
        let t = p.dispatch();
        assert!(p.record(t, Some(&Error::InvalidKey("k".into()))));
        assert!(!p.request_stop());
        assert!(!p.interrupted);
        assert_eq!(p.refused, Some(Error::InvalidKey("k".into())));
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("clé d'API refusée par le fournisseur")
        );
    }

    #[test]
    fn req189_ac7_request_stop_after_cap_sets_cause_and_keeps_cap() {
        let mut p = Policy::with_cap(Some(0));
        assert!(!p.take(1));
        assert!(p.request_stop());
        assert!(p.capped && p.interrupted);
        assert_eq!(p.stop_cause.as_deref(), Some(CAUSE_INTERRUPT.cause));
        assert!(p.refused.is_none());
    }

    #[test]
    fn req189_ac7_refused_key_after_request_keeps_the_cause() {
        let mut p = Policy::default();
        assert!(p.request_stop());
        let t = p.dispatch();
        assert!(p.record(t, Some(&Error::InvalidKey("k".into()))));
        assert!(p.refused.is_none());
        assert_eq!(p.stop_cause.as_deref(), Some(CAUSE_INTERRUPT.cause));
        assert_eq!(p.stop_label.as_deref(), Some(CAUSE_INTERRUPT.label));
    }

    #[test]
    fn req191_texts_are_the_designed_constants() {
        assert_eq!(CAUSE_INTERRUPT.label, "Ctrl-C reçu");
        assert_eq!(
            CAUSE_INTERRUPT.cause,
            "Ctrl-C reçu : relancez nephila update pour continuer"
        );
        assert_eq!(
            ANNOUNCE,
            "Ctrl-C reçu : plus aucun appel ne part, ceux en cours vont à leur terme et seront enregistrés ; appuyez de nouveau sur Ctrl-C pour quitter sans les attendre"
        );
    }

    fn sigint_action() -> (usize, libc::c_int) {
        // SAFETY: lecture de la disposition courante dans une structure locale.
        unsafe {
            let mut a: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(libc::SIGINT, std::ptr::null(), &mut a), 0);
            (
                a.sa_sigaction,
                a.sa_flags & (libc::SA_RESTART | libc::SA_SIGINFO),
            )
        }
    }

    /// Dépôt de `n` petits fichiers, chacun avec un nœud, dans un graphe prêt à enrichir.
    fn interrupt_store(n: usize) -> (tempfile::TempDir, Vec<FileRecord>) {
        let (dir, files) = probe_repo(&[], n);
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(&files).unwrap();
        for f in &files {
            let node = Node::new(NodeKind::Method, &f.path, "x", Some(&f.path), None, None);
            store.replace_structure(&f.path, &[node], &[]).unwrap();
        }
        (dir, files)
    }

    fn interrupt_opts(concurrency: usize) -> IndexOptions {
        IndexOptions {
            jobs: 1,
            concurrency,
            max_chunk_chars: 10,
            batch_files: 1,
            format_retries: 0,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
        }
    }

    #[test]
    fn req189_ac8_enrich_all_restores_sigint_disposition() {
        let _t = crate::interrupt::test_lock();
        let before = sigint_action();
        let (dir, files) = interrupt_store(2);
        let mut store = Store::open_existing(dir.path()).unwrap();
        let llm = Probe::new(1, &[]);
        let mut report = BuildReport::default();
        enrich_all(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            &files,
            &interrupt_opts(1),
            &llm,
            &mut store,
            &mut report,
        )
        .unwrap();
        assert_eq!(sigint_action(), before);
        assert!(!report.interrupted && report.stop_cause.is_none());
    }

    #[test]
    fn req189_ac8_enrich_all_restores_sigint_disposition_after_write_error() {
        let _t = crate::interrupt::test_lock();
        let before = sigint_action();
        let (dir, files) = interrupt_store(2);
        let mut store = Store::open_readonly(dir.path()).unwrap();
        let llm = Probe::new(1, &[]);
        let mut report = BuildReport::default();
        let r = enrich_all(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            &files,
            &interrupt_opts(1),
            &llm,
            &mut store,
            &mut report,
        );
        assert!(r.is_err());
        assert_eq!(sigint_action(), before);
    }

    /// Faux LLM qui envoie SIGINT au processus lors de son premier appel, puis attend que
    /// l'arrêt soit posé avant de répondre (aucun délai fixe).
    struct Interrupter<'a> {
        calls: std::sync::atomic::AtomicUsize,
        stop: &'a AtomicBool,
    }

    impl Interrupter<'_> {
        fn hit(&self) {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // SAFETY: signal envoyé à ce processus, dont le gestionnaire est installé.
                unsafe {
                    libc::kill(libc::getpid(), libc::SIGINT);
                }
                let start = Instant::now();
                while !self.stop.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(5)
                {
                    std::thread::yield_now();
                }
            }
        }
    }

    impl Llm for Interrupter<'_> {
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.hit();
            Ok(Enrichment::default())
        }
        fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            self.hit();
            Ok(items
                .iter()
                .map(|i| (i.file.clone(), Ok(Enrichment::default())))
                .collect())
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req189_req191_signal_stops_sending_and_records_the_cause() {
        let _t = crate::interrupt::test_lock();
        let (dir, files) = interrupt_store(4);
        let mut store = Store::open_existing(dir.path()).unwrap();
        let stop = AtomicBool::new(false);
        let llm = Interrupter {
            calls: std::sync::atomic::AtomicUsize::new(0),
            stop: &stop,
        };
        let mut report = BuildReport::default();
        let _guard = crate::interrupt::Guard::install();
        let failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            &files,
            &interrupt_opts(1),
            &llm,
            &mut store,
            &mut report,
            &stop,
        )
        .unwrap();
        assert!(failures.is_empty());
        // Le seul appel en vol a été enregistré ; plus aucun n'est parti.
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
        assert!(report.interrupted);
        assert_eq!(report.stop_cause.as_deref(), Some(CAUSE_INTERRUPT.cause));
        assert!(report.refused.is_none());
        assert_eq!(left_paths(store).len(), 3);
    }

    // ---- Plafond d'appels (REQ-186, REQ-187) ----

    #[test]
    fn req186_ac9_take_three_of_three_then_refused() {
        let mut p = Policy {
            budget: Some(3),
            ..Policy::default()
        };
        assert!(p.take(1) && p.take(1) && p.take(1));
        assert!(!p.capped);
        assert!(!p.take(1));
        assert!(p.capped);
    }

    #[test]
    fn req186_ac9_reservation_of_two_on_one_left_is_refused_and_keeps_the_rest() {
        let mut p = Policy {
            budget: Some(1),
            ..Policy::default()
        };
        assert!(!p.take(2));
        assert!(p.capped);
        assert_eq!(p.budget, Some(1));
        assert!(p.take(1));
    }

    #[test]
    fn req186_ac9_no_cap_never_refuses() {
        let mut p = Policy::default();
        for _ in 0..1000 {
            assert!(p.take(7));
        }
        assert!(!p.capped);
        assert_eq!(p.budget, None);
    }

    #[test]
    fn req186_ac9_cap_leaves_the_stop_cause_to_a_later_invalid_key() {
        let mut p = Policy {
            budget: Some(0),
            ..Policy::default()
        };
        assert!(!p.take(1));
        assert!(p.stop_cause.is_none() && p.stop_label.is_none() && p.refused.is_none());
        let t = p.dispatch();
        assert!(p.record(t, Some(&Error::InvalidKey("k".into()))));
        assert_eq!(p.refused, Some(Error::InvalidKey("k".into())));
        assert_eq!(
            p.stop_cause.as_deref(),
            Some("clé d'API refusée par le fournisseur")
        );
    }

    #[test]
    fn req186_cap_label_is_the_single_source() {
        assert_eq!(cap_label(3), "plafond de 3 appels atteint");
    }

    /// Chemins restés à enrichir (portée du code comprise : les fichiers du banc sont `.txt`).
    fn left_paths(mut store: Store) -> Vec<String> {
        store.set_enrich_code(true).unwrap();
        store
            .files_to_enrich()
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect()
    }

    fn unit_files(n: usize) -> Vec<FileRecord> {
        (0..n).map(|i| rec(&format!("F{i}.txt"), 1)).collect()
    }

    #[test]
    fn req186_next_unit_closes_the_queue_at_the_cap_without_touching_stop() {
        let files = unit_files(2);
        let shared = Mutex::new(Shared {
            queue: vec![
                Unit::Lot(vec![&files[0]], Pass::First),
                Unit::Lot(vec![&files[1]], Pass::First),
            ]
            .into(),
            failed: HashSet::new(),
            expanding: 0,
            split_calls: HashMap::new(),
        });
        let wake = Condvar::new();
        let stop = AtomicBool::new(false);
        let capped = AtomicBool::new(false);
        let first = next_unit(&shared, &wake, &stop, &capped);
        assert!(first.is_some());
        finish_unit(&shared, &wake, Vec::new(), true);
        capped.store(true, Ordering::SeqCst);
        assert!(next_unit(&shared, &wake, &stop, &capped).is_none());
        assert!(!stop.load(Ordering::SeqCst));
    }

    #[test]
    fn req186_next_unit_closed_queue_still_serves_reserved_calls_behind_a_lot() {
        let files = unit_files(2);
        let call = |f: &'static FileRecord| Unit::Call {
            file: f,
            index: 1,
            count: 1,
            excerpt: None,
            part: Arc::from("t"),
            requested: Vec::new(),
            known: Arc::from(Vec::<String>::new()),
            path: Vec::new(),
            cache: false,
            followers: Vec::new(),
        };
        let files: &'static [FileRecord] = Box::leak(files.into_boxed_slice());
        let shared = Mutex::new(Shared {
            queue: vec![
                Unit::Lot(vec![&files[0]], Pass::First),
                call(&files[1]),
                Unit::Lot(vec![&files[0]], Pass::First),
            ]
            .into(),
            failed: HashSet::new(),
            expanding: 0,
            split_calls: HashMap::new(),
        });
        let wake = Condvar::new();
        let stop = AtomicBool::new(false);
        let capped = AtomicBool::new(true);
        let got = next_unit(&shared, &wake, &stop, &capped);
        assert!(matches!(got, Some(Unit::Call { .. })), "{got:?}");
        assert!(next_unit(&shared, &wake, &stop, &capped).is_none());
        assert_eq!(shared.lock().unwrap().queue.len(), 2);
    }

    #[test]
    fn req186_ac2_cap_3_on_7_files_sends_3_calls_and_leaves_4() {
        let (dir, files) = probe_repo(&[], 7);
        let llm = Probe::new(0, &[]);
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 1, Some(3), 0);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(llm.calls(), ["S0.txt#1", "S1.txt#1", "S2.txt#1"]);
        assert_eq!(report.capped, Some(3));
        // Une prise refusée n'ajoute rien aux totaux de l'appel.
        assert_eq!(report.llm.calls, 3);
        assert_eq!(report.llm.format_retries, 0);
        assert_eq!(report.left_by_cap, 4);
        assert_eq!(report.failed, 0);
        assert!(report.stop_cause.is_none() && report.refused.is_none());
        let left = left_paths(store);
        assert_eq!(left, ["S3.txt", "S4.txt", "S5.txt", "S6.txt"]);
    }

    #[test]
    fn req186_ac4_work_that_fits_exactly_is_not_capped() {
        let (dir, files) = probe_repo(&[], 5);
        let llm = Probe::new(0, &[]);
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 1, Some(5), 0);
        assert!(failures.is_empty());
        assert_eq!(llm.calls().len(), 5);
        assert_eq!(report.capped, None);
        assert_eq!(report.left_by_cap, 0);
        assert!(left_paths(store).is_empty());
    }

    #[test]
    fn req186_ac10_no_cap_sends_everything() {
        let (dir, files) = probe_repo(&[], 7);
        let llm = Probe::new(0, &[]);
        let (_, report, _) = probe_run_capped(&dir, &files, &llm, 1, 1, None, 0);
        assert_eq!(llm.calls().len(), 7);
        assert_eq!((report.capped, report.left_by_cap), (None, 0));
    }

    /// Faux `Llm` dont le premier essai de chaque fichier est non conforme.
    struct FirstTryBad {
        seen: Mutex<HashMap<String, usize>>,
        sent: Mutex<Vec<String>>,
    }
    impl Llm for FirstTryBad {
        fn enrich(
            &self,
            file: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            let n = {
                let mut seen = self.seen.lock().unwrap();
                let n = seen.entry(file.to_string()).or_insert(0);
                *n += 1;
                *n
            };
            self.sent.lock().unwrap().push(format!("{file}:{n}"));
            if n == 1 {
                Err(Error::Format("réponse non conforme".into()))
            } else {
                Ok(Enrichment::default())
            }
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req186_ac5_refused_format_retry_leaves_the_file_without_outcome() {
        let (dir, files) = probe_repo(&[], 3);
        let llm = FirstTryBad {
            seen: Mutex::new(HashMap::new()),
            sent: Mutex::new(Vec::new()),
        };
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 1, Some(3), 2);
        assert_eq!(
            *llm.sent.lock().unwrap(),
            ["S0.txt:1", "S0.txt:2", "S1.txt:1"]
        );
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(report.capped, Some(3));
        assert_eq!(report.left_by_cap, 2);
        let left = left_paths(store);
        assert_eq!(left, ["S1.txt", "S2.txt"]);
    }

    fn two_small_then_big(lines: usize) -> (tempfile::TempDir, Vec<FileRecord>) {
        let (dir, mut files) = probe_repo(&[("Big.txt", lines)], 2);
        files.rotate_left(1);
        (dir, files)
    }

    #[test]
    fn req186_ac6_big_file_reservation_refused_then_accepted() {
        // 3 appels pour Big.txt, placé après deux petits fichiers.
        let (dir, files) = two_small_then_big(3);
        let llm = Probe::new(0, &[]);
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 1, Some(4), 0);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(llm.calls(), ["S0.txt#1", "S1.txt#1"]);
        assert_eq!(
            (report.capped, report.left_by_cap, report.failed),
            (Some(4), 1, 0)
        );
        assert_eq!(left_paths(store).len(), 1);

        let (dir, files) = two_small_then_big(3);
        let llm = Probe::new(0, &[]);
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 1, Some(5), 0);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(llm.calls().len(), 5);
        assert_eq!((report.capped, report.left_by_cap), (None, 0));
        assert!(left_paths(store).is_empty());
    }

    #[test]
    fn req186_ac6_big_file_alone_above_the_cap_fails_with_a11() {
        let (dir, files) = probe_repo(&[("Big.txt", 3)], 0);
        let llm = Probe::new(0, &[]);
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 1, Some(2), 0);
        assert!(llm.calls().is_empty());
        assert_eq!(names(&failures), ["Big.txt"]);
        assert_eq!(
            failures[0].1.to_string(),
            "trop d'appels d'enrichissement (3 nécessaires, au plus 2 par commande), aucun envoyé ; augmentez max_calls ou excluez ce fichier avec exclude dans .nephila.toml"
        );
        assert_eq!(report.failed, 1);
        assert_eq!((report.capped, report.left_by_cap), (None, 0));
        assert_eq!(left_paths(store).len(), 1);
    }

    #[test]
    fn req186_ac13_a11_does_not_stop_the_following_files() {
        let (dir, mut files) = probe_repo(&[("Big.txt", 3)], 1);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let llm = Probe::new(0, &[]);
        let (_, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 1, Some(2), 0);
        assert_eq!(names(&failures), ["Big.txt"]);
        assert_eq!(llm.calls(), ["S0.txt#1"]);
        assert_eq!((report.capped, report.left_by_cap), (None, 0));
    }

    #[test]
    fn req186_ac3_concurrency_4_cap_5_on_20_files_never_exceeds_5_attempts() {
        let (dir, files) = probe_repo(&[], 20);
        let llm = Probe::new(4, &[]);
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 4, 1, Some(5), 0);
        assert!(failures.is_empty());
        assert_eq!(llm.calls().len(), 5);
        assert_eq!(llm.max.load(Ordering::SeqCst), 4);
        assert_eq!(report.capped, Some(5));
        assert_eq!(report.left_by_cap, 15);
        assert_eq!(left_paths(store).len(), 15);
    }

    #[test]
    fn req186_lot_attempt_is_taken_once_for_the_whole_lot() {
        let (dir, files) = probe_repo(&[], 6);
        let llm = Probe::new(0, &[]);
        let (store, report, failures) = probe_run_capped(&dir, &files, &llm, 1, 3, Some(1), 0);
        assert!(failures.is_empty());
        assert_eq!(llm.calls(), ["lot:S0.txt,S1.txt,S2.txt"]);
        assert_eq!((report.capped, report.left_by_cap), (Some(1), 3));
        assert_eq!(left_paths(store).len(), 3);
    }

    /// Fichier `T.txt` de `lines` morceaux d'un nœud (`method:T.txt.w<i>`), dont le morceau
    /// `two_at` (à partir de 0) porte aussi le nœud `method:T.txt.zz`.
    fn split_repo(lines: usize, two_at: usize) -> (tempfile::TempDir, Vec<FileRecord>) {
        let dir = tempfile::tempdir().unwrap();
        let text: String = (0..lines)
            .map(|i| {
                if i == two_at {
                    format!("w{i} zz xxxx\n")
                } else {
                    format!("w{i} xxxxxxx\n")
                }
            })
            .collect();
        std::fs::write(dir.path().join("T.txt"), &text).unwrap();
        let files = vec![rec("T.txt", text.len() as u64)];
        (dir, files)
    }

    fn truncated() -> Error {
        Error::truncated("réponse tronquée par la limite de jetons")
    }

    /// Tronque une fois l'appel de deux nœuds du plan ; compte les envois.
    struct TruncOnce {
        calls: Mutex<Vec<(usize, Vec<String>)>>,
        done: AtomicBool,
    }

    impl Llm for TruncOnce {
        fn enrich(
            &self,
            _: &str,
            call: usize,
            _: &str,
            requested: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls.lock().unwrap().push((call, requested.to_vec()));
            if requested.len() == 2 && !self.done.swap(true, Ordering::SeqCst) {
                return Err(truncated());
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

    #[test]
    fn req230_ac6_sous_appels_comptes_dans_les_cinquante_appels() {
        // Plan de 48 appels dont un tronqué une fois : 48 + 2 sous-appels = 50.
        let (dir, files) = split_repo(48, 0);
        let llm = TruncOnce {
            calls: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
        };
        let (store, report, failures) = probe_run_extra(
            &dir,
            &files,
            &llm,
            1,
            1,
            None,
            0,
            PassMode::Normal,
            &[("T.txt", "zz")],
        );
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(llm.calls.lock().unwrap().len(), 50);
        assert!(store.enriched_hash("T.txt").unwrap().is_some());

        // Plan de 49 appels, même cas : 49 + 2 > 50, aucun sous-appel, échec de troncature.
        let (dir, files) = split_repo(49, 0);
        let llm = TruncOnce {
            calls: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
        };
        let (store, report, failures) = probe_run_extra(
            &dir,
            &files,
            &llm,
            1,
            1,
            None,
            0,
            PassMode::Normal,
            &[("T.txt", "zz")],
        );
        assert!(llm.calls.lock().unwrap().len() <= 49);
        assert_eq!(report.failed, 1);
        assert_eq!(names(&failures), ["T.txt"]);
        assert!(
            matches!(&failures[0].1, Error::Truncated { message: m, .. } if m.contains("réduisez max_chunk_chars")),
            "{failures:?}"
        );
        assert_eq!(store.enriched_hash("T.txt").unwrap(), None);
    }

    /// Porte à ouverture unique : `wait` bloque jusqu'à `open` (condition, pas d'attente active).
    struct Gate(Mutex<bool>, Condvar);

    impl Gate {
        fn new() -> Gate {
            Gate(Mutex::new(false), Condvar::new())
        }
        fn open(&self) {
            *self.0.lock().unwrap() = true;
            self.1.notify_all();
        }
        fn wait(&self) {
            let mut open = self.0.lock().unwrap();
            while !*open {
                open = self.1.wait(open).unwrap();
            }
        }
    }

    /// Faux `Llm` de l'ordre d'arrivée : le morceau 2 (deux nœuds) est tronqué ; en concurrence,
    /// le second sous-appel répond après le morceau 3 et le premier après le second. Les deux
    /// sous-appels donnent un résumé au même nœud. L'appel 1 reste en cours jusqu'au premier
    /// sous-appel : son fil est vivant quand la découpe met les sous-appels en file (un fil qui
    /// trouve la file vide s'arrête) et prend le second sous-appel. L'appel 3 ouvre `done3` et
    /// rend aussitôt.
    struct Reorder {
        ordered: bool,
        truncated: AtomicBool,
        done3: Gate,
        done_second: Gate,
        subs_started: Gate,
    }

    impl Llm for Reorder {
        fn enrich(
            &self,
            _: &str,
            call: usize,
            _: &str,
            requested: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            let answer = |text: &str| Enrichment {
                summaries: vec![("method:T.txt.w1".into(), text.into())],
                ..Enrichment::default()
            };
            if call != 2 {
                if call == 3 {
                    self.done3.open();
                }
                if call == 1 && self.ordered {
                    self.subs_started.wait();
                }
                return Ok(Enrichment::default());
            }
            if requested.len() == 2 && !self.truncated.swap(true, Ordering::SeqCst) {
                return Err(truncated());
            }
            self.subs_started.open();
            if requested == ["method:T.txt.w1"] {
                if self.ordered {
                    self.done_second.wait();
                }
                Ok(answer("du premier sous-appel"))
            } else {
                if self.ordered {
                    self.done3.wait();
                }
                self.done_second.open();
                Ok(answer("du second sous-appel"))
            }
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req230_ac9_fusion_dans_l_ordre_du_plan_quel_que_soit_l_ordre_d_arrivee() {
        let summary = |concurrency: usize| {
            let (dir, files) = split_repo(3, 1);
            let llm = Reorder {
                ordered: concurrency > 1,
                truncated: AtomicBool::new(false),
                done3: Gate::new(),
                done_second: Gate::new(),
                subs_started: Gate::new(),
            };
            let (store, report, failures) = probe_run_extra(
                &dir,
                &files,
                &llm,
                concurrency,
                1,
                None,
                0,
                PassMode::Normal,
                &[("T.txt", "zz")],
            );
            assert!(failures.is_empty(), "{failures:?}");
            assert_eq!(report.failed, 0);
            assert!(store.enriched_hash("T.txt").unwrap().is_some());
            store
                .node_by_id("method:T.txt.w1")
                .unwrap()
                .and_then(|n| n.summary)
        };
        let parallel = summary(3);
        assert_eq!(parallel.as_deref(), Some("du premier sous-appel"));
        assert_eq!(parallel, summary(1));
    }

    /// Fichiers `A`, `B`, `C`, `D` d'un morceau : `C` porte deux nœuds (`w0` et `zz`).
    struct Scripted {
        calls: Mutex<Vec<String>>,
    }

    impl Llm for Scripted {
        fn enrich(
            &self,
            file: &str,
            _: usize,
            _: &str,
            requested: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{file}:{}", requested.len()));
            match (file, requested.len()) {
                ("C", 2) => Err(truncated()),
                ("D", _) => Ok(Enrichment::default()),
                _ => Err(net()),
            }
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req230_ac12_troncature_decoupee_ni_comptee_ni_remise_a_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for f in ["A", "B", "C", "D"] {
            let text = if f == "C" {
                "w0 zz xxxx\n"
            } else {
                "w0 xxxxxxx\n"
            };
            std::fs::write(dir.path().join(f), text).unwrap();
            files.push(rec(f, text.len() as u64));
        }
        let llm = Scripted {
            calls: Mutex::new(Vec::new()),
        };
        let (_store, report, failures) = probe_run_extra(
            &dir,
            &files,
            &llm,
            1,
            1,
            None,
            0,
            PassMode::Normal,
            &[("C", "zz")],
        );
        // Deux erreurs réseau, la troncature de C découpée, puis l'erreur réseau de son premier
        // sous-appel : la troisième arrête l'envoi, D n'est pas envoyé.
        assert_eq!(*llm.calls.lock().unwrap(), ["A:1", "B:1", "C:2", "C:1"]);
        assert_eq!(names(&failures), ["A", "B", "C"]);
        assert!(failures.iter().all(|(_, e)| matches!(e, Error::Network(_))));
        assert!(report
            .stop_cause
            .as_deref()
            .is_some_and(|c| c.contains("panne réseau répétée")));
    }

    /// Lots de deux fichiers : les deux premiers et le deuxième échouent sur le réseau, le
    /// troisième est tronqué avec l'entrée de `E` complète.
    struct LotPartial {
        calls: Mutex<Vec<String>>,
        /// L'entrée de `F` est aussi complète : rien ne repart.
        all: bool,
    }

    impl Llm for LotPartial {
        fn enrich(
            &self,
            file: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls.lock().unwrap().push(file.to_string());
            Err(net())
        }
        fn enrich_lot(&self, items: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            let name: Vec<&str> = items.iter().map(|i| i.file.as_str()).collect();
            self.calls.lock().unwrap().push(name.join("+"));
            if name[0] == "E" {
                let cut = if self.all {
                    r#"{"file": "F", "summaries": [{"node": "F.w0", "summary": "Résumé F."}], "concepts": []}, {"file": "G"#
                } else {
                    r#"{"file": "F", "summ"#
                };
                let partial = format!(
                    r#"{{"version": 3, "files": [{{"file": "E", "summaries": [{{"node": "E.w0", "summary": "Résumé E."}}], "concepts": []}}, {cut}"#
                );
                return Err(Error::Truncated {
                    message: "réponse tronquée par la limite de jetons".into(),
                    partial,
                });
            }
            Err(net())
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req232_ac16_lot_tronque_recupere_ne_remet_pas_les_erreurs_reseau_a_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for f in ["A", "B", "C", "D", "E", "F", "G", "H"] {
            std::fs::write(dir.path().join(f), "w0 x\n").unwrap();
            files.push(rec(f, 5));
        }
        let llm = LotPartial {
            calls: Mutex::new(Vec::new()),
            all: false,
        };
        let (store, report, failures) =
            probe_run_extra(&dir, &files, &llm, 1, 2, None, 0, PassMode::Normal, &[]);
        // Deux lots en erreur réseau, le lot tronqué récupéré (E gardé, F repart seul), puis
        // l'erreur réseau de F : la troisième arrête l'envoi, G et H ne partent pas.
        assert_eq!(*llm.calls.lock().unwrap(), ["A+B", "C+D", "E+F", "F"]);
        assert_eq!(names(&failures), ["A", "B", "C", "D", "F"]);
        assert!(store.enriched_hash("E").unwrap().is_some());
        assert!(report
            .stop_cause
            .as_deref()
            .is_some_and(|c| c.contains("panne réseau répétée")));
    }

    #[test]
    fn req232_ac16_lot_tronque_tout_recu_ne_remet_pas_les_erreurs_reseau_a_zero() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for f in ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"] {
            std::fs::write(dir.path().join(f), "w0 x\n").unwrap();
            files.push(rec(f, 5));
        }
        let llm = LotPartial {
            calls: Mutex::new(Vec::new()),
            all: true,
        };
        let (store, report, failures) =
            probe_run_extra(&dir, &files, &llm, 1, 2, None, 0, PassMode::Normal, &[]);
        // Deux lots en erreur réseau, le lot tronqué dont tout est reçu (rien ne repart), puis
        // l'erreur réseau de G+H : la troisième arrête l'envoi, I et J ne partent pas.
        assert_eq!(*llm.calls.lock().unwrap(), ["A+B", "C+D", "E+F", "G+H"]);
        assert_eq!(names(&failures), ["A", "B", "C", "D", "G", "H"]);
        assert!(store.enriched_hash("E").unwrap().is_some());
        assert!(store.enriched_hash("F").unwrap().is_some());
        assert!(report
            .stop_cause
            .as_deref()
            .is_some_and(|c| c.contains("panne réseau répétée")));
    }

    #[test]
    fn req230_ac10_passe_de_collecte_seule_n_ecrit_rien() {
        let (dir, files) = split_repo(1, 0);
        let llm = TruncOnce {
            calls: Mutex::new(Vec::new()),
            done: AtomicBool::new(false),
        };
        let (store, report, failures) = probe_run_extra(
            &dir,
            &files,
            &llm,
            1,
            1,
            None,
            0,
            PassMode::Collect,
            &[("T.txt", "zz")],
        );
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert_eq!(llm.calls.lock().unwrap().len(), 3, "appel et 2 sous-appels");
        assert_eq!(store.enriched_hash("T.txt").unwrap(), None);
        assert!(store.call_results().unwrap().is_empty());
    }

    fn call_of(requested: &[&str], path: &[usize]) -> Unit<'static> {
        let file: &'static FileRecord = Box::leak(Box::new(rec("A.java", 1)));
        Unit::Call {
            file,
            index: 2,
            count: 3,
            excerpt: Some(Excerpt {
                rank: 2,
                total: 3,
                first_line: 4,
                last_line: 9,
            }),
            part: Arc::from("texte"),
            requested: requested.iter().map(|s| s.to_string()).collect(),
            known: Arc::from(vec!["x".to_string()]),
            path: path.to_vec(),
            cache: false,
            followers: Vec::new(),
        }
    }

    fn kept_of(nodes: &[&str], concepts: usize) -> Enrichment {
        Enrichment {
            summaries: nodes
                .iter()
                .map(|n| (n.to_string(), format!("Résumé de {n}.")))
                .collect(),
            concepts: (0..concepts)
                .map(|i| crate::llm::enrich::Concept {
                    name: format!("c{i}"),
                    summary: None,
                    sections: Vec::new(),
                })
                .collect(),
            rejected: Vec::new(),
        }
    }

    #[test]
    fn req232_recover_call_garde_les_resumes_et_reduit_la_liste_au_reste() {
        let unit = call_of(&["a", "b", "c", "d", "e"], &[2]);
        let Recovery::Rest(kept, sub) = recover_call(&unit, kept_of(&["a", "c"], 1)) else {
            panic!("reste attendu");
        };
        assert_eq!(kept.summaries.len(), 2);
        assert_eq!(kept.concepts.len(), 1);
        let Unit::Call {
            requested,
            path,
            index,
            count,
            part,
            excerpt,
            ..
        } = sub
        else {
            panic!("appel attendu");
        };
        assert_eq!(requested, ["b", "d", "e"]);
        assert_eq!(path, [2, 1]);
        assert_eq!((index, count, &*part), (2, 3, "texte"));
        assert_eq!(excerpt.unwrap().first_line, 4);
        // Les résumés hors liste ne comptent pas.
        assert!(matches!(
            recover_call(&unit, kept_of(&["x"], 0)),
            Recovery::Nothing
        ));
        // Rien de reçu, même avec un concept : la découpe s'applique.
        assert!(matches!(
            recover_call(&unit, kept_of(&[], 2)),
            Recovery::Nothing
        ));
    }

    #[test]
    fn req232_recover_call_tout_recu_et_liste_vide() {
        let unit = call_of(&["a", "b"], &[]);
        let Recovery::All(x) = recover_call(&unit, kept_of(&["b", "a"], 0)) else {
            panic!("tout attendu");
        };
        assert_eq!(x.summaries.len(), 2);
        let empty = call_of(&[], &[]);
        assert!(matches!(
            recover_call(&empty, kept_of(&[], 1)),
            Recovery::All(_)
        ));
        assert!(matches!(
            recover_call(&empty, kept_of(&[], 0)),
            Recovery::Nothing
        ));
    }

    /// Réponse tronquée dont les résumés complets sont ceux de `nodes`, puis un début d'élément.
    fn partial_of(nodes: &[(&str, &str)]) -> Error {
        let items: Vec<String> = nodes
            .iter()
            .map(|(n, t)| format!(r#"{{"node": "{n}", "summary": "{t}"}}"#))
            .collect();
        Error::Truncated {
            message: "réponse tronquée par la limite de jetons".into(),
            partial: format!(
                r#"{{"version": 2, "summaries": [{}, {{"nod"#,
                items.join(", ")
            ),
        }
    }

    /// Tronque une fois l'appel de deux nœuds du plan en gardant le résumé du premier.
    struct PartialOnce {
        calls: Mutex<Vec<Vec<String>>>,
        done: AtomicBool,
    }

    impl Llm for PartialOnce {
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            requested: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls.lock().unwrap().push(requested.to_vec());
            if requested.len() == 2 && !self.done.swap(true, Ordering::SeqCst) {
                return Err(partial_of(&[(&requested[0], "Reçu.")]));
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

    #[test]
    fn req232_ac11_sous_appel_unique_dans_les_cinquante_appels() {
        let run = |lines: usize| {
            let (dir, files) = split_repo(lines, 0);
            let llm = PartialOnce {
                calls: Mutex::new(Vec::new()),
                done: AtomicBool::new(false),
            };
            let (store, report, failures) = probe_run_extra(
                &dir,
                &files,
                &llm,
                1,
                1,
                None,
                0,
                PassMode::Normal,
                &[("T.txt", "zz")],
            );
            let n = llm.calls.lock().unwrap().len();
            (n, store, report, failures)
        };
        // Plan de 49 appels dont un tronqué : 49 + 1 = 50 appels, fichier enrichi.
        let (n, store, report, failures) = run(49);
        assert_eq!(n, 50);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(report.failed, 0);
        assert!(store.enriched_hash("T.txt").unwrap().is_some());
        assert_eq!(
            store
                .node_by_id("method:T.txt.w0")
                .unwrap()
                .and_then(|x| x.summary)
                .as_deref(),
            Some("Reçu.")
        );
        // Plan de 50 appels, même cas : aucun sous-appel, échec de troncature (A-5).
        let (n, store, report, failures) = run(50);
        assert!(n <= 50, "{n}");
        assert_eq!(report.failed, 1);
        assert!(
            matches!(&failures[0].1, Error::Truncated { message: m, .. } if m.contains("réduisez max_chunk_chars")),
            "{failures:?}"
        );
        assert_eq!(store.enriched_hash("T.txt").unwrap(), None);
    }

    /// Faux `Llm` de l'ordre d'arrivée : le morceau 2 (deux nœuds) est tronqué avec le résumé de
    /// `w1` ; en concurrence, le sous-appel de `zz` répond après le morceau 3 et donne aussi un
    /// autre résumé pour `w1`.
    struct RecoverReorder {
        ordered: bool,
        truncated: AtomicBool,
        done3: Gate,
    }

    impl Llm for RecoverReorder {
        fn enrich(
            &self,
            _: &str,
            call: usize,
            _: &str,
            requested: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            if call == 3 {
                self.done3.open();
                return Ok(Enrichment::default());
            }
            if call != 2 {
                return Ok(Enrichment::default());
            }
            if requested.len() == 2 && !self.truncated.swap(true, Ordering::SeqCst) {
                return Err(partial_of(&[("method:T.txt.w1", "de la réponse tronquée")]));
            }
            if self.ordered {
                self.done3.wait();
            }
            Ok(kept_of_texts(&[
                ("method:T.txt.w1", "du sous-appel"),
                ("method:T.txt.zz", "de zz"),
            ]))
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    fn kept_of_texts(items: &[(&str, &str)]) -> Enrichment {
        Enrichment {
            summaries: items
                .iter()
                .map(|(n, t)| (n.to_string(), t.to_string()))
                .collect(),
            ..Enrichment::default()
        }
    }

    #[test]
    fn req232_ac14_sous_appel_apres_le_dernier_appel_meme_resultat_qu_en_serie() {
        let summaries = |concurrency: usize| {
            let (dir, files) = split_repo(3, 1);
            let llm = RecoverReorder {
                ordered: concurrency > 1,
                truncated: AtomicBool::new(false),
                done3: Gate::new(),
            };
            let (store, report, failures) = probe_run_extra(
                &dir,
                &files,
                &llm,
                concurrency,
                1,
                None,
                0,
                PassMode::Normal,
                &[("T.txt", "zz")],
            );
            assert!(failures.is_empty(), "{failures:?}");
            assert_eq!(report.failed, 0);
            assert!(store.enriched_hash("T.txt").unwrap().is_some());
            ["method:T.txt.w1", "method:T.txt.zz"]
                .map(|id| store.node_by_id(id).unwrap().and_then(|n| n.summary))
        };
        let parallel = summaries(3);
        assert_eq!(
            parallel[0].as_deref(),
            Some("de la réponse tronquée"),
            "{parallel:?}"
        );
        assert_eq!(parallel[1].as_deref(), Some("de zz"));
        assert_eq!(parallel, summaries(1));
    }

    /// `A` et `B` en erreur réseau ; `C` (deux nœuds) tronqué avec des résumés complets ; le
    /// reste de `C`, `D` et `E` en erreur réseau.
    struct RecoverScripted {
        calls: Mutex<Vec<String>>,
        all: bool,
    }

    impl Llm for RecoverScripted {
        fn enrich(
            &self,
            file: &str,
            _: usize,
            _: &str,
            requested: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{file}:{}", requested.len()));
            if file == "C" && requested.len() == 2 {
                let mut got = vec![("method:C.w0", "R0.")];
                if self.all {
                    got.push(("method:C.zz", "R1."));
                }
                return Err(partial_of(&got));
            }
            Err(net())
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req232_ac16_appel_recupere_ne_remet_pas_les_erreurs_reseau_a_zero() {
        for all in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut files = Vec::new();
            for f in ["A", "B", "C", "D", "E"] {
                let text = if f == "C" {
                    "w0 zz xxxx\n"
                } else {
                    "w0 xxxxxxx\n"
                };
                std::fs::write(dir.path().join(f), text).unwrap();
                files.push(rec(f, text.len() as u64));
            }
            let llm = RecoverScripted {
                calls: Mutex::new(Vec::new()),
                all,
            };
            let (_store, report, _failures) = probe_run_extra(
                &dir,
                &files,
                &llm,
                1,
                1,
                None,
                0,
                PassMode::Normal,
                &[("C", "zz")],
            );
            // Deux erreurs réseau, l'appel de C récupéré (au cas 1 : rien ne repart ; sinon son
            // sous-appel est la troisième erreur), puis la troisième erreur arrête l'envoi.
            let want: &[&str] = if all {
                &["A:1", "B:1", "C:2", "D:1"]
            } else {
                &["A:1", "B:1", "C:2", "C:1"]
            };
            assert_eq!(*llm.calls.lock().unwrap(), want, "all = {all}");
            assert!(report
                .stop_cause
                .as_deref()
                .is_some_and(|c| c.contains("panne réseau répétée")));
        }
    }

    #[test]
    fn req230_split_call_coupe_la_liste_en_deux_sans_perte() {
        let ids = ["a", "b", "c", "d", "e"];
        let subs = split_call(&call_of(&ids, &[2]), &[]).unwrap();
        let lists: Vec<(Vec<String>, Vec<usize>)> = subs
            .iter()
            .map(|u| match u {
                Unit::Call {
                    requested,
                    path,
                    index,
                    count,
                    part,
                    excerpt,
                    ..
                } => {
                    assert_eq!((*index, *count, &**part), (2, 3, "texte"));
                    assert_eq!(excerpt.unwrap().first_line, 4);
                    (requested.clone(), path.clone())
                }
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            lists,
            [
                (vec!["a".into(), "b".into(), "c".into()], vec![2, 1]),
                (vec!["d".into(), "e".into()], vec![2, 2]),
            ]
        );
        assert!(split_call(&call_of(&["a"], &[]), &[]).is_none());
        assert!(split_call(&call_of(&[], &[]), &[]).is_none());
        assert!(split_call(&call_of(&ids, &[1, 1, 1]), &[]).is_none());
        assert!(split_call(&call_of(&ids, &[1, 1]), &[]).is_some());
    }

    /// Appel factice sur `part`, avec l'extrait `excerpt` s'il y en a un.
    fn text_call(
        path: &str,
        part: &str,
        excerpt: Option<Excerpt>,
        requested: &[&str],
    ) -> Unit<'static> {
        let known: Vec<&str> = requested.to_vec();
        text_call_known(path, part, excerpt, requested, &known)
    }

    fn text_call_known(
        path: &str,
        part: &str,
        excerpt: Option<Excerpt>,
        requested: &[&str],
        known: &[&str],
    ) -> Unit<'static> {
        let file: &'static FileRecord = Box::leak(Box::new(rec(path, 1)));
        Unit::Call {
            file,
            index: 1,
            count: 1,
            excerpt,
            part: Arc::from(part),
            requested: requested.iter().map(|s| s.to_string()).collect(),
            known: known.iter().map(|s| s.to_string()).collect(),
            path: vec![],
            cache: false,
            followers: Vec::new(),
        }
    }

    fn numbered_lines(n: usize) -> String {
        (1..=n).map(|i| format!("// ligne {i:0>50}\n")).collect()
    }

    fn named(list: &[&str]) -> Vec<(String, String)> {
        list.iter()
            .map(|s| {
                (
                    s.to_string(),
                    s.rsplit([':', '.']).next().unwrap().to_string(),
                )
            })
            .collect()
    }

    fn unit_shape(u: &Unit<'_>) -> (Vec<String>, Option<Excerpt>, usize, Vec<usize>) {
        match u {
            Unit::Call {
                requested,
                excerpt,
                part,
                path,
                ..
            } => (
                requested.clone(),
                *excerpt,
                part.chars().count(),
                path.clone(),
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn req230_split_call_texte_un_noeud_sans_perte_ni_doublon() {
        // Le nom du nœud ne figure que dans la seconde moitié : la première part est retirée.
        let text = format!("{}    void zed() {{}}\n", numbered_lines(30));
        let subs = split_call(
            &text_call("A.java", &text, None, &["method:A.zed"]),
            &named(&["method:A.zed"]),
        )
        .unwrap();
        assert_eq!(subs.len(), 1);
        let (list, e, chars, path) = unit_shape(&subs[0]);
        let e = e.unwrap();
        assert_eq!(list, ["method:A.zed"]);
        assert!(chars < text.chars().count());
        assert_eq!(path, [1]);
        assert_eq!((e.rank, e.total), (3, 3));
        // Dernière ligne réelle : 31 ; la première part de la seconde moitié est plus bas.
        assert_eq!(e.last_line, 31);
        assert!(e.first_line > 1);
    }

    #[test]
    fn req230_split_call_texte_repartit_les_noeuds_une_fois() {
        let text = format!(
            "    void aa() {{}}\n{}    void bb() {{}}\n",
            numbered_lines(30)
        );
        let subs = split_call(
            &text_call("A.java", &text, None, &["method:A.aa"]),
            &named(&["method:A.aa"]),
        )
        .unwrap();
        assert_eq!(subs.len(), 1);
        let both = named(&["method:A.aa", "method:A.bb"]);
        // Deux nœuds : liste coupée, non le texte.
        let by_list = split_call(
            &text_call("A.java", &text, None, &["method:A.aa", "method:A.bb"]),
            &both,
        )
        .unwrap();
        assert_eq!(by_list.len(), 2);
        assert_eq!(unit_shape(&by_list[0]).2, text.chars().count());
        // Markdown à deux nœuds : le texte d'abord, chacun dans la seule part qui le contient.
        let md = format!("# Aa\n{}# Bb\n{}", numbered_lines(15), numbered_lines(15));
        let nodes = vec![
            ("section:D.md#aa".to_string(), "aa".to_string()),
            ("section:D.md#bb".to_string(), "bb".to_string()),
        ];
        let subs = split_call(
            &text_call("D.md", &md, None, &["section:D.md#aa", "section:D.md#bb"]),
            &nodes,
        )
        .unwrap();
        let lists: Vec<Vec<String>> = subs.iter().map(|u| unit_shape(u).0).collect();
        assert_eq!(lists, [vec!["section:D.md#aa"], vec!["section:D.md#bb"]]);
        assert!(subs.iter().all(|u| unit_shape(u).2 < md.chars().count()));
    }

    #[test]
    fn req230_split_call_texte_trop_court_ou_une_ligne_ne_se_coupe_pas() {
        let short = numbered_lines(10);
        assert!(short.chars().count() < MIN_SPLIT_CHARS);
        assert!(split_call(
            &text_call("A.java", &short, None, &["type:A"]),
            &named(&["type:A"])
        )
        .is_none());
        let one_line = format!("class A {{ {} }}", "x".repeat(2000));
        assert!(split_call(
            &text_call("A.java", &one_line, None, &["type:A"]),
            &named(&["type:A"])
        )
        .is_none());
    }

    #[test]
    fn req230_split_call_lignes_reelles_d_un_parent_avec_en_tete_repete() {
        // Parent : morceau 2 d'un gros fichier, extrait 2/3 lignes 101-160, en-tête de 2 lignes.
        // Le nom `zed` est à la ligne 40 du corps, `zee` à la ligne 59.
        let mut body: Vec<String> = (1..=60).map(|i| format!("// ligne {i:0>50}\n")).collect();
        body[39] = format!("// zed {:0>52}\n", "");
        body[58] = format!("// zee {:0>52}\n", "");
        let text = format!("package p;\nclass Aa {{\n{}", body.concat());
        let parent = Excerpt {
            rank: 2,
            total: 3,
            first_line: 101,
            last_line: 160,
        };
        // Le corps se coupe en 3 : lignes 1-29 (corps), 30-58, 59-60, soit les lignes réelles
        // 101-129, 130-158 et 159-160 une fois l'en-tête (2 lignes) retiré.
        for (node, first, last) in [
            ("type:Aa", 101, 129),
            ("method:A.zed", 130, 158),
            ("method:A.zee", 159, 160),
        ] {
            let subs = split_call(
                &text_call("A.java", &text, Some(parent), &[node]),
                &named(&[node]),
            )
            .unwrap();
            assert_eq!(subs.len(), 1, "{node}");
            let (list, e, _, _) = unit_shape(&subs[0]);
            let e = e.unwrap();
            assert_eq!(list, [node]);
            assert_eq!((e.rank, e.total), (2, 3), "k/n du parent");
            assert_eq!((e.first_line, e.last_line), (first, last), "{node}");
        }
    }

    #[test]
    fn req230_split_call_markdown_trois_morceaux_sans_perte() {
        let sec = |t: &str| {
            format!(
                "# {t}\n\n{}\n",
                "Du texte de section qui remplit les lignes.\n".repeat(8)
            )
        };
        let md = format!("{}{}{}", sec("Alpha"), sec("Beta"), sec("Gamma"));
        let ids = [
            "section:D.md#alpha",
            "section:D.md#beta",
            "section:D.md#gamma",
        ];
        let subs = split_call(
            &text_call("D.md", &md, None, &ids),
            &[
                (ids[0].to_string(), "alpha".to_string()),
                (ids[1].to_string(), "beta".to_string()),
                (ids[2].to_string(), "gamma".to_string()),
            ],
        )
        .unwrap();
        assert_eq!(subs.len(), 3);
        let shapes: Vec<_> = subs.iter().map(unit_shape).collect();
        let paths: Vec<_> = shapes.iter().map(|s| s.3.clone()).collect();
        assert_eq!(paths, [vec![1], vec![2], vec![3]]);
        let mut all: Vec<String> = shapes.iter().flat_map(|s| s.0.clone()).collect();
        all.sort();
        assert_eq!(all, ids);
    }

    #[test]
    fn req230_split_call_markdown_a_sections_garde_le_morceau_a_liste_vide() {
        // Le morceau n'a plus de nœud demandé, mais le fichier a des sections (REQ-159).
        let text = numbered_lines(30);
        let subs = split_call(
            &text_call_known("D.md", &text, None, &[], &["section:D.md#a"]),
            &[],
        )
        .unwrap();
        assert!(subs.len() >= 2);
        assert!(subs.iter().all(|u| unit_shape(u).0.is_empty()));
        // Sans section dans le fichier, les parts vides sont retirées : aucune coupe.
        assert!(split_call(
            &text_call_known("D.md", &text, None, &[], &["document:D.md"]),
            &[]
        )
        .is_none());
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

    /// Plan de l'oracle réduit par REQ-144 : chaque identifiant gardé au plus petit morceau où il
    /// paraît, puis listes redécoupées par 150 (au moins un appel par morceau).
    fn first_only(plan: Vec<(usize, Vec<String>)>, n_parts: usize) -> Vec<(usize, Vec<String>)> {
        let mut per_part: Vec<Vec<String>> = vec![Vec::new(); n_parts];
        for (k, l) in plan {
            per_part[k].extend(l);
        }
        let mut seen = HashSet::new();
        for list in per_part.iter_mut() {
            list.retain(|id| seen.insert(id.clone()));
        }
        let mut out = Vec::new();
        for (k, list) in per_part.iter().enumerate() {
            for g in node_groups(list) {
                out.push((k, g.to_vec()));
            }
        }
        out
    }

    fn assert_each_id_once(plan: &[(usize, Vec<String>)], nodes: &[(String, String)]) {
        let mut all: Vec<&String> = plan.iter().flat_map(|(_, l)| l).collect();
        all.sort();
        let mut want: Vec<&String> = nodes.iter().map(|(id, _)| id).collect();
        want.sort();
        assert_eq!(all, want);
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
            let got = call_plan(&parts, &nodes, &homes);
            let oracle = oracle_call_plan(&parts, &nodes, &homes);
            let want = if parts.len() > 1 {
                first_only(oracle, parts.len())
            } else {
                oracle
            };
            assert_eq!(got, want, "cas {case} : {parts:?} {nodes:?}");
            if parts.len() > 1 {
                assert_each_id_once(&got, &nodes);
            }
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
        let oracle = oracle_call_plan(&parts, &nodes, &HashMap::new());
        let want = if parts.len() > 1 {
            first_only(oracle, parts.len())
        } else {
            oracle
        };
        assert_eq!(got, want);
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
        // REQ-144 : `dep:a` figure aux morceaux 1 à 3, il n'est demandé qu'au premier.
        assert_eq!(at(0), ["dep:a"]);
        assert!(at(1).is_empty() && at(2).is_empty());
        assert!(at(3).is_empty() && at(4).is_empty());
        assert_eq!(at(5), ["dep:b"]);
        assert!(at(6).is_empty());
        // Un cas discriminant par texte : reconnu, le nœud va au morceau 1 ; sinon repli au 0.
        let cases = [
            (
                "<artifactId>slf4j-api</artifactId>",
                "dep:a",
                "slf4j-api",
                1,
            ),
            ("slf4j-api-ext", "dep:a", "slf4j-api", 1),
            ("slf4j-api.jar", "dep:a", "slf4j-api", 1),
            ("xslf4j-api", "dep:a", "slf4j-api", 0),
            ("slf4j-api2", "dep:a", "slf4j-api", 0),
            ("\"@scope/pkg\":", "dep:b", "@scope/pkg", 1),
            ("\"@scope/pkgx\"", "dep:b", "@scope/pkg", 0),
        ];
        for (text, id, name, home) in cases {
            let plan = plan_for(&["x", text], &[(id, name)]);
            let mut want = vec![(0, vec![]), (1, vec![])];
            want[home].1.push(id.to_string());
            assert_eq!(plan, want, "texte {text:?}");
        }
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

    /// Faux `Llm` qui relève la catégorie reçue (REQ-177, AC-177-4) : erreur de format au
    /// premier appel, succès ensuite.
    struct Faults {
        seen: Mutex<Vec<Option<FormatFault>>>,
        first_error: Error,
    }

    impl Faults {
        fn new(first_error: Error) -> Faults {
            Faults {
                seen: Mutex::new(Vec::new()),
                first_error,
            }
        }
        fn next(&self, fault: Option<FormatFault>) -> Option<Error> {
            let mut seen = self.seen.lock().unwrap();
            seen.push(fault);
            (seen.len() == 1).then(|| self.first_error.clone())
        }
    }

    impl Llm for Faults {
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            Err(Error::Message("inutilisé".into()))
        }
        fn enrich_labeled(
            &self,
            _: &str,
            _: &str,
            _: usize,
            _: Option<Excerpt>,
            _: &str,
            _: &[String],
            _: &[String],
            _: &AtomicBool,
            _: &CallStats,
            fault: Option<FormatFault>,
            _: bool,
        ) -> Result<Enrichment> {
            match self.next(fault) {
                Some(e) => Err(e),
                None => Ok(Enrichment::default()),
            }
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn enrich_lot_labeled(
            &self,
            _: &str,
            items: &[LotItem],
            _: &AtomicBool,
            _: &CallStats,
            fault: Option<FormatFault>,
        ) -> Result<Vec<(String, Result<Enrichment>)>> {
            match self.next(fault) {
                Some(e) => Err(e),
                None => Ok(items
                    .iter()
                    .map(|i| (i.file.clone(), Ok(Enrichment::default())))
                    .collect()),
            }
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    #[test]
    fn req177_ac4_run_call_passes_previous_fault() {
        let llm = Faults::new(Error::Format(
            "réponse LLM non conforme : aucun objet JSON dans le texte".into(),
        ));
        let totals = Mutex::new(LlmTotals::default());
        let r = run_call(
            &llm,
            "A.java",
            1,
            1,
            None,
            "t",
            &[],
            &[],
            2,
            &AtomicBool::new(false),
            &totals,
            &|_, _| true,
            &[],
            false,
        );
        assert!(r.is_ok());
        assert_eq!(
            *llm.seen.lock().unwrap(),
            vec![None, Some(FormatFault::Absent)]
        );
    }

    #[test]
    fn req177_ac4_traced_lot_passes_previous_fault() {
        let llm = Faults::new(Error::Format(
            "réponse LLM non conforme au schéma : version 2 non prise en charge pour un lot (attendu : 3)"
                .into(),
        ));
        let items: Vec<LotItem> = ["A.java", "B.java"]
            .iter()
            .map(|f| LotItem {
                file: (*f).into(),
                text: "t".into(),
                node_ids: vec![],
            })
            .collect();
        let totals = Mutex::new(LlmTotals::default());
        let r = traced_lot(
            &llm,
            &items,
            2,
            &AtomicBool::new(false),
            &totals,
            &|_, _| true,
        );
        assert_eq!(r.unwrap().len(), 2);
        assert_eq!(
            *llm.seen.lock().unwrap(),
            vec![None, Some(FormatFault::Version)]
        );
    }
}

/// REQ-184, AC-184-5 : estimation pure du plan d'enrichissement (nombres écrits en dur).
#[cfg(test)]
mod estimate_tests {
    use super::*;
    use crate::index::estimate::{stdout_lines, trace_lines};
    use crate::llm::enrich::{enrichment_system_prompt, lot_system_prompt};
    use crate::model::FileState;

    const LANG: &str = "français";

    fn rec(path: &str, size: u64) -> FileRecord {
        FileRecord {
            path: path.into(),
            hash: String::new(),
            size,
            mtime: 0,
            recorded_at: 0,
            state: FileState::Indexed,
        }
    }

    fn ids_of(list: &[(&str, &[&str])]) -> HashMap<String, Vec<(String, String)>> {
        list.iter()
            .map(|(f, ns)| {
                (
                    f.to_string(),
                    ns.iter().map(|n| (n.to_string(), n.to_string())).collect(),
                )
            })
            .collect()
    }

    fn run(
        dir: &Path,
        files: &[FileRecord],
        ids: &HashMap<String, Vec<(String, String)>>,
        batch: usize,
        max: usize,
    ) -> Estimate {
        let abs = HashMap::new();
        let sources = Sources {
            repo: dir,
            abs: &abs,
            redact: None,
        };
        let kept = KeptCalls {
            results: HashMap::new(),
            model: "m".into(),
            language: LANG.into(),
            versions: PromptVersions::CURRENT,
        };
        estimate(sources, files, ids, LANG, batch, max, &kept)
    }

    /// Lignes de 10 octets `nNN xxxxx` : une par morceau pour `max_chunk_chars` = 10.
    fn lines_text(n: usize) -> String {
        (0..n).map(|i| format!("n{i:02} xxxxx\n")).collect()
    }

    fn ids_for_lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("n{i:02}")).collect()
    }

    #[test]
    fn req184_ac5_lots_and_big_with_three_calls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("d.txt"), lines_text(3)).unwrap();
        // max_chunk_chars = 10, batch_files = 2. a.md (3 octets) et b.md (4) forment un lot de 2
        // (7 ≤ 10) ; c.md (2) est seul, car d.txt (30 > 10) est un `Big` qui coupe le lot ; e.md
        // n'a aucun nœud (sans appel).
        let files = vec![
            rec("a.md", 3),
            rec("b.md", 4),
            rec("c.md", 2),
            rec("d.txt", 30),
            rec("e.md", 1),
        ];
        let ids = ids_of(&[
            ("a.md", &["id1"]),
            ("b.md", &["id22", "id3"]),
            ("c.md", &["id5"]),
            ("d.txt", &["n00", "n01", "n02"]),
        ]);
        let e = run(dir.path(), &files, &ids, 2, 10);
        assert_eq!((e.files, e.sent_alone, e.without_call), (5, 1, 1));
        assert_eq!(e.calls, 1 + 1 + 3);
        assert_eq!(e.nodes, 3 + 1 + 3);
        // Lot de 2 : système du lot + 14+4+1+19+1+1+9+1+1+21+4+2 = 78 d'encadrement par fichier,
        // + 4 par fichier pour ` [k]` de `=== FICHIER [k] : `, + 2 × chemin (4) par fichier, + liste numérotée
        // en continu (`[1] id1 — id1\n` = 4 + 3 + 5 + 3 + 1 = 16 ; `[2] id22 — id22\n` = 18 ;
        // `[3] id3 — id3\n` = 16 ; le tiret long « — » et ses deux espaces font 5 octets) + tailles
        // 3 + 4 = (78+8+4) + 16 + (78+8+4) + 34 + 7 = 90+16 + 90+34 + 7 = 237.
        // Lot de 1 (c.md), ordre de REQ-267 : système d'un fichier + encadrement (`Fichier : ` 10,
        // chemin 4, `\n\n` 2, `Contenu :` 9, `\n` 1, [contenu], `\n\n` 2, `Nœuds du fichier :` 19,
        // `\n` 1, [liste], `\n` 1, consigne de 72 octets → 121) + liste `[1] id5 — id5\n` 16 +
        // taille 2 = 121 + 16 + 2 = 139.
        let lot2 = lot_system_prompt(LANG).len() + 237;
        let lot1 = enrichment_system_prompt(LANG).len() + 139;
        // Big de 3 appels : par appel, système + `Fichier : d.txt` (15) + `\n\nContenu :\n` (12) +
        // morceau de 10 + `\n\nNœuds du fichier :\n` (22) + liste `[1] n0k — n0k\n` (16) + `\n` (1) +
        // ligne d'extrait `(extrait k/3, lignes k–k)\n` (28, le tiret demi-cadratin fait 3 octets) + consigne (72) = 176.
        let big = 3 * (enrichment_system_prompt(LANG).len() + 176);
        assert_eq!(e.bytes, lot2 + lot1 + big);
        assert_eq!(e.alone.len(), 1);
        assert_eq!(e.alone[0].path, "d.txt");
        assert_eq!(e.alone[0].calls, 3);
        assert_eq!(e.alone[0].bytes, big);
        assert_eq!(e.alone[0].case, AloneCase::Counted);
        assert!(e.capped.is_empty() && e.unreadable.is_empty());
        // Jetons : ⌈octets / 4⌉ en entrée, 40 par nœud demandé (7 nœuds) en sortie.
        let lines = stdout_lines(&e, 3, None);
        assert_eq!(
            lines[0],
            "estimation : 5 fichiers à enrichir dont 1 envoyés seuls et 1 sans appel"
        );
        assert_eq!(
            lines[1],
            format!(
                "estimation minimale : 5 appels, {} octets, {} jetons d'entrée, 280 jetons de sortie (un essai par appel)",
                e.bytes,
                e.bytes.div_ceil(4)
            )
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(
            trace_lines(&e),
            vec![format!("estimation d.txt : 3 appels, {big} octets")]
        );
    }

    type BigFixture = (
        tempfile::TempDir,
        Vec<FileRecord>,
        HashMap<String, Vec<(String, String)>>,
    );

    /// Fichier de `n` lignes, chacune demandant son nœud : `n` appels pour `max_chunk_chars` = 10.
    fn big_of(n: usize) -> BigFixture {
        let dir = tempfile::tempdir().unwrap();
        let text = lines_text(n);
        std::fs::write(dir.path().join("d.txt"), &text).unwrap();
        let names = ids_for_lines(n);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let ids = ids_of(&[("d.txt", &refs)]);
        (dir, vec![rec("d.txt", text.len() as u64)], ids)
    }

    #[test]
    fn req184_ac5_big_of_50_calls_is_counted() {
        let (dir, files, ids) = big_of(50);
        let e = run(dir.path(), &files, &ids, 5, 10);
        // Par appel : système + 15 (`Fichier : d.txt`) + extrait (26 + 3 × chiffres de k) + 49
        // (ligne `[1] n0k — n0k\n` de 16 octets) + 10 + 74 (consigne) ; 9 appels à un chiffre (177),
        // 41 à deux chiffres (180) : 9 × 177 + 41 × 180 = 8 973.
        let sys = enrichment_system_prompt(LANG).len();
        assert_eq!(e.calls, 50);
        assert_eq!(e.nodes, 50);
        assert_eq!(e.bytes, 50 * sys + 8_973);
        assert_eq!((e.files, e.sent_alone, e.without_call), (1, 1, 0));
        assert!(e.capped.is_empty());
        assert_eq!(e.alone[0].case, AloneCase::Counted);
    }

    #[test]
    fn req184_ac5_big_of_51_calls_is_capped_and_counts_nothing() {
        let (dir, files, ids) = big_of(51);
        let e = run(dir.path(), &files, &ids, 5, 10);
        assert_eq!(e.calls, 0);
        assert_eq!(e.bytes, 0);
        assert_eq!(e.nodes, 0);
        assert_eq!((e.files, e.sent_alone, e.without_call), (1, 1, 0));
        assert_eq!(e.capped, vec![("d.txt".to_string(), 51)]);
        assert!(e.unreadable.is_empty());
        assert_eq!(e.alone[0].case, AloneCase::OverCap(51));
        assert_eq!(
            trace_lines(&e),
            vec!["estimation d.txt : 0 appels, 0 octets (au plafond, 51 appels prévus)"]
        );
        let lines = stdout_lines(&e, 3, None);
        assert_eq!(
            lines[3],
            "estimation au plafond : d.txt (51 appels prévus, au plus 50 par fichier, aucun compté)"
        );
    }

    #[test]
    fn req184_ac5_unreadable_big_counts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ids = ids_of(&[("gone.txt", &["n00"])]);
        let e = run(dir.path(), &[rec("gone.txt", 30)], &ids, 5, 10);
        assert_eq!((e.calls, e.bytes, e.nodes), (0, 0, 0));
        assert_eq!((e.files, e.sent_alone, e.without_call), (1, 1, 0));
        assert_eq!(e.unreadable, vec!["gone.txt".to_string()]);
        assert!(e.capped.is_empty());
        assert_eq!(e.alone[0].case, AloneCase::Unreadable);
        assert_eq!(
            stdout_lines(&e, 3, None)[3],
            "estimation illisible : gone.txt (aucun appel compté)"
        );
        assert_eq!(
            trace_lines(&e),
            vec!["estimation gone.txt : 0 appels, 0 octets (illisible)"]
        );
    }

    #[test]
    fn req184_ac5_files_without_nodes_make_no_call() {
        let dir = tempfile::tempdir().unwrap();
        let e = run(
            dir.path(),
            &[rec("a.md", 3), rec("b.md", 9)],
            &HashMap::new(),
            5,
            10,
        );
        assert_eq!((e.files, e.sent_alone, e.without_call), (2, 0, 2));
        assert_eq!((e.calls, e.bytes, e.nodes), (0, 0, 0));
    }

    /// Octets d'un lot (ou d'un lot de 1) à textes vides, sans la taille des fichiers.
    fn empty_lot_bytes(paths: &[&str], ids: &HashMap<String, Vec<(String, String)>>) -> usize {
        if paths.len() == 1 {
            let requested: Vec<String> = ids[paths[0]].iter().map(|(id, _)| id.clone()).collect();
            return enrichment_system_prompt(LANG).len()
                + enrichment_user_prompt(paths[0], None, "", &requested).len();
        }
        let items: Vec<LotItem> = paths
            .iter()
            .map(|p| LotItem {
                file: p.to_string(),
                text: String::new(),
                node_ids: ids[*p].iter().map(|(id, _)| id.clone()).collect(),
            })
            .collect();
        lot_system_prompt(LANG).len() + lot_user_prompt(&items).len()
    }

    #[test]
    fn req241_ac3_data_file_in_a_lot_counts_at_most_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let ids = ids_of(&[
            ("a.json", &["id1"]),
            ("B.java", &["id2"]),
            ("s.sql", &["id3"]),
        ]);
        let e = run(
            dir.path(),
            &[rec("a.json", 10_000), rec("B.java", 100)],
            &ids,
            2,
            60_000,
        );
        assert_eq!(e.calls, 1);
        assert_eq!(
            e.bytes,
            empty_lot_bytes(&["a.json", "B.java"], &ids) + 6_000 + 100
        );
        // Un SQL garde sa taille enregistrée (majorant).
        let e = run(
            dir.path(),
            &[rec("s.sql", 10_000), rec("B.java", 100)],
            &ids,
            2,
            60_000,
        );
        assert_eq!(
            e.bytes,
            empty_lot_bytes(&["s.sql", "B.java"], &ids) + 10_000 + 100
        );
        // Seul dans son lot : système et message d'un fichier + 6 000.
        let e = run(dir.path(), &[rec("a.json", 10_000)], &ids, 2, 60_000);
        assert_eq!(e.calls, 1);
        assert_eq!(e.bytes, empty_lot_bytes(&["a.json"], &ids) + 6_000);
        // L = min(6 000, max_chunk_chars) : plafond de 2 000 caractères.
        let e = run(dir.path(), &[rec("a.json", 1_500)], &ids, 2, 2_000);
        assert_eq!(e.bytes, empty_lot_bytes(&["a.json"], &ids) + 1_500);
    }

    #[test]
    fn req184_ac5_lot_prompt_length_is_empty_text_plus_sizes() {
        let texts = ["Un texte é\n", "autre"];
        let mk = |with_text: bool| -> Vec<LotItem> {
            ["a.md", "b.md"]
                .iter()
                .zip(texts)
                .map(|(f, t)| LotItem {
                    file: f.to_string(),
                    text: if with_text {
                        t.to_string()
                    } else {
                        String::new()
                    },
                    node_ids: vec!["id1".into()],
                })
                .collect()
        };
        let sizes: usize = texts.iter().map(|t| t.len()).sum();
        assert_eq!(
            lot_user_prompt(&mk(true)).len(),
            lot_user_prompt(&mk(false)).len() + sizes
        );
    }
}

/// Place de la trace de repli de la sortie structurée dans l'enrichissement (REQ-203, T-297).
#[cfg(test)]
mod structured_fallback_tests {
    use super::*;
    use crate::config::Effort;
    use crate::llm::anthropic::AnthropicLlm;
    use crate::llm::openai::OpenAiLlm;
    use crate::llm::{recording_server, Generation, Structured, Tls};
    use crate::model::FileState;

    const NAMES: [&str; 3] = ["A.txt", "B.txt", "C.txt"];
    const REFUSED: &str = r#"{"error":{"message":"Invalid parameter: format is not supported"}}"#;

    fn node_id(path: &str) -> String {
        Node::new(
            NodeKind::Method,
            &format!("{path}.m"),
            "m",
            Some(path),
            None,
            None,
        )
        .id
    }

    /// Réponse HTTP 200 d'un fournisseur qui résume le nœud du fichier.
    fn ok(provider: &str, path: &str) -> String {
        let content = serde_json::json!({
            "summaries": [{"node": node_id(path), "summary": "Résumé"}],
            "concepts": [],
        })
        .to_string();
        if provider == "anthropic" {
            serde_json::json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": content}]})
                .to_string()
        } else {
            serde_json::json!({"choices": [{"message": {"content": content}, "finish_reason": "stop"}]})
                .to_string()
        }
    }

    fn adapter(provider: &str, base: String) -> Box<dyn Llm> {
        let gen = Generation {
            effort: Effort::Low,
            effort_explicit: false,
            thinking: false,
            max_output_tokens: 1000,
        };
        if provider == "anthropic" {
            Box::new(
                AnthropicLlm::new(
                    Some(base),
                    "m".into(),
                    "k".into(),
                    10,
                    &Tls::default(),
                    1,
                    gen,
                )
                .with_structured(Structured::Auto)
                .with_structured_key("structured_output"),
            )
        } else {
            Box::new(
                OpenAiLlm::new(
                    Some(base),
                    "m".into(),
                    Some("k".into()),
                    10,
                    &Tls::default(),
                    1,
                    gen,
                )
                .with_structured(Structured::Auto)
                .with_structured_key("structured_output"),
            )
        }
    }

    struct Run {
        events: Vec<String>,
        report: BuildReport,
        failures: Vec<(String, Error)>,
        sends: usize,
    }

    /// Enrichit les trois documents avec les réponses programmées ; traces capturées si `traced`.
    fn run(provider: &str, replies: Vec<(u16, String)>, traced: bool) -> Run {
        let _t = crate::interrupt::test_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for n in NAMES {
            std::fs::write(dir.path().join(n), "texte\n").unwrap();
            files.push(FileRecord {
                path: n.into(),
                hash: String::new(),
                size: 6,
                mtime: 0,
                recorded_at: 0,
                state: FileState::Indexed,
            });
        }
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(&files).unwrap();
        for f in &files {
            let node = Node::new(
                NodeKind::Method,
                &format!("{}.m", f.path),
                "m",
                Some(&f.path),
                None,
                None,
            );
            store.replace_structure(&f.path, &[node], &[]).unwrap();
        }
        let (base, bodies) = recording_server(replies, 0);
        let llm = adapter(provider, base);
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 1000,
            batch_files: 1,
            format_retries: 0,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
        };
        let mut report = BuildReport::default();
        let sources = Sources {
            repo: dir.path(),
            abs: &HashMap::new(),
            redact: None,
        };
        let stop = AtomicBool::new(false);
        let mut go = |report: &mut BuildReport| {
            enrich_with_stop(
                sources,
                &files,
                &opts,
                llm.as_ref(),
                &mut store,
                report,
                &stop,
            )
            .unwrap()
        };
        // Les lignes `llm enrich départ` et `fin` ne s'écrivent que sous `--verbose`.
        let (failures, mut events) = crate::trace::capture_all(traced, || go(&mut report));
        // Seuls les événements de la passe comptent, pas ceux des tests voisins.
        events.retain(|l| {
            NAMES.iter().any(|n| l.contains(n))
                || l.contains("sortie structurée")
                || l.contains("politique:")
        });
        let sends = bodies.lock().unwrap().len();
        Run {
            events,
            report,
            failures,
            sends,
        }
    }

    fn fallback_line(provider: &str) -> String {
        format!("sortie structurée: schéma JSON refusé par {provider} (HTTP 400), suite de la commande sans schéma (structured_output = \"auto\")")
    }

    fn count(events: &[String], needle: &str) -> usize {
        events.iter().filter(|l| l.contains(needle)).count()
    }

    #[test]
    fn req203_ac7_trace_de_repli_apres_le_depart_avant_la_fin() {
        for provider in ["openai", "anthropic"] {
            let replies = vec![
                (400, REFUSED.to_string()),
                (200, ok(provider, "A.txt")),
                (200, ok(provider, "B.txt")),
                (200, ok(provider, "C.txt")),
            ];
            let r = run(provider, replies.clone(), true);
            assert!(r.failures.is_empty(), "{provider}: {:?}", r.failures);
            let at = r
                .events
                .iter()
                .position(|l| l.starts_with("llm enrich départ A.txt [1/1] "))
                .unwrap_or_else(|| panic!("{provider}: {:?}", r.events));
            assert_eq!(r.events[at + 1], fallback_line(provider));
            assert!(
                r.events[at + 2].starts_with("llm enrich fin A.txt [1/1] ok ("),
                "{}",
                r.events[at + 2]
            );
            assert_eq!(count(&r.events, "sortie structurée"), 1, "{provider}");
            for banned in ["requête refusée", "politique:", "(essai "] {
                assert_eq!(count(&r.events, banned), 0, "{provider} {banned}");
            }
            assert!(
                calls_line(&r.report.llm)
                    .starts_with("appels: 3 appels, 0 nouveaux essais de format, 4 envois HTTP"),
                "{provider}: {}",
                calls_line(&r.report.llm)
            );
            assert_eq!(r.sends, 4);

            // Traces désactivées : rien n'est retenu par la capture et aucun échec n'est rendu.
            let quiet = run(provider, replies, false);
            assert!(quiet.events.is_empty(), "{:?}", quiet.events);
            assert_eq!(quiet.sends, 4);
            assert!(quiet.failures.is_empty() && quiet.report.refused.is_none());
            assert_eq!(quiet.report.failed, 0);
        }
    }

    /// E-19 telle que `finish` la rend, ligne `erreur:` sans son préfixe.
    fn rendered(r: &Run) -> String {
        crate::index::build::finish(&r.report)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn req203_ac8_refus_sans_les_mots_du_schema_garde_e19_sans_trace_de_repli() {
        for provider in ["openai", "anthropic"] {
            let body = r#"{"error":{"message":"Bad request"}}"#.to_string();
            let r = run(provider, vec![(400, body)], true);
            assert_eq!(count(&r.events, "sortie structurée"), 0, "{provider}");
            assert_eq!(r.sends, 1, "{provider}");
            let e = rendered(&r);
            assert!(
                e.starts_with(&format!("requête refusée par {provider} (HTTP 400) sur ")),
                "{e}"
            );
            assert!(
                e.ends_with(
                    " : Bad request ; vérifiez model, thinking, effort et max_output_tokens"
                ),
                "{e}"
            );
        }
    }

    #[test]
    fn req203_ac8_second_refus_une_trace_avant_la_fin_et_e19_du_second_refus() {
        for provider in ["openai", "anthropic"] {
            let second = r#"{"error":{"message":"second refus du modèle"}}"#.to_string();
            let r = run(
                provider,
                vec![(400, REFUSED.to_string()), (400, second)],
                true,
            );
            assert_eq!(r.sends, 2, "{provider}");
            assert_eq!(count(&r.events, "sortie structurée"), 1, "{provider}");
            let trace_at = r
                .events
                .iter()
                .position(|l| l.contains("sortie structurée"))
                .unwrap();
            let end_at = r
                .events
                .iter()
                .position(|l| l.starts_with("llm enrich fin A.txt [1/1] requête refusée "))
                .unwrap_or_else(|| panic!("{provider}: {:?}", r.events));
            assert!(trace_at < end_at, "{provider}");
            let e = rendered(&r);
            assert!(
                e.ends_with(" : second refus du modèle ; vérifiez model, thinking, effort et max_output_tokens"),
                "{e}"
            );
        }
    }
}

#[cfg(test)]
mod doc_tests {
    use super::*;

    fn file(path: &str) -> FileRecord {
        FileRecord {
            path: path.into(),
            hash: "h".into(),
            size: 3,
            mtime: 0,
            recorded_at: 0,
            state: crate::model::FileState::Indexed,
        }
    }

    fn method(q: &str, file: &str, doc: Option<&str>) -> Node {
        Node::new(NodeKind::Method, q, "m", Some(file), None, doc)
    }

    #[test]
    fn req244_ac7_documented_node_is_absent_and_shared_node_stays_asked_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store
            .insert_files(&[file("A.java"), file("B.java")])
            .unwrap();
        // `titre` documenté, `prix` sans doc ; `x:y` documenté dans A.java seulement.
        let dep = |doc| Node::new(NodeKind::Dependency, "x:y", "y", None, None, doc);
        store
            .replace_structure(
                "A.java",
                &[
                    method("p.A.titre", "A.java", Some("Le titre.")),
                    method("p.A.prix", "A.java", None),
                    dep(Some("Doc.")),
                ],
                &[],
            )
            .unwrap();
        store
            .replace_structure("B.java", &[dep(None)], &[])
            .unwrap();
        let map = nodes_by_file(&store).unwrap();
        let ids = |f: &str| -> Vec<&str> { map[f].iter().map(|(id, _)| id.as_str()).collect() };
        assert_eq!(ids("A.java"), ["method:p.A.prix"]);
        assert_eq!(ids("B.java"), ["dependency:x:y"]);
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    const FLOOR: usize = 10;

    fn file(path: &str) -> &'static FileRecord {
        Box::leak(Box::new(FileRecord {
            path: path.into(),
            hash: String::new(),
            size: 1,
            mtime: 0,
            recorded_at: 0,
            state: crate::model::FileState::Indexed,
        }))
    }

    fn nodes(n: usize) -> Vec<(String, String)> {
        (0..n)
            .map(|i| (format!("method:A.m{i}"), format!("m{i}")))
            .collect()
    }

    /// Appels du plan d'un fichier d'un seul morceau de `chars` caractères, puis marquage.
    fn marked(chars: usize, node_count: usize, held: usize) -> Vec<bool> {
        let text = "x".repeat(chars);
        let f = file("A.java");
        let calls = plan_calls(f, &text, 10_000, &nodes(node_count)).unwrap();
        let mut kept = KeptCalls {
            results: HashMap::new(),
            model: "m".into(),
            language: "français".into(),
            versions: PromptVersions::CURRENT,
        };
        for call in calls.iter().take(held) {
            let Unit::Call {
                file,
                index,
                count,
                excerpt,
                part,
                requested,
                known,
                ..
            } = call
            else {
                unreachable!()
            };
            let key = kept.key(&file.path, *index, *count, *excerpt, part, requested, known);
            kept.results.insert(key, Enrichment::default());
        }
        let (mut send, _) = split_kept(calls, &kept);
        mark_rereads(&mut send, Some(FLOOR));
        send.iter()
            .map(|u| match u {
                Unit::Call { cache, .. } => *cache,
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn req268_ac3_deux_appels_d_un_gros_morceau_sont_marques() {
        // 4 × 10 + 4 caractères : 11 jetons, au-dessus du minimum.
        assert_eq!(marked(44, 160, 0), [true, true]);
    }

    #[test]
    fn req268_ac3_un_seul_appel_n_est_pas_marque() {
        assert_eq!(marked(400, 100, 0), [false]);
    }

    #[test]
    fn req268_ac3_morceau_au_plus_4_fois_le_minimum_n_est_pas_marque() {
        assert_eq!(marked(40, 160, 0), [false, false]);
        assert_eq!(marked(43, 160, 0), [false, false]);
    }

    #[test]
    fn req268_ac3_appel_conserve_ne_compte_pas() {
        assert_eq!(marked(400, 160, 1), [false]);
    }

    #[test]
    fn req268_ac3_sans_minimum_aucun_appel_n_est_marque() {
        let f = file("A.java");
        let text = "x".repeat(400);
        let mut calls = plan_calls(f, &text, 10_000, &nodes(160)).unwrap();
        mark_rereads(&mut calls, None);
        assert!(calls
            .iter()
            .all(|u| matches!(u, Unit::Call { cache: false, .. })));
    }

    #[test]
    fn req268_ac3_deux_morceaux_distincts_ne_se_comptent_pas() {
        // Deux morceaux de même texte mais distincts : un appel chacun, rien n'est relu.
        let f = file("A.java");
        let one = |part: &str| Unit::Call {
            file: f,
            index: 1,
            count: 2,
            excerpt: None,
            part: Arc::from(part),
            requested: vec![],
            known: Arc::from(Vec::<String>::new()),
            path: vec![],
            cache: false,
            followers: Vec::new(),
        };
        let text = "x".repeat(400);
        let mut calls = vec![one(&text), one(&text)];
        mark_rereads(&mut calls, Some(FLOOR));
        assert!(calls
            .iter()
            .all(|u| matches!(u, Unit::Call { cache: false, .. })));
    }

    fn cached_call(requested: &[&str], text: &str) -> Unit<'static> {
        Unit::Call {
            file: file("A.java"),
            index: 1,
            count: 2,
            excerpt: None,
            part: Arc::from(text),
            requested: requested.iter().map(|s| s.to_string()).collect(),
            known: requested.iter().map(|s| s.to_string()).collect(),
            path: vec![],
            cache: true,
            followers: Vec::new(),
        }
    }

    /// Faux `Llm` qui annonce `floor` et enregistre le `cached` de chaque appel.
    struct Recorder {
        floor: Option<usize>,
        cached: Mutex<Vec<bool>>,
    }
    impl Llm for Recorder {
        fn cache_floor(&self) -> Option<usize> {
            self.floor
        }
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            Err(Error::Message("inutilisé".into()))
        }
        fn enrich_labeled(
            &self,
            _: &str,
            _: &str,
            _: usize,
            _: Option<Excerpt>,
            _: &str,
            _: &[String],
            _: &[String],
            _: &AtomicBool,
            _: &CallStats,
            _: Option<FormatFault>,
            cached: bool,
        ) -> Result<Enrichment> {
            self.cached.lock().unwrap().push(cached);
            Ok(Enrichment::default())
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    /// Enrichit un fichier de 60 caractères et 160 nœuds (2 appels, un seul morceau).
    fn cached_seen(floor: Option<usize>) -> Vec<bool> {
        let _t = crate::interrupt::test_lock();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.txt"), "x".repeat(60)).unwrap();
        let f = FileRecord {
            path: "A.txt".into(),
            hash: String::new(),
            size: 60,
            mtime: 0,
            recorded_at: 0,
            state: crate::model::FileState::Indexed,
        };
        let files = vec![f];
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(&files).unwrap();
        let ns: Vec<Node> = (0..160)
            .map(|i| {
                Node::new(
                    NodeKind::Method,
                    &format!("A.m{i}"),
                    "m",
                    Some("A.txt"),
                    None,
                    None,
                )
            })
            .collect();
        store.replace_structure("A.txt", &ns, &[]).unwrap();
        let llm = Recorder {
            floor,
            cached: Mutex::new(Vec::new()),
        };
        let opts = IndexOptions {
            jobs: 1,
            concurrency: 1,
            max_chunk_chars: 10_000,
            batch_files: 1,
            format_retries: 0,
            enrich_code: false,
            max_calls: None,
            ..IndexOptions::default()
        };
        enrich_all(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            &files,
            &opts,
            &llm,
            &mut store,
            &mut BuildReport::default(),
        )
        .unwrap();
        let seen = llm.cached.lock().unwrap().clone();
        seen
    }

    #[test]
    fn req268_ac3_enrich_all_passe_cached_a_enrich_labeled() {
        assert_eq!(cached_seen(Some(10)), [true, true]);
    }

    #[test]
    fn req268_ac4_enrich_all_sans_minimum_n_envoie_aucun_marqueur() {
        assert_eq!(cached_seen(None), [false, false]);
    }

    fn flags(units: &[Unit<'_>]) -> Vec<bool> {
        units
            .iter()
            .map(|u| match u {
                Unit::Call { cache, .. } => *cache,
                _ => unreachable!(),
            })
            .collect()
    }

    fn kept_of(nodes: &[&str]) -> Enrichment {
        Enrichment {
            summaries: nodes
                .iter()
                .map(|n| (n.to_string(), "Résumé.".to_string()))
                .collect(),
            ..Enrichment::default()
        }
    }

    #[test]
    fn req268_ac5_sous_appels_de_meme_texte_gardent_le_marqueur() {
        let unit = cached_call(&["a", "b", "c"], "texte");
        let Recovery::Rest(_, sub) = recover_call(&unit, kept_of(&["a"])) else {
            panic!("reste attendu");
        };
        assert_eq!(flags(&[sub]), [true]);
        let both = vec![
            ("a".to_string(), "a".to_string()),
            ("b".to_string(), "b".to_string()),
        ];
        let subs = split_call(&cached_call(&["a", "b"], "texte"), &both).unwrap();
        assert_eq!(flags(&subs), [true, true]);
        // Un appel non marqué ne le devient pas.
        let mut plain = cached_call(&["a", "b"], "texte");
        if let Unit::Call { cache, .. } = &mut plain {
            *cache = false;
        }
        assert_eq!(flags(&split_call(&plain, &both).unwrap()), [false, false]);
    }

    #[test]
    fn req268_ac5_sous_appel_a_texte_coupe_n_est_pas_marque() {
        let text: String = (1..=30)
            .map(|i| format!("// ligne {i:0>50}\n"))
            .collect::<String>()
            + "    void zed() {}\n";
        let unit = cached_call(&["method:A.zed"], &text);
        let nodes = vec![("method:A.zed".to_string(), "zed".to_string())];
        let subs = split_call(&unit, &nodes).unwrap();
        assert_eq!(flags(&subs), [false]);
        let Unit::Call { part, .. } = &subs[0] else {
            unreachable!()
        };
        assert!(part.len() < text.len());
    }
}

#[cfg(test)]
mod first_call_tests {
    use super::*;
    use std::collections::HashSet;
    use std::time::Instant;

    /// Attente maximale d'un événement attendu : filet contre un blocage, jamais atteint au vert.
    const LIMIT: Duration = Duration::from_secs(8);
    /// Fenêtre d'observation d'un envoi qui ne doit pas avoir lieu.
    const QUIET: Duration = Duration::from_millis(150);

    #[derive(Default)]
    struct State {
        /// Appels commencés : (premier nœud demandé, le premier appel du morceau était fini).
        started: Vec<(String, bool)>,
        /// Nœuds demandés par les appels réussis.
        ok: Vec<String>,
        seen_heads: HashSet<String>,
        head_done: bool,
        open_heads: bool,
        open_rest: bool,
    }

    /// Ce que rend le premier appel d'un morceau quand il est libéré.
    #[derive(Clone, Copy, PartialEq)]
    enum Head {
        Ok,
        Fails,
        Truncated,
        /// Tronqué après deux résumés complets (récupération partielle).
        Partial,
        /// Nouvel essai refusé par le plafond.
        Capped,
    }

    /// Faux `Llm` avec `cache_floor` : le premier appel de chaque morceau et les autres attendent
    /// chacun une barrière ouverte par le test.
    struct Gate {
        heads: Vec<String>,
        head: Head,
        st: Mutex<State>,
        cv: Condvar,
    }

    impl Gate {
        fn new(heads: &[&str], head: Head, open_rest: bool) -> Gate {
            Gate {
                heads: heads.iter().map(|s| s.to_string()).collect(),
                head,
                st: Mutex::new(State {
                    open_rest,
                    ..State::default()
                }),
                cv: Condvar::new(),
            }
        }

        fn set(&self, f: impl FnOnce(&mut State)) {
            f(&mut self.st.lock().unwrap());
            self.cv.notify_all();
        }

        fn wait_started(&self, n: usize) {
            let t0 = Instant::now();
            let mut g = self.st.lock().unwrap();
            while g.started.len() < n {
                let left = LIMIT.checked_sub(t0.elapsed()).expect("appels attendus");
                g = self.cv.wait_timeout(g, left).unwrap().0;
            }
        }

        /// Attend que `n` appels soient partis, puis `QUIET` : aucun autre ne doit partir.
        fn started_stays(&self, n: usize) -> usize {
            self.wait_started(n);
            let g = self.st.lock().unwrap();
            let g = self
                .cv
                .wait_timeout_while(g, QUIET, |s| s.started.len() <= n)
                .unwrap()
                .0;
            g.started.len()
        }
    }

    impl Llm for Gate {
        fn cache_floor(&self) -> Option<usize> {
            Some(1)
        }
        fn enrich(
            &self,
            _: &str,
            _: usize,
            _: &str,
            _: &[String],
            _: &[String],
        ) -> Result<Enrichment> {
            Err(Error::Message("inutilisé".into()))
        }
        fn enrich_labeled(
            &self,
            _: &str,
            _: &str,
            _: usize,
            _: Option<Excerpt>,
            _: &str,
            requested: &[String],
            _: &[String],
            _: &AtomicBool,
            _: &CallStats,
            _: Option<FormatFault>,
            _: bool,
        ) -> Result<Enrichment> {
            let name = requested[0].clone();
            let mut g = self.st.lock().unwrap();
            let is_head = self.heads.iter().any(|h| name.ends_with(h.as_str()))
                && g.seen_heads.insert(name.clone());
            let done = g.head_done;
            g.started.push((name, done));
            self.cv.notify_all();
            let t0 = Instant::now();
            while !(if is_head { g.open_heads } else { g.open_rest }) {
                let left = LIMIT.checked_sub(t0.elapsed()).expect("barrière ouverte");
                g = self.cv.wait_timeout(g, left).unwrap().0;
            }
            if is_head {
                g.head_done = true;
                self.cv.notify_all();
                match self.head {
                    Head::Ok => {}
                    Head::Fails => return Err(Error::Message("échec du premier".into())),
                    Head::Capped => return Err(Error::Capped),
                    Head::Partial => {
                        return Err(Error::Truncated {
                            message: "réponse tronquée par la limite de jetons".into(),
                            partial: r#"{"version":2,"summaries":[{"node":"method:A.txt.a0","summary":"x"},{"node":"method:A.txt.a1","summary":"y"},{"node":"me"#.into(),
                        })
                    }
                    Head::Truncated => {
                        return Err(Error::truncated("réponse tronquée par la limite de jetons"))
                    }
                }
            }
            g.ok.extend(requested.iter().cloned());
            Ok(Enrichment::default())
        }
        fn enrich_lot(&self, _: &[LotItem]) -> Result<Vec<(String, Result<Enrichment>)>> {
            Err(Error::Message("inutilisé".into()))
        }
        fn ask(&self, _: &str, _: &str) -> Result<crate::llm::AskReply> {
            Err(Error::Message("inutilisé".into()))
        }
    }

    /// Fichier `A.txt` dont chaque ligne est un morceau ; `lines[i]` = nœuds du morceau i, nommés
    /// `<lettre>0`, `<lettre>1`… Un seul morceau : texte quelconque.
    fn repo(letters: &[char], per_chunk: usize) -> (tempfile::TempDir, Vec<FileRecord>, Vec<Node>) {
        let dir = tempfile::tempdir().unwrap();
        let mut text = String::new();
        let mut nodes = Vec::new();
        for l in letters {
            for i in 0..per_chunk {
                text.push_str(&format!("{l}{i} "));
                nodes.push(Node::new(
                    NodeKind::Method,
                    &format!("A.txt.{l}{i}"),
                    &format!("{l}{i}"),
                    Some("A.txt"),
                    None,
                    None,
                ));
            }
            text.push('\n');
        }
        std::fs::write(dir.path().join("A.txt"), &text).unwrap();
        let f = FileRecord {
            path: "A.txt".into(),
            hash: String::new(),
            size: text.len() as u64,
            mtime: 0,
            recorded_at: 0,
            state: crate::model::FileState::Indexed,
        };
        (dir, vec![f], nodes)
    }

    fn run(
        dir: &tempfile::TempDir,
        files: &[FileRecord],
        nodes: &[Node],
        llm: &Gate,
        concurrency: usize,
        max_calls: Option<usize>,
    ) -> (Vec<(String, Error)>, BuildReport) {
        let _t = crate::interrupt::test_lock();
        let mut store = Store::open_or_create(dir.path()).unwrap();
        store.insert_files(files).unwrap();
        store.replace_structure("A.txt", nodes, &[]).unwrap();
        let opts = IndexOptions {
            jobs: 1,
            concurrency,
            max_chunk_chars: 1000,
            batch_files: 1,
            format_retries: 0,
            enrich_code: false,
            max_calls,
            ..IndexOptions::default()
        };
        let mut report = BuildReport::default();
        let failures = enrich_with_stop(
            Sources {
                repo: dir.path(),
                abs: &HashMap::new(),
                redact: None,
            },
            files,
            &opts,
            llm,
            &mut store,
            &mut report,
            &AtomicBool::new(false),
        )
        .unwrap();
        (failures, report)
    }

    #[test]
    fn req269_ac1_les_suivants_partent_apres_la_fin_du_premier() {
        let (dir, files, nodes) = repo(&['a'], 301);
        let llm = Gate::new(&["a0"], Head::Ok, false);
        std::thread::scope(|s| {
            let h = s.spawn(|| run(&dir, &files, &nodes, &llm, 3, None));
            assert_eq!(llm.started_stays(1), 1, "seul le premier appel part");
            llm.set(|s| s.open_heads = true);
            // Le 2e et le 3e partent ensemble (ils se chevauchent) une fois le premier fini.
            llm.wait_started(3);
            {
                let g = llm.st.lock().unwrap();
                assert!(
                    g.started[1..].iter().all(|(_, after)| *after),
                    "{:?}",
                    g.started
                );
            }
            llm.set(|s| s.open_rest = true);
            let (failures, _) = h.join().unwrap();
            assert!(failures.is_empty(), "{failures:?}");
        });
    }

    #[test]
    fn req269_ac2_deux_morceaux_ne_s_attendent_pas() {
        let (dir, files, nodes) = repo(&['a', 'b'], 151);
        let llm = Gate::new(&["a0", "b0"], Head::Ok, false);
        std::thread::scope(|s| {
            let h = s.spawn(|| run(&dir, &files, &nodes, &llm, 4, None));
            assert_eq!(llm.started_stays(2), 2, "un premier appel par morceau");
            {
                let g = llm.st.lock().unwrap();
                let mut got: Vec<&str> = g.started.iter().map(|(n, _)| n.as_str()).collect();
                got.sort_unstable();
                assert_eq!(got, ["method:A.txt.a0", "method:A.txt.b0"]);
            }
            llm.set(|s| {
                s.open_heads = true;
                s.open_rest = true;
            });
            let (failures, _) = h.join().unwrap();
            assert!(failures.is_empty(), "{failures:?}");
            assert_eq!(llm.st.lock().unwrap().started.len(), 4);
        });
    }

    #[test]
    fn req269_ac3_premier_appel_en_echec_pas_d_attente_infinie() {
        let (dir, files, nodes) = repo(&['a'], 301);
        let llm = Gate::new(&["a0"], Head::Fails, true);
        std::thread::scope(|s| {
            let h = s.spawn(|| run(&dir, &files, &nodes, &llm, 3, None));
            assert_eq!(llm.started_stays(1), 1);
            llm.set(|s| s.open_heads = true);
            let (failures, _) = h.join().unwrap();
            assert_eq!(failures.len(), 1, "{failures:?}");
            assert_eq!(failures[0].0, "A.txt");
            assert_eq!(llm.st.lock().unwrap().started.len(), 1, "suivants sautés");
        });
    }

    #[test]
    fn req269_ac4_premier_appel_tronque_rien_n_est_perdu() {
        let (dir, files, nodes) = repo(&['a'], 301);
        let llm = Gate::new(&["a0"], Head::Truncated, true);
        std::thread::scope(|s| {
            let h = s.spawn(|| run(&dir, &files, &nodes, &llm, 3, None));
            assert_eq!(llm.started_stays(1), 1);
            llm.set(|s| s.open_heads = true);
            let (failures, _) = h.join().unwrap();
            assert!(failures.is_empty(), "{failures:?}");
            let mut ok = llm.st.lock().unwrap().ok.clone();
            ok.sort_unstable();
            ok.dedup();
            assert_eq!(ok.len(), 301, "tous les nœuds demandés");
        });
    }

    #[test]
    fn req269_ac3_premier_appel_refuse_par_le_plafond_suivants_sautes() {
        let (dir, files, nodes) = repo(&['a'], 301);
        let llm = Gate::new(&["a0"], Head::Capped, true);
        std::thread::scope(|s| {
            let h = s.spawn(|| run(&dir, &files, &nodes, &llm, 3, None));
            assert_eq!(llm.started_stays(1), 1);
            llm.set(|s| s.open_heads = true);
            h.join().unwrap();
            assert_eq!(llm.st.lock().unwrap().started.len(), 1, "suivants sautés");
        });
    }

    #[test]
    fn req269_ac4_premier_appel_tronque_avec_resumes_recus() {
        let (dir, files, nodes) = repo(&['a'], 301);
        let llm = Gate::new(&["a0"], Head::Partial, true);
        std::thread::scope(|s| {
            let h = s.spawn(|| run(&dir, &files, &nodes, &llm, 3, None));
            assert_eq!(llm.started_stays(1), 1);
            llm.set(|s| s.open_heads = true);
            let (failures, _) = h.join().unwrap();
            assert!(failures.is_empty(), "{failures:?}");
            let mut ok = llm.st.lock().unwrap().ok.clone();
            ok.sort_unstable();
            ok.dedup();
            // Les 2 résumés reçus sont gardés : le reste (299 nœuds) repart, suivants compris.
            assert_eq!(ok.len(), 299);
            assert!(!ok.iter().any(|n| n.ends_with(".a0") || n.ends_with(".a1")));
        });
    }

    #[test]
    fn req269_ac5_plafond_inferieur_aux_appels_refuse_avant_tout_envoi() {
        let (dir, files, nodes) = repo(&['a'], 301);
        let llm = Gate::new(&["a0"], Head::Ok, true);
        let (failures, _) = run(&dir, &files, &nodes, &llm, 3, Some(2));
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            matches!(failures[0].1, Error::CallsOverCap { calls: 3, max: 2 }),
            "{failures:?}"
        );
        assert!(llm.st.lock().unwrap().started.is_empty());
    }

    #[test]
    fn req269_ac5_file_fermee_un_appel_a_suivants_compte_dans_expanding() {
        let f: &'static FileRecord = Box::leak(Box::new(FileRecord {
            path: "A.txt".into(),
            hash: String::new(),
            size: 1,
            mtime: 0,
            recorded_at: 0,
            state: crate::model::FileState::Indexed,
        }));
        let call = |followers: Vec<Unit<'static>>| Unit::Call {
            file: f,
            index: 1,
            count: 2,
            excerpt: None,
            part: Arc::from("x"),
            requested: vec![],
            known: Arc::from(Vec::<String>::new()),
            path: vec![],
            cache: true,
            followers,
        };
        let shared = Mutex::new(Shared {
            queue: vec![call(vec![call(vec![])])].into(),
            failed: HashSet::new(),
            expanding: 0,
            split_calls: HashMap::new(),
        });
        let wake = Condvar::new();
        let stop = AtomicBool::new(false);
        let capped = AtomicBool::new(true);
        let got = next_unit(&shared, &wake, &stop, &capped);
        assert!(has_followers(&got.unwrap()));
        assert_eq!(shared.lock().unwrap().expanding, 1);
    }

    #[test]
    fn req269_ac5_plafond_et_fils_disponibles_pendant_le_premier_appel() {
        let (dir, files, nodes) = repo(&['a'], 301);
        let llm = Gate::new(&["a0"], Head::Ok, false);
        std::thread::scope(|s| {
            let h = s.spawn(|| run(&dir, &files, &nodes, &llm, 2, Some(3)));
            assert_eq!(llm.started_stays(1), 1);
            llm.set(|s| s.open_heads = true);
            // Les deux fils sont encore là : le 2e et le 3e appel sont en cours en même temps.
            llm.wait_started(3);
            llm.set(|s| s.open_rest = true);
            let (failures, report) = h.join().unwrap();
            assert!(failures.is_empty(), "{failures:?}");
            assert_eq!(report.capped, None);
            assert_eq!(llm.st.lock().unwrap().started.len(), 3);
        });
    }
}
