//! Enrichissement par le mode lot du fournisseur (`--batch`, REQ-211, DEC-168).

use crate::error::{warn, Error, Result};
use crate::index::build::{BuildReport, IndexOptions};
use crate::index::enrich::{enrich_pass, PassMode, Sources};
use crate::llm::batch::{
    split_lots, Answer, BatchLink, CreateError, LotStatus, Pending, MAX_LOT_BYTES, MAX_LOT_LINES,
};
use crate::llm::{http, poll_interval, Llm};
use crate::model::FileRecord;
use crate::store::{address_fingerprint, EnrichBatch, Store};
use crate::trace;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Erreur E-31 (sans le préfixe `erreur: `) : seule source du texte.
pub(crate) const E31: &str =
    "ce fournisseur n'a pas de mode lot ; relancez nephila update sans --batch";

/// Un lot créé chez le fournisseur et les requêtes qu'il porte.
type Lot = (String, Vec<Pending>);

/// Enrichit `files` par le mode lot (DEC-168) : passes de collecte, envoi des requêtes mises de
/// côté, attente, réponses données au collecteur, jusqu'à ce que plus rien ne soit mis de côté ;
/// puis une dernière passe qui écrit tout. Sans mode lot (`mock`, serveur sans lot), la structure
/// reste écrite, les fichiers restent à enrichir et la commande se termine par E-31 ; rien ne part
/// au plein tarif. Un refus ou une panne à la création arrête l'envoi, sans dernière passe.
pub(crate) fn enrich_batched(
    sources: Sources<'_>,
    files: &[FileRecord],
    opts: &IndexOptions,
    llm: &dyn Llm,
    store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    let Some(link) = llm.batch() else {
        report.refused = Some(Error::Message(E31.to_string()));
        return Ok(());
    };
    let poll = poll_interval()?;
    let stop = AtomicBool::new(false);
    let mut registered = store.enrich_batches()?;
    loop {
        // Passe de collecte : rien n'est écrit, le bilan est jetable.
        let mut scratch = BuildReport::default();
        crate::trace::mute(true);
        let collected = enrich_pass(
            sources,
            files,
            opts,
            llm,
            store,
            &mut scratch,
            &stop,
            PassMode::Collect,
        );
        crate::trace::mute(false);
        collected?;
        let pending = link.collector.take_pending();
        if pending.is_empty() {
            break;
        }
        let mut lots: Vec<Lot> = Vec::new();
        for requests in split_lots(pending, MAX_LOT_LINES, MAX_LOT_BYTES) {
            match link.api.create(&requests, &stop) {
                Ok((id, _bytes)) => {
                    // Avant toute autre écriture : l'identifiant est ce qui permet de reprendre.
                    registered.push(EnrichBatch {
                        id: id.clone(),
                        provider: link.api.provider().to_string(),
                        address: address_fingerprint(&link.api.address()),
                    });
                    store.set_enrich_batches(&registered)?;
                    warn(&format!(
                        "lot du fournisseur envoyé : {} requêtes, identifiant {id}, réponse en 24 h au plus ; Ctrl-C ne perd rien : relancez nephila update --batch pour reprendre",
                        requests.len()
                    ));
                    lots.push((id, requests));
                }
                Err(e) => {
                    refuse_creation(report, e);
                    return Ok(());
                }
            }
        }
        if let Err(e) = await_lots(link, &lots, poll, &stop) {
            report.refused = Some(e);
            return Ok(());
        }
    }
    link.collector.close();
    enrich_pass(
        sources,
        files,
        opts,
        llm,
        store,
        report,
        &stop,
        PassMode::Last { capped: None },
    )?;
    store.set_enrich_batches(&[])
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

/// Attend la fin de tous les lots (un sondage par lot à chaque intervalle), lit leurs résultats
/// et donne au collecteur une réponse à chaque requête envoyée.
fn await_lots(link: BatchLink<'_>, lots: &[Lot], poll: Duration, stop: &AtomicBool) -> Result<()> {
    let mut statuses: Vec<Option<LotStatus>> = vec![None; lots.len()];
    while statuses.iter().any(Option::is_none) {
        http::wait(poll, Some(stop));
        for ((id, _), slot) in lots.iter().zip(statuses.iter_mut()) {
            if slot.is_none() {
                let status = link.api.status(id, stop)?;
                if status.finished {
                    *slot = Some(status);
                }
            }
        }
    }
    for ((_, requests), status) in lots.iter().zip(statuses.into_iter().flatten()) {
        let ids: HashSet<&str> = requests.iter().map(|p| p.id.as_str()).collect();
        let mut answered: HashSet<String> = HashSet::new();
        let results = match &status.failure {
            // Lot refusé en entier : toutes ses requêtes ont la même issue.
            Some(_) => Vec::new(),
            None => link.api.results(&status, stop)?,
        };
        for (id, answer) in results {
            if ids.contains(id.as_str()) && answered.insert(id.clone()) {
                link.collector.add_answer(id, answer);
            }
        }
        let default: &Answer = status.failure.as_ref().unwrap_or(&status.default_answer);
        for p in requests {
            if !answered.contains(&p.id) {
                link.collector.add_answer(p.id.clone(), default.clone());
            }
        }
    }
    Ok(())
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
