//! Choix du langage d'analyse d'après le nom du fichier (DEC-016 à DEC-018).

use std::path::Path;

use crate::error::{Error, Result};

/// Langage d'un fichier analysé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Java,
    Kotlin,
    /// Script Gradle Kotlin (`build.gradle.kts`, `settings.gradle.kts`).
    KotlinGradle,
    Scala,
    Groovy,
    /// Script Gradle Groovy (`build.gradle`, `settings.gradle`).
    GroovyGradle,
    /// `pom.xml` et `ivy.xml`.
    Xml,
    Markdown,
    Sql,
    /// XML hors build : le document seul pour l'instant (DEC-038).
    DataXml,
    Json,
    Yaml,
}

impl Lang {
    /// Langage déduit du nom de fichier ; `None` si le fichier n'est pas analysé.
    pub fn from_path(path: &Path) -> Option<Lang> {
        let name = path.file_name()?.to_str()?;
        match name {
            "build.gradle" | "settings.gradle" => return Some(Lang::GroovyGradle),
            "build.gradle.kts" | "settings.gradle.kts" => return Some(Lang::KotlinGradle),
            "pom.xml" | "ivy.xml" => return Some(Lang::Xml),
            _ => {}
        }
        let ext = path.extension()?.to_str()?;
        match ext {
            "java" => Some(Lang::Java),
            "kt" | "kts" => Some(Lang::Kotlin),
            "scala" => Some(Lang::Scala),
            "groovy" | "gradle" => Some(Lang::Groovy),
            "md" | "markdown" => Some(Lang::Markdown),
            "sql" => Some(Lang::Sql),
            "xml" => Some(Lang::DataXml),
            "json" => Some(Lang::Json),
            "yaml" | "yml" => Some(Lang::Yaml),
            _ => None,
        }
    }

    /// Grammaire tree-sitter, pour les langages de code.
    pub fn grammar(self) -> Option<tree_sitter::Language> {
        match self {
            Lang::Java => Some(tree_sitter_java::LANGUAGE.into()),
            Lang::Kotlin | Lang::KotlinGradle => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
            Lang::Scala => Some(tree_sitter_scala::LANGUAGE.into()),
            Lang::Groovy | Lang::GroovyGradle => Some(tree_sitter_groovy::LANGUAGE.into()),
            Lang::Xml | Lang::Markdown | Lang::Sql | Lang::DataXml | Lang::Json | Lang::Yaml => {
                None
            }
        }
    }

    /// Analyse `text` ; l'arbre est tolérant aux erreurs (nœuds `ERROR`/manquants).
    pub fn parse(self, text: &str) -> Result<tree_sitter::Tree> {
        let grammar = self
            .grammar()
            .ok_or_else(|| Error::Message(format!("{self:?} n'a pas de grammaire tree-sitter")))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| Error::Message(format!("grammaire {self:?} incompatible : {e}")))?;
        parser
            .parse(text, None)
            .ok_or_else(|| Error::Message(format!("analyse {self:?} interrompue")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(p: &str) -> Option<Lang> {
        Lang::from_path(Path::new(p))
    }

    #[test]
    fn choix_par_nom() {
        assert_eq!(l("a/build.gradle"), Some(Lang::GroovyGradle));
        assert_eq!(l("settings.gradle"), Some(Lang::GroovyGradle));
        assert_eq!(l("x/build.gradle.kts"), Some(Lang::KotlinGradle));
        assert_eq!(l("settings.gradle.kts"), Some(Lang::KotlinGradle));
        assert_eq!(l("run.kts"), Some(Lang::Kotlin));
        assert_eq!(l("gradle/deps.gradle"), Some(Lang::Groovy));
        assert_eq!(l("m/pom.xml"), Some(Lang::Xml));
        assert_eq!(l("ivy.xml"), Some(Lang::Xml));
        assert_eq!(l("README.md"), Some(Lang::Markdown));
        assert_eq!(l("a.markdown"), Some(Lang::Markdown));
        assert_eq!(l("A.java"), Some(Lang::Java));
        assert_eq!(l("A.kt"), Some(Lang::Kotlin));
        assert_eq!(l("A.scala"), Some(Lang::Scala));
        assert_eq!(l("A.groovy"), Some(Lang::Groovy));
        assert_eq!(l("other.xml"), Some(Lang::DataXml));
        assert_eq!(l("src/main/resources/beans.xml"), Some(Lang::DataXml));
        assert_eq!(l("db/V1.sql"), Some(Lang::Sql));
        assert_eq!(l("c.json"), Some(Lang::Json));
        assert_eq!(l("a.yaml"), Some(Lang::Yaml));
        assert_eq!(l("a.yml"), Some(Lang::Yaml));
        assert_eq!(l("a.properties"), None);
        assert_eq!(l("a.jsonc"), None);
        assert_eq!(l("noext"), None);
    }
}
