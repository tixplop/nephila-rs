//! `package.json` (DEC-040) : module npm et dépendances, lus par `serde_json`.

use std::path::Path;

use serde_json::Value;

use super::data::doc_path;
use super::{Declared, FileStructure, ParseCtx, Reference};
use crate::model::{EdgeKind, NodeKind};

/// Sections dont chaque clé est une dépendance (la version est ignorée).
const SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

fn warning(path: &Path, problem: &str) -> String {
    format!(
        "{} : package.json mal formé, ignoré : {problem}",
        doc_path(path)
    )
}

/// Nom du dossier du fichier, ou de la racine du dépôt pour un `package.json` racine.
fn folder_name(path: &Path, ctx: &ParseCtx) -> Option<String> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = match parent {
        Some(p) => p.file_name(),
        None => ctx.repo_root.as_deref().and_then(|r| r.file_name()),
    };
    name.map(|n| n.to_string_lossy().into_owned())
}

/// Extrait le module et ses dépendances d'un `package.json`.
pub fn extract(path: &Path, text: &str, ctx: &ParseCtx) -> FileStructure {
    let mut out = FileStructure::default();
    let root = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(o)) => o,
        Ok(_) => {
            out.warnings
                .push(warning(path, "la racine n'est pas un objet"));
            return out;
        }
        Err(e) => {
            out.warnings.push(warning(path, &e.to_string()));
            return out;
        }
    };
    let qualified = root
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| folder_name(path, ctx))
        .unwrap_or_else(|| "package".to_string());
    let short = qualified
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&qualified)
        .to_string();
    let source = format!("module:{qualified}");
    out.declares.push(Declared {
        kind: NodeKind::Module,
        qualified,
        subkind: None,
        summary: None,
        short: Some(short),
    });
    for section in SECTIONS {
        let Some(Value::Object(deps)) = root.get(section) else {
            continue;
        };
        for name in deps.keys().filter(|k| !k.is_empty()) {
            let r = Reference {
                source: source.clone(),
                kind: EdgeKind::DependsOn,
                target: name.clone(),
            };
            if !out.references.contains(&r) {
                out.references.push(r);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn run(path: &str, text: &str) -> FileStructure {
        let ctx = ParseCtx {
            repo_root: Some(PathBuf::from("/work/monrepo")),
            ..Default::default()
        };
        extract(Path::new(path), text, &ctx)
    }

    fn targets(f: &FileStructure) -> Vec<&str> {
        f.references.iter().map(|r| r.target.as_str()).collect()
    }

    #[test]
    fn req035_ac1_module_et_dependances() {
        let f = run(
            "web/package.json",
            r#"{"name": "@acme/web", "dependencies": {"react": "^18.3.0", "@acme/core": "workspace:*"}, "devDependencies": {"typescript": "5.6.0"}}"#,
        );
        assert_eq!(f.declares.len(), 1);
        assert_eq!(f.declares[0].id(), "module:@acme/web");
        assert_eq!(f.declares[0].short.as_deref(), Some("web"));
        let mut t = targets(&f);
        t.sort();
        assert_eq!(t, ["@acme/core", "react", "typescript"]);
        assert_eq!(f.references.len(), 3);
        assert!(f
            .references
            .iter()
            .all(|r| r.source == "module:@acme/web" && r.kind == EdgeKind::DependsOn));
        assert!(f.warnings.is_empty());
        let g = run("core/package.json", r#"{"name": "@acme/core"}"#);
        assert_eq!(g.declares[0].short.as_deref(), Some("core"));
        assert!(g.references.is_empty());
    }

    #[test]
    fn req035_ac2_quatre_sections_dedoublonnees_et_autres_ignorees() {
        let f = run(
            "package.json",
            r#"{"name":"app","dependencies":{"a":"1"},"devDependencies":{"a":"2","b":"1"},
            "peerDependencies":{"c":"*"},"optionalDependencies":{"d":"1"},
            "scripts":{"s":"x"},"workspaces":["w"],"engines":{"node":">=20"}}"#,
        );
        let mut t = targets(&f);
        t.sort();
        assert_eq!(t, ["a", "b", "c", "d"]);
    }

    #[test]
    fn req035_ac2_name_absent_prend_le_dossier() {
        let f = run("package.json", r#"{"private": true}"#);
        assert_eq!(f.declares[0].id(), "module:monrepo");
        let f = run("a/b/package.json", r#"{"name": ""}"#);
        assert_eq!(f.declares[0].id(), "module:b");
        let f = run("a/package.json", r#"{"name": 3}"#);
        assert_eq!(f.declares[0].id(), "module:a");
        let ctx = ParseCtx::default();
        let f = extract(Path::new("package.json"), "{}", &ctx);
        assert_eq!(f.declares[0].id(), "module:package");
    }

    #[test]
    fn req035_ac3_json_tronque_ou_racine_non_objet() {
        let f = run("web/package.json", r#"{"name": "x","#);
        assert!(f.declares.is_empty() && f.references.is_empty());
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("web/package.json"));
        let f = run("package.json", "[1, 2]");
        assert!(f.declares.is_empty());
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("package.json"));
    }
}
