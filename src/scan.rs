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
/// Dossiers exclus par défaut, à tout niveau, nom exact (REQ-079, DEC-091).
pub const DEFAULT_EXCLUDED_DIRS: [&str; 7] = [
    "dist", "out", "coverage", ".next", ".nuxt", "vendor", ".idea",
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
#[derive(Debug)]
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
    /// Contenu lu pour l'empreinte, retenu jusqu'à l'analyse (REQ-073, DEC-085).
    pub content: Option<Retained>,
}

/// Budget des contenus retenus à la fois par commande (DEC-085).
pub const RETAIN_BUDGET: usize = 256 * 1024 * 1024;

/// Octets de contenu actuellement retenus (tous fichiers confondus).
static RETAINED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Maximum atteint par `RETAINED` (diagnostic des tests, AC-073-3).
#[cfg(test)]
pub(crate) static RETAINED_PEAK: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Contenu UTF-8 d'un fichier retenu en mémoire ; ses octets sont réservés dans le budget tant
/// qu'il existe et rendus quand il est libéré.
pub struct Retained {
    text: String,
}

impl Retained {
    /// Retient `text` si le budget le permet (réservation atomique avant de garder). Un budget nul
    /// ne retient rien.
    fn reserve(text: String, budget: usize) -> std::result::Result<Retained, String> {
        use std::sync::atomic::Ordering;
        let len = text.len();
        if budget == 0 {
            return Err(text);
        }
        // Réservation atomique : refusée si elle déborde ou dépasse le budget.
        let Ok(_prev) = RETAINED.try_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
            cur.checked_add(len).filter(|n| *n <= budget)
        }) else {
            return Err(text);
        };
        #[cfg(test)]
        RETAINED_PEAK.fetch_max(_prev + len, Ordering::SeqCst);
        Ok(Retained { text })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl Drop for Retained {
    fn drop(&mut self) {
        RETAINED.fetch_sub(self.text.len(), std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for Retained {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Retained({} octets)", self.text.len())
    }
}

/// Le contenu retenu est un cache : il ne compte pas dans l'égalité de deux `ScannedFile`.
impl PartialEq for ScannedFile {
    fn eq(&self, o: &Self) -> bool {
        self.path == o.path
            && self.hash == o.hash
            && self.size == o.size
            && self.mtime == o.mtime
            && self.module == o.module
            && self.abs == o.abs
            && self.rehashed == o.rehashed
    }
}

impl Eq for ScannedFile {}

/// Lectures de contenu comptées par chemin et par sorte (diagnostic des tests, AC-073-1 à 3).
#[cfg(test)]
pub(crate) mod reads {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Kind {
        Hash,
        Extract,
        Settings,
    }

    static READS: LazyLock<Mutex<HashMap<(String, Kind), usize>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub fn record(path: &str, kind: Kind) {
        let mut m = READS.lock().unwrap_or_else(|p| p.into_inner());
        *m.entry((path.to_string(), kind)).or_insert(0) += 1;
    }

    pub fn reset() {
        READS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Lectures d'une sorte.
    pub fn kind(kind: Kind) -> usize {
        let m = READS.lock().unwrap_or_else(|p| p.into_inner());
        m.iter()
            .filter(|((_, k), _)| *k == kind)
            .map(|(_, n)| n)
            .sum()
    }

    /// Chemins lus, avec leur nombre total de lectures.
    pub fn all() -> HashMap<String, usize> {
        let m = READS.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: HashMap<String, usize> = HashMap::new();
        for ((p, _), n) in m.iter() {
            *out.entry(p.clone()).or_insert(0) += n;
        }
        out
    }
}

/// Enregistre une lecture de contenu (sans effet hors tests).
pub(crate) fn count_read(_path: &str, _kind: ReadKind) {
    #[cfg(test)]
    reads::record(_path, _kind);
}

#[cfg(test)]
pub(crate) use reads::Kind as ReadKind;

/// Sorte de lecture (sans effet hors tests).
#[cfg(not(test))]
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReadKind {
    Hash,
    Extract,
    Settings,
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
    /// Groupes de fichiers de même chemin après conversion (DEC-071, DEC-104), triés par chemin.
    pub collisions: Vec<Collision>,
}

/// Groupe de fichiers de même chemin converti (DEC-071, DEC-104). Le premier, dans l'ordre de
/// choix, est dans `WalkResult::files` ; les suivants, non lus, ne passent en jeu que si la
/// lecture de celui qui les précède échoue (REQ-102).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// Chemin converti, commun à tous les fichiers du groupe.
    pub path: String,
    /// Chemin réel relatif du premier fichier du groupe.
    pub head: PathBuf,
    /// Fichiers suivants, dans l'ordre de choix : chemin réel relatif et métadonnées.
    pub rest: Vec<(PathBuf, WalkedFile)>,
}

/// Ordre de choix des fichiers d'un groupe de même chemin converti (DEC-104). `group` donne, par
/// fichier, son chemin réel relatif, sa taille et sa date ; `known` la taille et la date de
/// l'enregistrement du graphe pour ce chemin converti. Clé : d'abord le fichier qui correspond à
/// l'enregistrement (REQ-103), puis celui dont le chemin entier est de l'UTF-8 valide, puis le
/// plus petit dans l'ordre de `OsStr` (DEC-071). Rend les indices de `group`, le retenu en
/// premier. Seul endroit à changer pour modifier la règle.
fn choice_order(group: &[(&Path, u64, i64)], known: Option<(u64, i64)>) -> Vec<usize> {
    let key = |i: usize| {
        let (p, size, mtime) = group[i];
        (
            known != Some((size, mtime)),
            p.to_str().is_none(),
            p.as_os_str(),
        )
    };
    let mut order: Vec<usize> = (0..group.len()).collect();
    order.sort_by(|&a, &b| key(a).cmp(&key(b)));
    order
}

/// Forme échappée d'un chemin réel relatif pour les avertissements (V-006) : octets invalides en
/// `\xHH` majuscules ; dans les suites valides `\` et `'` précédés d'un `\`, puis la table de `escape` (contrôles C0 et C1, marques de direction).
pub fn escaped(rel: &Path) -> String {
    fn valid(out: &mut String, s: &str) {
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\'' => out.push_str("\\'"),
                c => match crate::escape::control(c) {
                    Some(e) => out.push_str(&e),
                    None => out.push(c),
                },
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

/// Texte d'une erreur d'entrée, identique à celui du parcours séquentiel de la 0.22.0 : le
/// parcours parallèle de `ignore` omet le « IO error for operation on » que `walkdir` ajoute.
fn entry_error_text(e: &ignore::Error) -> String {
    let mut inner = e;
    while let ignore::Error::WithDepth { err, .. } = inner {
        inner = err;
    }
    if let ignore::Error::WithPath { path, err } = inner {
        if let ignore::Error::Io(io) = &**err {
            return format!(
                "{}: IO error for operation on {}: {io}",
                path.display(),
                path.display()
            );
        }
    }
    e.to_string()
}

/// Traite une entrée du parcours dans le tampon local d'un fil.
fn walk_entry(
    root: &Path,
    matcher: &Matcher,
    export_name: &str,
    entry: std::result::Result<ignore::DirEntry, ignore::Error>,
    acc: &mut WalkAcc,
) {
    let entry = match entry {
        Ok(e) => e,
        Err(e) => {
            acc.warnings.push((
                String::new(),
                format!("entrée illisible ignorée : {}", entry_error_text(&e)),
            ));
            return;
        }
    };
    if !entry.file_type().is_some_and(|t| t.is_file()) {
        return;
    }
    let name = entry.file_name().to_string_lossy();
    if !is_supported(&name)
        || name == export_name
        || (matcher.default_excludes() && name.ends_with(".min.js"))
    {
        return;
    }
    let real_rel = match entry.path().strip_prefix(root) {
        Ok(p) => p.to_path_buf(),
        Err(e) => {
            let e = e.to_string();
            if acc.strip_error.as_ref().is_none_or(|old| e < *old) {
                acc.strip_error = Some(e);
            }
            return;
        }
    };
    let rel = real_rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if matcher.excludes(&rel) {
        acc.excluded += 1;
        return;
    }
    let meta = match entry.metadata() {
        Ok(m) => m,
        Err(e) => {
            let msg = format!(
                "{} ignoré : lecture impossible ({e})",
                crate::escape::path(&rel)
            );
            acc.warnings.push((rel, msg));
            return;
        }
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as i64);
    acc.found.push((
        WalkedFile {
            path: rel,
            abs: entry.path().to_path_buf(),
            size: meta.len(),
            mtime,
        },
        real_rel,
    ));
}

/// Résultats accumulés par un fil du parcours, versés dans l'accumulateur partagé à la
/// destruction du visiteur.
#[derive(Default)]
struct WalkAcc {
    found: Vec<(WalkedFile, PathBuf)>,
    /// (chemin converti ou `""`, texte).
    warnings: Vec<(String, String)>,
    excluded: usize,
    strip_error: Option<String>,
}

struct LocalAcc<'a> {
    shared: &'a std::sync::Mutex<WalkAcc>,
    acc: WalkAcc,
}

impl Drop for LocalAcc<'_> {
    fn drop(&mut self) {
        let mut g = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        let acc = std::mem::take(&mut self.acc);
        g.found.extend(acc.found);
        g.warnings.extend(acc.warnings);
        g.excluded += acc.excluded;
        if let Some(e) = acc.strip_error {
            if g.strip_error.as_ref().is_none_or(|old| e < *old) {
                g.strip_error = Some(e);
            }
        }
    }
}

/// Parcourt `root` sans rien lire : dossiers exclus (REQ-004), types pris en charge, export
/// `<slug>.json` écarté, filtres de REQ-037 puis métadonnées (chemin, taille, date). Le parcours
/// utilise `jobs` fils (DEC-084) ; les fichiers sont rendus triés par chemin et les
/// avertissements, triés, écrits après le parcours. `known` (vide sans graphe) fait passer en tête
/// d'un groupe de collision le fichier déjà indexé (REQ-103).
pub fn walk(
    root: &Path,
    matcher: &Matcher,
    jobs: usize,
    known: &[FileRecord],
) -> Result<WalkResult> {
    if !root.is_dir() {
        return Err(Error::Message(format!(
            "{} n'est pas un dossier lisible",
            root.display()
        )));
    }
    // L'export par défaut (`<slug>.json`) peut être écrit dans le dépôt : jamais indexé (AC-004-5).
    let export_name = format!("{}.json", crate::export::slug(root));
    let defaults = matcher.default_excludes();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .follow_links(false)
        .threads(jobs.max(1))
        .filter_entry(move |e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            !(is_dir && e.depth() > 0 && {
                let name = e.file_name().to_string_lossy();
                EXCLUDED_DIRS.contains(&name.as_ref())
                    || (defaults && DEFAULT_EXCLUDED_DIRS.contains(&name.as_ref()))
            })
        })
        .build_parallel();

    let shared = std::sync::Mutex::new(WalkAcc::default());
    walker.run(|| {
        let mut local = LocalAcc {
            shared: &shared,
            acc: WalkAcc::default(),
        };
        let export_name = &export_name;
        Box::new(move |entry| {
            walk_entry(root, matcher, export_name, entry, &mut local.acc);
            ignore::WalkState::Continue
        })
    });
    let acc = shared.into_inner().unwrap_or_else(|e| e.into_inner());
    if let Some(e) = acc.strip_error {
        return Err(Error::Message(e));
    }
    let mut res = WalkResult {
        excluded: acc.excluded,
        ..WalkResult::default()
    };
    let mut warnings = acc.warnings;
    warnings.sort();
    for (_, msg) in warnings {
        warn(&msg);
        res.warnings.push(msg);
    }
    let mut found = acc.found;
    found.sort_by(|a, b| (&a.0.path, &a.1).cmp(&(&b.0.path, &b.1)));

    // DEC-071 : un seul fichier par chemin converti, avant tout usage.
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (f, _)) in found.iter().enumerate() {
        groups.entry(f.path.as_str()).or_default().push(i);
    }
    let mut dropped = vec![false; found.len()];
    // Taille et date des enregistrements connus, par chemin converti (construite une fois, et
    // seulement s'il y a une collision).
    let mut known_meta: Option<HashMap<&str, (u64, i64)>> = None;
    for (path, idx) in groups.iter().filter(|(_, idx)| idx.len() > 1) {
        let table = known_meta.get_or_insert_with(|| {
            known
                .iter()
                .map(|r| (r.path.as_str(), (r.size, r.mtime)))
                .collect()
        });
        let metas: Vec<(&Path, u64, i64)> = idx
            .iter()
            .map(|&i| (found[i].1.as_path(), found[i].0.size, found[i].0.mtime))
            .collect();
        let order: Vec<usize> = choice_order(&metas, table.get(path).copied())
            .into_iter()
            .map(|k| idx[k])
            .collect();
        for &i in &order[1..] {
            dropped[i] = true;
        }
        res.collisions.push(Collision {
            path: (*path).to_string(),
            head: found[order[0]].1.clone(),
            rest: order[1..]
                .iter()
                .map(|&i| (found[i].1.clone(), found[i].0.clone()))
                .collect(),
        });
    }
    res.collisions.sort_by(|a, b| a.path.cmp(&b.path));
    res.files = found
        .into_iter()
        .zip(dropped)
        .filter_map(|((f, _), d)| (!d).then_some(f))
        .collect();
    Ok(res)
}

/// Taille des blocs de `read_utf8`.
const UTF8_BLOCK: usize = 64 * 1024;

/// Contrôle que `abs` se lit et est de l'UTF-8 valide, sans garder son contenu : lecture par
/// blocs de 64 Kio dans un tampon réutilisé, une séquence coupée en fin de bloc (au plus 3
/// octets) étant reprise au bloc suivant. Verdict et message identiques à ceux de `read_text`.
/// Critère unique de `scan` et de `diff::quick` (DEC-068).
pub fn read_utf8(abs: &Path, rel: &str) -> std::result::Result<(), String> {
    use std::io::Read;
    let label = crate::escape::path(rel);
    let mut file = std::fs::File::open(abs).map_err(|e| unreadable(&label, &e))?;
    // Reste reporté (au plus 3 octets) puis bloc lu.
    let mut buf = vec![0u8; 3 + UTF8_BLOCK];
    let mut carry = 0usize;
    loop {
        let n = match file.read(&mut buf[carry..]) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(unreadable(&label, &e)),
        };
        let end = carry + n;
        if n == 0 {
            return if carry == 0 {
                Ok(())
            } else {
                Err(not_utf8(&label))
            };
        }
        match std::str::from_utf8(&buf[..end]) {
            Ok(_) => carry = 0,
            Err(e) if e.error_len().is_none() => {
                // Séquence incomplète en fin de bloc : reprise au bloc suivant.
                let valid = e.valid_up_to();
                carry = end - valid;
                buf.copy_within(valid..end, 0);
            }
            Err(_) => return Err(not_utf8(&label)),
        }
    }
}

/// Lit `abs` en entier ; comme `read_utf8` pour le verdict, le contenu étant rendu en `String` (validée une seule fois, sans copie).
pub fn read_text(abs: &Path, rel: &str) -> std::result::Result<String, String> {
    read_text_as(abs, &crate::escape::path(rel))
}

/// Comme `read_text`, `label` étant le texte déjà échappé de `<où>` : le chemin converti, ou le
/// nom réel pour un fichier d'un groupe de collision (REQ-102, V-006).
fn read_text_as(abs: &Path, label: &str) -> std::result::Result<String, String> {
    let bytes = std::fs::read(abs).map_err(|e| unreadable(label, &e))?;
    String::from_utf8(bytes).map_err(|_| not_utf8(label))
}

/// Message d'un fichier illisible (ouverture ou lecture), commun à `read_utf8` et `read_text_as`.
fn unreadable(label: &str, e: &std::io::Error) -> String {
    format!("{label} ignoré : lecture impossible ({e})")
}

/// Message d'un fichier non UTF-8, commun à `read_utf8` et `read_text_as`.
fn not_utf8(label: &str) -> String {
    format!("{label} ignoré : fichier non UTF-8")
}

/// Empreinte lue d'un fichier et son contenu retenu s'il tient dans le budget.
type Hashed = std::result::Result<(String, Option<Retained>), String>;

/// Fils d'empreintes lancés (diagnostic des tests, AC-072-2).
#[cfg(test)]
pub(crate) static HASH_THREADS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Lit et empreinte `files[i]` pour chaque `i` de `todo`, par min(`jobs`, n) fils qui prennent
/// leurs indices dans une file partagée ; résultat à la position de `todo`. Avec `jobs` = 1 ou un
/// seul fichier, lecture dans le fil appelant.
fn hash_files(
    files: &[WalkedFile],
    todo: &[usize],
    jobs: usize,
    budget: usize,
    labels: &HashMap<&str, String>,
) -> Vec<Hashed> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let work = || {
        let mut out: Vec<(usize, Hashed)> = Vec::new();
        loop {
            let k = next.fetch_add(1, Ordering::SeqCst);
            let Some(&i) = todo.get(k) else { break };
            let f = &files[i];
            count_read(&f.path, ReadKind::Hash);
            let label = labels
                .get(f.path.as_str())
                .cloned()
                .unwrap_or_else(|| crate::escape::path(&f.path).into_owned());
            out.push((
                k,
                read_text_as(&f.abs, &label).map(|text| {
                    let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                    (hash, Retained::reserve(text, budget).ok())
                }),
            ));
        }
        out
    };
    let workers = jobs.min(todo.len());
    let mut done: Vec<(usize, Hashed)> = Vec::new();
    if workers > 1 {
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..workers {
                match std::thread::Builder::new()
                    .name("nephila-hash".into())
                    .spawn_scoped(scope, work)
                {
                    Ok(h) => {
                        #[cfg(test)]
                        HASH_THREADS.fetch_add(1, Ordering::SeqCst);
                        handles.push(h);
                    }
                    Err(_) => break,
                }
            }
            for h in handles {
                match h.join() {
                    Ok(v) => done.extend(v),
                    Err(p) => std::panic::resume_unwind(p),
                }
            }
        });
    }
    // Reste éventuel (jobs = 1, un seul fichier, ou création de fil impossible).
    done.extend(work());
    done.sort_by_key(|(k, _)| *k);
    done.into_iter().map(|(_, r)| r).collect()
}

/// Avertissement A-7 : `ignored` n'est pas dans le graphe, `kept` y est sous `path`.
fn collision_message(ignored: &Path, kept: &Path, path: &str) -> String {
    format!(
        "{} ignoré : même chemin dans le graphe que {} ({}), seul retenu ; renommez ce fichier",
        escaped(ignored),
        escaped(kept),
        crate::escape::path(path)
    )
}

/// Parcourt `root`. `known` : enregistrements de la table `files`, dont l'empreinte est
/// réutilisée quand taille et mtime n'ont pas changé. `matcher` : filtres de REQ-037, appliqués
/// après les exclusions de REQ-004 et avant toute lecture.
pub fn scan(
    root: &Path,
    known: &[FileRecord],
    matcher: &Matcher,
    jobs: usize,
) -> Result<ScanResult> {
    scan_with_budget(root, known, matcher, jobs, RETAIN_BUDGET)
}

/// `scan` avec un budget de contenus retenus explicite (réglé par les tests seulement).
pub(crate) fn scan_with_budget(
    root: &Path,
    known: &[FileRecord],
    matcher: &Matcher,
    jobs: usize,
    budget: usize,
) -> Result<ScanResult> {
    let walked = walk(root, matcher, jobs, known)?;
    let known: HashMap<&str, &FileRecord> = known.iter().map(|r| (r.path.as_str(), r)).collect();
    let mut res = ScanResult {
        warnings: walked.warnings,
        excluded: walked.excluded,
        ..ScanResult::default()
    };
    // Groupes de collision par chemin converti ; `<où>` des lectures de leur premier fichier :
    // le nom réel (REQ-102).
    let groups: HashMap<&str, &Collision> = walked
        .collisions
        .iter()
        .map(|c| (c.path.as_str(), c))
        .collect();
    let labels: HashMap<&str, String> = walked
        .collisions
        .iter()
        .map(|c| (c.path.as_str(), escaped(&c.head)))
        .collect();
    // Fichiers dont l'empreinte n'est pas réutilisable (règle taille, date, « racy-clean »).
    let reusable = |rel: &str, size: u64, mtime: i64| -> Option<String> {
        match known.get(rel) {
            Some(k)
                if k.size == size
                    && k.mtime == mtime
                    && !k.hash.is_empty()
                    && k.recorded_at.saturating_sub(k.mtime) >= RACY_WINDOW_MS =>
            {
                Some(k.hash.clone())
            }
            _ => None,
        }
    };
    let mut files = walked.files;
    let mut hashes: Vec<Option<String>> = files
        .iter()
        .map(|f| reusable(&f.path, f.size, f.mtime))
        .collect();
    let to_read: Vec<usize> = (0..hashes.len()).filter(|&i| hashes[i].is_none()).collect();
    let read = hash_files(&files, &to_read, jobs, budget, &labels);
    let mut fresh: HashMap<usize, Hashed> = to_read.iter().copied().zip(read).collect();
    // (chemin converti, chemin réel, texte).
    let mut read_warnings: Vec<(String, PathBuf, String)> = Vec::new();
    let mut collision_notes: Vec<(String, PathBuf, String)> = Vec::new();
    let mut no_file: HashSet<usize> = HashSet::new();
    // Repli au premier fichier lisible de chaque groupe (REQ-102, DEC-104), dans le fil appelant.
    for (i, f) in files.iter_mut().enumerate() {
        let Some(c) = groups.get(f.path.as_str()) else {
            continue;
        };
        let mut kept_real = c.head.clone();
        if matches!(fresh.get(&i), Some(Err(_))) {
            let Some(Err(msg)) = fresh.remove(&i) else {
                unreachable!()
            };
            read_warnings.push((c.path.clone(), c.head.clone(), msg));
            let mut found = false;
            for (real, wf) in &c.rest {
                if found {
                    collision_notes.push((
                        c.path.clone(),
                        real.clone(),
                        collision_message(real, &kept_real, &c.path),
                    ));
                    continue;
                }
                count_read(&wf.path, ReadKind::Hash);
                match read_text_as(&wf.abs, &escaped(real)) {
                    Ok(text) => {
                        let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
                        fresh.insert(i, Ok((hash, Retained::reserve(text, budget).ok())));
                        *f = wf.clone();
                        kept_real = real.clone();
                        found = true;
                    }
                    Err(m) => read_warnings.push((c.path.clone(), real.clone(), m)),
                }
            }
            if !found {
                no_file.insert(i);
            }
        } else {
            for (real, _) in &c.rest {
                collision_notes.push((
                    c.path.clone(),
                    real.clone(),
                    collision_message(real, &kept_real, &c.path),
                ));
            }
        }
    }
    for (i, f) in files.into_iter().enumerate() {
        if no_file.contains(&i) {
            continue;
        }
        let (hash, rehashed, content) = match hashes[i].take() {
            Some(h) => (h, false, None),
            None => match fresh.remove(&i).expect("résultat de lecture") {
                Ok((h, c)) => (h, true, c),
                Err(msg) => {
                    let real = PathBuf::from(&f.path);
                    read_warnings.push((f.path, real, msg));
                    continue;
                }
            },
        };
        res.files.push(ScannedFile {
            path: f.path,
            hash,
            size: f.size,
            mtime: f.mtime,
            module: None,
            abs: f.abs,
            rehashed,
            content,
        });
    }
    // Après les lectures, avant toute trace : A-7 puis lectures (V-006).
    let order = |a: &(String, PathBuf, String), b: &(String, PathBuf, String)| {
        (&a.0, a.1.as_os_str()).cmp(&(&b.0, b.1.as_os_str()))
    };
    collision_notes.sort_by(order);
    read_warnings.sort_by(order);
    for (_, _, msg) in collision_notes.into_iter().chain(read_warnings) {
        warn(&msg);
        res.warnings.push(msg);
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
            let name = match &f.content {
                Some(c) => crate::parse::gradle::root_project_name(c.as_str()),
                None => {
                    count_read(&f.path, ReadKind::Settings);
                    std::fs::read_to_string(&f.abs)
                        .ok()
                        .and_then(|t| crate::parse::gradle::root_project_name(&t))
                }
            };
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

    #[test]
    fn req073_ac6_reservation_concurrente_respecte_le_budget() {
        use std::sync::atomic::Ordering;
        use std::sync::{Barrier, Mutex};
        let base = RETAINED.load(Ordering::SeqCst);
        let peak0 = RETAINED_PEAK.load(Ordering::SeqCst);
        let budget = base + 450;
        let barrier = Barrier::new(8);
        let kept = Mutex::new(Vec::new());
        let errs = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    barrier.wait();
                    match Retained::reserve("x".repeat(100), budget) {
                        Ok(r) => kept.lock().unwrap().push(r),
                        Err(t) => errs.lock().unwrap().push(t),
                    }
                });
            }
        });
        let kept = kept.into_inner().unwrap();
        let errs = errs.into_inner().unwrap();
        assert_eq!(kept.len(), 4);
        assert_eq!(errs.len(), 4);
        assert!(errs.iter().all(|t| t.len() == 100));
        assert_eq!(RETAINED.load(Ordering::SeqCst), base + 400);
        assert!(RETAINED_PEAK.load(Ordering::SeqCst).max(peak0) <= budget);
        drop(kept);
        assert_eq!(RETAINED.load(Ordering::SeqCst), base);

        assert!(Retained::reserve("x".repeat(10), 0).is_err());
        assert_eq!(RETAINED.load(Ordering::SeqCst), base);
        let empty = Retained::reserve(String::new(), base + 10);
        assert!(empty.is_ok());
    }

    /// Verdict et message de `read_utf8` comparés à `from_utf8` du contenu entier et à `read_text`.
    fn assert_read_utf8_matches(path: &Path, content: &[u8], what: &str) {
        std::fs::write(path, content).unwrap();
        let want_ok = std::str::from_utf8(content).is_ok();
        let got = read_utf8(path, "f.txt");
        assert_eq!(got.is_ok(), want_ok, "verdict : {what}");
        match (got, read_text(path, "f.txt")) {
            (Ok(()), Ok(_)) => {}
            (Err(a), Err(b)) => {
                assert_eq!(a, b, "message : {what}");
                assert_eq!(a, "f.txt ignoré : fichier non UTF-8", "{what}");
            }
            (a, b) => panic!("{what} : {a:?} contre {:?}", b.map(|_| ())),
        }
    }

    #[test]
    fn req109_ac1_read_utf8_equivaut_a_from_utf8_autour_des_frontieres() {
        const B: usize = 65_536;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
        for n in [0, 1, B - 1, B, B + 1, 3 * B - 3, 3 * B, 3 * B + 3] {
            cases.push((format!("{n} octets"), vec![b'a'; n]));
        }
        let chars: [(&str, &[u8]); 3] = [
            ("2 octets", "é".as_bytes()),
            ("3 octets", "€".as_bytes()),
            ("4 octets", "😀".as_bytes()),
        ];
        for boundary in [B, 2 * B] {
            for (name, ch) in chars {
                for off in boundary - 5..=boundary + 2 {
                    let mut v = vec![b'a'; off];
                    v.extend_from_slice(ch);
                    v.extend_from_slice(b"zz");
                    cases.push((format!("{name} à {off}"), v));
                    // Séquence tronquée en fin de fichier, sa fin sur ou près de la frontière.
                    for cut in 1..ch.len() {
                        let mut t = vec![b'a'; off];
                        t.extend_from_slice(&ch[..cut]);
                        cases.push((format!("{name} tronqué à {cut} après {off}"), t));
                    }
                }
            }
            for off in [boundary - 1, boundary, boundary + 1] {
                for bad in [0xFFu8, 0x80, 0xC3] {
                    let mut v = vec![b'a'; off];
                    v.push(bad);
                    v.extend_from_slice(b"zz");
                    cases.push((format!("octet {bad:#x} à {off}"), v));
                }
            }
            for (name, seq) in [
                ("forme trop longue 2", &[0xC0u8, 0x80][..]),
                ("forme trop longue 3", &[0xE0, 0x80, 0x80][..]),
                ("demi-substitut", &[0xED, 0xA0, 0x80][..]),
                ("hors plage", &[0xF4, 0x90, 0x80, 0x80][..]),
            ] {
                for off in boundary - 4..=boundary + 1 {
                    let mut v = vec![b'a'; off];
                    v.extend_from_slice(seq);
                    cases.push((format!("{name} à {off}"), v.clone()));
                    v.extend_from_slice(b"zz");
                    cases.push((format!("{name} suivi de zz à {off}"), v));
                }
            }
        }
        // Valide : un caractère de 4 octets sur chaque frontière, deux fois de suite.
        let mut v = vec![b'a'; B - 2];
        v.extend_from_slice("😀😀".as_bytes());
        cases.push(("deux caractères de 4 octets sur la frontière".into(), v));
        for (what, content) in &cases {
            assert_read_utf8_matches(&path, content, what);
        }
    }

    #[test]
    fn req109_ac1_read_utf8_erreurs_de_lecture() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent.txt");
        let a = read_utf8(&absent, "absent.txt").unwrap_err();
        let b = read_text(&absent, "absent.txt").unwrap_err();
        assert_eq!(a, b);
        assert!(
            a.starts_with("absent.txt ignoré : lecture impossible ("),
            "{a}"
        );
        // Un dossier s'ouvre mais ne se lit pas : erreur pendant la lecture.
        let d = dir.path().join("dossier");
        std::fs::create_dir(&d).unwrap();
        let a = read_utf8(&d, "dossier").unwrap_err();
        assert_eq!(a, read_text(&d, "dossier").unwrap_err());
        assert!(
            a.starts_with("dossier ignoré : lecture impossible ("),
            "{a}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = dir.path().join("secret.txt");
            std::fs::write(&f, "x").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o0)).unwrap();
            let got = read_utf8(&f, "secret.txt");
            let want = read_text(&f, "secret.txt");
            // Vert sans assertion si le processus lit quand même (root).
            assert_eq!(got.is_ok(), want.is_ok());
            if let (Err(a), Err(b)) = (got, want) {
                assert_eq!(a, b);
            }
        }
    }

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
        let res = scan(dir.path(), &[], &Matcher::none(), 1).unwrap();
        assert_eq!(res.files.len(), 1, "{:?}", res.warnings);
        assert!(res.warnings.is_empty(), "{:?}", res.warnings);
        assert!(res.files[0].rehashed);
    }

    /// Dépôt de `A\xfe.java` (`class A {}`) et `A\xff.java` (`class B {}`, même taille), aux dates
    /// données en secondes ; `None` si le système de fichiers refuse un nom non UTF-8.
    #[cfg(unix)]
    fn collision_pair(dir: &Path, fe_secs: u64, ff_secs: u64) -> Option<(u64, i64, i64)> {
        use std::os::unix::ffi::OsStrExt;
        let mut size = 0;
        for (name, body, secs) in [
            (&b"A\xfe.java"[..], "class A {}\n", fe_secs),
            (&b"A\xff.java"[..], "class B {}\n", ff_secs),
        ] {
            let p = dir.join(std::ffi::OsStr::from_bytes(name));
            std::fs::write(&p, body).ok()?;
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(secs))
                .unwrap();
            size = body.len() as u64;
        }
        Some((size, fe_secs as i64 * 1000, ff_secs as i64 * 1000))
    }

    fn indexed(path: &str, size: u64, mtime: i64) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: String::new(),
            size,
            mtime,
            recorded_at: 0,
            state: crate::model::FileState::Indexed,
        }
    }

    #[cfg(unix)]
    #[test]
    fn req103_ac3_ordre_de_choix_du_groupe_de_collision() {
        use std::os::unix::ffi::OsStrExt;
        let name = |b: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(b));
        let fe = name(b"A\xfe.java");
        let ff = name(b"A\xff.java");
        for jobs in [1, 8] {
            // Cas 1 : `known` correspond à `A\xff.java` seul -> retenu, `A\xfe.java` ignoré.
            let dir = tempfile::tempdir().unwrap();
            let Some((size, fe_ms, ff_ms)) =
                collision_pair(dir.path(), 1_600_000_000, 1_700_000_000)
            else {
                return;
            };
            let known = [indexed("A\u{FFFD}.java", size, ff_ms)];
            let w = walk(dir.path(), &Matcher::none(), jobs, &known).unwrap();
            assert_eq!(w.files.len(), 1);
            assert_eq!(w.files[0].abs, dir.path().join(&ff), "jobs {jobs}");
            assert_eq!(w.collisions.len(), 1);
            assert_eq!(w.collisions[0].rest[0].0, fe);
            assert_eq!(w.collisions[0].head, ff);
            // Cas 2 : autre taille -> DEC-071, `A\xfe.java` retenu.
            let known = [indexed("A\u{FFFD}.java", size + 1, ff_ms)];
            let w = walk(dir.path(), &Matcher::none(), jobs, &known).unwrap();
            assert_eq!(w.files[0].abs, dir.path().join(&fe), "jobs {jobs}");
            assert_eq!(w.collisions[0].rest[0].0, ff);
            // Autre date -> idem ; la date de `A\xfe.java` seule le fait retenir.
            let known = [indexed("A\u{FFFD}.java", size, ff_ms + 1)];
            let w = walk(dir.path(), &Matcher::none(), jobs, &known).unwrap();
            assert_eq!(w.files[0].abs, dir.path().join(&fe), "jobs {jobs}");
            let known = [indexed("A\u{FFFD}.java", size, fe_ms)];
            let w = walk(dir.path(), &Matcher::none(), jobs, &known).unwrap();
            assert_eq!(w.files[0].abs, dir.path().join(&fe), "jobs {jobs}");
            // Cas 3 : mêmes dates, `known` correspond aux deux -> DEC-071.
            let dir = tempfile::tempdir().unwrap();
            let (size, _, ms) = collision_pair(dir.path(), 1_650_000_000, 1_650_000_000).unwrap();
            let known = [indexed("A\u{FFFD}.java", size, ms)];
            let w = walk(dir.path(), &Matcher::none(), jobs, &known).unwrap();
            assert_eq!(w.files[0].abs, dir.path().join(&fe), "jobs {jobs}");
            assert_eq!(w.collisions[0].rest[0].0, ff);
            // Cas 4 (REQ-102) : `A\xff.java` correspond à `known` (empreinte vide, donc lu) mais
            // n'est pas de l'UTF-8 : `A\xfe.java` prend la place.
            let dir = tempfile::tempdir().unwrap();
            let (size, _, ff_ms) =
                collision_pair(dir.path(), 1_600_000_000, 1_700_000_000).unwrap();
            std::fs::write(dir.path().join(&ff), vec![0xff; size as usize]).unwrap();
            std::fs::File::options()
                .write(true)
                .open(dir.path().join(&ff))
                .unwrap()
                .set_modified(UNIX_EPOCH + std::time::Duration::from_millis(ff_ms as u64))
                .unwrap();
            let known = [indexed("A\u{FFFD}.java", size, ff_ms)];
            let w = walk(dir.path(), &Matcher::none(), jobs, &known).unwrap();
            assert_eq!(w.files[0].abs, dir.path().join(&ff), "tête = fichier connu");
            let r = scan(dir.path(), &known, &Matcher::none(), jobs).unwrap();
            assert_eq!(r.files.len(), 1, "{:?}", r.warnings);
            assert_eq!(r.files[0].abs, dir.path().join(&fe), "jobs {jobs}");
            assert_eq!(
                r.warnings,
                vec!["A\\xFF.java ignoré : fichier non UTF-8".to_string()],
                "jobs {jobs}"
            );
        }
    }

    /// Dossier d'AC-102-1 sans `B.java` : `A\xfe.java` (contenu `\xff`) et `A\xff.java`.
    #[cfg(unix)]
    fn fallback_dir(files: &[(&[u8], &[u8])]) -> Option<tempfile::TempDir> {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(std::ffi::OsStr::from_bytes(name)), body).ok()?;
        }
        Some(dir)
    }

    #[cfg(unix)]
    #[test]
    fn req102_ac6_le_suivant_lisible_prend_la_place() {
        use std::os::unix::ffi::OsStrExt;
        let Some(dir) =
            fallback_dir(&[(b"A\xfe.java", b"\xff"), (b"A\xff.java", b"class A2 {}\n")])
        else {
            return;
        };
        let mut first = None;
        for jobs in [1, 8] {
            let r = scan(dir.path(), &[], &Matcher::none(), jobs).unwrap();
            assert_eq!(r.files.len(), 1, "{:?}", r.warnings);
            let f = &r.files[0];
            assert_eq!(f.path, "A\u{FFFD}.java");
            assert_eq!(f.hash, blake3::hash(b"class A2 {}\n").to_hex().to_string());
            assert_eq!(f.size, 12);
            assert!(f.rehashed);
            assert!(f.abs.as_os_str().as_bytes().ends_with(b"A\xff.java"));
            assert_eq!(
                r.warnings,
                vec!["A\\xFE.java ignoré : fichier non UTF-8".to_string()]
            );
            match &first {
                None => first = Some((r.files, r.warnings)),
                Some((files, warnings)) => {
                    assert_eq!(&r.files, files);
                    assert_eq!(&r.warnings, warnings);
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn req102_ac3_le_compteur_ne_voit_pas_le_fichier_non_lu() {
        let Some(dir) = fallback_dir(&[
            (b"A\xfd.java", b"\xff"),
            (b"A\xfe.java", b"class A2 {}\n"),
            (b"A\xff.java", b"\xff"),
        ]) else {
            return;
        };
        reads::reset();
        let r = scan(dir.path(), &[], &Matcher::none(), 1).unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(
            r.warnings,
            vec![
                "A\\xFF.java ignoré : même chemin dans le graphe que A\\xFE.java (A\u{FFFD}.java), seul retenu ; renommez ce fichier".to_string(),
                "A\\xFD.java ignoré : fichier non UTF-8".to_string(),
            ]
        );
        // Deux lectures du chemin converti (`A\xfd`, `A\xfe`), jamais une troisième (`A\xff`).
        assert_eq!(reads::all().get("A\u{FFFD}.java"), Some(&2));
    }

    #[cfg(unix)]
    #[test]
    fn req102_aucun_fichier_lisible_un_avertissement_par_fichier() {
        let Some(dir) = fallback_dir(&[(b"A\xfe.java", b"\xff"), (b"A\xff.java", b"\xfe")]) else {
            return;
        };
        let r = scan(dir.path(), &[], &Matcher::none(), 1).unwrap();
        assert!(r.files.is_empty());
        assert_eq!(
            r.warnings,
            vec![
                "A\\xFE.java ignoré : fichier non UTF-8".to_string(),
                "A\\xFF.java ignoré : fichier non UTF-8".to_string(),
            ]
        );
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

    #[cfg(unix)]
    #[test]
    fn req101_ac1_escaped_controles_c1_et_marques_de_direction() {
        use std::os::unix::ffi::OsStrExt;
        let f = |b: &[u8]| escaped(Path::new(std::ffi::OsStr::from_bytes(b)));
        assert_eq!(f(b"a\xc2\x9bb.java"), "a\\u{009B}b.java");
        assert_eq!(f(b"a\xc2\x85.java"), "a\\u{0085}.java");
        for cp in [
            0x202Au32, 0x202B, 0x202C, 0x202D, 0x202E, 0x2066, 0x2067, 0x2068, 0x2069,
        ] {
            let name = format!("x{}.java", char::from_u32(cp).unwrap());
            assert_eq!(f(name.as_bytes()), format!("x\\u{{{cp:04X}}}.java"));
        }
        for c in ['\u{200F}', '\u{2028}', '\u{FFFD}'] {
            let name = format!("x{c}.java");
            assert_eq!(f(name.as_bytes()), name);
        }
        assert_eq!(f(b"a\x9bb.java"), "a\\x9Bb.java");
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
        let res = scan(dir.path(), &[], &Matcher::none(), 1).unwrap();
        assert_eq!(res.gradle_settings.get(""), Some(&Some("app".to_string())));
        assert_eq!(res.gradle_settings.get("t"), Some(&None));
    }

    // ---- AC-079-5 : exclusions par défaut non parcourues ----

    #[cfg(unix)]
    #[test]
    fn req079_ac5_dossiers_exclus_par_defaut_non_parcourus() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(&root.join("A.java"), b"class A {}\n");
        let locked = root.join("sub/vendor");
        put(&locked.join("V.java"), b"class V {}\n");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(&locked).is_err();
        let on = walk(root, &Matcher::none(), 2, &[]).unwrap();
        let off_matcher = crate::filter::Filters {
            defaults_off: true,
            ..Default::default()
        }
        .compile(root)
        .unwrap();
        let off = walk(root, &off_matcher, 2, &[]).unwrap();
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755));
        assert!(on.warnings.is_empty(), "{:?}", on.warnings);
        assert_eq!(on.excluded, 0);
        let paths: Vec<_> = on.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["A.java"]);
        if unreadable {
            assert!(!off.warnings.is_empty(), "avertissement attendu");
        } else {
            assert!(off.files.iter().any(|f| f.path == "sub/vendor/V.java"));
        }
    }

    // ---- AC-072-1 : oracles (0.22.0, HEAD avant T-154) et générateur d'arbres à graine fixe ----

    /// Règle de choix de la 0.22.0 (DEC-071), conservée comme oracle.
    fn choose_kept(group: &[&Path]) -> usize {
        let key = |i: usize| (group[i].to_str().is_none(), group[i].as_os_str());
        (0..group.len()).min_by_key(|&i| key(i)).unwrap_or(0)
    }

    /// Fichier écarté d'une collision selon la 0.22.0.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct OracleCollision {
        ignored: PathBuf,
        kept: PathBuf,
        path: String,
    }

    /// Résultat du `walk` de la 0.22.0.
    #[derive(Default)]
    struct OracleWalk {
        files: Vec<WalkedFile>,
        warnings: Vec<String>,
        excluded: usize,
        collisions: Vec<OracleCollision>,
    }

    /// Groupes du `walk` courant ramenés à la forme de la 0.22.0 (`known` vide : l'ordre de
    /// choix est celui de DEC-071, le premier du groupe est le retenu de l'oracle).
    fn as_oracle(w: &WalkResult) -> Vec<OracleCollision> {
        let mut v: Vec<OracleCollision> = w
            .collisions
            .iter()
            .flat_map(|c| {
                c.rest.iter().map(|(real, _)| OracleCollision {
                    ignored: real.clone(),
                    kept: c.head.clone(),
                    path: c.path.clone(),
                })
            })
            .collect();
        v.sort_by(|a, b| (&a.path, a.ignored.as_os_str()).cmp(&(&b.path, b.ignored.as_os_str())));
        v
    }

    /// `walk` de la 0.22.0, séquentiel, conservé tel quel comme oracle.
    fn oracle_walk(root: &Path, matcher: &Matcher) -> Result<OracleWalk> {
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

        let mut res = OracleWalk::default();
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
                    let msg = format!(
                        "{} ignoré : lecture impossible ({e})",
                        crate::escape::path(&rel)
                    );
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
                res.collisions.push(OracleCollision {
                    ignored: found[i].1.clone(),
                    kept: found[kept].1.clone(),
                    path: (*path).to_string(),
                });
            }
        }
        res.collisions.sort_by(|a, b| {
            (&a.path, a.ignored.as_os_str()).cmp(&(&b.path, b.ignored.as_os_str()))
        });
        res.files = found
            .into_iter()
            .zip(dropped)
            .filter_map(|((f, _), d)| (!d).then_some(f))
            .collect();
        Ok(res)
    }

    /// `scan` de la 0.22.0, conservé tel quel comme oracle.
    fn oracle_scan(root: &Path, known: &[FileRecord], matcher: &Matcher) -> Result<ScanResult> {
        let walked = oracle_walk(root, matcher)?;
        let known: HashMap<&str, &FileRecord> =
            known.iter().map(|r| (r.path.as_str(), r)).collect();
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
                crate::escape::path(&c.path)
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
                    let bytes = match read_text(&abs, &rel) {
                        Ok(b) => b.into_bytes(),
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
                content: None,
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

    /// Générateur congruentiel à graine fixe (sans dépendance).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % n
        }
    }

    fn put(path: &Path, content: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Arbre aléatoire reproductible ; rend le dossier de la racine, les filtres et les dossiers
    /// rendus illisibles (à rétablir avant destruction).
    pub(crate) fn gen_tree(seed: u64, root: &Path) -> (crate::filter::Filters, Vec<PathBuf>) {
        let mut g = Lcg(seed.wrapping_mul(7919).wrapping_add(13));
        let dirs = [
            "a",
            "b",
            "src",
            "gen",
            "docs",
            "x/y",
            "m1/m2/m3",
            "target",
            "build",
            "node_modules",
            ".git",
            ".gradle",
            ".nephila",
            "src/build",
            "a/target/z",
        ];
        let files = [
            "A.java",
            "B.kt",
            "C.scala",
            "D.groovy",
            "r.md",
            "q.sql",
            "c.yaml",
            "d.xml",
            "e.json",
            "keep.java",
            "note.txt",
            "blob.bin",
            "package.json",
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
            "ivy.xml",
            "a.js",
            "t.ts",
        ];
        put(&root.join("README.md"), b"# r\n");
        for _ in 0..(10 + g.next(40)) {
            let d = dirs[g.next(dirs.len())];
            let f = files[g.next(files.len())];
            let content: &[u8] = match g.next(4) {
                0 => b"",
                1 => b"\xff\xfe not utf8 \x80",
                2 => b"rootProject.name = 'p'\ninclude 'x'\n",
                _ => b"class A {}\n",
            };
            let base = if g.next(5) == 0 {
                root.to_path_buf()
            } else {
                root.join(d)
            };
            put(&base.join(f), content);
        }
        if g.next(2) == 0 {
            put(&root.join("settings.gradle"), b"include 'a'\n");
        }
        if g.next(2) == 0 {
            put(
                &root.join(format!("{}.json", crate::export::slug(root))),
                b"{}",
            );
            put(
                &root
                    .join("sub")
                    .join(format!("{}.json", crate::export::slug(root))),
                b"{}",
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            if g.next(2) == 0 {
                let _ = std::os::unix::fs::symlink("README.md", root.join("link.md"));
                let _ = std::os::unix::fs::symlink("a", root.join("linkdir"));
                let _ = std::os::unix::fs::symlink("nowhere.java", root.join("broken.java"));
            }
            if g.next(2) == 0 && !cfg!(target_os = "macos") {
                // DEC-071 : noms non UTF-8 en collision après conversion.
                let d = root.join("nu");
                std::fs::create_dir_all(&d).unwrap();
                for n in [
                    &b"N\xff.java"[..],
                    b"N\xfe.java",
                    "N\u{FFFD}.java".as_bytes(),
                    b"Z\x80.md",
                ] {
                    // REQ-102 (AC-102-8) : une graine sur trois rend non UTF-8 le fichier que
                    // l'oracle 0.22.0 retient (`N\u{FFFD}.java`).
                    let body: &[u8] = if seed.is_multiple_of(3) && n == "N\u{FFFD}.java".as_bytes()
                    {
                        b"\xff"
                    } else {
                        b"class N {}\n"
                    };
                    std::fs::write(d.join(std::ffi::OsStr::from_bytes(n)), body).unwrap();
                }
            }
        }
        let mut filters = crate::filter::Filters::default();
        match g.next(4) {
            0 => filters.patterns = vec!["gen/".into(), "*.md".into(), "!r.md".into()],
            1 => filters.patterns = vec!["**/a/**".into(), "!**/keep.java".into()],
            2 => {
                filters.types = [FileType::Java, FileType::Markdown].into_iter().collect();
                filters.patterns = vec!["src/".into()];
            }
            _ => {}
        }
        #[allow(unused_mut)]
        let mut locked = Vec::new();
        #[cfg(unix)]
        if g.next(3) == 0 {
            use std::os::unix::fs::PermissionsExt;
            // Plusieurs dossiers illisibles, à des profondeurs différentes (avertissements de parcours).
            let all = [
                "locked",
                "b/locked2",
                "m1/m2/locked3",
                "zz/yy/xx/locked4",
                "src/aa/locked5",
                "docs/k",
            ];
            let n = 2 + g.next(all.len() - 1);
            for rel in all.iter().take(n) {
                let d = root.join(rel);
                put(&d.join("L.java"), b"class L {}\n");
                locked.push(d);
            }
            for d in &locked {
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o000)).unwrap();
            }
        }
        (filters, locked)
    }

    pub(crate) fn unlock(locked: &[PathBuf]) {
        #[cfg(unix)]
        for d in locked {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
        }
        #[cfg(not(unix))]
        let _ = locked;
    }

    /// Vrai si le groupe `nu/N*.java` du générateur a pour retenu de l'oracle un fichier non UTF-8.
    #[cfg(unix)]
    fn bad_group(root: &Path) -> bool {
        std::fs::read(root.join("nu").join("N\u{FFFD}.java"))
            .is_ok_and(|b| String::from_utf8(b).is_err())
    }

    #[cfg(unix)]
    const BAD_A7: &str = "nu/N\\xFF.java ignoré : même chemin dans le graphe que nu/N\\xFE.java (nu/N\u{FFFD}.java), seul retenu ; renommez ce fichier";

    /// Pour ces seuls groupes, la comparaison à l'oracle est remplacée par la règle de REQ-102
    /// (AC-102-8) : `nu/N\xfe.java`, le premier lisible, prend la place, `nu/N\xff.java` reçoit
    /// l'A-7 qui le nomme. Retire le groupe des deux résultats après l'avoir vérifié sur `got`.
    #[cfg(unix)]
    fn settle_bad_group(got: &mut ScanResult, want: &mut ScanResult) {
        use std::os::unix::ffi::OsStrExt;
        let p = "nu/N\u{FFFD}.java";
        let f = got
            .files
            .iter()
            .find(|f| f.path == p)
            .expect("groupe retenu");
        assert!(f.abs.as_os_str().as_bytes().ends_with(b"N\xfe.java"));
        assert_eq!(f.hash, blake3::hash(b"class N {}\n").to_hex().to_string());
        assert!(f.rehashed);
        assert!(
            got.warnings.iter().any(|w| w == BAD_A7),
            "{:?}",
            got.warnings
        );
        got.files.retain(|f| f.path != p);
        for r in [got, want] {
            r.warnings
                .retain(|w| !(w.starts_with("nu/N") && w.contains("même chemin")));
        }
    }

    fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
        v.sort();
        v
    }

    #[test]
    fn req072_ac1_walk_equals_sequential_oracle_for_any_jobs() {
        let mut collisions = 0;
        let mut excluded = 0;
        let mut multi = 0;
        for seed in 0..100u64 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("repo");
            std::fs::create_dir_all(&root).unwrap();
            let (filters, locked) = gen_tree(seed, &root);
            let matcher = filters.compile(&root).unwrap();
            let mut want = oracle_walk(&root, &matcher).unwrap();
            want.files.sort_by(|a, b| a.path.cmp(&b.path));
            collisions += want.collisions.len();
            excluded += want.excluded;
            for jobs in [1, 2, 8] {
                let got = walk(&root, &matcher, jobs, &[]).unwrap();
                assert_eq!(got.files, want.files, "seed {seed}, jobs {jobs}");
                assert_eq!(as_oracle(&got), want.collisions, "seed {seed}, jobs {jobs}");
                assert_eq!(got.excluded, want.excluded, "seed {seed}, jobs {jobs}");
                assert_eq!(
                    sorted(got.warnings.clone()),
                    sorted(want.warnings.clone()),
                    "seed {seed}, jobs {jobs}"
                );
                // Groupe trié (chemin, texte) : le chemin est le début du texte quand il existe.
                let key = |w: &String| -> (String, String) {
                    if w.starts_with("entrée illisible ignorée") {
                        (String::new(), w.clone())
                    } else {
                        (w.split(" ignoré : ").next().unwrap().to_string(), w.clone())
                    }
                };
                let mut by_key: Vec<&String> = got.warnings.iter().collect();
                by_key.sort_by_key(|w| key(w));
                assert_eq!(
                    got.warnings.iter().collect::<Vec<_>>(),
                    by_key,
                    "avertissements triés par (chemin, texte), seed {seed}, jobs {jobs}"
                );
                if got.warnings.len() > 1 {
                    multi += 1;
                }
                let sw: Vec<&String> = got.warnings.iter().collect();
                let again = walk(&root, &matcher, jobs, &[]).unwrap();
                assert_eq!(
                    sw,
                    again.warnings.iter().collect::<Vec<_>>(),
                    "ordre stable"
                );
            }
            unlock(&locked);
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        assert!(collisions > 0, "le générateur doit produire des collisions");
        let _ = collisions;
        assert!(excluded > 0, "le générateur doit produire des exclus");
        #[cfg(unix)]
        if !running_as_root() {
            assert!(multi > 0, "plusieurs avertissements de parcours attendus");
        }
        let _ = multi;
    }

    #[cfg(unix)]
    fn running_as_root() -> bool {
        std::fs::read_dir("/root").is_ok() && std::env::var("USER").is_ok_and(|u| u == "root")
    }

    #[test]
    fn req072_ac1_scan_equals_sequential_oracle_for_any_jobs() {
        let mut bad_seen = 0;
        let mut compared = 0;
        for seed in 0..100u64 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("repo");
            std::fs::create_dir_all(&root).unwrap();
            let (filters, locked) = gen_tree(seed, &root);
            let matcher = filters.compile(&root).unwrap();
            let mut want = oracle_scan(&root, &[], &matcher).unwrap();
            #[cfg(unix)]
            let bad = bad_group(&root)
                && want
                    .warnings
                    .iter()
                    .any(|w| w == "nu/N\u{FFFD}.java ignoré : fichier non UTF-8");
            #[cfg(not(unix))]
            let bad = false;
            bad_seen += usize::from(bad);
            compared +=
                usize::from(!bad && want.warnings.iter().any(|w| w.contains("même chemin")));
            for jobs in [1, 2, 8] {
                let mut got = scan(&root, &[], &matcher, jobs).unwrap();
                #[cfg(unix)]
                if bad {
                    settle_bad_group(&mut got, &mut want);
                }
                assert_eq!(got.files, want.files, "seed {seed}, jobs {jobs}");
                assert_eq!(got.gradle_settings, want.gradle_settings, "seed {seed}");
                assert_eq!(got.excluded, want.excluded, "seed {seed}");
                assert_eq!(
                    sorted(got.warnings.clone()),
                    sorted(want.warnings.clone()),
                    "seed {seed}, jobs {jobs}"
                );
            }
            unlock(&locked);
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert!(
                bad_seen > 0,
                "AC-102-8 : groupes à retenu non UTF-8 attendus"
            );
            assert!(
                compared > 0,
                "AC-102-8 : collisions comparées à l'oracle attendues"
            );
        }
        let _ = (bad_seen, compared);
    }

    #[test]
    fn req072_ac1_scan_with_known_equals_sequential_oracle_for_any_jobs() {
        use crate::model::FileState;
        for seed in 0..100u64 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("repo");
            std::fs::create_dir_all(&root).unwrap();
            let (filters, locked) = gen_tree(seed, &root);
            let matcher = filters.compile(&root).unwrap();
            let base = oracle_scan(&root, &[], &matcher).unwrap();
            // Variantes : réutilisable, taille périmée, date périmée, « racy ».
            let known: Vec<FileRecord> = base
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let (size, mtime, recorded_at) = match i % 4 {
                        0 => (f.size, f.mtime, f.mtime + 10_000),
                        1 => (f.size + 1, f.mtime, f.mtime + 10_000),
                        2 => (f.size, f.mtime - 5_000, f.mtime + 10_000),
                        _ => (f.size, f.mtime, f.mtime),
                    };
                    FileRecord {
                        path: f.path.clone(),
                        hash: format!("faux-{i}"),
                        size,
                        mtime,
                        recorded_at,
                        state: FileState::Indexed,
                    }
                })
                .collect();
            let mut want = oracle_scan(&root, &known, &matcher).unwrap();
            #[cfg(unix)]
            let bad = bad_group(&root)
                && want
                    .warnings
                    .iter()
                    .any(|w| w == "nu/N\u{FFFD}.java ignoré : fichier non UTF-8");
            #[cfg(not(unix))]
            let bad = false;
            for jobs in [1, 2, 8] {
                let mut got = scan(&root, &known, &matcher, jobs).unwrap();
                #[cfg(unix)]
                if bad {
                    settle_bad_group(&mut got, &mut want);
                }
                assert_eq!(got.files, want.files, "seed {seed}, jobs {jobs}");
                assert_eq!(got.gradle_settings, want.gradle_settings, "seed {seed}");
                assert_eq!(got.excluded, want.excluded, "seed {seed}");
                assert_eq!(
                    sorted(got.warnings.clone()),
                    sorted(want.warnings.clone()),
                    "seed {seed}, jobs {jobs}"
                );
                // Groupe des lectures trié par chemin puis texte.
                let reads: Vec<&String> = got
                    .warnings
                    .iter()
                    .filter(|w| w.contains("lecture impossible") || w.contains("non UTF-8"))
                    .collect();
                let mut again = reads.clone();
                again.sort();
                assert_eq!(reads, again, "seed {seed}, jobs {jobs}");
            }
            unlock(&locked);
        }
    }

    /// 40 fichiers dont `reusable` à empreinte réutilisable ; fils d'empreintes lancés.
    fn hash_threads_for(reusable: usize, jobs: usize) -> usize {
        use crate::model::FileState;
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..40 {
            put(&root.join(format!("f{i:02}.md")), b"contenu");
        }
        let base = oracle_scan(root, &[], &Matcher::none()).unwrap();
        let known: Vec<FileRecord> = base
            .files
            .iter()
            .take(reusable)
            .map(|f| FileRecord {
                path: f.path.clone(),
                hash: f.hash.clone(),
                size: f.size,
                mtime: f.mtime,
                recorded_at: f.mtime + 10_000,
                state: FileState::Indexed,
            })
            .collect();
        HASH_THREADS.store(0, Ordering::SeqCst);
        let got = scan(root, &known, &Matcher::none(), jobs).unwrap();
        assert_eq!(got.files.len(), 40);
        assert_eq!(
            got.files.iter().filter(|f| f.rehashed).count(),
            40 - reusable
        );
        HASH_THREADS.load(Ordering::SeqCst)
    }

    #[test]
    fn req072_ac2_hash_threads_are_min_jobs_and_files_to_read() {
        assert_eq!(hash_threads_for(0, 3), 3);
    }

    #[test]
    fn req072_ac2_two_files_to_read_two_threads() {
        assert_eq!(hash_threads_for(38, 3), 2);
    }

    #[test]
    fn req072_ac2_jobs_one_spawns_no_thread() {
        assert_eq!(hash_threads_for(0, 1), 0);
    }

    #[test]
    fn req072_ac2_single_file_to_read_spawns_no_thread() {
        assert_eq!(hash_threads_for(39, 8), 0);
    }
}
