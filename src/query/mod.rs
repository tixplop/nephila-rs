//! Commandes `query` (lecture seule, sans LLM ni configuration).

pub mod lookup;
pub mod search;

use std::path::Path;

use crate::diff;
use crate::error::{self, Result};
use crate::store::Store;

/// Ouvre le graphe pour une requête. Sans graphe : erreur invitant à lancer `rkg build`.
/// Si des fichiers ont changé depuis l'indexation, avertit sur stderr (REQ-017) ; la requête
/// continue sur le dernier état enregistré.
pub fn open(repo: &Path) -> Result<Store> {
    let store = Store::open_readonly(repo)?;
    let n = diff::compute(repo, &store)?.changes.len();
    if n > 0 {
        let (noun, adj) = if n == 1 {
            ("fichier", "modifié")
        } else {
            ("fichiers", "modifiés")
        };
        error::warn(&format!("graphe pas à jour : {n} {noun} {adj}"));
    }
    Ok(store)
}
