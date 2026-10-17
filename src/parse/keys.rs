//! Clés de configuration des fichiers JSON et YAML : dédoublonnage, tri, plafond et références
//! `mentions` (REQ-032, REQ-033, DEC-038).

use std::collections::BTreeMap;
use std::path::Path;

use super::data::{self, Candidates, Format};
use super::{Declared, FileStructure};
use crate::model::{node_id, NodeKind};

/// Plafond de clés retenues par fichier.
pub const MAX_KEYS: usize = 1_000;

/// Clés feuilles d'un fichier et candidats de nom JVM de leurs valeurs.
#[derive(Debug, Default)]
pub struct KeySet {
    /// Chemin de clé → candidats (noms cibles normalisés, dans l'ordre de lecture).
    keys: BTreeMap<String, Vec<String>>,
    /// Candidats rattachés au document.
    doc: Vec<String>,
}

impl KeySet {
    pub fn new() -> KeySet {
        KeySet::default()
    }

    /// Enregistre la clé feuille `path` (sans candidat).
    pub fn add_key(&mut self, path: &str) {
        self.keys.entry(path.to_string()).or_default();
    }

    /// Enregistre la clé feuille `path` et les candidats de nom JVM de `value`.
    pub fn add_key_value(&mut self, path: &str, value: &str) {
        self.keys
            .entry(path.to_string())
            .or_default()
            .extend(data::jvm_candidates(value));
    }

    /// Candidats de nom JVM de `value`, rattachés au document.
    pub fn add_doc_value(&mut self, value: &str) {
        self.doc.extend(data::jvm_candidates(value));
    }

    /// Structure du fichier : document, nœuds `key` (1 000 premiers chemins par ordre
    /// lexicographique), références `mentions` et avertissements de plafond.
    pub fn finish(self, path: &Path, format: Format) -> FileStructure {
        let doc_path = data::doc_path(path);
        let doc_id = data::document_id(path);
        let mut declares = vec![data::document(path, format)];
        let mut cands = Candidates::new();
        let mut warnings = Vec::new();
        for n in self.doc {
            cands.add_name(&doc_id, n);
        }
        let total = self.keys.len();
        let mut dropped: Vec<String> = Vec::new();
        for (i, (key, names)) in self.keys.into_iter().enumerate() {
            if i < MAX_KEYS {
                let qualified = format!("{doc_path}#{key}");
                let id = node_id(NodeKind::Key, &qualified);
                declares.push(Declared {
                    kind: NodeKind::Key,
                    qualified,
                    subkind: None,
                    summary: None,
                    short: Some(key),
                });
                for n in names {
                    cands.add_name(&id, n);
                }
            } else {
                dropped.extend(names);
            }
        }
        for n in dropped {
            cands.add_name(&doc_id, n);
        }
        if total > MAX_KEYS {
            warnings.push(format!(
                "{doc_path} : {} clés non retenues (plafond de 1 000)",
                total - MAX_KEYS
            ));
        }
        let (references, w) = cands.finish(path);
        warnings.extend(w);
        FileStructure {
            context: None,
            declares,
            references,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedoublonne_trie_et_rattache_les_candidats() {
        let mut k = KeySet::new();
        k.add_key_value("b", "x.A");
        k.add_key("a");
        k.add_key_value("b", "x.B x.A");
        k.add_doc_value("x.D");
        let f = k.finish(Path::new("c/a.json"), Format::Json);
        let ids: Vec<String> = f.declares.iter().map(|d| d.id()).collect();
        assert_eq!(
            ids,
            ["document:c/a.json", "key:c/a.json#a", "key:c/a.json#b"]
        );
        assert_eq!(f.declares[2].short.as_deref(), Some("b"));
        let refs: Vec<(&str, &str)> = f
            .references
            .iter()
            .map(|r| (r.source.as_str(), r.target.as_str()))
            .collect();
        assert_eq!(
            refs,
            [
                ("document:c/a.json", "x.D"),
                ("key:c/a.json#b", "x.A"),
                ("key:c/a.json#b", "x.B")
            ]
        );
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn plafond_de_cles_et_candidats_des_cles_ecartees_au_document() {
        let mut k = KeySet::new();
        // Ordre mélangé : le tri doit être fait par `finish`, pas par l'appelant.
        for j in 0..1_500 {
            let i = (j * 7 + 3) % 1_500;
            if i >= 1_000 {
                k.add_key_value(&format!("k{i:04}"), &format!("p.C{i}"));
            } else {
                k.add_key(&format!("k{i:04}"));
            }
        }
        let f = k.finish(Path::new("a.json"), Format::Json);
        assert_eq!(f.declares.len(), 1_001);
        assert_eq!(f.declares[1_000].short.as_deref(), Some("k0999"));
        assert_eq!(f.declares[1].short.as_deref(), Some("k0000"));
        assert!(!f
            .declares
            .iter()
            .any(|d| d.short.as_deref() == Some("k1000")));
        assert_eq!(
            f.warnings,
            ["a.json : 500 clés non retenues (plafond de 1 000)"]
        );
        // Les candidats des 500 clés écartées sont rattachés au document.
        assert_eq!(f.references.len(), 500);
        assert!(f.references.iter().all(|r| r.source == "document:a.json"));
    }
}
