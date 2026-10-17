//! Fichiers JSON : clés de configuration et candidats de nom JVM (REQ-032, DEC-038).

use std::path::Path;

use serde_json::Value;

use super::data::{self, Format};
use super::keys::KeySet;
use super::FileStructure;

/// Chaînes de `v` à toute profondeur (tableaux et objets compris), dans l'ordre des tableaux et
/// des membres (triés par `serde_json`).
fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

fn walk(keys: &mut KeySet, prefix: &str, v: &Value) {
    let Value::Object(members) = v else {
        return;
    };
    for (name, value) in members {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            Value::Object(o) if !o.is_empty() => walk(keys, &path, value),
            _ => {
                keys.add_key(&path);
                let mut s = Vec::new();
                strings(value, &mut s);
                for x in s {
                    keys.add_key_value(&path, x);
                }
            }
        }
    }
}

/// Extrait le document, les clés et les mentions d'un fichier JSON.
pub fn extract(path: &Path, text: &str) -> FileStructure {
    let mut keys = KeySet::new();
    let mut problem = None;
    if !text.trim().is_empty() {
        match serde_json::from_str::<Value>(text) {
            Ok(root @ Value::Object(_)) => walk(&mut keys, "", &root),
            Ok(root) => {
                let mut s = Vec::new();
                strings(&root, &mut s);
                for x in s {
                    keys.add_doc_value(x);
                }
            }
            Err(e) => problem = Some(e.to_string()),
        }
    }
    let mut st = keys.finish(path, Format::Json);
    if let Some(p) = problem {
        st.warnings.push(data::warning(path, Format::Json, &p));
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &str, text: &str) -> FileStructure {
        extract(Path::new(p), text)
    }

    fn keys(f: &FileStructure) -> Vec<String> {
        f.declares
            .iter()
            .filter(|d| d.id().starts_with("key:"))
            .map(|d| d.short.clone().unwrap())
            .collect()
    }

    fn refs(f: &FileStructure) -> Vec<(String, String)> {
        f.references
            .iter()
            .map(|r| (r.source.clone(), r.target.clone()))
            .collect()
    }

    const APP: &str = r#"{"server": {"port": 8080, "handler": "com.acme.core.OrderService"}, "plugins": ["com.acme.core.OrderRepository", {"impl": "com.acme.Absent"}], "empty": {}, "nothing": null}"#;

    /// AC-032-1
    #[test]
    fn req032_ac1_cles_et_mentions() {
        let f = run("config/app.json", APP);
        assert_eq!(f.declares[0].id(), "document:config/app.json");
        assert_eq!(
            keys(&f),
            [
                "empty",
                "nothing",
                "plugins",
                "server.handler",
                "server.port"
            ]
        );
        let r = refs(&f);
        let k = "key:config/app.json#";
        assert!(r.contains(&(
            format!("{k}server.handler"),
            "com.acme.core.OrderService".into()
        )));
        assert!(r.contains(&(
            format!("{k}plugins"),
            "com.acme.core.OrderRepository".into()
        )));
        assert!(r.contains(&(format!("{k}plugins"), "com.acme.Absent".into())));
        assert!(f.warnings.is_empty());
    }

    /// AC-032-2
    #[test]
    fn req032_ac2_plafond_de_cles() {
        let body: Vec<String> = (0..1_500)
            .rev()
            .map(|i| format!("\"k{i:04}\": 1"))
            .collect();
        let f = run("big.json", &format!("{{{}}}", body.join(",")));
        let k = keys(&f);
        assert_eq!(k.len(), 1_000);
        assert_eq!(k[0], "k0000");
        assert_eq!(k[999], "k0999");
        assert_eq!(
            f.warnings,
            ["big.json : 500 clés non retenues (plafond de 1 000)"]
        );
    }

    /// AC-032-3
    #[test]
    fn req032_ac3_json_mal_forme_document_seul() {
        for t in [
            "{\"a\": 1,",
            "{\"a\": 1 // commentaire",
            "{\"a\": 1,}",
            "{\"a\":",
        ] {
            let f = run("a.json", t);
            assert_eq!(f.declares.len(), 1, "{t}");
            assert!(f.references.is_empty());
            assert_eq!(f.warnings.len(), 1, "{t}");
            assert!(f.warnings[0].starts_with("a.json : JSON mal formé"), "{t}");
        }
        assert_eq!(keys(&run("a.json", "{\"a\": 1}")), ["a"]);
    }

    /// AC-032-4
    #[test]
    fn req032_ac4_racine_non_objet() {
        let f = run("a.json", r#"["com.acme.core.OrderService"]"#);
        assert!(keys(&f).is_empty());
        assert_eq!(
            refs(&f),
            [(
                "document:a.json".into(),
                "com.acme.core.OrderService".into()
            )]
        );
        let f = run("b.json", r#""com.acme.B""#);
        assert_eq!(refs(&f), [("document:b.json".into(), "com.acme.B".into())]);
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn fichier_vide_document_seul_sans_avertissement() {
        for t in ["", "  \n"] {
            let f = run("e.json", t);
            assert_eq!(f.declares.len(), 1);
            assert!(f.warnings.is_empty() && f.references.is_empty());
        }
    }

    #[test]
    fn nom_de_membre_avec_point_garde_tel_quel() {
        let f = run("a.json", r#"{"a.b": {"c": 1}, "x": {"y": {"z": "p.Q"}}}"#);
        assert_eq!(keys(&f), ["a.b.c", "x.y.z"]);
    }

    /// AC-029-3 (JSON : plafond de candidats)
    #[test]
    fn plafond_de_candidats() {
        let vals: Vec<String> = (0..1_500).map(|i| format!("\"p.C{i}\"")).collect();
        let f = run("a.json", &format!("{{\"k\": [{}]}}", vals.join(",")));
        assert_eq!(f.references.len(), 1_000);
        assert!(f.warnings.iter().any(|w| w.contains("500")));
    }

    /// AC-029-5 (partie `t.json`)
    #[test]
    fn ac029_5_t_json() {
        let f = run("t.json", "{\"a\":");
        assert_eq!(f.declares.len(), 1);
        assert_eq!(f.warnings.len(), 1);
        assert!(f.warnings[0].contains("t.json"));
    }
}
