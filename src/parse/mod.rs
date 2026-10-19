//! Extraction de la structure d'un fichier par analyse syntaxique (DEC-016 à DEC-020).

pub mod calls;
pub mod data;
pub mod gradle;
pub mod gradle_lex;
pub mod groovy;
pub mod ivy;
pub mod java;
pub mod js;
pub mod js_calls;
pub mod js_imports;
pub mod json;
pub mod jvm;
pub mod keys;
pub mod kotlin;
pub mod lang;
pub mod markdown;
pub mod maven;
pub mod npm;
pub mod scala;
pub mod sql;
pub mod xml_data;
pub mod yaml;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::{node_id, EdgeKind, NodeKind, SubKind};

pub use lang::Lang;

/// Nœud déclaré par le fichier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub kind: NodeKind,
    pub qualified: String,
    pub subkind: Option<SubKind>,
    pub summary: Option<String>,
    /// Nom court fixé par l'extracteur (DEC-024) ; `None` = dérivé du nom qualifié.
    pub short: Option<String>,
}

impl Declared {
    pub fn id(&self) -> String {
        node_id(self.kind, &self.qualified)
    }
}

/// Référence par nom depuis un nœud déclaré.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// Identifiant `genre:nom.qualifié` de la source.
    pub source: String,
    pub kind: EdgeKind,
    /// Nom cible tel qu'écrit dans le fichier.
    pub target: String,
}

/// Structure d'un fichier : nœuds déclarés, références par nom et contexte de résolution.
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
        // Fichiers de données (DEC-038).
        Some(Lang::Sql) => Ok(sql::extract(path, text)),
        Some(Lang::DataXml) => Ok(xml_data::extract(path, text)),
        Some(Lang::Json) => Ok(json::extract(path, text)),
        Some(Lang::Yaml) => Ok(yaml::extract(path, text)),
        Some(l @ (Lang::JavaScript | Lang::TypeScript | Lang::Tsx)) => js::extract(l, path, text),
        Some(Lang::PackageJson) => Ok(npm::extract(path, text)),
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
        // XML hors build : le document seul.
        let f = extract(Path::new("x/other.xml"), pom, &ctx).unwrap();
        assert_eq!(f.declares.len(), 1);
        assert_eq!(f.declares[0].id(), "document:x/other.xml");
        assert_eq!(f.declares[0].subkind, Some(SubKind::Xml));
        assert!(f.references.is_empty());
    }

    #[test]
    fn extract_aiguille_les_formats_de_donnees_vers_un_document() {
        let ctx = ParseCtx::default();
        for (p, sub) in [
            ("db/a.sql", SubKind::Sql),
            ("a.json", SubKind::Json),
            ("a.yaml", SubKind::Yaml),
            ("a.yml", SubKind::Yaml),
        ] {
            let f = extract(Path::new(p), "", &ctx).unwrap();
            assert_eq!(f.declares.len(), 1, "{p}");
            assert_eq!(f.declares[0].id(), format!("document:{p}"));
            assert_eq!(f.declares[0].subkind, Some(sub));
            assert!(f.warnings.is_empty());
        }
    }
}
