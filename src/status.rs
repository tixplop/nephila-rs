//! Commande `status` (REQ-012) : lecture seule, sans LLM ni configuration.

use std::path::Path;

use serde_json::json;

use crate::diff;
use crate::error::Result;
use crate::filter::ProjectFilters;
use crate::out::Out;
use crate::store::Store;

pub fn run(repo: &Path, as_json: bool, out: &mut Out) -> Result<()> {
    let matcher = ProjectFilters::load_matcher(repo)?;
    let store = Store::open_readonly(repo)?;
    let d = diff::compute(repo, &store, &matcher, crate::index::default_jobs())?;
    let to_enrich = store.count_files_to_enrich()?;
    if as_json {
        let files: Vec<_> = d
            .changes
            .iter()
            .map(|c| json!({"chemin": c.path, "état": c.change.label()}))
            .collect();
        let v = json!({"a_jour": d.is_up_to_date(), "fichiers": files, "to_enrich": to_enrich});
        writeln!(out, "{v}")?;
        return Ok(());
    }
    if d.is_up_to_date() {
        writeln!(out, "graphe à jour")?;
    } else {
        writeln!(out, "graphe pas à jour")?;
        for c in &d.changes {
            writeln!(out, "{}\t{}", c.change.label(), c.path)?;
        }
    }
    if to_enrich > 0 {
        writeln!(out, "{to_enrich} fichiers à enrichir (résumés et concepts)")?;
    }
    Ok(())
}
