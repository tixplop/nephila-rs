//! Parcours du dépôt : filtrage, empreintes BLAKE3 et rattachement aux modules.

use crate::error::{warn, Error, Result};
use crate::filter::{FileType, Matcher};
use crate::model::FileRecord;
use ignore::WalkBuilder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Règle « racy-clean » : l'empreinte enregistrée n'est réutilisée que si la date de modification
/// enregistrée précède l'enregistrement d'au moins ce délai ; sinon une modification de même
/// taille dans le même pas de date du système de fichiers passerait inaperçue.
const RACY_WINDOW_MS: i64 = 2_000;

const EXCLUDED_DIRS: [&str; 6] = [
    "target",
    "build",
    ".gradle",
    ".git",
    ".nephila",
    "node_modules",
];
const BUILD_FILES: [&str; 7] = [
    "package.json",
    "pom.xml",
    "ivy.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
];

/// Fichier retenu par le parcours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    /// Chemin relatif à la racine, séparateur `/`.
    pub path: String,
    pub hash: String,
    pub size: u64,
    /// Date de modification en millisecondes depuis l'epoch.
    pub mtime: i64,
    /// Dossier (relatif, `""` = racine) du fichier de build ancêtre le plus proche.
    pub module: Option<String>,
    /// Chemin réel (celui du parcours), par lequel le fichier est lu (DEC-070).
    pub abs: PathBuf,
    /// `true` si l'empreinte a été calculée (et non réutilisée).
    pub rehashed: bool,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    pub warnings: Vec<String>,
    /// Dossiers (relatifs, `""` = racine) contenant un `settings.gradle(.kts)`, avec `rootProject.name`
    /// (laissé à `None` par le parcours).
    pub gradle_settings: HashMap<String, Option<String>>,
    /// Fichiers que le parcours aurait retenus sans filtre (REQ-037).
    pub excluded: usize,
}

fn is_build_file(name: &str) -> bool {
    BUILD_FILES.contains(&name)
}

fn is_supported(name: &str) -> bool {
    FileType::of(name).is_some()
}

/// Module d'un fichier : dossier du fichier de build le plus proche ancêtre
/// (le dossier du fichier lui-même compte). `build_dirs` contient des dossiers relatifs.
pub fn module_of(path: &str, build_dirs: &HashSet<String>) -> Option<String> {
    let mut dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    loop {
        if build_dirs.contains(dir) {
            return Some(dir.to_string());
        }
        if dir.is_empty() {
            return None;
        }
        dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
    }
}

/// Fichier retenu par le parcours, sans lecture de son contenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkedFile {
    /// Chemin relatif à la racine, séparateur `/`.
    pub path: String,
    /// Chemin réel sur le disque (peut ne pas être de l'UTF-8 valide) : c'est lui qu'on lit.
    pub abs: PathBuf,
    pub size: u64,
    /// Date de modification en millisecondes depuis l'epoch.
    pub mtime: i64,
}

#[derive(Debug, Default)]
pub struct WalkResult {
    pub files: Vec<WalkedFile>,
    pub warnings: Vec<String>,
    /// Fichiers que le parcours aurait retenus sans filtre (REQ-037).
    pub excluded: usize,
    /// Fichiers écartés parce qu'un autre a le même chemin après conversion (DEC-071).
    pub collisions: Vec<Collision>,
}

/// Fichier écarté : son chemin converti est celui d'un autre fichier, seul retenu (DEC-071).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// Chemin réel relatif du fichier ignoré.
    pub ignored: PathBuf,
    /// Chemin réel relatif du fichier retenu.
    pub kept: PathBuf,
    /// Chemin converti, commun aux deux.
    pub path: String,
}

/// Choix du fichier retenu dans un groupe de chemins réels relatifs de même chemin converti
/// (DEC-071) : celui dont le chemin entier est de l'UTF-8 valide, sinon le plus petit dans
/// l'ordre de `OsStr`. Rend l'indice dans `group` (non vide). Seul endroit à changer pour
/// modifier la règle.
fn choose_kept(group: &[&Path]) -> usize {
    let key = |i: usize| (group[i].to_str().is_none(), group[i].as_os_str());
    (0..group.len()).min_by_key(|&i| key(i)).unwrap_or(0)
}

/// Forme échappée d'un chemin réel relatif pour les avertissements (V-006) : octets invalides en
/// `\xHH` majuscules ; dans les suites valides `\` et `'` précédés d'un `\`, contrôles en `\xHH`.
pub fn escaped(rel: &Path) -> String {
    fn valid(out: &mut String, s: &str) {
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                c if c.is_control() && (c as u32) < 0x20 || c == '\u{7f}' => {
                    out.push_str(&format!("\\x{:02X}", c as u32));
                }
                c => out.push(c),
            }
        }
    }
    let mut out = String::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        for chunk in rel.as_os_str().as_bytes().utf8_chunks() {
            valid(&mut out, chunk.valid());
            for b in chunk.invalid() {
                out.push_str(&format!("\\x{b:02X}"));
            }
        }
    }
    #[cfg(not(unix))]
    valid(&mut out, &rel.to_string_lossy());
    out
}

/// Parcourt `root` sans rien lire : dossiers exclus (REQ-004), types pris en charge, export
/// `<slug>.json` écarté, filtres de REQ-037 puis métadonnées (chemin, taille, date).
pub fn walk(root: &Path, matcher: &Matcher) -> Result<WalkResult> {
    if !root.is_dir() {
        return Err(Error::Message(format!(
            "{} n'est pas un dossier lisible",
            root.display()
        )));
    }
    // L'export par défaut (`<slug>.json`) peut être écrit dans le dépôt : jamais indexé (AC-004-5).
    let export_name = format!("{}.json", crate::export::slug(root));
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            !(is_dir
                && e.depth() > 0
                && EXCLUDED_DIRS.contains(&e.file_name().to_string_lossy().as_ref()))
        })
        .build();

    let mut res = WalkResult::default();
    let mut found: Vec<(WalkedFile, PathBuf)> = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let msg = format!("entrée illisible ignorée : {e}");
                warn(&msg);
                res.warnings.push(msg);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !is_supported(&name) || name == export_name {
            continue;
        }
        let real_rel = entry
            .path()
            .strip_prefix(root)
            .map_err(|e| Error::Message(e.to_string()))?
            .to_path_buf();
        let rel = real_rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if matcher.excludes(&rel) {
            res.excluded += 1;
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("{rel} ignoré : lecture impossible ({e})");
                warn(&msg);
                res.warnings.push(msg);
                continue;
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);
        found.push((
            WalkedFile {
                path: rel,
                abs: entry.path().to_path_buf(),
                size: meta.len(),
                mtime,
            },
            real_rel,
        ));
    }

    // DEC-071 : un seul fichier par chemin converti, avant tout usage.
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (f, _)) in found.iter().enumerate() {
        groups.entry(f.path.as_str()).or_default().push(i);
    }
    let mut dropped = vec![false; found.len()];
    for (path, idx) in groups.iter().filter(|(_, idx)| idx.len() > 1) {
        let reals: Vec<&Path> = idx.iter().map(|&i| found[i].1.as_path()).collect();
        let kept = idx[choose_kept(&reals)];
        for &i in idx.iter().filter(|&&i| i != kept) {
            dropped[i] = true;
            res.collisions.push(Collision {
                ignored: found[i].1.clone(),
                kept: found[kept].1.clone(),
                path: (*path).to_string(),
            });
        }
    }
    res.collisions
        .sort_by(|a, b| (&a.path, a.ignored.as_os_str()).cmp(&(&b.path, b.ignored.as_os_str())));
    res.files = found
        .into_iter()
        .zip(dropped)
        .filter_map(|((f, _), d)| (!d).then_some(f))
        .collect();
    Ok(res)
}

/// Lit `rel` sous `root` ; refuse (message d'avertissement) un fichier illisible ou non UTF-8.
/// Critère unique de `scan` et de `diff::quick` (DEC-068).
pub fn read_utf8(abs: &Path, rel: &str) -> std::result::Result<Vec<u8>, String> {
    let bytes =
        std::fs::read(abs).map_err(|e| format!("{rel} ignoré : lecture impossible ({e})"))?;
    if std::str::from_utf8(&bytes).is_err() {
        return Err(format!("{rel} ignoré : fichier non UTF-8"));
    }
    Ok(bytes)
}

/// Parcourt `root`. `known` : enregistrements de la table `files`, dont l'empreinte est
/// réutilisée quand taille et mtime n'ont pas changé. `matcher` : filtres de REQ-037, appliqués
/// après les exclusions de REQ-004 et avant toute lecture.
pub fn scan(root: &Path, known: &[FileRecord], matcher: &Matcher) -> Result<ScanResult> {
    let walked = walk(root, matcher)?;
    let known: HashMap<&str, &FileRecord> = known.iter().map(|r| (r.path.as_str(), r)).collect();
    let mut res = ScanResult {
        warnings: walked.warnings,
        excluded: walked.excluded,
        ..ScanResult::default()
    };
    for c in &walked.collisions {
        let msg = format!(
            "{} ignoré : même chemin dans le graphe que {} ({}), seul retenu ; renommez ce fichier",
            escaped(&c.ignored),
            escaped(&c.kept),
            c.path
        );
        warn(&msg);
        res.warnings.push(msg);
    }
    for WalkedFile {
        path: rel,
        abs,
        size,
        mtime,
    } in walked.files
    {
        let (hash, rehashed) = match known.get(rel.as_str()) {
            Some(k)
                if k.size == size
                    && k.mtime == mtime
                    && !k.hash.is_empty()
                    && k.recorded_at.saturating_sub(k.mtime) >= RACY_WINDOW_MS =>
            {
                (k.hash.clone(), false)
            }
            _ => {
                let bytes = match read_utf8(&abs, &rel) {
                    Ok(b) => b,
                    Err(msg) => {
                        warn(&msg);
                        res.warnings.push(msg);
                        continue;
                    }
                };
                (blake3::hash(&bytes).to_hex().to_string(), true)
            }
        };
        res.files.push(ScannedFile {
            path: rel,
            hash,
            size,
            mtime,
            module: None,
            abs,
            rehashed,
        });
    }
    res.files.sort_by(|a, b| a.path.cmp(&b.path));

    let build_dirs: HashSet<String> = res
        .files
        .iter()
        .filter(|f| is_build_file(f.path.rsplit('/').next().unwrap_or(&f.path)))
        .map(|f| f.path.rsplit_once('/').map_or("", |(d, _)| d).to_string())
        .collect();
    for f in &mut res.files {
        f.module = module_of(&f.path, &build_dirs);
    }
    res.gradle_settings = res
        .files
        .iter()
        .filter(|f| {
            matches!(
                f.path.rsplit('/').next(),
                Some("settings.gradle" | "settings.gradle.kts")
            )
        })
        .map(|f| {
            let name = std::fs::read_to_string(&f.abs)
                .ok()
                .and_then(|t| crate::parse::gradle::root_project_name(&t));
            (
                f.path.rsplit_once('/').map_or("", |(d, _)| d).to_string(),
                name,
            )
        })
        .collect();
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn scan_lit_un_fichier_au_nom_non_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let name = std::ffi::OsStr::from_bytes(b"A\xff.java");
        // APFS refuse les noms non UTF-8 (EILSEQ) : le cas ne peut pas exister sur ce système.
        if let Err(e) = std::fs::write(dir.path().join(name), "class A {}\n") {
            if !cfg!(target_os = "macos") {
                panic!("création de A\\xff.java refusée hors macOS : {e}");
            }
            eprintln!("test non applicable : le système de fichiers refuse un nom non UTF-8 ({e})");
            return;
        }
        let res = scan(dir.path(), &[], &Matcher::none()).unwrap();
        assert_eq!(res.files.len(), 1, "{:?}", res.warnings);
        assert!(res.warnings.is_empty(), "{:?}", res.warnings);
        assert!(res.files[0].rehashed);
    }

    #[cfg(unix)]
    #[test]
    fn escaped_met_en_forme_le_nom_reel() {
        use std::os::unix::ffi::OsStrExt;
        let f = |b: &[u8]| escaped(Path::new(std::ffi::OsStr::from_bytes(b)));
        assert_eq!(f(b"x\\y\xff.java"), "x\\\\y\\xFF.java");
        assert_eq!(f(b"a\nb\xfe.java"), "a\\x0Ab\\xFE.java");
        assert_eq!(f("A\u{FFFD}.java".as_bytes()), "A\u{FFFD}.java");
        assert_eq!(f(b"\x80\x81.md"), "\\x80\\x81.md");
        assert_eq!(f(b"l'a.java"), "l\\'a.java");
    }

    #[test]
    fn scan_remplit_root_project_name_des_settings() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.gradle"),
            "rootProject.name = 'app'\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("t")).unwrap();
        std::fs::write(dir.path().join("t/settings.gradle.kts"), "include(\"x\")\n").unwrap();
        let res = scan(dir.path(), &[], &Matcher::none()).unwrap();
        assert_eq!(res.gradle_settings.get(""), Some(&Some("app".to_string())));
        assert_eq!(res.gradle_settings.get("t"), Some(&None));
    }
}
