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
    /// JavaScript, JSX, CommonJS et modules ES (DEC-039).
    JavaScript,
    /// TypeScript (`.ts`, `.mts`, `.cts`, dont `.d.ts`).
    TypeScript,
    /// TypeScript avec JSX (`.tsx`).
    Tsx,
    /// `package.json` (DEC-040).
    PackageJson,
}

impl Lang {
    /// Langage déduit du nom de fichier ; `None` si le fichier n'est pas analysé.
    pub fn from_path(path: &Path) -> Option<Lang> {
        let name = path.file_name()?.to_str()?;
        match name {
            "build.gradle" | "settings.gradle" => return Some(Lang::GroovyGradle),
            "build.gradle.kts" | "settings.gradle.kts" => return Some(Lang::KotlinGradle),
            "pom.xml" | "ivy.xml" => return Some(Lang::Xml),
            "package.json" => return Some(Lang::PackageJson),
            _ => {}
        }
        let ext = path.extension()?.to_str()?;
        match ext {
            "java" => Some(Lang::Java),
            "kt" | "kts" => Some(Lang::Kotlin),
            "scala" => Some(Lang::Scala),
            "groovy" | "gradle" => Some(Lang::Groovy),
            "js" | "mjs" | "cjs" | "jsx" => Some(Lang::JavaScript),
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
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
            Lang::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            Lang::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            Lang::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            Lang::Xml
            | Lang::PackageJson
            | Lang::Markdown
            | Lang::Sql
            | Lang::DataXml
            | Lang::Json
            | Lang::Yaml => None,
        }
    }

    /// Vrai pour JavaScript, TypeScript et TSX.
    pub fn is_js_ts(self) -> bool {
        matches!(self, Lang::JavaScript | Lang::TypeScript | Lang::Tsx)
    }

    /// Case du cache de parsers (une par grammaire ; les scripts Gradle partagent celle de leur langage).
    fn grammar_slot(self) -> Option<usize> {
        match self {
            Lang::Java => Some(0),
            Lang::Kotlin | Lang::KotlinGradle => Some(1),
            Lang::Scala => Some(2),
            Lang::Groovy | Lang::GroovyGradle => Some(3),
            Lang::JavaScript => Some(4),
            Lang::TypeScript => Some(5),
            Lang::Tsx => Some(6),
            Lang::Xml
            | Lang::PackageJson
            | Lang::Markdown
            | Lang::Sql
            | Lang::DataXml
            | Lang::Json
            | Lang::Yaml => None,
        }
    }

    /// Analyse `text` ; l'arbre est tolérant aux erreurs (nœuds `ERROR`/manquants). Le parser est
    /// pris dans un cache propre au fil, un par grammaire (DEC-090) ; l'analyse est toujours complète.
    pub fn parse(self, text: &str) -> Result<tree_sitter::Tree> {
        #[cfg(test)]
        if crate::diag::force_fresh() {
            return tests::parse_fresh(self, text);
        }
        let (grammar, slot) = self
            .grammar()
            .zip(self.grammar_slot())
            .ok_or_else(|| Error::Message(format!("{self:?} n'a pas de grammaire tree-sitter")))?;
        PARSERS.with(|cell| {
            let mut parsers = cell.borrow_mut();
            if parsers[slot].is_none() {
                let mut parser = tree_sitter::Parser::new();
                parser.set_language(&grammar).map_err(|e| {
                    Error::Message(format!("grammaire {self:?} incompatible : {e}"))
                })?;
                crate::diag::add(crate::diag::Counter::ParsersCreated, 1);
                parsers[slot] = Some(parser);
            }
            let parser = parsers[slot].as_mut().expect("parser créé ci-dessus");
            let tree = parser.parse(text, None);
            if tree.is_none() {
                parser.reset();
            }
            tree.ok_or_else(|| Error::Message(format!("analyse {self:?} interrompue")))
        })
    }
}

thread_local! {
    /// Parsers du fil courant, un par grammaire, créés au premier usage (REQ-078).
    static PARSERS: std::cell::RefCell<[Option<tree_sitter::Parser>; 7]> =
        const { std::cell::RefCell::new([None, None, None, None, None, None, None]) };
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Implémentation de la 0.23.2 : un parser neuf par appel.
    pub(crate) fn parse_fresh(lang: Lang, text: &str) -> Result<tree_sitter::Tree> {
        let grammar = lang
            .grammar()
            .ok_or_else(|| Error::Message(format!("{lang:?} n'a pas de grammaire tree-sitter")))?;
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&grammar)
            .map_err(|e| Error::Message(format!("grammaire {lang:?} incompatible : {e}")))?;
        parser
            .parse(text, None)
            .ok_or_else(|| Error::Message(format!("analyse {lang:?} interrompue")))
    }

    fn sample_texts() -> Vec<(Lang, String)> {
        let mut v = vec![
            (
                Lang::Java,
                "package p;\npublic class A { void m() { b(); } }\n".to_string(),
            ),
            (
                Lang::Java,
                "package p; class { void ( ;;; }}}\n".to_string(),
            ),
            (
                Lang::Groovy,
                "class G { def f() { println 'x' } }\n".to_string(),
            ),
            (
                Lang::GroovyGradle,
                "plugins { id 'java' }\ndependencies { implementation 'a:b:1' }\n".to_string(),
            ),
            (Lang::Groovy, "def x = [1, 2,\nclass {{{ ))\n".to_string()),
            (
                Lang::Tsx,
                "export const A = () => <div>{x}</div>;\n".to_string(),
            ),
            (Lang::Tsx, "const = <div <<< >>> {{ ;\n".to_string()),
            (Lang::Java, String::new()),
            (Lang::Tsx, String::new()),
            (Lang::Groovy, String::new()),
        ];
        let mut big = String::from("package p;\npublic class Big {\n");
        let mut i = 0;
        while big.len() < 1_000_000 {
            big.push_str(&format!("  void m{i}() {{ n{i}(); }}\n"));
            i += 1;
        }
        big.push_str("}\n");
        v.push((Lang::Java, big));
        v.push((
            Lang::Groovy,
            "class G { def f() { println 'y' } }\n".to_string(),
        ));
        v.push((Lang::Tsx, "let a: number = ;\n".to_string()));
        v
    }

    fn check_suite(texts: &[(Lang, String)], refs: &[String], rounds: usize) {
        for round in 0..rounds {
            for ((lang, text), expected) in texts.iter().zip(refs) {
                let a = lang.parse(text).unwrap().root_node().to_sexp();
                assert_eq!(
                    &a,
                    expected,
                    "{lang:?} tour {round} ({} octets)",
                    text.len()
                );
            }
        }
    }

    /// AC-078-2 : sur un même fil, suite alternée ; puis sur 4 fils en parallèle.
    #[test]
    fn req078_ac2_arbres_identiques_a_un_parser_neuf() {
        let texts = sample_texts();
        let refs: Vec<String> = texts
            .iter()
            .map(|(lang, text)| parse_fresh(*lang, text).unwrap().root_node().to_sexp())
            .collect();
        check_suite(&texts, &refs, 2);
        std::thread::scope(|s| {
            for _ in 0..4 {
                std::thread::Builder::new()
                    .stack_size(16 * 1024 * 1024)
                    .spawn_scoped(s, || check_suite(&texts, &refs, 1))
                    .unwrap();
            }
        });
    }

    #[test]
    fn req078_grammaires_partagees_par_les_scripts_gradle() {
        assert_eq!(
            Lang::Kotlin.grammar_slot(),
            Lang::KotlinGradle.grammar_slot()
        );
        assert_eq!(
            Lang::Groovy.grammar_slot(),
            Lang::GroovyGradle.grammar_slot()
        );
        assert_eq!(Lang::Json.grammar_slot(), None);
        let slots: std::collections::HashSet<_> = [
            Lang::Java,
            Lang::Kotlin,
            Lang::Scala,
            Lang::Groovy,
            Lang::JavaScript,
            Lang::TypeScript,
            Lang::Tsx,
        ]
        .iter()
        .map(|l| l.grammar_slot().unwrap())
        .collect();
        assert_eq!(slots.len(), 7);
        assert!(Lang::Json.parse("{}").is_err());
    }

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
        assert_eq!(l("a.js"), Some(Lang::JavaScript));
        assert_eq!(l("a.mjs"), Some(Lang::JavaScript));
        assert_eq!(l("a.cjs"), Some(Lang::JavaScript));
        assert_eq!(l("a.jsx"), Some(Lang::JavaScript));
        assert_eq!(l("a.ts"), Some(Lang::TypeScript));
        assert_eq!(l("a.mts"), Some(Lang::TypeScript));
        assert_eq!(l("a.cts"), Some(Lang::TypeScript));
        assert_eq!(l("a.d.ts"), Some(Lang::TypeScript));
        assert_eq!(l("a.tsx"), Some(Lang::Tsx));
        assert_eq!(l("x/package.json"), Some(Lang::PackageJson));
        assert_eq!(l("tsconfig.json"), Some(Lang::Json));
        assert_eq!(l("a.vue"), None);
        assert!(Lang::Tsx.is_js_ts() && Lang::JavaScript.is_js_ts() && Lang::TypeScript.is_js_ts());
        assert!(!Lang::Json.is_js_ts() && !Lang::PackageJson.is_js_ts() && !Lang::Java.is_js_ts());
        assert_eq!(l("a.properties"), None);
        assert_eq!(l("a.jsonc"), None);
        assert_eq!(l("noext"), None);
    }
}
