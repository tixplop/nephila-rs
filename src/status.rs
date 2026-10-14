//! Commande `status` (REQ-012) : lecture seule, sans LLM ni configuration.

use std::path::Path;

use serde_json::json;

use crate::diff;
use crate::error::Result;
use crate::store::Store;

pub fn run(repo: &Path, as_json: bool) -> Result<()> {
    let store = Store::open_readonly(repo)?;
    let d = diff::compute(repo, &store)?;
    if as_json {
        let files: Vec<_> = d
            .changes
            .iter()
            .map(|c| json!({"chemin": c.path, "état": c.change.label()}))
            .collect();
        let v = json!({"a_jour": d.is_up_to_date(), "fichiers": files});
        println!("{v}");
        return Ok(());
    }
    if d.is_up_to_date() {
        println!("graphe à jour");
    } else {
        println!("graphe pas à jour");
        for c in &d.changes {
            println!("{}\t{}", c.change.label(), c.path);
        }
    }
    Ok(())
}
