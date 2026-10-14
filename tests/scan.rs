use rkg::model::{FileRecord, FileState};
use rkg::scan::{scan, ScanResult};
use std::fs;
use std::path::Path;

fn put(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn paths(r: &ScanResult) -> Vec<String> {
    r.files.iter().map(|f| f.path.clone()).collect()
}

#[test]
fn req004_ac1_gitignore_root_and_nested() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    put(r, ".gitignore", b"ignored.md\nskipdir/\n");
    put(r, "ignored.md", b"x");
    put(r, "skipdir/a.md", b"x");
    put(r, "sub/.gitignore", b"local.java\n");
    put(r, "sub/local.java", b"x");
    put(r, "sub/kept.java", b"x");
    put(r, "kept.md", b"x");
    let res = scan(r, &[]).unwrap();
    assert_eq!(paths(&res), vec!["kept.md", "sub/kept.java"]);
}

#[test]
fn req004_ac2_forced_exclusions() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    for dir in [
        "target", "build", ".gradle", ".git", ".rkg", "m/target", "m/build",
    ] {
        put(r, &format!("{dir}/x.java"), b"x");
    }
    put(r, "ok.java", b"x");
    let res = scan(r, &[]).unwrap();
    assert_eq!(paths(&res), vec!["ok.java"]);
}

#[test]
fn req004_ac3_unsupported_extensions() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    put(r, "a.py", b"x");
    put(r, "b.txt", b"x");
    put(r, "noext", b"x");
    put(r, "pom.xml.bak", b"x");
    put(r, "other.xml", b"x");
    put(r, "a.sbt", b"x");
    assert!(scan(r, &[]).unwrap().files.is_empty());
}

fn check_type(name: &str) {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), name, b"contenu");
    let res = scan(d.path(), &[]).unwrap();
    assert_eq!(paths(&res), vec![name.to_string()], "{name}");
}

#[test]
fn req004_ac4_md() {
    check_type("a.md");
}
#[test]
fn req004_ac4_markdown() {
    check_type("a.markdown");
}
#[test]
fn req004_ac4_java() {
    check_type("A.java");
}
#[test]
fn req004_ac4_kt() {
    check_type("A.kt");
}
#[test]
fn req004_ac4_kts() {
    check_type("a.kts");
}
#[test]
fn req004_ac4_groovy() {
    check_type("a.groovy");
}
#[test]
fn req004_ac4_gradle() {
    check_type("a.gradle");
}
#[test]
fn req004_ac4_scala() {
    check_type("A.scala");
}
#[test]
fn req004_ac4_pom() {
    check_type("pom.xml");
}
#[test]
fn req004_ac4_ivy() {
    check_type("ivy.xml");
}
#[test]
fn req004_ac4_build_gradle() {
    check_type("build.gradle");
}
#[test]
fn req004_ac4_build_gradle_kts() {
    check_type("build.gradle.kts");
}
#[test]
fn req004_ac4_settings_gradle() {
    check_type("settings.gradle");
}
#[test]
fn req004_ac4_settings_gradle_kts() {
    check_type("settings.gradle.kts");
}

#[test]
fn req004_non_utf8_ignored_with_warning() {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "bad.java", &[0xff, 0xfe, 0x00, 0xc3]);
    put(d.path(), "good.java", b"ok");
    let res = scan(d.path(), &[]).unwrap();
    assert_eq!(paths(&res), vec!["good.java"]);
    assert_eq!(res.warnings.len(), 1);
    assert!(res.warnings[0].contains("bad.java"));
}

#[test]
fn req005_ac5_module_nearest_build_ancestor() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    put(r, "pom.xml", b"x");
    put(r, "Root.java", b"x");
    put(r, "api/pom.xml", b"x");
    put(r, "api/src/main/java/A.java", b"x");
    put(r, "core/build.gradle.kts", b"x");
    put(r, "core/src/B.kt", b"x");
    put(r, "core/deep/pom.xml", b"x");
    put(r, "core/deep/C.java", b"x");
    put(r, "docs/readme.md", b"x");
    let res = scan(r, &[]).unwrap();
    let m = |p: &str| {
        res.files
            .iter()
            .find(|f| f.path == p)
            .unwrap_or_else(|| panic!("{p}"))
            .module
            .clone()
    };
    assert_eq!(m("Root.java").as_deref(), Some(""));
    assert_eq!(m("pom.xml").as_deref(), Some(""));
    assert_eq!(m("api/src/main/java/A.java").as_deref(), Some("api"));
    assert_eq!(m("api/pom.xml").as_deref(), Some("api"));
    assert_eq!(m("core/src/B.kt").as_deref(), Some("core"));
    assert_eq!(m("core/deep/C.java").as_deref(), Some("core/deep"));
    assert_eq!(m("docs/readme.md").as_deref(), Some(""));
}

#[test]
fn req005_ac5_no_build_file_means_no_module() {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "A.java", b"x");
    let res = scan(d.path(), &[]).unwrap();
    assert_eq!(res.files[0].module, None);
}

#[test]
fn req011_ac6_hash_is_blake3_and_reused_when_size_mtime_unchanged() {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "a.md", b"hello");
    let first = scan(d.path(), &[]).unwrap();
    let f = &first.files[0];
    assert_eq!(f.hash, blake3::hash(b"hello").to_hex().to_string());
    assert_eq!(f.size, 5);

    // Enregistrement connu avec une empreinte factice : réutilisée si taille+mtime identiques.
    let known = vec![FileRecord {
        path: "a.md".into(),
        hash: "sentinelle".into(),
        size: f.size,
        mtime: f.mtime,
        state: FileState::Indexed,
    }];
    let second = scan(d.path(), &known).unwrap();
    assert_eq!(second.files[0].hash, "sentinelle");
    assert!(!second.files[0].rehashed);
    assert!(first.files[0].rehashed);

    // Taille différente : recalcul.
    let known2 = vec![FileRecord {
        size: 99,
        ..known[0].clone()
    }];
    let third = scan(d.path(), &known2).unwrap();
    assert_eq!(third.files[0].hash, f.hash);
    assert!(third.files[0].rehashed);
}

#[test]
fn req011_ac6_touch_rehashes_to_same_hash() {
    // Après un touch, le mtime change : empreinte recalculée mais identique (contenu inchangé).
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "a.md", b"hello");
    let first = scan(d.path(), &[]).unwrap();
    let f = &first.files[0];
    let known = vec![FileRecord {
        path: "a.md".into(),
        hash: f.hash.clone(),
        size: f.size,
        mtime: f.mtime - 5_000,
        state: FileState::Indexed,
    }];
    let again = scan(d.path(), &known).unwrap();
    assert_eq!(again.files[0].hash, f.hash);
    assert!(again.files[0].rehashed);
}

#[test]
fn req004_empty_repo_gives_empty_scan() {
    let d = tempfile::tempdir().unwrap();
    assert!(scan(d.path(), &[]).unwrap().files.is_empty());
}

#[test]
fn req004_missing_root_is_error() {
    assert!(scan(Path::new("/nonexistent/rkg-xyz"), &[]).is_err());
}

#[cfg(unix)]
#[test]
fn req004_unreadable_dir_is_warned_and_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    put(r, "ok.md", b"x");
    put(r, "locked/a.md", b"x");
    let locked = r.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // exécution privilégiée : le dossier reste lisible, test non applicable
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let res = scan(r, &[]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let res = res.unwrap();
    assert_eq!(paths(&res), vec!["ok.md"]);
    assert!(res.warnings.iter().any(|w| w.contains("locked")));
}
