//! Enrichissement par le mode lot du fournisseur (`--batch`, REQ-211, DEC-168).

use crate::error::{Error, Result};
use crate::index::build::{BuildReport, IndexOptions};
use crate::index::enrich::Sources;
use crate::llm::Llm;
use crate::model::FileRecord;
use crate::store::Store;

/// Erreur E-31 (sans le préfixe `erreur: `) : seule source du texte.
pub(crate) const E31: &str =
    "ce fournisseur n'a pas de mode lot ; relancez nephila update sans --batch";

/// Enrichit `files` par le mode lot. Sans mode lot, la structure reste écrite, les fichiers
/// restent à enrichir et la commande se termine par E-31 ; rien ne part au plein tarif. Le
/// pilote qui envoie les lots aux fournisseurs HTTP vient avec T-306 : ils suivent ici le même
/// chemin.
pub(crate) fn enrich_batched(
    _sources: Sources<'_>,
    _files: &[FileRecord],
    _opts: &IndexOptions,
    _llm: &dyn Llm,
    _store: &mut Store,
    report: &mut BuildReport,
) -> Result<()> {
    report.refused = Some(Error::Message(E31.to_string()));
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
