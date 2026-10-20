//! Filtres d'analyse, partie fichier de projet `.nephila.toml` (REQ-037, DEC-045).

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

impl Env {
    /// Dépôt vide nommé `repo`, sans configuration LLM.
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        Env {
            config: tmp.path().join("absent.toml"),
            log: tmp.path().join("mock.log"),
            repo,
            _tmp: tmp,
        }
    }

    fn from_fixture(rel: &str) -> Env {
        let e = Env::new();
        copy_dir(&fixtures().join(rel), &e.repo);
        e
    }

    fn put(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn project(&self, content: &str) {
        self.put(".nephila.toml", content);
    }

    fn command(&self) -> Command {
        let mut c = Command::cargo_bin("nephila").unwrap();
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env("NEPHILA_MOCK_DIR", fixtures().join("mock"));
        c
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.command()
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> (String, String) {
        let o = self.run(args);
        assert!(
            o.status.success(),
            "{args:?} : {}",
            String::from_utf8_lossy(&o.stderr)
        );
        (
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn export(&self) -> String {
        self.ok(&["export", "--output", "-"]).0
    }

    fn status_lines(&self) -> Vec<String> {
        self.ok(&["status"]).0.lines().map(String::from).collect()
    }
}

fn base_repo() -> Env {
    let e = Env::new();
    e.put("README.md", "# Titre\n");
    e.put("docs/legacy/old.md", "# Old\n");
    e.put("docs/garde.md", "# Garde\n");
    e.put("config/app.json", "{\"clef\": 1}\n");
    e.put("package.json", "{\"name\": \"p\"}\n");
    e.put("src/A.java", "package p;\npublic class A {}\n");
    e
}

#[test]
fn req037_ac7_fichier_de_projet_exclut_et_status_reste_a_jour() {
    let e = base_repo();
    e.project("exclude = [\"docs/legacy/\"]\nexclude_types = [\"json\"]\n");
    let (out, _) = e.ok(&["build"]);
    assert!(out.contains("fichiers exclus par les filtres"), "{out}");
    let g = e.export();
    assert!(!g.contains("docs/legacy/old.md"));
    assert!(!g.contains("config/app.json"));
    assert!(g.contains("docs/garde.md"));
    assert!(g.contains("package.json"));
    assert!(!g.contains(".nephila.toml"));
    assert_eq!(e.status_lines()[0], "graphe à jour");
    let (_, err) = e.ok(&["query", "search", "Garde"]);
    assert!(!err.contains("pas à jour"), "{err}");

    // motif retiré : le fichier entre
    e.project("exclude_types = [\"json\"]\n");
    let st = e.status_lines();
    assert_eq!(st[0], "graphe pas à jour");
    assert!(
        st.contains(&"ajouté\tdocs/legacy/old.md".to_string()),
        "{st:?}"
    );
    assert!(st.iter().all(|l| !l.contains(".nephila.toml")));
    e.ok(&["update"]);
    assert!(e.export().contains("docs/legacy/old.md"));
    assert_eq!(e.status_lines()[0], "graphe à jour");

    // type ajouté : les Markdown sortent
    e.project("exclude_types = [\"json\", \"markdown\"]\n");
    let st = e.status_lines();
    assert!(st.contains(&"supprimé\tREADME.md".to_string()), "{st:?}");
    assert!(
        st.contains(&"supprimé\tdocs/garde.md".to_string()),
        "{st:?}"
    );
    e.ok(&["update"]);
    let g = e.export();
    assert!(!g.contains("README.md") && !g.contains("docs/garde.md"));
    assert!(g.contains("src/A.java"));
}

#[test]
fn req037_ac8_reinclusion_dans_le_fichier_de_projet() {
    let e = base_repo();
    e.project("exclude = [\"docs/\", \"!docs/garde.md\"]\n");
    e.ok(&["build"]);
    let g = e.export();
    assert!(g.contains("docs/garde.md"));
    assert!(!g.contains("docs/legacy/old.md"));
    // ordre inverse : l'exclusion du dossier l'emporte
    e.project("exclude = [\"!docs/garde.md\", \"docs/\"]\n");
    e.ok(&["build"]);
    assert!(!e.export().contains("docs/garde.md"));
}

#[test]
fn req037_ac9_les_filtres_nelargissent_pas_req004() {
    let e = Env::new();
    e.put(".gitignore", "gen/\n");
    e.put("gen/A.java", "package g;\npublic class A {}\n");
    e.put("target/B.java", "package t;\npublic class B {}\n");
    e.put("node_modules/x/index.js", "export const x = 1;\n");
    e.put("ok/C.java", "package o;\npublic class C {}\n");
    e.project("exclude = [\"!gen/\", \"!gen/**\", \"!target/\", \"!node_modules/\"]\n");
    e.ok(&["build"]);
    let g = e.export();
    assert!(g.contains("ok/C.java"));
    assert!(!g.contains("gen/A.java"));
    assert!(!g.contains("target/B.java"));
    assert!(!g.contains("node_modules"));
}

const BAD: [(&str, &str); 5] = [
    ("exclud = [\"a\"]\n", "clé inconnue `exclud` (clés permises : exclude, exclude_types, model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries)"),
    (
        "exclude_types = \"json\"\n",
        "clé `exclude_types` : tableau de chaînes attendu, ex. exclude_types = [\"json\"]",
    ),
    (
        "exclude_types = [\"python\"]\n",
        "clé `exclude_types` : type inconnu `python` (types valides : markdown, java, kotlin, groovy, scala, javascript, typescript, sql, xml, json, yaml, maven, ivy, gradle, npm)",
    ),
    ("exclude = [\"a[b\"]\n", "clé `exclude` : motif invalide `a[b` ("),
    ("# c\nexclude = [\n", "TOML invalide ligne 2 : "),
];

#[test]
fn req037_ac12_ac16_fichier_invalide_une_ligne_et_rien_ecrit() {
    for (content, expected) in BAD {
        let e = base_repo();
        e.project(content);
        let shown = format!("{}/.nephila.toml", e.repo.display());
        let want = format!("erreur: {shown} : {expected}");
        for args in [
            vec!["build"],
            vec!["update"],
            vec!["build", "--no-llm"],
            vec!["status"],
            vec!["query", "search", "x"],
        ] {
            let o = e.run(&args);
            assert!(!o.status.success(), "{args:?} {content}");
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            assert_eq!(err.lines().count(), 1, "{args:?} : {err}");
            assert!(err.starts_with(&want), "{args:?} : {err} / {want}");
            assert!(!e.repo.join(".nephila").exists(), "{args:?}");
        }
        assert!(!e.log.exists());
    }
}

#[test]
fn req037_ac12_export_et_summary_ignorent_le_fichier_de_projet() {
    let e = base_repo();
    e.ok(&["build"]);
    e.project("exclud = []\n");
    e.ok(&["export", "--output", "-"]);
    e.ok(&["summary"]);
}

#[test]
fn req037_ac12_fichier_absent_ou_vide_sans_message() {
    let e = base_repo();
    let (out, err) = e.ok(&["build"]);
    assert!(!out.contains("exclus") && !err.contains("exclus"));
    e.project("");
    let (out, err) = e.ok(&["build"]);
    assert!(
        !out.contains("exclus") && !err.contains("exclus"),
        "{out}{err}"
    );
    e.project("# rien\n");
    let (out, _) = e.ok(&["update"]);
    assert_eq!(out.trim(), "graphe à jour");
    e.ok(&["status"]);
}

#[test]
fn req037_ac16_chemin_par_defaut_dot_slash() {
    let e = base_repo();
    e.project("exclud = []\n");
    let o = e
        .command()
        .current_dir(&e.repo)
        .args(["status"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert_eq!(
        err,
        "erreur: ./.nephila.toml : clé inconnue `exclud` (clés permises : exclude, exclude_types, model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries)\n"
    );
    let o = e
        .command()
        .current_dir(&e.repo)
        .args(["query", "search", "x"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&o.stderr).starts_with("erreur: ./.nephila.toml : "));
}

#[test]
fn req037_ac16_lecture_impossible() {
    let e = base_repo();
    fs::create_dir(e.repo.join(".nephila.toml")).unwrap();
    let o = e.run(&["status"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        err.contains(".nephila.toml : lecture impossible : "),
        "{err}"
    );
    assert_eq!(err.lines().count(), 1);
}

/// Dépôt `repo` : graphe de référence d'un dépôt dont on a retiré `removed` (ou d'un dépôt avec
/// filtre), comparé par `export --output -`.
fn graph_without(fixture: &str, remove: &[&str]) -> String {
    let e = Env::from_fixture(fixture);
    for r in remove {
        fs::remove_file(e.repo.join(r)).unwrap();
    }
    e.ok(&["build"]);
    e.export()
}

#[test]
fn req037_ac5_fichier_de_build_exclu_maven() {
    let reference = graph_without(
        "maven3/repo",
        &["api/pom.xml", "core/pom.xml", "store/pom.xml"],
    );
    let e = Env::from_fixture("maven3/repo");
    e.project("exclude_types = [\"maven\"]\n");
    e.ok(&["build"]);
    assert_eq!(e.export(), reference);
    assert!(!e.export().contains("depends_on"));
    // par update après un build sans filtre
    let e = Env::from_fixture("maven3/repo");
    e.ok(&["build"]);
    assert!(e.export().contains("\"module\""));
    e.project("exclude_types = [\"maven\"]\n");
    let (out, _) = e.ok(&["update"]);
    assert!(out.starts_with("update terminé"), "{out}");
    assert_eq!(e.export(), reference);
}

#[test]
fn req037_ac5_npm_exclu_pom_et_package_a_la_racine() {
    let make = |e: &Env| {
        e.put(
            "pom.xml",
            "<project><groupId>com.acme</groupId><artifactId>app</artifactId><version>1</version></project>\n",
        );
        e.put(
            "package.json",
            "{\"name\": \"app-front\", \"version\": \"1\"}\n",
        );
        e.put(
            "src/main/java/com/acme/A.java",
            "package com.acme;\npublic class A {}\n",
        );
        e.put(
            "web/src/main/B.java",
            "package web.src.main;\npublic class B {}\n",
        );
    };
    let r = Env::new();
    make(&r);
    fs::remove_file(r.repo.join("package.json")).unwrap();
    r.ok(&["build"]);
    let reference = r.export();
    let e = Env::new();
    make(&e);
    e.project("exclude_types = [\"npm\"]\n");
    e.ok(&["build"]);
    let g = e.export();
    assert_eq!(g, reference);
    assert!(!g.contains("app-front"));
    assert!(g.contains("module:com.acme:app"));
    // via update après un build complet
    let e = Env::new();
    make(&e);
    e.ok(&["build"]);
    assert!(e.export().contains("app-front"));
    e.project("exclude_types = [\"npm\"]\n");
    e.ok(&["update"]);
    assert_eq!(e.export(), reference);
}

#[test]
fn req037_ac5_settings_gradle_exclu() {
    let reference = graph_without("gradle/groovy", &["settings.gradle"]);
    let e = Env::from_fixture("gradle/groovy");
    e.project("exclude = [\"settings.gradle\"]\n");
    e.ok(&["build"]);
    assert_eq!(e.export(), reference);
    let e = Env::from_fixture("gradle/groovy");
    e.ok(&["build"]);
    assert_ne!(e.export(), reference);
    e.project("exclude = [\"settings.gradle\"]\n");
    e.ok(&["update"]);
    assert_eq!(e.export(), reference);
}

#[test]
fn req037_ac14_bilan_compte_et_segments() {
    let e = base_repo();
    e.project("exclude = [\"absent/\"]\n");
    let (out, _) = e.ok(&["build"]);
    assert!(
        out.trim_end()
            .ends_with(", enrichissement : désactivé, 0 fichiers exclus par les filtres"),
        "{out}"
    );
    e.project("exclude = [\"docs/legacy/\"]\n");
    e.put("README.md", "# Autre\n");
    let (out, _) = e.ok(&["update"]);
    assert!(
        out.trim_end()
            .ends_with(", 1 fichiers exclus par les filtres"),
        "{out}"
    );
    // aucun filtre : aucun « exclus »
    let e = base_repo();
    let (out, _) = e.ok(&["build"]);
    assert!(!out.contains("exclus"));
    let (out, _) = e.ok(&["update"]);
    assert_eq!(out, "graphe à jour\n");
}

#[test]
fn req037_ac11_trace_parcours_avec_fichier_de_projet() {
    let e = base_repo();
    e.ok(&["build"]);
    e.project("exclude = [\"docs/legacy/\"]\n");
    let (_, err) = e.ok(&["update", "--verbose"]);
    let line = err.lines().find(|l| l.contains("parcours")).unwrap();
    assert!(
        line.ends_with("0 modifiés, 1 supprimés, 0 en attente (filtres : 1 motifs, types aucun, 1 fichiers exclus)")
            || line.ends_with(" (filtres : 1 motifs, types aucun, 1 fichiers exclus)"),
        "{line}"
    );
    assert_eq!(err.lines().filter(|l| l.contains("parcours")).count(), 1);
    // build : même suffixe ; sans filtre, trace inchangée
    let (_, err) = e.ok(&["build", "--verbose"]);
    assert!(
        err.contains("parcours: 5 fichiers (filtres : 1 motifs, types aucun, 1 fichiers exclus)"),
        "{err}"
    );
    let e = base_repo();
    let (_, err) = e.ok(&["build", "--verbose"]);
    assert!(err.contains("parcours: 6 fichiers\n"), "{err}");
}

#[test]
fn req037_ac8_no_llm_et_fichier_de_projet_sans_appel_llm() {
    let e = base_repo();
    fs::write(&e.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    e.project("exclude = [\"docs/\"]\nexclude_types = [\"yaml\"]\n");
    let (out, _) = e.ok(&["build", "--no-llm"]);
    assert!(out.contains("enrichissement : désactivé"));
    assert!(!e.log.exists(), "appel LLM inattendu");
    assert!(!e.export().contains("docs/garde.md"));
}

#[test]
fn req037_fichier_de_projet_sans_config_llm_n_active_pas_le_llm() {
    let e = base_repo();
    e.project("exclude_types = [\"json\"]\n");
    let (out, _) = e.ok(&["build"]);
    assert!(out.contains("enrichissement : désactivé"), "{out}");
}

// ---- Options `--exclude` et `--exclude-type` (T-075) ----

const WARN: &str = "avertissement: --exclude et --exclude-type valent pour cette commande seulement : status et update sans option ne les appliquent pas (filtre durable : .nephila.toml)";
const TYPES: &str = "markdown, java, kotlin, groovy, scala, javascript, typescript, sql, xml, json, yaml, maven, ivy, gradle, npm";

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn req037_ac1_ac13_aide_de_build_et_update() {
    let e = Env::new();
    for cmd in ["build", "update"] {
        let (out, _) = e.ok(&[cmd, "--help"]);
        let h = norm(&out);
        for needle in [
            "--exclude <MOTIF> Exclut de l'analyse les fichiers qui correspondent au motif (syntaxe .gitignore, relatif à la racine du dépôt ; répétable ; !motif réinclut un fichier exclu par un motif précédent)",
            "--exclude-type <TYPES>",
            &format!("Types : {TYPES}"),
            "Filtres durables (lus aussi par status et query) : fichier .nephila.toml à la racine du dépôt",
            "exclude = [\"docs/legacy/\", \"*.min.js\"]",
            "exclude_types = [\"json\"]",
            "Filtres : --exclude et --exclude-type valent pour cette commande seulement.",
            "Exemple : nephila build --exclude '*.min.js' --exclude-type json,yaml",
        ] {
            assert!(h.contains(needle), "{cmd} : {needle}\n{h}");
        }
        assert!(!h.contains("possible values"), "{h}");
    }
    let o = e.run(&["status", "--exclude", "x"]);
    assert_eq!(o.status.code(), Some(2));
}

fn web_repo() -> Env {
    let e = base_repo();
    e.put("web/dist/app.min.js", "export const m = 1;\n");
    e.put("web/src/app.js", "export const a = 1;\n");
    e.put("src/gen/Gen.java", "package g;\npublic class Gen {}\n");
    e.put(
        "tools/src/gen/Keep.java",
        "package k;\npublic class Keep {}\n",
    );
    e
}

#[test]
fn req037_ac3_motifs_en_ligne_de_commande() {
    let e = web_repo();
    let (out, err) = e.ok(&[
        "build",
        "--exclude",
        "docs/legacy/",
        "--exclude",
        "*.min.js",
        "--exclude",
        "/src/gen/",
    ]);
    assert!(
        out.starts_with("build terminé : 7 fichiers traités"),
        "{out}"
    );
    assert!(err.contains(WARN));
    let g = e.export();
    for gone in [
        "docs/legacy/old.md",
        "web/dist/app.min.js",
        "src/gen/Gen.java",
    ] {
        assert!(!g.contains(gone), "{gone}");
    }
    assert!(g.contains("web/src/app.js"));
    assert!(g.contains("tools/src/gen/Keep.java"));
}

#[test]
fn req037_ac4_types_en_ligne_de_commande() {
    let e = Env::new();
    for (p, c) in [
        ("a.md", "# A\n"),
        ("A.java", "package p;\npublic class A {}\n"),
        ("K.kt", "package p\nclass K\n"),
        ("G.groovy", "package p\nclass G {}\n"),
        ("S.scala", "package p\nclass S\n"),
        ("j.js", "export const j = 1;\n"),
        ("t.ts", "export const t = 1;\n"),
        ("sub/x.d.ts", "export declare const d: number;\n"),
        ("sub/y.tsx", "export const y = 1;\n"),
        ("db/s.sql", "CREATE TABLE tbl (id INT);\n"),
        ("beans.xml", "<beans><bean id=\"b\"/></beans>\n"),
        ("c.json", "{\"clef\": 1}\n"),
        ("y.yaml", "cle: 1\n"),
        (
            "pom.xml",
            "<project><groupId>g</groupId><artifactId>a</artifactId><version>1</version></project>\n",
        ),
        ("ivy.xml", "<ivy-module version=\"2.0\"><info organisation=\"o\" module=\"m\"/></ivy-module>\n"),
        ("build.gradle.kts", "plugins { }\n"),
        ("package.json", "{\"name\": \"p\"}\n"),
    ] {
        e.put(p, c);
    }
    e.ok(&[
        "build",
        "--exclude-type",
        "json,yaml",
        "--exclude-type",
        "sql",
    ]);
    let g = e.export();
    for gone in ["c.json", "y.yaml", "s.sql", "\"clef\"", "tbl"] {
        assert!(!g.contains(gone), "{gone}");
    }
    for kept in [
        "package.json",
        "beans.xml",
        "a.md",
        "A.java",
        "j.js",
        "t.ts",
    ] {
        assert!(g.contains(kept), "{kept}");
    }
    e.ok(&["build", "--exclude-type", "typescript"]);
    let g = e.export();
    for gone in ["t.ts", "x.d.ts", "y.tsx"] {
        assert!(!g.contains(gone), "{gone}");
    }
    assert!(g.contains("j.js"));
}

#[test]
fn req037_ac5_options_fichier_de_build_exclu() {
    let reference = graph_without(
        "maven3/repo",
        &["api/pom.xml", "core/pom.xml", "store/pom.xml"],
    );
    let e = Env::from_fixture("maven3/repo");
    e.ok(&["build", "--exclude-type", "maven"]);
    assert_eq!(e.export(), reference);

    let reference = graph_without("gradle/groovy", &["settings.gradle"]);
    let e = Env::from_fixture("gradle/groovy");
    e.ok(&["build", "--exclude", "settings.gradle"]);
    assert_eq!(e.export(), reference);

    let make = |e: &Env| {
        e.put(
            "pom.xml",
            "<project><groupId>com.acme</groupId><artifactId>app</artifactId><version>1</version></project>\n",
        );
        e.put(
            "package.json",
            "{\"name\": \"app-front\", \"version\": \"1\"}\n",
        );
        e.put(
            "src/main/java/com/acme/A.java",
            "package com.acme;\npublic class A {}\n",
        );
        e.put(
            "web/src/main/B.java",
            "package web.src.main;\npublic class B {}\n",
        );
    };
    let r = Env::new();
    make(&r);
    fs::remove_file(r.repo.join("package.json")).unwrap();
    r.ok(&["build"]);
    let e = Env::new();
    make(&e);
    e.ok(&["build", "--exclude-type", "npm"]);
    assert_eq!(e.export(), r.export());
}

fn log_lines(e: &Env) -> usize {
    fs::read_to_string(&e.log).map_or(0, |s| s.lines().count())
}

#[test]
fn req037_ac6_options_transitoires_et_update_sans_appel_llm() {
    let e = base_repo();
    fs::write(&e.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    e.ok(&["build", "--exclude-type", "json"]);
    let st = e.status_lines();
    assert_eq!(st[0], "graphe pas à jour");
    assert!(
        st.contains(&"ajouté\tconfig/app.json".to_string()),
        "{st:?}"
    );
    let (_, err) = e.ok(&["query", "search", "Garde"]);
    assert!(err.contains("pas à jour"), "{err}");
    e.ok(&["update"]);
    assert!(e.export().contains("\"clef\""));
    let before = log_lines(&e);
    let (out, err) = e.ok(&["update", "--exclude-type", "json"]);
    assert!(
        out.starts_with("update terminé : 0 fichiers traités"),
        "{out}"
    );
    assert!(err.contains(WARN));
    let g = e.export();
    assert!(!g.contains("config/app.json") && !g.contains("\"clef\""));
    assert!(g.contains("docs/garde.md"));
    assert_eq!(log_lines(&e), before, "appel LLM inattendu");
}

#[test]
fn req037_ac8_combinaison_fichier_et_options() {
    let e = base_repo();
    e.put("data/q.sql", "CREATE TABLE tbl (id INT);\n");
    e.put("data/y.yaml", "cle: 1\n");
    e.project("exclude = [\"docs/\"]\nexclude_types = [\"yaml\"]\n");
    e.ok(&[
        "build",
        "--exclude",
        "!docs/garde.md",
        "--exclude-type",
        "sql",
    ]);
    let g = e.export();
    assert!(g.contains("docs/garde.md"));
    assert!(!g.contains("docs/legacy/old.md"));
    assert!(!g.contains("y.yaml") && !g.contains("q.sql"));
}

#[test]
fn req037_ac9_options_nelargissent_pas_req004() {
    let e = Env::new();
    e.put(".gitignore", "gen/\n");
    e.put("gen/A.java", "package g;\npublic class A {}\n");
    e.put("node_modules/x/index.js", "export const x = 1;\n");
    e.put("ok/C.java", "package o;\npublic class C {}\n");
    e.ok(&[
        "build",
        "--exclude",
        "!gen/",
        "--exclude",
        "!gen/**",
        "--exclude",
        "!node_modules/",
    ]);
    let g = e.export();
    assert!(g.contains("ok/C.java"));
    assert!(!g.contains("gen/A.java") && !g.contains("node_modules"));
}

#[test]
fn req037_ac10_ac17_erreurs_d_usage_des_options() {
    let e = base_repo();
    fs::write(&e.config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let cases: [(&[&str], String); 3] = [
        (
            &["build", "--exclude-type", "python"],
            format!("type inconnu `python` (types valides : {TYPES})"),
        ),
        (
            &["update", "--exclude-type", "json,,"],
            "type vide dans la liste (séparer les types par une seule virgule, ex. json,yaml)"
                .to_string(),
        ),
        (
            &["build", "--exclude", "a[b"],
            "motif invalide `a[b`".to_string(),
        ),
    ];
    for (args, needle) in cases {
        let o = e.run(args);
        let err = String::from_utf8_lossy(&o.stderr).into_owned();
        assert!(!o.status.success(), "{args:?}");
        assert_eq!(o.status.code(), Some(2));
        assert!(err.contains(&needle), "{args:?} : {err}");
        assert!(err.contains(args[args.len() - 1]), "{err}");
        assert!(o.stdout.is_empty());
        assert!(!o.stdout.contains(&0x1B) && !o.stderr.contains(&0x1B));
        assert!(!err.contains("valent pour cette commande"));
    }
    assert!(!e.log.exists());
    assert!(!e.repo.join(".nephila").exists());
}

#[test]
fn req037_ac11_ac26_8_trace_avec_options() {
    let e = base_repo();
    let (_, err) = e.ok(&["build", "--verbose", "--exclude-type", "json"]);
    assert!(
        err.contains("parcours: 5 fichiers (filtres : 0 motifs, types json, 1 fichiers exclus)"),
        "{err}"
    );
    let e = base_repo();
    e.project("exclude = [\"docs/\"]\nexclude_types = [\"yaml\"]\n");
    e.put("q.sql", "CREATE TABLE tbl (id INT);\n");
    e.put("y.yaml", "cle: 1\n");
    let (out, err) = e.ok(&[
        "build",
        "--verbose",
        "--exclude",
        "!docs/garde.md",
        "--exclude-type",
        "sql,yaml",
    ]);
    let line = err.lines().find(|l| l.contains("parcours")).unwrap();
    assert!(
        line.ends_with(" (filtres : 2 motifs, types sql,yaml, 3 fichiers exclus)"),
        "{line}"
    );
    assert_eq!(err.lines().filter(|l| l.contains("parcours")).count(), 1);
    assert!(
        out.trim_end()
            .ends_with(", 3 fichiers exclus par les filtres"),
        "{out}"
    );
}

#[test]
fn req037_ac14_ac15_bilan_et_avertissement_une_seule_fois() {
    let e = base_repo();
    let (out, err) = e.ok(&["build", "--exclude-type", "json"]);
    assert!(
        out.trim_end()
            .ends_with(", enrichissement : désactivé, 1 fichiers exclus par les filtres"),
        "{out}"
    );
    assert_eq!(err.matches(WARN).count(), 1, "{err}");
    // après la ligne de bilan : stdout et stderr sont distincts, on vérifie l'unicité
    let (out, err) = e.ok(&["update", "--exclude", "*.min.js"]);
    assert_eq!(err.matches(WARN).count(), 1, "{err}");
    assert!(
        out.starts_with("update terminé") || out == "graphe à jour\n",
        "{out}"
    );
    // update sans changement avec option
    let e = base_repo();
    e.ok(&["build"]);
    let (out, err) = e.ok(&["update", "--exclude-type", "sql"]);
    assert_eq!(out, "graphe à jour\n");
    assert_eq!(err.matches(WARN).count(), 1, "{err}");
    // filtre du seul fichier de projet : pas d'avertissement
    let e = base_repo();
    e.project("exclude_types = [\"json\"]\n");
    let (_, err) = e.ok(&["build"]);
    assert!(!err.contains("valent pour cette commande"), "{err}");
    let (_, err) = e.ok(&["update"]);
    assert!(!err.contains("valent pour cette commande"), "{err}");
}

#[test]
fn req037_ac15_pas_d_avertissement_sur_erreur_avant_bilan() {
    let e = base_repo();
    e.project("exclud = []\n");
    let o = e.run(&["build", "--exclude-type", "json"]);
    assert!(!o.status.success());
    assert!(!String::from_utf8_lossy(&o.stderr).contains("valent pour cette commande"));
}
