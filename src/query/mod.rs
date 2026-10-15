//! Commandes `query` (lecture seule, sans LLM ni configuration).

pub mod ask;
pub mod graph;
pub mod lookup;
pub mod neighbors;
pub mod path;
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

/// Ligne « N résultat(s) supplémentaire(s) » avec l'accord correct.
pub fn remaining_line(n: usize) -> String {
    if n == 1 {
        "1 résultat supplémentaire".to_string()
    } else {
        format!("{n} résultats supplémentaires")
    }
}

/// Genre affiché d'un nœud : sous-genre s'il existe, sinon genre.
pub fn label(n: &crate::model::Node) -> &'static str {
    n.subkind.map_or(n.kind.as_str(), |s| s.as_str())
}

/// `nom.qualifié (genre, fichier)`.
pub fn describe(n: &crate::model::Node) -> String {
    match &n.file {
        Some(f) => format!("{} ({}, {f})", n.qualified, label(n)),
        None => format!("{} ({})", n.qualified, label(n)),
    }
}
