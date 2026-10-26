//! AC-073-7 : aucune mise à jour atomique manuelle (`compare_exchange`, `.fetch_update(`) dans les
//! sources ; la réservation du budget passe par `try_update` (REQ-073, DEC-089).
//! REQ-076 : chaîne d'outils minimale déclarée (`rust-version = "1.99"`) et tests de performance
//! sans attente réelle.

#![allow(non_snake_case)]

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

#[test]
fn req081_ac1_profil_release() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(path).unwrap();
    let table: toml::Table = toml::from_str(&text).unwrap();
    let profile = table["profile"].as_table().expect("table profile");
    let release = profile["release"].as_table().expect("profile.release");
    assert_eq!(release.get("lto").and_then(|v| v.as_str()), Some("fat"));
    assert_eq!(
        release.get("codegen-units").and_then(|v| v.as_integer()),
        Some(1)
    );
    assert_eq!(release.get("panic").and_then(|v| v.as_str()), Some("abort"));
    assert!(!profile.contains_key("dev"), "table profile.dev interdite");
    assert!(
        !profile.contains_key("test"),
        "table profile.test interdite"
    );
}

fn cherche_catch_unwind(dir: &Path, root: &Path, found: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            cherche_catch_unwind(&p, root, found);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            for (i, line) in text.lines().enumerate() {
                if line.contains("catch_unwind") {
                    let rel = p.strip_prefix(root).unwrap().display();
                    found.push(format!("{rel}:{}", i + 1));
                }
            }
        }
    }
}

#[test]
fn req081_ac2_pas_de_catch_unwind() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    cherche_catch_unwind(&root.join("src"), root, &mut found);
    assert!(found.is_empty(), "catch_unwind : {found:?}");
}

/// Vrai si la ligne contient `motif` hors d'un identifiant plus long (`eprintln!` n'est pas
/// `println!`).
fn appelle(line: &str, motif: &str) -> bool {
    line.match_indices(motif).any(|(i, _)| {
        !line[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Fichiers `src/**/*.rs`, avec leur texte lu jusqu'à la première ligne `#[cfg(test)]`.
fn sources_hors_tests(dir: &Path, root: &Path, found: &mut Vec<(String, Vec<String>)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            sources_hors_tests(&p, root, found);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            let lines = text
                .lines()
                .take_while(|l| l.trim() != "#[cfg(test)]")
                .map(str::to_string)
                .collect();
            found.push((p.strip_prefix(root).unwrap().display().to_string(), lines));
        }
    }
}

#[test]
fn req086_ac3_aucun_println_et_un_seul_texte_d_erreur_de_stdout() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources_hors_tests(&root.join("src"), root, &mut files);
    let mut found = Vec::new();
    for (name, lines) in &files {
        for (i, line) in lines.iter().enumerate() {
            if appelle(line, "println!") || appelle(line, "print!(") {
                found.push(format!("{name}:{}", i + 1));
            }
        }
    }
    assert!(found.is_empty(), "println!/print! hors tests : {found:?}");

    let mut texts = Vec::new();
    for (name, lines) in &files {
        for (i, line) in lines.iter().enumerate() {
            if line.contains("écriture sur stdout impossible") {
                texts.push(format!("{name}:{}", i + 1));
            }
        }
    }
    assert_eq!(texts.len(), 1, "texte d'E-17 : {texts:?}");
}

#[test]
fn req123_ac1_rust_toolchain_toml() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join("rust-toolchain.toml"))
        .expect("rust-toolchain.toml absent");
    let table: toml::Table = toml::from_str(&text).unwrap();
    let tc = table["toolchain"].as_table().expect("table toolchain");
    let channel = tc.get("channel").and_then(|v| v.as_str());
    assert_eq!(channel, Some("1.99.0"));
    let cargo: toml::Table =
        toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    let rv = cargo["package"]["rust-version"].as_str().unwrap();
    assert_eq!(channel, Some(format!("{rv}.0").as_str()));
    let comps: Vec<&str> = tc["components"]
        .as_array()
        .expect("components")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        comps.contains(&"rustfmt") && comps.contains(&"clippy"),
        "{comps:?}"
    );
}

fn cherche_statiques(dir: &Path, root: &Path, found: &mut Vec<String>) {
    const NOMS: [&str; 8] = [
        "EXTRACT_THREADS",
        "WALK_THREADS",
        "HASH_THREADS",
        "PARSERS_CREATED",
        "PREPARED_TOTAL",
        "RETAINED_PEAK",
        "FORCE_FRESH",
        "READS",
    ];
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            cherche_statiques(&p, root, found);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            for (i, line) in text.lines().enumerate() {
                for nom in NOMS {
                    let motif = format!("static {nom}");
                    let declare = line.match_indices(&motif).any(|(at, _)| {
                        !line[at + motif.len()..]
                            .starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    });
                    if declare {
                        let rel = p.strip_prefix(root).unwrap().display();
                        found.push(format!("{rel}:{} : static {nom}", i + 1));
                    }
                }
            }
        }
    }
}

/// AC-124-2 : les compteurs de diagnostic vivent dans la portée du test (DEC-112), pas dans des
/// `static` du processus.
#[test]
fn REQ124_AC2_aucun_compteur_de_diagnostic_static() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    cherche_statiques(&root.join("src"), root, &mut found);
    assert!(found.is_empty(), "compteurs static : {found:?}");
}

/// Fichiers `.rs` de `src/` avec leur contenu, dans l'ordre.
fn sources(dir: &Path, out: &mut Vec<(std::path::PathBuf, String)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&p).unwrap();
            out.push((p, text));
        }
    }
}

/// Le bloc ou module qui commence à la ligne `at` est précédé de `#[cfg(test)]` (attributs et
/// commentaires `///` sautés).
fn precede_cfg_test(lines: &[&str], at: usize) -> bool {
    let mut i = at;
    while i > 0 {
        i -= 1;
        let l = lines[i].trim();
        if l == "#[cfg(test)]" {
            return true;
        }
        if !(l.starts_with("///") || l.starts_with("//") || l.starts_with("#[")) {
            return false;
        }
    }
    false
}

/// La déclaration de la ligne `at` est dans un `thread_local!` ou un module précédé de
/// `#[cfg(test)]`.
fn sous_cfg_test(lines: &[&str], at: usize) -> bool {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let mut depth = indent(lines[at]);
    for i in (0..at).rev() {
        let l = lines[i];
        if l.trim().is_empty() || indent(l) >= depth {
            continue;
        }
        depth = indent(l);
        let t = l.trim_start();
        if (t.starts_with("thread_local!") || t.starts_with("mod ") || t.starts_with("pub mod "))
            && precede_cfg_test(lines, i)
        {
            return true;
        }
        if depth == 0 {
            break;
        }
    }
    false
}

/// AC-129-1 : plus de `cfg_attr(not(test), allow(dead_code))`, une seule `enum ReadKind`, et les
/// compteurs de diagnostic compilés sous `#[cfg(test)]` seulement.
#[test]
fn REQ129_AC1_compteurs_sous_cfg_test_une_seule_read_kind() {
    const NOMS: [&str; 5] = [
        "PREPARATIONS",
        "NAME_KEYS_CALLS",
        "COMMITS",
        "PROJECTIONS",
        "STORE_READS",
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut found = Vec::new();
    let mut read_kinds = Vec::new();
    for (p, text) in &files {
        let rel = p.strip_prefix(root).unwrap().display().to_string();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.contains("cfg_attr(not(test), allow(dead_code))") {
                found.push(format!(
                    "{rel}:{} : cfg_attr(not(test), allow(dead_code))",
                    i + 1
                ));
            }
            if line.contains("enum ReadKind") {
                read_kinds.push(format!("{rel}:{}", i + 1));
            }
            for nom in NOMS {
                let t = line.trim_start();
                if t.starts_with(&format!("static {nom}:")) && !sous_cfg_test(&lines, i) {
                    found.push(format!("{rel}:{} : static {nom} hors #[cfg(test)]", i + 1));
                }
            }
        }
    }
    if read_kinds.len() != 1 {
        found.push(format!(
            "enum ReadKind déclarée {} fois : {read_kinds:?}",
            read_kinds.len()
        ));
    }
    assert!(found.is_empty(), "{found:#?}");
}

// ---- REQ-138 : aucun élément de premier niveau des tests utilisé seulement sous `cfg(target_os)`.

struct Item {
    name: String,
    decl: usize,
    first: usize,
    last: usize,
    reserved: bool,
}

fn item_name(line: &str) -> Option<String> {
    let mut rest = line;
    if let Some(r) = rest.strip_prefix("pub(crate) ") {
        rest = r;
    } else if let Some(r) = rest.strip_prefix("pub ") {
        rest = r;
    }
    let rest = ["fn ", "const ", "static "]
        .iter()
        .find_map(|k| rest.strip_prefix(k))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn est_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn contient_ident(line: &str, name: &str) -> bool {
    line.match_indices(name).any(|(i, _)| {
        let avant = line[..i].chars().next_back();
        let apres = line[i + name.len()..].chars().next();
        !avant.is_some_and(est_ident) && !apres.is_some_and(est_ident)
    })
}

/// Éléments de premier niveau d'un fichier de test (indices de lignes à partir de 0).
fn items(lines: &[&str]) -> Vec<Item> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(name) = item_name(line) else {
            continue;
        };
        let mut first = i;
        while first > 0
            && (lines[first - 1].starts_with("#[") || lines[first - 1].starts_with("//"))
        {
            first -= 1;
        }
        let reserved = lines[first..i]
            .iter()
            .any(|l| l.starts_with("#[cfg(") && l.contains("target_os"));
        let t = line.trim_end();
        let last = if t.ends_with('}') || t.ends_with(';') {
            i
        } else if line.contains("fn ") {
            (i + 1..lines.len())
                .find(|&j| lines[j].starts_with('}'))
                .unwrap_or(lines.len() - 1)
        } else {
            (i + 1..lines.len())
                .find(|&j| lines[j].trim_end().ends_with(';'))
                .unwrap_or(lines.len() - 1)
        };
        out.push(Item {
            name,
            decl: i,
            first,
            last,
            reserved,
        });
    }
    out
}

/// Violations `<rel>:<ligne> <nom>` : éléments non réservés dont les seuls usages sont dans des
/// éléments réservés à un système (`#[cfg(... target_os ...)]`).
fn violations(rel: &str, text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let items = items(&lines);
    let mut out = Vec::new();
    for it in items.iter().filter(|it| !it.reserved) {
        let (mut reserve, mut ailleurs) = (false, false);
        for (j, l) in lines.iter().enumerate() {
            if j == it.decl || l.trim_start().starts_with("//") || !contient_ident(l, &it.name) {
                continue;
            }
            if items
                .iter()
                .any(|o| o.reserved && o.first <= j && j <= o.last)
            {
                reserve = true;
            } else {
                ailleurs = true;
            }
        }
        if reserve && !ailleurs {
            out.push(format!("{rel}:{} {}", it.decl + 1, it.name));
        }
    }
    out
}

#[test]
fn REQ138_AC2_aide_violations_cas_de_l_ac() {
    let seul = "/// doc\nconst K: i64 = 1;\n\n#[cfg(target_os = \"linux\")]\nfn f() {\n    let _ = K;\n}\n";
    assert_eq!(violations("t.rs", seul), vec!["t.rs:2 K"]);

    let aussi = format!("{seul}\nfn g() {{\n    let _ = K;\n}}\n");
    assert!(violations("t.rs", &aussi).is_empty());

    let commentaire = "/// vaut K\nfn h() {}\n\nconst K: i64 = 1;\n\n#[cfg(target_os = \"linux\")]\nfn f() {\n    let _ = K;\n}\n";
    assert_eq!(violations("t.rs", commentaire), vec!["t.rs:4 K"]);

    let deux = "#[cfg(target_os = \"linux\")]\nconst E17: i64 = 1;\n#[cfg(not(target_os = \"linux\"))]\nconst E17: i64 = 2;\n\n#[cfg(target_os = \"linux\")]\nfn f() {\n    let _ = E17;\n}\n";
    assert!(violations("t.rs", deux).is_empty());

    let proche = "const EXPORT_RSS0_KIB: i64 = 1;\nfn g() {\n    let _ = EXPORT_RSS0_KIBX;\n}\n\n#[cfg(target_os = \"linux\")]\nfn f() {\n    let _ = EXPORT_RSS0_KIB;\n}\n";
    assert_eq!(violations("t.rs", proche), vec!["t.rs:1 EXPORT_RSS0_KIB"]);
    let proche2 = proche.replace("let _ = EXPORT_RSS0_KIB;", "let _ = 0;");
    assert!(violations("t.rs", &proche2).is_empty());
}

#[test]
fn REQ138_AC1_aucun_element_utilise_seulement_sous_cfg_target_os() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<_> = std::fs::read_dir(root.join("tests"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "rs"))
        .collect();
    files.sort();
    let mut found = Vec::new();
    for p in files {
        let text = std::fs::read_to_string(&p).unwrap();
        let rel = p.strip_prefix(root).unwrap().display().to_string();
        found.extend(violations(&rel, &text));
    }
    assert!(
        found.is_empty(),
        "éléments réservés à un système : {found:?}"
    );
}

/// REQ-139 : violations `<rel>:<ligne> <appel>` : création ou écriture d'un fichier dont le chemin
/// est construit par `from_bytes(` et qui se termine par `.unwrap()` ou `.expect(`.
fn raw_name_unwraps(rel: &str, text: &str) -> Vec<String> {
    const CALLS: [&str; 4] = [
        "fs::write(",
        "fs::create_dir(",
        "fs::create_dir_all(",
        "File::create(",
    ];
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut line_start = 0;
    for (n, line) in text.split('\n').enumerate() {
        let start = line_start;
        line_start += line.len() + 1;
        if line.trim_start().starts_with("//") {
            continue;
        }
        for call in CALLS {
            let mut from = 0;
            while let Some(p) = line[from..].find(call) {
                let at = from + p;
                from = at + call.len();
                let prev = line[..at].chars().next_back();
                if prev.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    continue;
                }
                let open = start + at + call.len() - 1;
                let (mut depth, mut i) = (0usize, open);
                while i < bytes.len() {
                    match bytes[i] {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                if i >= bytes.len() || !text[open..i].contains("from_bytes(") {
                    continue;
                }
                let after = text[i + 1..].trim_start();
                if after.starts_with(".unwrap()") || after.starts_with(".expect(") {
                    out.push(format!("{rel}:{} {}", n + 1, call.trim_end_matches('(')));
                }
            }
        }
    }
    out
}

#[test]
fn REQ139_AC2_aide_raw_name_unwraps_cas_de_l_ac() {
    let un = "fs::write(d.join(OsStr::from_bytes(b\"A\\xff.java\")), \"x\").unwrap();\n";
    assert_eq!(raw_name_unwraps("t.rs", un), vec!["t.rs:1 fs::write"]);

    let multi = "fn t() {\n    fs::write(\n        d.join(OsStr::from_bytes(b\"A\\xff.java\")),\n        \"x\",\n    )\n    .unwrap();\n}\n";
    assert_eq!(raw_name_unwraps("t.rs", multi), vec!["t.rs:2 fs::write"]);

    let garde =
        "if fs::write(d.join(OsStr::from_bytes(b\"A\\xff\")), \"x\").is_err() {\n    return;\n}\n";
    assert!(raw_name_unwraps("t.rs", garde).is_empty());
    let option = "fs::write(d.join(OsStr::from_bytes(b\"A\\xff\")), \"x\").ok()?;\n";
    assert!(raw_name_unwraps("t.rs", option).is_empty());
    let utf8 = "fs::write(d.join(\"B.java\"), \"x\").unwrap();\n";
    assert!(raw_name_unwraps("t.rs", utf8).is_empty());
    let autre = "my_fs::write(d.join(OsStr::from_bytes(b\"A\\xff\")), \"x\").unwrap();\n";
    assert!(raw_name_unwraps("t.rs", autre).is_empty());
    let comm = "// fs::write(d.join(OsStr::from_bytes(b\"A\\xff\")), \"x\").unwrap();\n";
    assert!(raw_name_unwraps("t.rs", comm).is_empty());
}

fn tests_de_premier_niveau() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<_> = std::fs::read_dir(root.join("tests"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "rs"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            (rel, std::fs::read_to_string(&p).unwrap())
        })
        .filter(|(rel, _)| rel != "tests/toolchain.rs")
        .collect()
}

#[test]
fn REQ139_AC1_aucune_creation_de_nom_en_octets_ne_panique() {
    let mut found = Vec::new();
    for (rel, text) in tests_de_premier_niveau() {
        found.extend(raw_name_unwraps(&rel, &text));
    }
    assert!(
        found.is_empty(),
        "créations de noms en octets qui paniquent :\n{}",
        found.join("\n")
    );
}

#[test]
fn REQ111_AC8_rlimit_nproc_reserve_a_linux() {
    const ITEMS: [&str; 7] = [
        "use ", "fn ", "const ", "static ", "struct ", "impl", "mod ",
    ];
    let mut defauts = Vec::new();
    let mut controles = 0;
    for (rel, text) in tests_de_premier_niveau() {
        if !text.contains("libc::RLIMIT_NPROC") {
            continue;
        }
        controles += 1;
        let first = text
            .lines()
            .position(|l| ITEMS.iter().any(|k| l.trim_start().starts_with(k)))
            .unwrap_or(usize::MAX);
        let lines: Vec<&str> = text.lines().collect();
        let avant = &lines[..first.min(lines.len())];
        let ok =
            avant.contains(&"#![cfg(target_os = \"linux\")]") && !lines.contains(&"#![cfg(unix)]");
        if !ok {
            defauts.push(rel);
        }
    }
    assert!(controles >= 1, "aucun fichier contrôlé");
    assert!(defauts.is_empty(), "{}", defauts.join("\n"));
}
