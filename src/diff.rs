//! Comparaison du parcours du dépôt avec la table `files` (REQ-012, réutilisée par update
//! et par l'avertissement « pas à jour » des requêtes).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::error::Result;
use crate::filter::Matcher;
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
    /// Fichiers que le parcours aurait retenus sans filtre (REQ-037).
    pub excluded: usize,
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
/// « supprimé » (c'est aussi le cas d'un fichier indexé que `matcher` exclut désormais).
pub fn compute(repo: &Path, store: &Store, matcher: &Matcher) -> Result<Diff> {
    let known = store.files()?;
    let scan::ScanResult {
        files: scanned,
        gradle_settings,
        excluded,
        ..
    } = scan::scan(repo, &known, matcher)?;
    let on_disk: HashMap<&str, &ScannedFile> =
        scanned.iter().map(|f| (f.path.as_str(), f)).collect();
    // Mise à niveau 0.9 -> 0.10 : un `package.json` enregistré comme document de données doit
    // être réanalysé comme fichier de build même si son contenu n'a pas changé (AC-035-6).
    let is_pkg = |p: &str| p.rsplit('/').next() == Some("package.json");
    let pkgs: Vec<&str> = known
        .iter()
        .filter(|k| k.state == FileState::Indexed && is_pkg(&k.path))
        .map(|k| k.path.as_str())
        .collect();
    let legacy_pkg: HashSet<String> = if pkgs.is_empty() {
        HashSet::new()
    } else {
        store.files_declaring_document(&pkgs)?.into_iter().collect()
    };
    let mut changes = Vec::new();
    let mut seen = HashSet::new();
    for k in &known {
        seen.insert(k.path.as_str());
        let change = match (on_disk.get(k.path.as_str()), k.state) {
            (None, _) => Some(Change::Deleted),
            (Some(_), FileState::Pending) => Some(Change::Pending),
            (Some(f), FileState::Indexed) if f.hash != k.hash || legacy_pkg.contains(&k.path) => {
                Some(Change::Modified)
            }
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
        excluded,
    })
}

/// Nombre de fichiers qui diffèrent du graphe d'après leur seule taille et leur date de
/// modification (millisecondes) : ajoutés, supprimés, en attente, ou indexés dont l'une des deux
/// valeurs diffère de `files`. Seuls les fichiers absents de `files` sont lus (pour écarter ceux
/// que `update` écarterait, DEC-068) ; ni empreinte, ni `settings.gradle`
/// (REQ-017, DEC-067).
pub fn quick(repo: &Path, store: &Store, matcher: &Matcher) -> Result<usize> {
    let known = store.files()?;
    let walked = scan::walk(repo, matcher)?;
    let on_disk: HashMap<&str, &scan::WalkedFile> =
        walked.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut count = 0;
    for k in &known {
        let changed = match (on_disk.get(k.path.as_str()), k.state) {
            (None, _) | (Some(_), FileState::Pending) => true,
            (Some(f), FileState::Indexed) => f.size != k.size || f.mtime != k.mtime,
        };
        count += usize::from(changed);
    }
    let seen: HashSet<&str> = known.iter().map(|k| k.path.as_str()).collect();
    count += walked
        .files
        .iter()
        .filter(|f| !seen.contains(f.path.as_str()))
        // Un fichier que `update` écarterait (illisible, non UTF-8) n'est pas « ajouté » (DEC-068).
        .filter(|f| scan::read_utf8(&f.abs, &f.path).is_ok())
        .count();
    Ok(count)
}
