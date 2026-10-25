//! Extraction d'un `ivy.xml` par lecture XML en flux (DEC-018, DEC-020).

use std::path::Path;

use super::maven::{read_events, warning, Ev};
use super::FileStructure;
use crate::model::{EdgeKind, NodeKind};
use crate::parse::{Declared, OrderedSet, Reference};

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// Extrait le module (`<info>`) et ses dépendances.
pub fn extract(path: &Path, text: &str) -> FileStructure {
    let mut out = FileStructure::default();
    let mut module: Option<String> = None;
    let mut org: Option<String> = None;
    let mut deps: Vec<(Option<String>, String)> = Vec::new();
    let problem = read_events(text, |ev| {
        let Ev::Open(p, a) = ev else { return };
        if p.len() == 2 && p[0] == "ivy-module" && p[1] == "info" {
            org = attr(&a, "organisation").map(str::to_string);
            module = attr(&a, "module").map(str::to_string);
        } else if p.len() == 3
            && p[0] == "ivy-module"
            && p[1] == "dependencies"
            && p[2] == "dependency"
        {
            if let Some(name) = attr(&a, "name") {
                deps.push((attr(&a, "org").map(str::to_string), name.to_string()));
            }
        }
    });
    if let Some(problem) = problem {
        out.warnings.push(warning(path, &problem));
    }
    let Some(module) = module else { return out };
    let qualified = match &org {
        Some(o) => format!("{o}:{module}"),
        None => module.clone(),
    };
    let source = format!("module:{qualified}");
    out.declares.push(Declared {
        kind: NodeKind::Module,
        qualified,
        subkind: None,
        summary: None,
        short: Some(module),
    });
    let mut refs = OrderedSet::new();
    for (o, name) in deps {
        let target = match o.as_deref().or(org.as_deref()) {
            Some(o) => format!("{o}:{name}"),
            None => name,
        };
        let r = Reference {
            source: source.clone(),
            kind: EdgeKind::DependsOn,
            target,
        };
        refs.insert(r);
    }
    out.references = refs.into_vec();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> FileStructure {
        extract(Path::new("ivy.xml"), text)
    }

    #[test]
    fn req022_ac6_module_et_dependance() {
        let f = run(
            r#"<ivy-module version="2.0"><info organisation="com.acme" module="legacy"/>
            <dependencies><dependency org="commons-lang" name="commons-lang" rev="2.6"/></dependencies></ivy-module>"#,
        );
        assert_eq!(f.declares[0].id(), "module:com.acme:legacy");
        assert_eq!(f.declares[0].short.as_deref(), Some("legacy"));
        assert_eq!(f.references[0].source, "module:com.acme:legacy");
        assert_eq!(f.references[0].kind, EdgeKind::DependsOn);
        assert_eq!(f.references[0].target, "commons-lang:commons-lang");
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn org_absent_prend_celle_du_module() {
        let f = run(r#"<ivy-module><info organisation="com.acme" module="m"/>
            <dependencies><dependency name="core"/></dependencies></ivy-module>"#);
        assert_eq!(f.references[0].target, "com.acme:core");
    }

    #[test]
    fn xml_tronque_garde_le_debut_et_avertit() {
        let f = run(
            r#"<ivy-module><info organisation="o" module="m"/><dependencies><dependency org="a" name="b"/>"#,
        );
        assert_eq!(f.declares[0].id(), "module:o:m");
        assert_eq!(f.references.len(), 1);
        assert_eq!(f.warnings.len(), 1);
    }
}
