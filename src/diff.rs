//! Comparaison du parcours du dépôt avec la table `files` (REQ-012, réutilisée par update
//! et par l'avertissement « pas à jour » des requêtes).

use std::collections::HashMap;
use std::path::Path;

use crate::error::Result;
use crate::model::{FileRecord, FileState};
use crate::scan::{self, ScannedFile};
use crate::store::Store;

/// État d'un fichier qui diffère du graphe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Pending,
}

impl Change {
    pub fn label(self) -> &'static str {
        match self {
            Change::Added => "ajouté",
            Change::Modified => "modifié",
            Change::Deleted => "supprimé",
            Change::Pending => "en attente",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub change: Change,
}

/// Résultat de la comparaison ; `scanned` permet à `update` de ne pas reparcourir.
#[derive(Debug, Default)]
pub struct Diff {
    /// Changements triés par chemin.
    pub changes: Vec<FileChange>,
    pub scanned: Vec<ScannedFile>,
    pub known: Vec<FileRecord>,
    /// Dossiers contenant un `settings.gradle(.kts)`, avec `rootProject.name` s'il est connu.
    pub gradle_settings: HashMap<String, Option<String>>,
}

impl Diff {
    pub fn is_up_to_date(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn paths(&self, change: Change) -> impl Iterator<Item = &str> {
        self.changes
            .iter()
            .filter(move |c| c.change == change)
            .map(|c| c.path.as_str())
    }
}

/// Compare le dépôt avec les fichiers connus du graphe. Un fichier en attente reste « en
/// attente » même s'il a changé (il sera retraité de toute façon) ; absent du disque, il est
/// « supprimé ».
pub fn compute(repo: &Path, store: &Store) -> Result<Diff> {
    let known = store.files()?;
    let scan::ScanResult {
        files: scanned,
        gradle_settings,
        ..
    } = scan::scan(repo, &known)?;
    let on_disk: HashMap<&str, &ScannedFile> =
        scanned.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut changes = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for k in &known {
        seen.insert(k.path.as_str());
        let change = match (on_disk.get(k.path.as_str()), k.state) {
            (None, _) => Some(Change::Deleted),
            (Some(_), FileState::Pending) => Some(Change::Pending),
            (Some(f), FileState::Indexed) if f.hash != k.hash => Some(Change::Modified),
            _ => None,
        };
        if let Some(change) = change {
            changes.push(FileChange {
                path: k.path.clone(),
                change,
            });
        }
    }
    for f in &scanned {
        if !seen.contains(f.path.as_str()) {
            changes.push(FileChange {
                path: f.path.clone(),
                change: Change::Added,
            });
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Diff {
        changes,
        scanned,
        known,
        gradle_settings,
    })
}
