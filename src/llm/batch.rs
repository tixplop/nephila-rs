//! Collecteur de requêtes du mode lot (REQ-214, DEC-168) : l'adaptateur lui demande la réponse à
//! un corps de requête ; connue, elle est rendue, sinon la requête est mise de côté pour le lot
//! du fournisseur et l'appel finit en `Error::Deferred`.

use super::CallStats;
use crate::error::{Error, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Requêtes au plus par lot du fournisseur.
pub const MAX_LOT_LINES: usize = 50_000;
/// Octets au plus du fichier JSONL d'un lot du fournisseur.
pub const MAX_LOT_BYTES: usize = 200_000_000;

/// Enveloppe d'une ligne JSONL la plus longue des deux fournisseurs (`openai`), sans l'identifiant
/// ni le corps ; la fin de ligne s'ajoute.
const LONGEST_ENVELOPE: &str =
    r#"{"custom_id":"","method":"POST","url":"/v1/chat/completions","body":}"#;
/// Longueur d'un `custom_id` : une empreinte BLAKE3 en hexadécimal.
const ID_CHARS: usize = 64;

/// Réponse du fournisseur à une requête du lot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Corps de la réponse, tel que le synchrone le recevrait.
    Raw(String),
    /// Requête en échec : `cause` est le texte de l'avertissement (A-17) ; `refused` = message d'un
    /// refus HTTP 400 ou `invalid_request_error`, lu par le repli de `structured_output`.
    Failed {
        cause: String,
        refused: Option<String>,
    },
}

/// Requête mise de côté : `id` = empreinte de son corps, aussi son `custom_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub id: String,
    pub body: Value,
}

#[derive(Default)]
struct State {
    answers: HashMap<String, Answer>,
    pending: Vec<Pending>,
    seen: HashSet<String>,
}

#[derive(Default)]
pub struct Collector {
    state: Mutex<State>,
    closed: AtomicBool,
}

/// `custom_id` d'un corps de requête.
pub fn body_id(body: &Value) -> String {
    blake3::hash(body.to_string().as_bytes())
        .to_hex()
        .to_string()
}

impl Collector {
    pub fn new() -> Collector {
        Collector::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Une fois fermé, une requête inconnue n'est plus mise de côté : `Error::Capped`.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// Enregistre la réponse à une requête (par son `custom_id`).
    pub fn add_answer(&self, id: String, answer: Answer) {
        self.state().answers.insert(id, answer);
    }

    /// Requêtes mises de côté depuis le dernier appel, dans l'ordre de leur arrivée.
    pub fn take_pending(&self) -> Vec<Pending> {
        let mut st = self.state();
        st.seen.clear();
        std::mem::take(&mut st.pending)
    }

    /// Réponse à `body` : texte rendu si elle est connue, erreur du lot si la requête a échoué,
    /// sinon requête mise de côté (`Deferred`) ou, collecteur fermé, `Capped`.
    pub fn exchange(&self, body: &Value, stats: &CallStats) -> Result<String> {
        let id = body_id(body);
        let mut st = self.state();
        match st.answers.get(&id) {
            Some(Answer::Raw(text)) => {
                stats.add_send();
                Ok(text.clone())
            }
            Some(Answer::Failed { cause, refused }) => {
                stats.add_send();
                Err(Error::BatchItem {
                    cause: cause.clone(),
                    refused: refused.clone(),
                })
            }
            None if self.closed.load(Ordering::SeqCst) => Err(Error::Capped),
            None => {
                if st.seen.insert(id.clone()) {
                    st.pending.push(Pending {
                        id,
                        body: body.clone(),
                    });
                }
                Err(Error::Deferred)
            }
        }
    }
}

/// Octets d'une ligne JSONL complète de la requête, fin de ligne comprise.
fn line_bytes(p: &Pending) -> usize {
    LONGEST_ENVELOPE.len() + ID_CHARS + p.body.to_string().len() + 1
}

/// Découpe les requêtes en lots d'au plus `max_lines` lignes et `max_bytes` octets, dans l'ordre.
/// Une requête seule plus grande que `max_bytes` forme son propre lot (le fournisseur la refusera).
pub fn split_lots(requests: Vec<Pending>, max_lines: usize, max_bytes: usize) -> Vec<Vec<Pending>> {
    let mut lots: Vec<Vec<Pending>> = Vec::new();
    let mut bytes = 0usize;
    for p in requests {
        let n = line_bytes(&p);
        match lots.last_mut() {
            Some(lot) if lot.len() < max_lines && bytes + n <= max_bytes => {
                lot.push(p);
                bytes += n;
            }
            _ => {
                lots.push(vec![p]);
                bytes = n;
            }
        }
    }
    lots
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(n: usize) -> Value {
        json!({"n": n})
    }

    fn pending(n: usize) -> Pending {
        let b = body(n);
        Pending {
            id: body_id(&b),
            body: b,
        }
    }

    fn pending_of_size(n: usize, bytes: usize) -> Pending {
        let b = json!({"n": n, "pad": "x".repeat(bytes)});
        Pending {
            id: body_id(&b),
            body: b,
        }
    }

    #[test]
    fn req214_ac8_limites_du_fournisseur() {
        assert_eq!(MAX_LOT_LINES, 50_000);
        assert_eq!(MAX_LOT_BYTES, 200_000_000);
    }

    #[test]
    fn req214_ac8_50001_petites_requetes_donnent_2_lots() {
        let reqs: Vec<Pending> = (0..50_001).map(pending).collect();
        let lots = split_lots(reqs.clone(), MAX_LOT_LINES, MAX_LOT_BYTES);
        assert_eq!(lots.len(), 2);
        assert_eq!(lots[0].len(), 50_000);
        assert_eq!(lots[1].len(), 1);
        let flat: Vec<Pending> = lots.into_iter().flatten().collect();
        assert_eq!(flat, reqs);
        let exact = split_lots(reqs[..50_000].to_vec(), MAX_LOT_LINES, MAX_LOT_BYTES);
        assert_eq!(exact.len(), 1);
    }

    #[test]
    fn req214_ac8_octets_comptes_en_ligne_jsonl_complete() {
        let reqs: Vec<Pending> = (0..5).map(|i| pending_of_size(i, 1000)).collect();
        let size = line_bytes(&reqs[0]);
        // Une ligne complète d'openai, fin de ligne comprise.
        let line = format!(
            r#"{{"custom_id":"{}","method":"POST","url":"/v1/chat/completions","body":{}}}"#,
            reqs[0].id, reqs[0].body
        );
        assert_eq!(size, line.len() + 1);
        // Exactement 2 lignes par lot : 2 * size tient, 2 * size - 1 ne tient pas.
        assert_eq!(split_lots(reqs.clone(), 100, 2 * size).len(), 3);
        assert_eq!(split_lots(reqs.clone(), 100, 2 * size - 1).len(), 5);
        assert_eq!(split_lots(reqs.clone(), 100, 5 * size).len(), 1);
        // Total = limite + 1 octet : 2 lots.
        let lots = split_lots(reqs.clone(), 100, 5 * size - 1);
        assert_eq!(lots.len(), 2);
        for lot in &lots {
            assert!(lot.iter().map(line_bytes).sum::<usize>() < 5 * size);
        }
        let flat: Vec<Pending> = lots.into_iter().flatten().collect();
        assert_eq!(flat, reqs);
    }

    #[test]
    fn req214_ac8_requete_trop_grosse_seule_dans_son_lot() {
        let reqs = vec![pending(1), pending_of_size(2, 5000), pending(3)];
        let lots = split_lots(reqs, 100, 1000);
        assert_eq!(lots.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 1]);
        assert!(split_lots(Vec::new(), 10, 10).is_empty());
    }

    #[test]
    fn req214_ac8_corps_identiques_une_seule_requete_deux_reponses() {
        let c = Collector::new();
        let b = body(1);
        let s = CallStats::default();
        assert_eq!(c.exchange(&b, &s), Err(Error::Deferred));
        assert_eq!(c.exchange(&b, &s), Err(Error::Deferred));
        let pend = c.take_pending();
        assert_eq!(
            pend,
            vec![Pending {
                id: body_id(&b),
                body: b.clone()
            }]
        );
        c.add_answer(pend[0].id.clone(), Answer::Raw("rep".into()));
        assert_eq!(c.exchange(&b, &s).unwrap(), "rep");
        assert_eq!(c.exchange(&b, &s).unwrap(), "rep");
        assert!(c.take_pending().is_empty());
    }

    #[test]
    fn req214_reponse_connue_rendue_et_comptee_comme_un_envoi() {
        let c = Collector::new();
        let b = body(1);
        c.add_answer(body_id(&b), Answer::Raw("texte".into()));
        let s = CallStats::default();
        assert_eq!(c.exchange(&b, &s).unwrap(), "texte");
        assert_eq!(s.sends(), 1);
        // Requête inconnue : rien n'est compté.
        let s = CallStats::default();
        assert_eq!(c.exchange(&body(2), &s), Err(Error::Deferred));
        assert_eq!(s.sends(), 0);
    }

    #[test]
    fn req214_requetes_inconnues_mises_de_cote_dans_l_ordre() {
        let c = Collector::new();
        let s = CallStats::default();
        for n in [3, 1, 2, 1] {
            assert_eq!(c.exchange(&body(n), &s), Err(Error::Deferred));
        }
        let ids: Vec<String> = c.take_pending().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, [3, 1, 2].map(|n| body_id(&body(n))));
    }

    #[test]
    fn req214_collecteur_ferme_capped_rien_de_cote() {
        let c = Collector::new();
        let s = CallStats::default();
        c.add_answer(body_id(&body(1)), Answer::Raw("ok".into()));
        c.close();
        assert_eq!(c.exchange(&body(2), &s), Err(Error::Capped));
        assert!(c.take_pending().is_empty());
        // Une réponse connue reste rendue.
        assert_eq!(c.exchange(&body(1), &s).unwrap(), "ok");
    }

    #[test]
    fn req214_custom_id_est_64_hexa_minuscules() {
        let id = body_id(&body(7));
        assert_eq!(id.len(), 64);
        assert!(id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        assert_eq!(id, body_id(&body(7)));
        assert_ne!(id, body_id(&body(8)));
    }

    #[test]
    fn req214_reponse_en_echec_donne_batch_item() {
        let c = Collector::new();
        let b = body(1);
        c.add_answer(
            body_id(&b),
            Answer::Failed {
                cause: "expirée".into(),
                refused: Some("bad".into()),
            },
        );
        let s = CallStats::default();
        let e = c.exchange(&b, &s).unwrap_err();
        assert_eq!(
            e,
            Error::BatchItem {
                cause: "expirée".into(),
                refused: Some("bad".into())
            }
        );
        assert_eq!(
            e.to_string(),
            "requête du lot du fournisseur en échec (expirée) ; relancez nephila update --batch"
        );
        assert_eq!(s.sends(), 1);
    }
}
