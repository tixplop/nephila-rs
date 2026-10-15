//! Page HTML autonome d'exploration du graphe (REQ-025, DEC-028).

use std::collections::BTreeSet;

use serde_json::{json, Value};

const TEMPLATE: &str = include_str!("view/index.html");
const STYLE: &str = include_str!("view/style.css");
const MODEL_JS: &str = include_str!("view/model.js");
const APP_JS: &str = include_str!("view/app.js");

/// Version du format de données embarqué.
pub const DATA_VERSION: u32 = 1;

/// Données compactes (version 1) à partir de l'export JSON `graph`.
pub fn compact(name: &str, graph: &Value) -> Value {
    let empty = Vec::new();
    let nodes_in = graph["nodes"].as_array().unwrap_or(&empty);
    let edges_in = graph["edges"].as_array().unwrap_or(&empty);
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();

    let mut nodes: Vec<&Value> = nodes_in.iter().collect();
    nodes.sort_by(|a, b| s(&a["id"]).cmp(&s(&b["id"])));
    let mut edges: Vec<(String, String, String)> = edges_in
        .iter()
        .map(|e| (s(&e["source"]), s(&e["target"]), s(&e["kind"])))
        .collect();
    edges.sort();

    let node_kinds: Vec<String> = nodes
        .iter()
        .map(|n| s(&n["kind"]))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let edge_kinds: Vec<String> = edges
        .iter()
        .map(|e| e.2.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let index: std::collections::HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n["id"].as_str().map(|id| (id, i)))
        .collect();
    let kind_idx = |k: &str, all: &[String]| all.iter().position(|x| x == k).unwrap_or(0);

    let nodes_out: Vec<Value> = nodes
        .iter()
        .map(|n| {
            json!([
                n["id"],
                kind_idx(n["kind"].as_str().unwrap_or_default(), &node_kinds),
                n["short"],
                n["qualified"],
                n["file"],
                n["subkind"],
                n["summary"],
            ])
        })
        .collect();
    let edges_out: Vec<Value> = edges
        .iter()
        .filter_map(|(src, dst, kind)| {
            let (a, b) = (index.get(src.as_str())?, index.get(dst.as_str())?);
            Some(json!([a, b, kind_idx(kind, &edge_kinds)]))
        })
        .collect();
    json!({
        "version": DATA_VERSION,
        "name": name,
        "nodeKinds": node_kinds,
        "edgeKinds": edge_kinds,
        "nodes": nodes_out,
        "edges": edges_out,
    })
}

/// Sérialise puis échappe `<`, `>`, `&`, U+2028 et U+2029 en `\uXXXX`.
pub fn escape_json(v: &Value) -> String {
    let text = v.to_string();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

/// Page complète : gabarit, style, données, puis scripts (assemblage en une passe,
/// le contenu inséré n'est jamais réinterprété comme un marqueur).
pub fn render(name: &str, graph: &Value) -> String {
    let data = escape_json(&compact(name, graph));
    let title = name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let parts: [(&str, &str); 5] = [
        ("{{name}}", &title),
        ("{{style}}", STYLE),
        ("{{data}}", &data),
        ("{{model}}", MODEL_JS),
        ("{{app}}", APP_JS),
    ];
    let mut out = String::with_capacity(TEMPLATE.len() + data.len() + 4096);
    let mut rest = TEMPLATE;
    while !rest.is_empty() {
        let next = parts
            .iter()
            .filter_map(|(m, v)| rest.find(m).map(|p| (p, *m, *v)))
            .min_by_key(|(p, _, _)| *p);
        match next {
            Some((p, m, v)) => {
                out.push_str(&rest[..p]);
                out.push_str(v);
                rest = &rest[p + m.len()..];
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Value {
        json!({"nodes": [
            {"id": "type:b", "kind": "type", "short": "b", "qualified": "b", "file": null, "subkind": "class", "summary": "x </script> & \u{2028}"},
            {"id": "method:a", "kind": "method", "short": "a", "qualified": "a", "file": "A.java", "subkind": null, "summary": null}],
          "edges": [
            {"source": "type:b", "target": "method:a", "kind": "contains"},
            {"source": "method:a", "target": "type:b", "kind": "calls"}],
          "unresolved": [{"source": "x", "kind": "calls", "target": "y"}]})
    }

    #[test]
    fn req025_ac2_compact_is_sorted_and_indexed() {
        let c = compact("p", &graph());
        assert_eq!(c["version"], 1);
        assert_eq!(c["nodeKinds"], json!(["method", "type"]));
        assert_eq!(c["edgeKinds"], json!(["calls", "contains"]));
        assert_eq!(c["nodes"][0][0], "method:a");
        assert_eq!(c["nodes"][1][1], 1);
        assert_eq!(c["edges"], json!([[0, 1, 0], [1, 0, 1]]));
        assert!(c.get("unresolved").is_none());
    }

    #[test]
    fn req025_ac3_escape_has_no_raw_special_chars() {
        let s = escape_json(&compact("p", &graph()));
        for bad in ['<', '>', '&', '\u{2028}', '\u{2029}'] {
            assert!(!s.contains(bad), "{bad:?} brut");
        }
        let back: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["nodes"][1][6], "x </script> & \u{2028}");
    }

    #[test]
    fn req025_ac1_render_is_a_standalone_page() {
        let h = render("p", &graph());
        assert!(h.starts_with("<!DOCTYPE html>"));
        assert!(h.contains("<style>") && h.contains("id=\"rkg-data\""));
        assert!(!h.contains("{{"));
    }
}
