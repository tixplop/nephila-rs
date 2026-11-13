//! Commande `nephila cache export|import` (REQ-295, DEC-226) : les enrichissements du graphe,
//! emportés hors du dépôt. Ni configuration, ni parcours du dépôt, ni appel au LLM.

use std::path::Path;

use crate::error::{Error, Result};
use crate::index::lock::RepoLock;
use crate::out::Out;
use crate::store::{ensure_no_symlink, Store, SCHEMA_VERSION};

/// Écrit les enrichissements du graphe dans `fichier` (E-41 si le graphe est d'une version
/// précédente).
pub fn export(repo: &Path, fichier: &Path, out: &mut Out) -> Result<()> {
    let store = Store::open_readonly(repo)?;
    if store.opened_schema_version() < SCHEMA_VERSION {
        return Err(Error::Message(format!(
            "graphe d'une version précédente dans {} : lancez `nephila update`",
            repo.display()
        )));
    }
    let n = store.export_enrichments(fichier)?;
    writeln!(out, "cache export terminé : {n} enrichissements écrits")?;
    Ok(())
}

/// Ajoute au graphe les enrichissements de `fichier`, sans écraser ceux qui existent.
pub fn import(repo: &Path, fichier: &Path, out: &mut Out) -> Result<()> {
    Store::check_enrichments_file(fichier)?;
    ensure_no_symlink(repo)?;
    let _lock = RepoLock::acquire(repo)?;
    let mut store = Store::open_or_create(repo)?;
    let (added, present) = store.import_enrichments(fichier)?;
    writeln!(
        out,
        "cache import terminé : {added} enrichissements ajoutés, {present} déjà présents ; lancez nephila build pour les appliquer"
    )?;
    Ok(())
}
