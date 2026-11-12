//! Scénario Java de l'enrichissement des seuls nœuds manquants (REQ-291) : `p/A.java` (classe `A`,
//! méthodes `m1` à `m5` sans doc, deux morceaux) et `p/B.java`, faux serveur `openai` qui rend un
//! résumé `R<k>` (`k` = rang de la requête) pour chaque numéro de nœud possible, et montée de
//! version simulée dans la base.

use std::fs;
use std::path::Path;

pub const A: &str = "p/A.java";
pub const B: &str = "p/B.java";
/// Plus grand nombre de nœuds d'une requête : les réponses numérotent de 1 à `MAX_LIST`.
const MAX_LIST: usize = 8;

/// Nombre de requêtes que le faux serveur sait servir.
pub const REPLIES: usize = 8;

/// Ligne de méthode de longueur fixe, pour que chaque méthode pèse autant.
fn method(i: usize) -> String {
    format!("    void m{i}() {{ int x{i} = {i}; /* ---------------- */ }}\n")
}

pub fn write_repo(repo: &Path, max_chunk_chars: usize) {
    fs::create_dir_all(repo.join("p")).unwrap();
    let mut a = String::from("package p;\n\npublic class A {\n");
    for i in 1..=5 {
        a.push_str(&method(i));
    }
    a.push_str("}\n");
    fs::write(repo.join(A), a).unwrap();
    fs::write(repo.join(B), "package p;\n\npublic class B {\n}\n").unwrap();
    fs::write(
        repo.join(".nephila.toml"),
        format!("max_chunk_chars = {max_chunk_chars}\n"),
    )
    .unwrap();
}

/// Réponse `openai` de rang `k` : un résumé `R<k>` pour chaque numéro de 1 à `MAX_LIST`.
pub fn reply(k: usize, answered: usize) -> (u16, String) {
    let items: Vec<serde_json::Value> = (1..=answered.min(MAX_LIST))
        .map(|n| serde_json::json!({"n": n, "summary": format!("R{k}")}))
        .collect();
    let text = serde_json::json!({"summaries": items, "concepts": []}).to_string();
    let body = serde_json::json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]});
    (200, body.to_string())
}

/// Montée de version simulée : dans l'entrée de `p/A.java`, les identifiants des résumés de `m4`
/// et `m5` prennent une forme qu'aucun fichier ne déclare, et `nodes_hash` devient l'empreinte
/// blake3 de la liste triée des identifiants ainsi réécrits (ce qu'écrit la migration 16 -> 17).
pub fn simulate_upgrade(repo: &Path) {
    let conn = rusqlite::Connection::open(repo.join(".nephila/graph.db")).unwrap();
    let (rowid, payload): (i64, String) = conn
        .query_row(
            "SELECT rowid, payload FROM enrichments WHERE path = ?1",
            [A],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let mut ids = Vec::new();
    for pair in v["summaries"].as_array_mut().unwrap() {
        let id = pair[0].as_str().unwrap().to_string();
        let id = if id.ends_with(".m4") || id.ends_with(".m5") {
            format!("{id}(int)")
        } else {
            id
        };
        pair[0] = serde_json::Value::String(id.clone());
        ids.push(id);
    }
    ids.sort();
    let hash = blake3::hash(ids.join("\n").as_bytes()).to_hex().to_string();
    conn.execute(
        "UPDATE enrichments SET payload = ?2, nodes_hash = ?3 WHERE rowid = ?1",
        rusqlite::params![rowid, v.to_string(), hash],
    )
    .unwrap();
}
