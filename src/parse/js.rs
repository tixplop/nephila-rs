//! Extracteur JavaScript et TypeScript (REQ-034, DEC-039, DEC-040) : amorce, le nœud package du
//! module ES seulement.

use std::path::Path;

use crate::error::Result;
use crate::model::NodeKind;

use super::{Declared, FileStructure, Lang};

/// Nom du module ES d'un fichier : (qualifié, court). Le qualifié est le chemin relatif sans
/// extension (`.d.ts`, `.d.mts`, `.d.cts` retirés en entier, sinon la dernière extension) ; le
/// court est le nom du fichier sans extension.
pub fn module_name(path: &Path) -> (String, String) {
    let rel = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let (dir, file) = match rel.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, rel.as_str()),
    };
    let stem = [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .find_map(|s| file.strip_suffix(s))
        .or_else(|| file.rsplit_once('.').map(|(s, _)| s))
        .filter(|s| !s.is_empty())
        .unwrap_or(file);
    let qualified = match dir {
        Some(d) => format!("{d}/{stem}"),
        None => stem.to_string(),
    };
    (qualified, stem.to_string())
}

/// Extrait la structure d'un fichier JavaScript ou TypeScript.
pub fn extract(_lang: Lang, path: &Path, _text: &str) -> Result<FileStructure> {
    let (module, short) = module_name(path);
    Ok(FileStructure {
        context: Some(format!("package {module}")),
        declares: vec![Declared {
            kind: NodeKind::Package,
            qualified: module,
            subkind: None,
            summary: None,
            short: Some(short),
        }],
        references: Vec::new(),
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str) -> (String, String) {
        module_name(Path::new(p))
    }

    #[test]
    fn module_name_cas_usuels() {
        assert_eq!(m("src/a.ts"), ("src/a".into(), "a".into()));
        assert_eq!(m("src/a.d.ts"), ("src/a".into(), "a".into()));
        assert_eq!(m("src/a.d.mts"), ("src/a".into(), "a".into()));
        assert_eq!(m("src/a.d.cts"), ("src/a".into(), "a".into()));
        assert_eq!(
            m("web/src/order.service.ts"),
            ("web/src/order.service".into(), "order.service".into())
        );
        assert_eq!(m("main.ts"), ("main".into(), "main".into()));
        assert_eq!(m("src/b.jsx"), ("src/b".into(), "b".into()));
    }

    #[test]
    fn extract_rend_le_noeud_package() {
        let s = extract(Lang::TypeScript, Path::new("src/a.d.ts"), "").unwrap();
        assert_eq!(s.declares.len(), 1);
        assert_eq!(s.declares[0].id(), "package:src/a");
        assert_eq!(s.declares[0].short.as_deref(), Some("a"));
        assert_eq!(s.context.as_deref(), Some("package src/a"));
        assert!(s.references.is_empty());
    }
}
