//! Extraction de la structure d'un fichier par analyse syntaxique (DEC-016 à DEC-020).

pub mod calls;
pub mod gradle;
pub mod gradle_lex;
pub mod groovy;
pub mod ivy;
pub mod java;
pub mod jvm;
pub mod kotlin;
pub mod lang;
pub mod markdown;
pub mod maven;
pub mod scala;

use std::collections::HashMap;
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
    /// Avertissements à remonter (fichier exploité au mieux : XML mal formé…), un message par ligne.
    pub warnings: Vec<String>,
}

/// Contexte d'extraction fourni par l'appelant (utile aux scripts Gradle).
#[derive(Debug, Clone, Default)]
pub struct ParseCtx {
    /// Racine du dépôt, si connue.
    pub repo_root: Option<PathBuf>,
    /// Dossiers (relatifs, `""` = racine) contenant un `settings.gradle(.kts)` → `rootProject.name` s'il est connu.
    pub gradle_settings: HashMap<String, Option<String>>,
}

/// Extrait la structure de `text`. Java, Kotlin, Scala et Groovy sont extraits ; les autres langages renvoient pour
/// l'instant une structure vide, comme un fichier de type inconnu.
pub fn extract(path: &Path, text: &str, ctx: &ParseCtx) -> Result<FileStructure> {
    match Lang::from_path(path) {
        Some(Lang::Java) => java::extract(text),
        Some(Lang::Kotlin) => kotlin::extract(path, text),
        Some(Lang::Scala) => scala::extract(path, text),
        Some(Lang::Groovy) => groovy::extract(path, text),
        Some(Lang::Xml) => match path.file_name().and_then(|n| n.to_str()) {
            Some("pom.xml") => Ok(maven::extract(path, text)),
            Some("ivy.xml") => Ok(ivy::extract(path, text)),
            _ => Ok(FileStructure::default()),
        },
        Some(Lang::Markdown) => Ok(markdown::extract(path, text)),
        Some(Lang::KotlinGradle) | Some(Lang::GroovyGradle) => Ok(gradle::extract(path, text, ctx)),
        None => Ok(FileStructure::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_aiguille_pom_vers_maven_et_ivy_vers_ivy() {
        let ctx = ParseCtx::default();
        let pom = "<project><groupId>g</groupId><artifactId>a</artifactId></project>";
        let f = extract(Path::new("x/pom.xml"), pom, &ctx).unwrap();
        assert_eq!(f.declares[0].id(), "module:g:a");
        let ivy = r#"<ivy-module><info organisation="o" module="m"/></ivy-module>"#;
        let f = extract(Path::new("x/ivy.xml"), ivy, &ctx).unwrap();
        assert_eq!(f.declares[0].id(), "module:o:m");
        let f = extract(Path::new("x/other.xml"), pom, &ctx).unwrap();
        assert!(f.declares.is_empty());
    }
}
