//! Commande `export` (REQ-018) : le graphe entier en JSON, lecture seule, sans LLM.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::ExportFormat;
use crate::error::{Error, Result};
use crate::resolve;
use crate::store::Store;

/// Version du format d'export.
pub const EXPORT_VERSION: u32 = 1;

pub fn build_json(store: &Store) -> Result<Value> {
    let nodes: Vec<Value> = store
        .nodes()?
        .into_iter()
        .map(|n| {
            json!({
                "id": n.id,
                "kind": n.kind.as_str(),
                "qualified": n.qualified,
                "short": n.short,
                "file": n.file,
                "subkind": n.subkind.map(|s| s.as_str()),
                "summary": n.summary,
            })
        })
        .collect();
    let edges: Vec<Value> = store
        .edges()?
        .into_iter()
        .map(|e| json!({"source": e.source, "target": e.target, "kind": e.kind.as_str()}))
        .collect();
    let unresolved: Vec<Value> = resolve::unresolved(store)?
        .into_iter()
        .map(|r| json!({"source": r.source, "kind": r.kind.as_str(), "target": r.target_name}))
        .collect();
    Ok(json!({"version": EXPORT_VERSION, "nodes": nodes, "edges": edges, "unresolved": unresolved}))
}

/// Nom de base des fichiers d'export : dernier composant du chemin canonique du dépôt,
/// en minuscules ASCII, suites hors `[a-z0-9]` réduites à un `-` (AC-018-7).
pub fn slug(repo: &Path) -> String {
    let canon = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let name = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "graphe".to_string()
    } else {
        out.to_string()
    }
}

/// Destination d'un export.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Stdout,
    File(PathBuf),
}

/// `None` : `<slug>.<ext>` dans le dossier courant ; `-` : stdout ; sinon le chemin donné.
pub fn output_path(repo: &Path, output: Option<&Path>, ext: &str) -> Target {
    match output {
        None => Target::File(PathBuf::from(format!("{}.{ext}", slug(repo)))),
        Some(p) if p == Path::new("-") => Target::Stdout,
        Some(p) => Target::File(p.to_path_buf()),
    }
}

pub fn run(repo: &Path, output: Option<&Path>, format: ExportFormat) -> Result<()> {
    let store = Store::open_readonly(repo)?;
    let graph = build_json(&store)?;
    let (text, ext) = match format {
        ExportFormat::Json => (graph.to_string(), "json"),
        ExportFormat::Html => (crate::view::render(&slug(repo), &graph), "html"),
    };
    match output_path(repo, output, ext) {
        Target::Stdout => println!("{text}"),
        Target::File(path) => {
            std::fs::write(&path, format!("{text}\n")).map_err(|e| {
                Error::Message(format!("écriture de {} impossible : {e}", path.display()))
            })?;
            println!("{}", path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req018_ac7_slug_rules() {
        assert_eq!(slug(Path::new("/x/Mon Dépôt_2")), "mon-d-p-t-2");
        assert_eq!(slug(Path::new("/x/--x--")), "x");
        assert_eq!(slug(Path::new("/")), "graphe");
        assert_eq!(slug(Path::new("/x/ééé")), "graphe");
    }

    #[test]
    fn req018_ac7_output_path_choices() {
        let repo = Path::new("/x/Proj");
        assert_eq!(
            output_path(repo, None, "json"),
            Target::File("proj.json".into())
        );
        assert_eq!(
            output_path(repo, Some(Path::new("-")), "json"),
            Target::Stdout
        );
        assert_eq!(
            output_path(repo, Some(Path::new("a/b.json")), "json"),
            Target::File("a/b.json".into())
        );
    }
}
