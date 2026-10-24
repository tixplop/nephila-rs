//! AC-073-7 : aucun appel à `fetch_update` (dépréciée en rustc 1.99, remplacée par `try_update`
//! absente de la 1.98) dans les sources.

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
                if line.contains(".fetch_update(") {
                    let rel = p.strip_prefix(root).unwrap().display();
                    found.push(format!("{rel}:{}", i + 1));
                }
            }
        }
    }
}

#[test]
fn req073_ac7_aucun_appel_a_fetch_update() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    visit(&root.join("src"), root, &mut found);
    assert!(found.is_empty(), "appels à fetch_update : {found:?}");
}
