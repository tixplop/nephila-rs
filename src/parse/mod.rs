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
pub(crate) mod walk;
pub mod xml_data;
pub mod yaml;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::model::{node_id, EdgeKind, NodeKind, SubKind};

pub use lang::Lang;

/// Ensemble ordonné (DEC-090) : garde le premier exemplaire de chaque valeur, dans l'ordre
/// d'insertion, en temps constant amorti par insertion.
#[derive(Debug, Clone)]
pub struct OrderedSet<T: Eq + Hash + Clone> {
    items: Vec<T>,
    seen: HashSet<T>,
}

impl<T: Eq + Hash + Clone> Default for OrderedSet<T> {
    fn default() -> Self {
        OrderedSet {
            items: Vec::new(),
            seen: HashSet::new(),
        }
    }
}

impl<T: Eq + Hash + Clone> OrderedSet<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ajoute `x` s'il est absent ; `true` si l'ajout a eu lieu.
    pub fn insert(&mut self, x: T) -> bool {
        if self.seen.contains(&x) {
            return false;
        }
        self.seen.insert(x.clone());
        self.items.push(x);
        true
    }

    pub fn as_slice(&self) -> &[T] {
        &self.items
    }

    pub fn into_vec(self) -> Vec<T> {
        self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
        Some(Lang::PackageJson) => Ok(npm::extract(path, text, ctx)),
        Some(Lang::KotlinGradle) | Some(Lang::GroovyGradle) => Ok(gradle::extract(path, text, ctx)),
        None => Ok(FileStructure::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    fn oracle<T: PartialEq + Clone>(items: &[T]) -> Vec<T> {
        let mut v = Vec::new();
        for x in items {
            if !v.contains(x) {
                v.push(x.clone());
            }
        }
        v
    }

    fn check<T: Eq + Hash + Clone + std::fmt::Debug>(mut gen: impl FnMut(usize) -> T) {
        let mut rng = Lcg(0x5eed);
        for _ in 0..300 {
            let n = rng.next(2001);
            let items: Vec<T> = (0..n).map(|_| gen(rng.next(1_000_000))).collect();
            let mut set = OrderedSet::new();
            let mut firsts = 0;
            for x in &items {
                if set.insert(x.clone()) {
                    firsts += 1;
                }
            }
            assert_eq!(set.len(), firsts);
            assert_eq!(set.is_empty(), items.is_empty());
            let expected = oracle(&items);
            assert_eq!(set.as_slice(), expected.as_slice());
            assert_eq!(set.into_vec(), expected);
        }
    }

    #[test]
    fn req077_ac1_ensemble_ordonne_comme_vec_contains() {
        let kinds = [EdgeKind::Extends, EdgeKind::Imports, EdgeKind::Calls];
        check(|r| Reference {
            source: format!("type:S{}", r % 4),
            kind: kinds[(r / 4) % 3],
            target: format!("T{}", (r / 12) % 5),
        });
        check(|r| jvm::Import {
            path: format!("p.C{}", r % 6),
            wildcard: (r / 6) % 2 == 0,
            is_static: (r / 12) % 2 == 0,
        });
        check(|r| format!("s{}", r % 40));
    }

    /// Résumé en clair : contexte, puis références `source kind -> cible`.
    fn resume(path: &str, text: &str) -> (Vec<String>, Vec<String>) {
        let f = extract(Path::new(path), text, &ParseCtx::default()).unwrap();
        let ctx = f
            .context
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let refs = f
            .references
            .iter()
            .map(|r| format!("{} {} {}", r.source, r.kind.as_str(), r.target))
            .collect();
        (ctx, refs)
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    const AC2_CASES: [(&str, &str); 6] = [
        (
            "X.java",
            "package p;\nimport a.B;\nimport c.*;\nimport a.B;\nimport c.*;\nimport a.B;\nimport c.*;\nclass X implements Y, Z, Y, Z, Y {\n  void m() { Util.run(); other(); Util.run(); other(); Util.run(); }\n}\n",
        ),
        (
            "X.kt",
            "package p\nimport a.B\nimport c.*\nimport a.B\nimport c.*\nimport a.B\nimport c.*\nclass X : Y(), Z, Y(), Z, Y() {\n  fun m() { Util.run(); other(); Util.run(); other(); Util.run() }\n}\n",
        ),
        (
            "X.groovy",
            "package p\nimport a.B\nimport c.*\nimport a.B\nimport c.*\nimport a.B\nimport c.*\nclass X extends Y implements Z, Z, Z {\n  void m() { Util.run(); other(); Util.run(); other(); Util.run() }\n}\n",
        ),
        (
            "X.scala",
            "package p\nimport a.B\nimport c._\nimport a.B\nimport c._\nimport a.B\nimport c._\nclass X extends Y with Z with Z with Z {\n  def m(): Unit = { Util.run(); other(); Util.run(); other(); Util.run() }\n}\n",
        ),
        (
            "src/x.js",
            "import { B } from './a';\nimport * as ns from './c';\nimport { B } from './a';\nimport * as ns from './c';\nimport { B } from './a';\nimport * as ns from './c';\nclass X extends Y {\n  m() { B.run(); ns.go(); B.run(); ns.go(); B.run(); }\n}\n",
        ),
        (
            "src/x.ts",
            "import { B } from './a';\nimport * as ns from './c';\nimport { B } from './a';\nimport * as ns from './c';\nimport { B } from './a';\nimport * as ns from './c';\nclass X extends Y implements Z, Z, Z {\n  m() { B.run(); ns.go(); B.run(); ns.go(); B.run(); }\n}\n",
        ),
    ];

    #[test]
    fn req077_ac2_repetitions_presentes_une_fois_dans_l_ordre() {
        let jvm_ctx = v(&["package p", "import a.B", "import c.*"]);
        let js_ctx = v(&[
            "package src/x",
            "import src/a.B",
            "import src/a/index.B",
            "import src/c.go",
            "import src/c/index.go",
        ]);
        let expected: [(Vec<String>, Vec<String>); 6] = [
            (
                jvm_ctx.clone(),
                v(&[
                    "type:p.X implements Y",
                    "type:p.X implements Z",
                    "method:p.X.m calls Util.run",
                    "method:p.X.m calls other",
                    "type:p.X imports a.B",
                ]),
            ),
            (
                jvm_ctx.clone(),
                v(&[
                    "type:p.X extends Y",
                    "type:p.X implements Z",
                    "method:p.X.m calls Util.run",
                    "method:p.X.m calls other",
                    "type:p.X imports a.B",
                ]),
            ),
            (
                jvm_ctx.clone(),
                v(&[
                    "type:p.X extends Y",
                    "type:p.X implements Z",
                    "method:p.X.m calls Util.run",
                    "method:p.X.m calls other",
                    "type:p.X imports a.B",
                ]),
            ),
            (
                jvm_ctx,
                v(&[
                    "type:p.X extends Y",
                    "type:p.X implements Z",
                    "method:p.X.m calls Util.run",
                    "method:p.X.m calls other",
                    "type:p.X imports a.B",
                ]),
            ),
            (
                js_ctx.clone(),
                v(&[
                    "package:src/x imports B",
                    "type:src/x.X extends Y",
                    "method:src/x.X.m calls B.run",
                    "method:src/x.X.m calls go",
                ]),
            ),
            (
                js_ctx,
                v(&[
                    "package:src/x imports B",
                    "type:src/x.X extends Y",
                    "type:src/x.X implements Z",
                    "method:src/x.X.m calls B.run",
                    "method:src/x.X.m calls go",
                ]),
            ),
        ];
        for ((p, t), want) in AC2_CASES.iter().zip(expected) {
            assert_eq!(resume(p, t), want, "{p}");
        }
    }

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
