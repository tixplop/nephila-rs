//! Résolution d'un nom saisi par l'utilisateur vers un nœud du graphe (partagée par les
//! requêtes `neighbors`, `path`, ...).
//!
//! Formes acceptées, sensibles à la casse : identifiant complet (`type:com.acme.A`), nom
//! qualifié, nom court, ou suffixe du nom qualifié commençant à une frontière de segment
//! (`OrderRepository.save`, `core.OrderService`). Un nom qualifié exact l'emporte sur les
//! correspondances par nom court ou par suffixe.

use crate::error::{Error, Result};
use crate::model::Node;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(Node),
    /// Plusieurs nœuds correspondent ; triés par identifiant.
    Ambiguous(Vec<Node>),
    NotFound,
}

fn is_suffix(qualified: &str, name: &str) -> bool {
    qualified
        .strip_suffix(name)
        .and_then(|head| head.chars().next_back())
        .is_some_and(|c| matches!(c, '.' | '#' | '/' | ':'))
}

fn classify(mut found: Vec<Node>) -> Lookup {
    found.sort_by(|a, b| a.id.cmp(&b.id));
    match found.len() {
        0 => Lookup::NotFound,
        1 => Lookup::Found(found.remove(0)),
        _ => Lookup::Ambiguous(found),
    }
}

pub fn resolve(store: &Store, name: &str) -> Result<Lookup> {
    if name.is_empty() {
        return Ok(Lookup::NotFound);
    }
    let nodes = store.nodes()?;
    if let Some(n) = nodes.iter().find(|n| n.id == name) {
        return Ok(Lookup::Found(n.clone()));
    }
    let exact: Vec<Node> = nodes
        .iter()
        .filter(|n| n.qualified == name)
        .cloned()
        .collect();
    if !exact.is_empty() {
        return Ok(classify(exact));
    }
    let loose: Vec<Node> = nodes
        .into_iter()
        .filter(|n| n.short == name || is_suffix(&n.qualified, name))
        .collect();
    Ok(classify(loose))
}

/// Comme [`resolve`], mais l'ambiguïté (avec les candidats) et l'absence sont des erreurs.
pub fn require(store: &Store, name: &str) -> Result<Node> {
    match resolve(store, name)? {
        Lookup::Found(n) => Ok(n),
        Lookup::NotFound => Err(Error::Message(format!(
            "nœud introuvable : « {name} » (essayez `rkg query search`)"
        ))),
        Lookup::Ambiguous(c) => {
            let list: Vec<String> = c.iter().map(|n| format!("  {}", n.id)).collect();
            Err(Error::Message(format!(
                "« {name} » est ambigu, précisez avec le nom qualifié :\n{}",
                list.join("\n")
            )))
        }
    }
}
