//! Gros fichier Java de plusieurs morceaux, partagé par `tests/enrich_cache.rs` et
//! `tests/dry_run.rs` (REQ-294) : classe `p/Big.java`, méthodes séparées par une ligne vide.

use std::fs;
use std::path::Path;

pub const BIG: &str = "p/Big.java";
/// `max_chunk_chars` du dépôt.
pub const LIMIT: usize = 1500;
/// Méthode modifiée par les tests et rang du morceau qui la porte.
pub const TARGET: usize = 160;
pub const TARGET_CHUNK: usize = 3;
/// Rang d'une méthode du morceau 2.
pub const CHUNK_2_METHOD: usize = 100;
/// Nombre de méthodes de la classe.
pub const METHODS: usize = 240;

/// Une méthode et une ligne vide (les coupes sûres suivent une ligne vide) ; `{ }` devient `{;}`
/// sans changer la longueur ni le nœud.
pub fn method(i: usize) -> String {
    format!("    void m{i:03}() {{ }}\n\n")
}

/// Texte de la classe : `head` avant le `package`, `imports` après, `extra` : `(rang, ligne)`
/// insérée avant la méthode de ce rang.
pub fn text(head: &str, imports: &str, extra: Option<(usize, &str)>) -> String {
    let mut s = format!("{head}package p;\n\n{imports}public class Big {{\n");
    for i in 1..=METHODS {
        if let Some((at, line)) = extra {
            if at == i {
                s.push_str(line);
            }
        }
        s.push_str(&method(i));
    }
    s.push_str("}\n");
    s
}

pub fn write_repo(repo: &Path, content: &str) {
    fs::create_dir_all(repo.join("p")).unwrap();
    fs::write(repo.join(BIG), content).unwrap();
    fs::write(
        repo.join(".nephila.toml"),
        format!("max_chunk_chars = {LIMIT}\n"),
    )
    .unwrap();
}

/// Réponse `openai` de rang `k` : un résumé `R<k>` pour chacun des 120 premiers numéros.
pub fn reply(k: usize) -> (u16, String) {
    let items: Vec<serde_json::Value> = (1..=120)
        .map(|n| serde_json::json!({"n": n, "summary": format!("R{k}")}))
        .collect();
    let text = serde_json::json!({"summaries": items, "concepts": []}).to_string();
    let body = serde_json::json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]});
    (200, body.to_string())
}
