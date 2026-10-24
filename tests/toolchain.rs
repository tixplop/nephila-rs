//! AC-073-7 : aucune mise à jour atomique manuelle (`compare_exchange`, `.fetch_update(`) dans les
//! sources ; la réservation du budget passe par `try_update` (REQ-073, DEC-089).
//! REQ-076 : chaîne d'outils minimale déclarée (`rust-version = "1.99"`) et tests de performance
//! sans attente réelle.

use std::path::Path;

fn visit(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            visit(&p, root, found);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            for (i, line) in text.lines().enumerate() {
                if line.contains(".fetch_update(") || line.contains("compare_exchange") {
                    let rel = p.strip_prefix(root).unwrap().display();
                    found.push(format!("{rel}:{}", i + 1));
                }
            }
        }
    }
}

#[test]
fn req073_ac7_aucune_mise_a_jour_atomique_manuelle() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    visit(&root.join("src"), root, &mut found);
    assert!(
        found.is_empty(),
        "mises à jour atomiques manuelles : {found:?}"
    );
}

#[test]
fn req076_ac1_rust_version_1_99() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(path).unwrap();
    let table: toml::Table = toml::from_str(&text).unwrap();
    let version = table["package"]
        .get("rust-version")
        .and_then(|v| v.as_str());
    assert_eq!(version, Some("1.99"));
}

#[test]
fn req076_ac3_perf_sans_attente() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/perf.rs");
    let text = std::fs::read_to_string(path).unwrap();
    let lines: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("thread::sleep("))
        .map(|(i, _)| i + 1)
        .collect();
    assert!(lines.is_empty(), "thread::sleep( aux lignes {lines:?}");
}

/// Lignes (`fichier:ligne`) de la partie non test des `src/parse/*.rs` qui dédupliquent par liste :
/// `!` (espaces permis) puis un chemin commençant par une minuscule ou `self.`, puis `.contains(&`.
fn dedup_par_liste(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let c: Vec<char> = line.chars().collect();
        for (k, _) in c.iter().enumerate().filter(|(_, ch)| **ch == '!') {
            let mut j = k + 1;
            while j < c.len() && c[j].is_whitespace() {
                j += 1;
            }
            let start = j;
            if !(c.get(j).is_some_and(|ch| ch.is_lowercase())) {
                continue;
            }
            while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_' || c[j] == '.') {
                j += 1;
            }
            let path: String = c[start..j].iter().collect();
            let rest: String = c[j..].iter().collect();
            if path.ends_with(".contains") && rest.starts_with("(&") {
                found.push(i + 1);
                break;
            }
        }
    }
    found
}

#[test]
fn req077_ac3_aucune_deduplication_par_liste() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/parse");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for l in dedup_par_liste(&text) {
                found.push(format!(
                    "{}:{l}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    found.sort();
    assert!(found.is_empty(), "déduplications par liste : {found:?}");
}
