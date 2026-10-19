//! `package.json` (DEC-040) : amorce, structure vide en attendant l'extracteur npm.

use std::path::Path;

use super::FileStructure;

/// Extrait la structure d'un `package.json` (pour l'instant vide).
pub fn extract(_path: &Path, _text: &str) -> FileStructure {
    FileStructure::default()
}
