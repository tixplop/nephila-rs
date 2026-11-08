//! Schémas JSON des réponses du LLM, envoyés selon `structured_output` (REQ-202, REQ-204, DEC-163).

use serde_json::{json, Value};

/// Réponse attendue d'un appel : choisit le schéma et son nom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reply {
    File,
    Lot,
    Ask,
    Subsystem,
}

impl Reply {
    /// Nom du schéma dans le corps OpenAI.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Reply::File => "nephila_fichier",
            Reply::Lot => "nephila_lot",
            Reply::Ask => "nephila_ask",
            Reply::Subsystem => "nephila_sous_systeme",
        }
    }

    pub(crate) fn schema(self) -> Value {
        match self {
            Reply::File => file(),
            Reply::Lot => lot(),
            Reply::Ask => ask(),
            Reply::Subsystem => subsystem(),
        }
    }
}

fn summaries() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {"n": {"type": "integer"}, "summary": {"type": "string"}},
            "required": ["n", "summary"],
            "additionalProperties": false
        }
    })
}

fn concepts() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "summary": {"type": ["string", "null"]},
                "sections": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["name", "summary", "sections"],
            "additionalProperties": false
        }
    })
}

/// Schéma de la réponse pour un fichier seul.
pub(crate) fn file() -> Value {
    json!({
        "type": "object",
        "properties": {"summaries": summaries(), "concepts": concepts()},
        "required": ["summaries", "concepts"],
        "additionalProperties": false
    })
}

/// Schéma de la réponse pour un lot de fichiers.
pub(crate) fn lot() -> Value {
    json!({
        "type": "object",
        "properties": {
            "files": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "f": {"type": "integer"},
                        "summaries": summaries(),
                        "concepts": concepts()
                    },
                    "required": ["f", "summaries", "concepts"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["files"],
        "additionalProperties": false
    })
}

/// Schéma de la réponse de `query ask`.
pub(crate) fn ask() -> Value {
    json!({
        "type": "object",
        "properties": {
            "found": {"type": "boolean"},
            "answer": {"type": ["string", "null"]},
            "nodes": {"type": ["array", "null"], "items": {"type": "string"}}
        },
        "required": ["found", "answer", "nodes"],
        "additionalProperties": false
    })
}

/// Schéma de la réponse de la passe des sous-systèmes.
pub(crate) fn subsystem() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "description": {"type": "string"}
        },
        "required": ["name", "description"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &str) -> Value {
        json!({"type": t})
    }

    #[test]
    fn req202_ac3_schemas_egaux_aux_litteraux() {
        let sums = json!({"type": "array", "items": {"type": "object",
            "properties": {"n": s("integer"), "summary": s("string")},
            "required": ["n", "summary"], "additionalProperties": false}});
        let cons = json!({"type": "array", "items": {"type": "object",
            "properties": {"name": s("string"), "summary": {"type": ["string", "null"]},
                "sections": {"type": "array", "items": s("string")}},
            "required": ["name", "summary", "sections"], "additionalProperties": false}});
        assert_eq!(
            file(),
            json!({"type": "object",
                "properties": {"summaries": sums, "concepts": cons},
                "required": ["summaries", "concepts"], "additionalProperties": false})
        );
        assert_eq!(
            lot(),
            json!({"type": "object", "properties": {"files": {"type": "array",
                "items": {"type": "object",
                    "properties": {"f": s("integer"), "summaries": sums, "concepts": cons},
                    "required": ["f", "summaries", "concepts"],
                    "additionalProperties": false}}},
                "required": ["files"], "additionalProperties": false})
        );
        assert_eq!(
            ask(),
            json!({"type": "object", "properties": {
                "found": s("boolean"), "answer": {"type": ["string", "null"]},
                "nodes": {"type": ["array", "null"], "items": s("string")}},
                "required": ["found", "answer", "nodes"], "additionalProperties": false})
        );
    }

    fn walk(v: &Value, seen: &mut usize) {
        match v {
            Value::Object(m) => {
                if let Some(props) = m.get("properties") {
                    *seen += 1;
                    assert_eq!(m["additionalProperties"], json!(false), "{v}");
                    let mut keys: Vec<&String> = props.as_object().unwrap().keys().collect();
                    let mut req: Vec<&str> = m["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|k| k.as_str().unwrap())
                        .collect();
                    keys.sort();
                    req.sort();
                    assert_eq!(keys, req, "{v}");
                }
                for (k, c) in m {
                    assert!(k != "enum" && k != "version", "{k}");
                    walk(c, seen);
                }
            }
            Value::Array(a) => a.iter().for_each(|c| walk(c, seen)),
            _ => {}
        }
    }

    #[test]
    fn req202_ac3_objets_fermes_et_sans_version_ni_enum() {
        for (reply, objects) in [
            (Reply::File, 3),
            (Reply::Lot, 4),
            (Reply::Ask, 1),
            (Reply::Subsystem, 1),
        ] {
            let mut seen = 0;
            walk(&reply.schema(), &mut seen);
            assert_eq!(seen, objects, "{reply:?}");
        }
        assert_eq!(Reply::File.name(), "nephila_fichier");
        assert_eq!(Reply::Lot.name(), "nephila_lot");
        assert_eq!(Reply::Ask.name(), "nephila_ask");
        assert_eq!(Reply::Subsystem.name(), "nephila_sous_systeme");
    }
}
