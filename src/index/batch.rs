//! Enrichissement par le mode lot du fournisseur (`--batch`, REQ-211, DEC-168).

use crate::error::{warn, Error, Result};
use crate::index::build::{BuildReport, IndexOptions};
use crate::index::enrich::{enrich_pass, PassMode, Sources, MAX_CONSECUTIVE_NETWORK_ERRORS};
use crate::llm::batch::{
    split_lots, Answer, BatchLink, CreateError, LotStatus, Pending, MAX_LOT_BYTES, MAX_LOT_LINES,
};
use crate::llm::{http, poll_interval, Llm};
use crate::model::FileRecord;
use crate::progress::{Meter, Phase};
use crate::store::{address_fingerprint, EnrichBatch, Store};
use crate::trace;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Erreur E-31 (sans le préfixe `erreur: `) : seule source du texte.
pub(crate) const E31: &str =
    "ce fournisseur n'a pas de mode lot ; relancez nephila update sans --batch";

/// Cause d'E-33 (Ctrl-C), derrière `envoi interrompu : ` : seule source du texte.
const E33_CAUSE: &str = "Ctrl-C reçu : relancez nephila update --batch pour continuer";

/// Cause d'E-35 (panne du suivi), derrière `envoi interrompu : ` : seule source du texte.
const E35_CAUSE: &str = "suivi du lot du fournisseur impossible (3 échecs consécutifs) : relancez nephila update --batch pour continuer";

/// Annonce A-16 (sans le préfixe `avertissement: `) : seule source du texte.
const A16: &str = "Ctrl-C reçu : plus rien ne part et les lots du fournisseur déjà envoyés ne sont pas annulés ; appuyez de nouveau sur Ctrl-C pour quitter sans attendre";

/// Un lot du fournisseur : ses requêtes sont connues quand la commande l'a créé, pas quand elle
/// le reprend d'une commande précédente.
struct Lot {
    id: String,
    requests: Option<Vec<Pending>>,
    /// Création du lot, ou reprise pour un lot enregistré : départ du temps de la trace `fini`.
    started: Instant,
}

/// Suite de la commande après une étape.
enum Flow {
    Go,
    Halt,
}

/// Que faire d'un échec du suivi.
enum Step {
    Retry,
    Gone,
    Halt,
}

/// Lecture des résultats d'un lot.
enum Read {
    Got(Vec<(String, Answer)>),
    Gone,
    Halt,
}

/// Enrichit `files` par le mode lot (DEC-168) : reprise des lots enregistrés, puis passes de
/// collecte, envoi des requêtes mises de côté, attente, réponses données au collecteur, jusqu'à ce
/// que plus rien ne soit mis de côté ; puis une dernière passe qui écrit tout. Sans mode lot
/// (`mock`, serveur sans lot), la structure reste écrite, les fichiers restent à enrichir et la
/// commande se termine par E-31 ; rien ne part au plein tarif. Un refus, une panne ou Ctrl-C
/// arrête l'envoi, sans dernière passe, identifiants gardés.
pub(crate) fn enrich_batched(
    sources: Sources<'_>,
    files: &[FileRecord],
    opts: &IndexOptions,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    report.batch = true;
    let Some(link) = llm.batch() else {
        report.refused = Some(Error::Message(E31.to_string()));
        return Ok(());
    };
    let poll = poll_interval()?;
    let _guard = crate::interrupt::Guard::install();
    let stop = AtomicBool::new(false);
    let done = AtomicBool::new(false);
    let registered = store.enrich_batches()?;
    crate::interrupt::set_batch_mode(true);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| watch_interrupt(&stop, &done));
        let mut driver = Driver {
            link,
            poll,
            stop: &stop,
            store,
            report,
            registered,
            failures: 0,
            sent: 0,
        };
        let result = driver.run(sources, files, opts, llm);
        done.store(true, Ordering::SeqCst);
        result
    });
    crate::interrupt::set_batch_mode(false);
    result
}

/// Fil de veille : au premier Ctrl-C, pose `stop` et écrit A-16, une fois.
fn watch_interrupt(stop: &AtomicBool, done: &AtomicBool) {
    loop {
        if crate::interrupt::received() >= 1 {
            stop.store(true, Ordering::SeqCst);
            warn(A16);
            return;
        }
        if done.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Driver<'a> {
    link: BatchLink<'a>,
    poll: Duration,
    stop: &'a AtomicBool,
    store: &'a mut Store,
    report: &'a mut BuildReport,
    /// Lots enregistrés dans le graphe, dans l'ordre de leur création.
    registered: Vec<EnrichBatch>,
    /// Échecs de suivi consécutifs, remis à zéro par une réussite.
    failures: usize,
    /// Requêtes envoyées par cette commande (les lots repris n'y comptent pas, REQ-216).
    sent: usize,
}

impl Driver<'_> {
    fn signalled(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Ctrl-C reçu : E-33, sauf si une autre cause est déjà posée.
    fn interrupt(&mut self) {
        if self.report.stop_cause.is_none() && self.report.refused.is_none() {
            self.report.stop_cause = Some(E33_CAUSE.to_string());
            self.report.interrupted = true;
        }
    }

    /// Les identifiants de lot du graphe suivent `self.registered`.
    fn save(&mut self) -> Result<()> {
        self.store.set_enrich_batches(&self.registered)
    }

    fn run(
        &mut self,
        sources: Sources<'_>,
        files: &[FileRecord],
        opts: &IndexOptions,
        llm: &dyn Llm,
    ) -> Result<()> {
        if let Flow::Halt = self.resume()? {
            return Ok(());
        }
        let stop = self.stop;
        let mut first = true;
        let mut capped = None;
        loop {
            if self.signalled() {
                self.interrupt();
                return Ok(());
            }
            // Passe de collecte : rien n'est écrit, le bilan est jetable.
            let mut scratch = BuildReport::default();
            crate::trace::mute(true);
            let collected = enrich_pass(
                sources,
                files,
                opts,
                llm,
                self.store,
                &mut scratch,
                stop,
                PassMode::Collect,
            );
            crate::trace::mute(false);
            collected?;
            if self.signalled() {
                self.interrupt();
                return Ok(());
            }
            let pending = self.link.collector.take_pending();
            if pending.is_empty() {
                break;
            }
            if let Some(m) = opts.max_calls {
                if first && pending.len() > m {
                    // E-32 : rien ne part, aucune dernière passe.
                    self.report.refused = Some(Error::Message(format!(
                        "plafond de {m} appels dépassé : le lot du fournisseur compte {} requêtes, aucune envoyée ; relancez nephila update --batch --max-calls {} ou réduisez la portée avec enrich_exclude",
                        pending.len(),
                        pending.len()
                    )));
                    return Ok(());
                }
                if self.sent + pending.len() > m {
                    // Le lot ne part pas du tout : ses fichiers restent à enrichir (A-10).
                    capped = Some(m);
                    break;
                }
            }
            first = false;
            let mut lots: Vec<Lot> = Vec::new();
            for requests in split_lots(pending, MAX_LOT_LINES, MAX_LOT_BYTES) {
                if self.signalled() {
                    self.interrupt();
                    return Ok(());
                }
                match self.link.api.create(&requests, stop) {
                    Ok((id, bytes)) => {
                        self.sent += requests.len();
                        // Avant toute autre écriture : l'identifiant permet de reprendre.
                        self.registered.push(EnrichBatch {
                            id: id.clone(),
                            provider: self.link.api.provider().to_string(),
                            address: address_fingerprint(&self.link.api.address()),
                        });
                        self.save()?;
                        warn(&format!(
                            "lot du fournisseur envoyé : {} requêtes, identifiant {id}, réponse en 24 h au plus ; Ctrl-C ne perd rien : relancez nephila update --batch pour reprendre",
                            requests.len()
                        ));
                        trace!(
                            "lot du fournisseur: créé {}, {} requêtes, {bytes} octets",
                            crate::escape::data(&id),
                            requests.len()
                        );
                        lots.push(Lot {
                            id,
                            requests: Some(requests),
                            started: Instant::now(),
                        });
                    }
                    Err(e) => {
                        if self.signalled() {
                            self.interrupt();
                        } else {
                            refuse_creation(self.report, e);
                        }
                        return Ok(());
                    }
                }
            }
            if self.signalled() {
                self.interrupt();
                return Ok(());
            }
            // Un lot tout juste créé n'est pas fini : seule la reprise sonde sans attendre.
            if http::wait(self.poll, Some(stop)) {
                self.interrupt();
                return Ok(());
            }
            if let Flow::Halt = self.await_lots(lots)? {
                return Ok(());
            }
        }
        self.link.collector.close();
        if self.signalled() {
            self.interrupt();
            return Ok(());
        }
        enrich_pass(
            sources,
            files,
            opts,
            llm,
            self.store,
            self.report,
            stop,
            PassMode::Last { capped },
        )?;
        if self.signalled() {
            // Les identifiants ne sont oubliés que si la dernière passe a tout écrit.
            if self.store.files_to_enrich()?.is_empty() {
                self.registered.clear();
                self.save()?;
            }
            self.interrupt();
            return Ok(());
        }
        self.registered.clear();
        self.save()
    }

    /// Lots enregistrés par une commande précédente : ceux d'un autre fournisseur ou d'une autre
    /// adresse sont oubliés sans requête (A-18), les autres repris (A-15) et attendus.
    fn resume(&mut self) -> Result<Flow> {
        if self.registered.is_empty() {
            return Ok(Flow::Go);
        }
        let provider = self.link.api.provider().to_string();
        let address = address_fingerprint(&self.link.api.address());
        let (kept, foreign): (Vec<EnrichBatch>, Vec<EnrichBatch>) =
            std::mem::take(&mut self.registered)
                .into_iter()
                .partition(|b| b.provider == provider && b.address == address);
        for b in &foreign {
            warn(&format!(
                "lot du fournisseur oublié : identifiant {}, envoyé à un autre fournisseur ou une autre adresse ; ses requêtes repartent dans un nouveau lot",
                b.id
            ));
        }
        self.registered = kept;
        if !foreign.is_empty() {
            self.save()?;
        }
        let lots: Vec<Lot> = self
            .registered
            .iter()
            .map(|b| Lot {
                id: b.id.clone(),
                requests: None,
                started: Instant::now(),
            })
            .collect();
        for lot in &lots {
            warn(&format!(
                "lot du fournisseur repris : identifiant {} ; Ctrl-C ne perd rien : relancez nephila update --batch pour reprendre",
                lot.id
            ));
        }
        if lots.is_empty() {
            Ok(Flow::Go)
        } else {
            self.await_lots(lots)
        }
    }

    /// Échec du suivi du lot `id` (état ou résultats). Ctrl-C : arrêt. Clé ou requête refusée :
    /// arrêt avec l'erreur du fournisseur. Lot repris inconnu du fournisseur (HTTP 404) : oublié
    /// (A-18). Autre échec : compté, E-35 au troisième de suite.
    fn track_error(&mut self, id: &str, e: Error, resumed: bool) -> Result<Step> {
        if self.signalled() {
            self.interrupt();
            return Ok(Step::Halt);
        }
        if resumed && matches!(e, Error::Rejected { code: 404, .. }) {
            warn(&format!(
                "lot du fournisseur oublié : identifiant {}, inconnu du fournisseur ou périmé ; ses requêtes repartent dans un nouveau lot",
                id
            ));
            self.registered.retain(|b| b.id != id);
            self.save()?;
            return Ok(Step::Gone);
        }
        if matches!(e, Error::InvalidKey(_)) {
            self.report.refused = Some(e);
            return Ok(Step::Halt);
        }
        self.failures += 1;
        trace!(
            "lot du fournisseur: état {} échec {}/{MAX_CONSECUTIVE_NETWORK_ERRORS} : {}",
            crate::escape::data(id),
            self.failures,
            failure_cause(&e)
        );
        if self.failures >= MAX_CONSECUTIVE_NETWORK_ERRORS {
            self.report.stop_cause = Some(E35_CAUSE.to_string());
            return Ok(Step::Halt);
        }
        Ok(Step::Retry)
    }

    /// Attend la fin de tous les lots (un sondage par lot à chaque intervalle, le premier sans
    /// attendre), lit leurs résultats et donne au collecteur une réponse à chaque requête.
    fn await_lots(&mut self, lots: Vec<Lot>) -> Result<Flow> {
        enum Slot {
            Waiting,
            Finished(LotStatus),
            Gone,
        }
        let mut slots: Vec<Slot> = lots.iter().map(|_| Slot::Waiting).collect();
        // Compteurs `(faites, total)` du dernier sondage de chaque lot, et progression V-011.
        let mut counts: Vec<(usize, usize)> = vec![(0, 0); lots.len()];
        let mut meter: Option<Meter> = None;
        loop {
            let mut waiting = false;
            for ((lot, slot), count) in lots.iter().zip(slots.iter_mut()).zip(counts.iter_mut()) {
                if !matches!(slot, Slot::Waiting) {
                    continue;
                }
                if self.signalled() {
                    self.interrupt();
                    return Ok(Flow::Halt);
                }
                match self.link.api.status(&lot.id, self.stop) {
                    Ok(status) => {
                        self.failures = 0;
                        *count = (status.done, status.total);
                        trace!(
                            "lot du fournisseur: état {} {} ({}/{} requêtes)",
                            crate::escape::data(&lot.id),
                            crate::escape::data(&status.status),
                            status.done,
                            status.total
                        );
                        if status.finished {
                            *slot = Slot::Finished(status);
                        } else {
                            waiting = true;
                        }
                    }
                    Err(e) => match self.track_error(&lot.id, e, lot.requests.is_none())? {
                        Step::Halt => return Ok(Flow::Halt),
                        Step::Gone => *slot = Slot::Gone,
                        Step::Retry => waiting = true,
                    },
                }
            }
            let (k, n) = counts.iter().fold((0, 0), |(k, n), (a, b)| (k + a, n + b));
            // Un lot sondé sans total encore (`total` 0) fausserait `n` : on attend qu'il en ait un.
            let counted = counts
                .iter()
                .zip(slots.iter())
                .all(|(c, s)| c.1 > 0 || !matches!(s, Slot::Waiting));
            if meter.is_none() && n > 0 && counted {
                meter = Meter::start(Phase::ProviderBatch, n);
            }
            if let Some(m) = meter.as_mut() {
                m.advance_to(k);
            }
            if !waiting {
                break;
            }
            if http::wait(self.poll, Some(self.stop)) {
                self.interrupt();
                return Ok(Flow::Halt);
            }
        }
        for (lot, slot) in lots.iter().zip(slots) {
            let Slot::Finished(status) = slot else {
                continue;
            };
            let results = match self.read_results(lot, &status)? {
                Read::Got(results) => results,
                Read::Gone => continue,
                Read::Halt => return Ok(Flow::Halt),
            };
            trace!(
                "lot du fournisseur: fini {} {}, {} réponses en {} ms",
                crate::escape::data(&lot.id),
                crate::escape::data(&status.status),
                results.len(),
                lot.started.elapsed().as_millis()
            );
            let mut answered: HashSet<String> = HashSet::new();
            let ids: HashSet<&str> = lot
                .requests
                .iter()
                .flatten()
                .map(|p| p.id.as_str())
                .collect();
            for (id, answer) in results {
                // Un lot repris ne connaît pas ses requêtes : toute réponse est donnée, et seules
                // celles dont le corps n'a pas changé servent.
                if (lot.requests.is_none() || ids.contains(id.as_str()))
                    && answered.insert(id.clone())
                {
                    self.link.collector.add_answer(id, answer);
                }
            }
            let default: &Answer = status.failure.as_ref().unwrap_or(&status.default_answer);
            for p in lot.requests.iter().flatten() {
                if !answered.contains(&p.id) {
                    self.link
                        .collector
                        .add_answer(p.id.clone(), default.clone());
                }
            }
        }
        Ok(Flow::Go)
    }

    /// Lit les résultats d'un lot fini, avec les nouveaux essais comptés par `track_error`.
    fn read_results(&mut self, lot: &Lot, status: &LotStatus) -> Result<Read> {
        // Lot refusé en entier : toutes ses requêtes ont la même issue.
        if status.failure.is_some() {
            return Ok(Read::Got(Vec::new()));
        }
        loop {
            if self.signalled() {
                self.interrupt();
                return Ok(Read::Halt);
            }
            match self.link.api.results(status, self.stop) {
                Ok(results) => {
                    self.failures = 0;
                    return Ok(Read::Got(results));
                }
                Err(e) => match self.track_error(&lot.id, e, lot.requests.is_none())? {
                    Step::Halt => return Ok(Read::Halt),
                    Step::Gone => return Ok(Read::Gone),
                    Step::Retry => {
                        http::wait(self.poll, Some(self.stop));
                    }
                },
            }
        }
    }
}

/// Cause d'un échec de suivi, pour la trace : `délai dépassé`, `HTTP <code>` ou `réseau`.
fn failure_cause(e: &Error) -> String {
    let text = e.to_string();
    if let Some((_, rest)) = text.split_once("(HTTP ") {
        let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !code.is_empty() {
            return format!("HTTP {code}");
        }
    }
    match e {
        Error::Network(_) | Error::Saturated(_) | Error::Timeout(_) => trace::classify(e),
        _ => "erreur".to_string(),
    }
}

/// Création refusée ou en échec : E-31, E-36, ou l'erreur du fournisseur (E-12, E-19, E-20).
fn refuse_creation(report: &mut BuildReport, e: CreateError) {
    match e {
        CreateError::NoBatchMode { code, url } => {
            trace!("lot du fournisseur: création sans mode lot (HTTP {code}) sur {url}");
            report.refused = Some(Error::Message(E31.to_string()));
        }
        CreateError::Failed { cause } => {
            report.stop_cause = Some(format!(
                "création du lot du fournisseur en échec ({cause}) : vérifiez sa console puis relancez nephila update --batch"
            ));
        }
        CreateError::Other(e) => report.refused = Some(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn req211_e31_text_and_refusal() {
        assert_eq!(
            E31,
            "ce fournisseur n'a pas de mode lot ; relancez nephila update sans --batch"
        );
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path()).unwrap();
        let mut report = BuildReport::default();
        let abs = HashMap::new();
        enrich_batched(
            Sources {
                repo: Path::new("."),
                abs: &abs,
                redact: None,
            },
            &[],
            &IndexOptions::default(),
            &crate::llm::mock::MockLlm::new(Default::default()),
            &mut store,
            &mut report,
        )
        .unwrap();
        assert_eq!(report.refused, Some(Error::Message(E31.to_string())));
    }
}
