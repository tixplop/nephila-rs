//! Filtres d'analyse (REQ-037, DEC-045) : types de fichiers, motifs `.gitignore`, fichier de
//! projet `.nephila.toml`. Les filtres ne font qu'exclure : ils s'appliquent après les exclusions
//! de REQ-004 et ne peuvent jamais en réinclure un fichier.

use std::collections::BTreeSet;
use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::config::{self, ProjectSettings, CONNECTION_KEYS, PROJECT_SETTINGS};
use crate::error::{Error, Result};

/// Nom du fichier de projet, à la racine du dépôt.
pub const PROJECT_FILE: &str = ".nephila.toml";

/// Les 15 types de fichiers pris en charge, dans l'ordre de la table de REQ-037.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileType {
    Markdown,
    Java,
    Kotlin,
    Groovy,
    Scala,
    JavaScript,
    TypeScript,
    Sql,
    Xml,
    Json,
    Yaml,
    Maven,
    Ivy,
    Gradle,
    Npm,
}

/// Liste des types, telle qu'elle est affichée (aide, erreurs).
pub const TYPE_LIST: &str =
    "markdown, java, kotlin, groovy, scala, javascript, typescript, sql, xml, json, yaml, maven, ivy, gradle, npm";

impl FileType {
    pub const ALL: [FileType; 15] = [
        FileType::Markdown,
        FileType::Java,
        FileType::Kotlin,
        FileType::Groovy,
        FileType::Scala,
        FileType::JavaScript,
        FileType::TypeScript,
        FileType::Sql,
        FileType::Xml,
        FileType::Json,
        FileType::Yaml,
        FileType::Maven,
        FileType::Ivy,
        FileType::Gradle,
        FileType::Npm,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FileType::Markdown => "markdown",
            FileType::Java => "java",
            FileType::Kotlin => "kotlin",
            FileType::Groovy => "groovy",
            FileType::Scala => "scala",
            FileType::JavaScript => "javascript",
            FileType::TypeScript => "typescript",
            FileType::Sql => "sql",
            FileType::Xml => "xml",
            FileType::Json => "json",
            FileType::Yaml => "yaml",
            FileType::Maven => "maven",
            FileType::Ivy => "ivy",
            FileType::Gradle => "gradle",
            FileType::Npm => "npm",
        }
    }

    /// Nom exact (minuscules) vers type ; `None` si inconnu.
    pub fn parse(s: &str) -> Option<FileType> {
        FileType::ALL.into_iter().find(|t| t.name() == s)
    }

    /// Type d'un fichier pris en charge (un seul par fichier) ; `None` pour un fichier non pris
    /// en charge ou un fichier verrou.
    pub fn of(path: impl AsRef<Path>) -> Option<FileType> {
        let path = path.as_ref();
        let name = path.file_name()?.to_str()?;
        match name {
            "package.json" => return Some(FileType::Npm),
            "pom.xml" => return Some(FileType::Maven),
            "ivy.xml" => return Some(FileType::Ivy),
            "build.gradle.kts" | "settings.gradle.kts" => return Some(FileType::Gradle),
            "package-lock.json" | "npm-shrinkwrap.json" | "pnpm-lock.yaml" => return None,
            _ => {}
        }
        Some(match path.extension()?.to_str()? {
            "md" | "markdown" => FileType::Markdown,
            "java" => FileType::Java,
            "kt" | "kts" => FileType::Kotlin,
            "groovy" => FileType::Groovy,
            "scala" => FileType::Scala,
            "js" | "mjs" | "cjs" | "jsx" => FileType::JavaScript,
            "ts" | "mts" | "cts" | "tsx" => FileType::TypeScript,
            "sql" => FileType::Sql,
            "xml" => FileType::Xml,
            "json" => FileType::Json,
            "yaml" | "yml" => FileType::Yaml,
            "gradle" => FileType::Gradle,
            _ => return None,
        })
    }
}

/// Motif invalide : message `motif invalide `<m>` (<cause>)`.
fn pattern_error(pattern: &str, e: &ignore::Error) -> String {
    let cause = match e {
        ignore::Error::Glob { err, .. } => err.clone(),
        other => other.to_string(),
    };
    let cause = cause.lines().next().unwrap_or("").trim().to_string();
    format!("motif invalide `{pattern}` ({cause})")
}

/// Validation stricte par globset : `ignore` accepte, comme git, un `[` sans `]` (pris
/// littéralement), ce que REQ-037 rejette comme motif invalide. Le moteur de types de `ignore`
/// compile ses globs sans cette tolérance.
fn strict_check(pattern: &str) -> std::result::Result<(), String> {
    let body = pattern.strip_prefix('!').unwrap_or(pattern);
    let body = body.strip_suffix('/').unwrap_or(body);
    let mut b = ignore::types::TypesBuilder::new();
    let res = b
        .add("p", body)
        .and_then(|()| b.select("p").build().map(|_| ()));
    res.map_err(|e| pattern_error(pattern, &e))
}

/// Compile un motif seul ; erreur : texte `motif invalide ...`.
pub fn compile_pattern(root: &Path, pattern: &str) -> std::result::Result<Gitignore, String> {
    strict_check(pattern)?;
    let mut b = GitignoreBuilder::new(root);
    b.add_line(None, pattern)
        .map_err(|e| pattern_error(pattern, &e))?;
    b.build().map_err(|e| pattern_error(pattern, &e))
}

/// Filtres bruts : une seule liste de motifs (fichier de projet puis options) et des types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    pub patterns: Vec<String>,
    pub types: BTreeSet<FileType>,
}

impl Filters {
    /// Ajoute les filtres de `other` : motifs à la suite, types en union.
    pub fn merge(mut self, other: Filters) -> Filters {
        self.patterns.extend(other.patterns);
        self.types.extend(other.types);
        self
    }

    pub fn is_active(&self) -> bool {
        !self.patterns.is_empty() || !self.types.is_empty()
    }

    /// Compile les motifs pour la racine `root`.
    pub fn compile(&self, root: &Path) -> Result<Matcher> {
        let mut rules = Vec::with_capacity(self.patterns.len());
        for p in &self.patterns {
            rules.push(compile_pattern(root, p).map_err(Error::Message)?);
        }
        Ok(Matcher {
            rules,
            types: self.types.clone(),
            patterns: self.patterns.len(),
        })
    }
}

/// Filtres compilés : décision par fichier.
#[derive(Debug, Clone, Default)]
pub struct Matcher {
    rules: Vec<Gitignore>,
    types: BTreeSet<FileType>,
    patterns: usize,
}

impl Matcher {
    /// Aucun filtre.
    pub fn none() -> Matcher {
        Matcher::default()
    }

    pub fn is_active(&self) -> bool {
        self.patterns > 0 || !self.types.is_empty()
    }

    /// Le fichier `rel` (relatif à la racine, séparateur `/`) est-il exclu ? Le motif le plus
    /// récent qui s'applique au chemin ou à l'un de ses dossiers ancêtres l'emporte ; un type
    /// exclu l'est sans appel.
    pub fn excludes(&self, rel: &str) -> bool {
        if !self.is_active() {
            return false;
        }
        if !self.types.is_empty() && FileType::of(rel).is_some_and(|t| self.types.contains(&t)) {
            return true;
        }
        let mut excluded = false;
        for r in &self.rules {
            let m = r.matched_path_or_any_parents(rel, false);
            if m.is_ignore() {
                excluded = true;
            } else if m.is_whitelist() {
                excluded = false;
            }
        }
        excluded
    }

    /// Suffixe de la trace `parcours` : ` (filtres : <m> motifs, types <liste|aucun>, <k> fichiers
    /// exclus)` ; vide sans filtre.
    pub fn trace_suffix(&self, excluded: usize) -> String {
        if !self.is_active() {
            return String::new();
        }
        let types = if self.types.is_empty() {
            "aucun".to_string()
        } else {
            self.types
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            " (filtres : {} motifs, types {types}, {excluded} fichiers exclus)",
            self.patterns
        )
    }
}

/// Contenu validé du fichier de projet `<dépôt>/.nephila.toml` : filtres et réglages LLM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFile {
    pub filters: Filters,
    pub settings: ProjectSettings,
}

const EXAMPLES: [(&str, &str); 2] = [
    ("exclude", "exclude = [\"docs/legacy/\"]"),
    ("exclude_types", "exclude_types = [\"json\"]"),
];

impl ProjectFile {
    /// Lit le fichier de projet ; absent ou vide : aucun filtre ni réglage. Erreurs en une
    /// ligne (E-1 à E-6, E-9, E-10), chemin = `repo` + `/.nephila.toml` sans normalisation.
    pub fn load(repo: &Path) -> Result<ProjectFile> {
        let path = repo.join(PROJECT_FILE);
        let shown = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ProjectFile::default()),
            Err(e) => {
                return Err(Error::Message(format!(
                    "{shown} : lecture impossible : {e}"
                )))
            }
        };
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
            let line = e.span().map_or(1, |s| {
                text[..s.start.min(text.len())].matches('\n').count() + 1
            });
            let msg = e.message().lines().next().unwrap_or("").trim().to_string();
            Error::Message(format!("{shown} : TOML invalide ligne {line} : {msg}"))
        })?;
        for key in table.keys() {
            let key = key.as_str();
            if CONNECTION_KEYS.contains(&key) {
                let at = config::config_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "~/.config/nephila/config.toml".to_string());
                return Err(Error::Message(format!(
                    "{shown} : clé `{key}` permise seulement dans la configuration personnelle ({at})"
                )));
            }
            if key != "exclude" && key != "exclude_types" && !PROJECT_SETTINGS.contains(&key) {
                let allowed = ["exclude", "exclude_types"]
                    .into_iter()
                    .chain(PROJECT_SETTINGS)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(Error::Message(format!(
                    "{shown} : clé inconnue `{key}` (clés permises : {allowed})"
                )));
            }
        }
        let mut filters = Filters::default();
        for (key, example) in EXAMPLES {
            let Some(value) = table.get(key) else {
                continue;
            };
            let items: Option<Vec<&str>> = value
                .as_array()
                .and_then(|a| a.iter().map(|v| v.as_str()).collect());
            let Some(items) = items else {
                return Err(Error::Message(format!(
                    "{shown} : clé `{key}` : tableau de chaînes attendu, ex. {example}"
                )));
            };
            for item in items {
                if key == "exclude" {
                    compile_pattern(repo, item)
                        .map_err(|m| Error::Message(format!("{shown} : clé `exclude` : {m}")))?;
                    filters.patterns.push(item.to_string());
                } else {
                    let t = FileType::parse(item).ok_or_else(|| {
                        Error::Message(format!(
                            "{shown} : clé `exclude_types` : type inconnu `{item}` (types valides : {TYPE_LIST})"
                        ))
                    })?;
                    filters.types.insert(t);
                }
            }
        }
        let settings = ProjectSettings::from_table(&table, &shown)?;
        Ok(ProjectFile { filters, settings })
    }
}

/// Filtres du fichier de projet (enveloppe de `ProjectFile`, qui valide le fichier entier).
pub struct ProjectFilters;

impl ProjectFilters {
    pub fn load(repo: &Path) -> Result<Filters> {
        Ok(ProjectFile::load(repo)?.filters)
    }

    /// Lit et compile le fichier de projet.
    pub fn load_matcher(repo: &Path) -> Result<Matcher> {
        Self::load(repo)?.compile(repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(patterns: &[&str], types: &[FileType]) -> Matcher {
        Filters {
            patterns: patterns.iter().map(|s| s.to_string()).collect(),
            types: types.iter().copied().collect(),
        }
        .compile(Path::new("/r"))
        .unwrap()
    }

    #[test]
    fn req037_ac2_table_des_types() {
        let cases = [
            ("a.md", FileType::Markdown),
            ("a.markdown", FileType::Markdown),
            ("A.java", FileType::Java),
            ("a.kt", FileType::Kotlin),
            ("script.kts", FileType::Kotlin),
            ("build.gradle.kts", FileType::Gradle),
            ("x/settings.gradle.kts", FileType::Gradle),
            ("a.groovy", FileType::Groovy),
            ("a.scala", FileType::Scala),
            ("a.js", FileType::JavaScript),
            ("a.mjs", FileType::JavaScript),
            ("a.cjs", FileType::JavaScript),
            ("a.jsx", FileType::JavaScript),
            ("a.ts", FileType::TypeScript),
            ("a.mts", FileType::TypeScript),
            ("a.cts", FileType::TypeScript),
            ("a.tsx", FileType::TypeScript),
            ("a.d.ts", FileType::TypeScript),
            ("a.sql", FileType::Sql),
            ("beans.xml", FileType::Xml),
            ("a.json", FileType::Json),
            ("tsconfig.json", FileType::Json),
            ("a.yaml", FileType::Yaml),
            ("x.yml", FileType::Yaml),
            ("pom.xml", FileType::Maven),
            ("m/ivy.xml", FileType::Ivy),
            ("settings.gradle", FileType::Gradle),
            ("gradle/deps.gradle", FileType::Gradle),
            ("package.json", FileType::Npm),
        ];
        for (p, t) in cases {
            assert_eq!(FileType::of(p), Some(t), "{p}");
        }
        for p in [
            ".nephila.toml",
            "a.toml",
            "a.py",
            "package-lock.json",
            "noext",
        ] {
            assert_eq!(FileType::of(p), None, "{p}");
        }
        for t in FileType::ALL {
            assert_eq!(FileType::parse(t.name()), Some(t));
        }
        assert_eq!(
            FileType::ALL.map(|t| t.name()).join(", "),
            TYPE_LIST.to_string()
        );
    }

    #[test]
    fn req037_motifs_reinclusion_et_ordre() {
        let a = m(&["docs/", "!docs/garde.md"], &[]);
        assert!(a.excludes("docs/x.md"));
        assert!(!a.excludes("docs/garde.md"));
        let b = m(&["!docs/garde.md", "docs/"], &[]);
        assert!(b.excludes("docs/garde.md"));
    }

    #[test]
    fn req037_motifs_ancrage() {
        let a = m(&["*.min.js", "/src/gen/"], &[]);
        assert!(a.excludes("web/dist/app.min.js"));
        assert!(a.excludes("src/gen/Gen.java"));
        assert!(!a.excludes("tools/src/gen/Keep.java"));
        assert!(!a.excludes("web/src/app.js"));
    }

    #[test]
    fn req037_types_et_sensibilite() {
        let a = m(&[], &[FileType::Json]);
        assert!(a.excludes("a.json"));
        assert!(!a.excludes("package.json"));
        assert!(!m(&["A.md"], &[]).excludes("a.md"));
        assert!(!Matcher::none().excludes("a.json"));
    }

    #[test]
    fn req037_motif_invalide() {
        let e = Filters {
            patterns: vec!["a[b".into()],
            types: BTreeSet::new(),
        }
        .compile(Path::new("/r"))
        .unwrap_err();
        assert!(e.to_string().starts_with("motif invalide `a[b` ("), "{e}");
        assert!(!e.to_string().contains('\n'));
    }
}
