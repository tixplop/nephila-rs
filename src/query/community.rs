//! `query community` (REQ-148) : liste des communautés du graphe, ou détail d'une communauté
//! (numéro, ou communauté d'un nœud) avec ses membres classés par degré décroissant.

use std::path::Path;

use serde_json::{json, Value};

use super::lookup;
use super::search::{self, DEFAULT_LIMIT};
use crate::error::{Error, Result};
use crate::out::Out;
use crate::store::CommunityRow;

/// Ligne d'une communauté, construite ici seulement (composant « Ligne de communauté ») :
/// `<numéro> <nom> (<taille> nœuds, cohésion <c>)` en variante complète, sans la cohésion en
/// variante courte. Non échappée : l'appelant passe la ligne entière par `escape::data`.
pub(crate) fn community_line(row: &CommunityRow, full: bool) -> String {
    if full {
        format!(
            "{} {} ({} nœuds, cohésion {:.2})",
            row.id, row.label, row.size, row.cohesion
        )
    } else {
        format!("{} {} ({} nœuds)", row.id, row.label, row.size)
    }
}

fn community_json(row: &CommunityRow) -> Value {
    json!({
        "cohesion": row.cohesion,
        "id": row.id,
        "label": row.label,
        "pivot": row.pivot,
        "size": row.size,
    })
}

pub fn run(
    repo: &Path,
    number: Option<u64>,
    node: Option<&str>,
    limit: Option<usize>,
    as_json: bool,
    check_stale: bool,
    out: &mut Out,
) -> Result<()> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(Error::Message("--limit doit valoir au moins 1".to_string()));
    }
    let store = super::open(repo, check_stale)?;
    if !store.communities_computed()? {
        return Err(Error::Message(
            "communautés non calculées : lancez nephila update".to_string(),
        ));
    }
    let id: u64 = match (number, node) {
        (Some(n), _) => n,
        (None, Some(name)) => {
            let start = lookup::require(&store, name)?;
            match store.community_of(&start.id)? {
                Some(c) => c as u64,
                None => return Err(not_found_node(&start.qualified)),
            }
        }
        (None, None) => return list(&store, limit, as_json, out),
    };
    let Some(row) = i64::try_from(id)
        .ok()
        .map(|i| store.community(i))
        .transpose()?
        .flatten()
    else {
        return Err(Error::Message(format!(
            "aucun résultat : communauté {id} introuvable (essayez `nephila query community`)"
        )));
    };

    let degrees = store.community_degrees(row.id)?;
    let mut members = store.community_members(row.id)?;
    members.sort_by(|a, b| {
        let da = degrees.get(&a.id).copied().unwrap_or(0);
        let db = degrees.get(&b.id).copied().unwrap_or(0);
        db.cmp(&da).then_with(|| a.id.cmp(&b.id))
    });
    let remaining = members.len().saturating_sub(limit);
    members.truncate(members.len() - remaining);
    if as_json {
        let list: Vec<Value> = members.iter().map(search::to_json).collect();
        writeln!(
            out,
            "{}",
            json!({"community": community_json(&row), "members": list, "remaining": remaining})
        )?;
        return Ok(());
    }
    writeln!(out, "{}", crate::escape::data(&community_line(&row, true)))?;
    for n in &members {
        let l = format!("  {}", search::line(n));
        writeln!(out, "{}", crate::escape::data(&l))?;
    }
    if remaining > 0 {
        writeln!(out, "{}", super::remaining_line(remaining))?;
    }
    Ok(())
}

fn not_found_node(qualified: &str) -> Error {
    Error::Message(format!(
        "aucun résultat : {} n'a pas de communauté (essayez `nephila query community`)",
        crate::escape::data(qualified)
    ))
}

fn list(store: &crate::store::Store, limit: usize, as_json: bool, out: &mut Out) -> Result<()> {
    let mut rows = store.community_list()?;
    if rows.is_empty() {
        return Err(Error::Message(
            "aucun résultat : aucune communauté (graphe vide)".to_string(),
        ));
    }
    let remaining = rows.len().saturating_sub(limit);
    rows.truncate(rows.len() - remaining);
    if as_json {
        let list: Vec<Value> = rows.iter().map(community_json).collect();
        writeln!(
            out,
            "{}",
            json!({"communities": list, "remaining": remaining})
        )?;
        return Ok(());
    }
    for r in &rows {
        writeln!(out, "{}", crate::escape::data(&community_line(r, true)))?;
    }
    if remaining > 0 {
        writeln!(out, "{}", super::remaining_line(remaining))?;
    }
    Ok(())
}
