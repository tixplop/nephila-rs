//! Scripts Gradle : chemin de projet des `build.gradle(.kts)` (REQ-022). Le contenu des scripts n'est pas lu ici.

use std::path::Path;

use super::{FileStructure, ParseCtx};
use crate::llm::extract::Declared;
use crate::model::NodeKind;

/// Dossier (séparateur `/`, `""` = racine du dépôt) du fichier `path`.
fn dir_of(path: &Path) -> String {
    let parts: Vec<String> = path
        .parent()
        .map(|p| {
            p.components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    parts.join("/")
}

/// Extrait un script `build.gradle(.kts)` ou `settings.gradle(.kts)`.
pub fn extract(path: &Path, ctx: &ParseCtx) -> FileStructure {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.starts_with("settings.gradle") {
        return FileStructure::default();
    }
    let dir = dir_of(path);
    // Plus proche ancêtre (dossier du script compris) contenant un settings, sinon la racine du dépôt.
    let mut base = dir.as_str();
    let base = loop {
        if ctx.gradle_settings.contains_key(base) || base.is_empty() {
            break base;
        }
        base = base.rsplit_once('/').map_or("", |(d, _)| d);
    };
    let rel = dir
        .strip_prefix(base)
        .map_or("", |r| r.trim_start_matches('/'));
    let segments: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let (qualified, short) = if segments.is_empty() {
        let own = ctx.gradle_settings.get(base).and_then(|n| n.clone());
        let folder = if base.is_empty() {
            ctx.repo_root
                .as_deref()
                .and_then(|r| r.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        } else {
            base.rsplit('/').next().map(str::to_string)
        };
        match own.filter(|n| !n.is_empty()).or(folder) {
            Some(s) => (":".to_string(), s),
            None => return FileStructure::default(),
        }
    } else {
        let last = segments[segments.len() - 1].to_string();
        (format!(":{}", segments.join(":")), last)
    };
    FileStructure {
        declares: vec![Declared {
            kind: NodeKind::Module,
            qualified,
            subkind: None,
            summary: None,
            short: Some(short),
        }],
        ..FileStructure::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn ctx(settings: &[(&str, Option<&str>)]) -> ParseCtx {
        ParseCtx {
            repo_root: Some(PathBuf::from("/work/monrepo")),
            gradle_settings: settings
                .iter()
                .map(|(d, n)| (d.to_string(), n.map(str::to_string)))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn decl(path: &str, c: &ParseCtx) -> (String, Option<String>) {
        let f = super::super::extract(Path::new(path), "", c).unwrap();
        assert_eq!(f.declares.len(), 1, "{path}");
        assert!(f.references.is_empty());
        (f.declares[0].qualified.clone(), f.declares[0].short.clone())
    }

    #[test]
    fn sous_projet_sans_settings() {
        let (q, s) = decl("services/api/build.gradle", &ctx(&[]));
        assert_eq!((q.as_str(), s.as_deref()), (":services:api", Some("api")));
        let (q, _) = decl("a/build.gradle.kts", &ctx(&[("", None)]));
        assert_eq!(q, ":a");
    }

    #[test]
    fn settings_imbrique() {
        let c = ctx(&[("", None), ("tools", None)]);
        let (q, s) = decl("tools/x/build.gradle", &c);
        assert_eq!((q.as_str(), s.as_deref()), (":x", Some("x")));
        let (q, s) = decl("tools/build.gradle", &c);
        assert_eq!((q.as_str(), s.as_deref()), (":", Some("tools")));
    }

    #[test]
    fn racine_nom_du_contexte_puis_nom_du_dossier() {
        let (q, s) = decl("build.gradle", &ctx(&[("", Some("mon-projet"))]));
        assert_eq!((q.as_str(), s.as_deref()), (":", Some("mon-projet")));
        let (q, s) = decl("build.gradle.kts", &ctx(&[("", None)]));
        assert_eq!((q.as_str(), s.as_deref()), (":", Some("monrepo")));
        let (_, s) = decl("build.gradle", &ctx(&[]));
        assert_eq!(s.as_deref(), Some("monrepo"));
    }

    #[test]
    fn settings_ne_declare_rien() {
        let c = ctx(&[("", Some("x"))]);
        for p in [
            "settings.gradle",
            "settings.gradle.kts",
            "a/settings.gradle",
        ] {
            let f = super::super::extract(Path::new(p), "rootProject.name = 'x'", &c).unwrap();
            assert_eq!(f, FileStructure::default(), "{p}");
        }
    }

    #[test]
    fn autre_gradle_reste_groovy() {
        let c = ctx(&[]);
        let f = super::super::extract(Path::new("gradle/deps.gradle"), "class A {}\n", &c).unwrap();
        assert!(f.declares.iter().any(|d| d.kind != NodeKind::Module));
    }
}
