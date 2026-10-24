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
