//! Extraction de la structure d'un fichier par analyse syntaxique (DEC-016 à DEC-020).

pub mod java;
pub mod jvm;
pub mod lang;

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::llm::extract::{Declared, Reference};

pub use lang::Lang;

/// Structure d'un fichier : même forme que l'ancienne `Extraction`, sans résumés.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileStructure {
    /// Contexte de résolution (package, imports), au format de `resolve::parse_context`.
    pub context: Option<String>,
    pub declares: Vec<Declared>,
    pub references: Vec<Reference>,
}

/// Contexte d'extraction fourni par l'appelant (utile aux scripts Gradle).
#[derive(Debug, Clone, Default)]
pub struct ParseCtx {
    /// Racine du dépôt, si connue.
    pub repo_root: Option<PathBuf>,
}

/// Extrait la structure de `text`. Java est extrait ; les autres langages renvoient pour
/// l'instant une structure vide, comme un fichier de type inconnu.
pub fn extract(path: &Path, text: &str, ctx: &ParseCtx) -> Result<FileStructure> {
    let _ = ctx;
    match Lang::from_path(path) {
        Some(Lang::Java) => java::extract(text),
        Some(Lang::Kotlin)
        | Some(Lang::KotlinGradle)
        | Some(Lang::Scala)
        | Some(Lang::Groovy)
        | Some(Lang::GroovyGradle)
        | Some(Lang::Xml)
        | Some(Lang::Markdown)
        | None => Ok(FileStructure::default()),
    }
}
