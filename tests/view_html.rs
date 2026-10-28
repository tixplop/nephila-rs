//! `export --format html` (REQ-025, AC-025-1 à 025-4, AC-018-4).

use assert_cmd::cargo::cargo_bin;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    mock: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven3");
        let repo = tmp.path().join("repo");
        copy_dir(&fx.join("repo"), &repo);
        let mock = tmp.path().join("mock");
        copy_dir(&fx.join("mock"), &mock);
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\n",
        )
        .unwrap();
        Env {
            _tmp: tmp,
            repo,
            config,
            mock,
        }
    }

    fn built() -> Env {
        let env = Env::new();
        env.build();
        env
    }

    fn build(&self) {
        let out = Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .arg("build")
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    fn read_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .current_dir(cwd)
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", self.repo.join("absent.toml"))
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    }

    fn export_json(&self) -> Value {
        let cwd = tempfile::tempdir().unwrap();
        let out = self.read_in(cwd.path(), &["export", "--output", "-"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const OPEN: &str = "<script type=\"application/json\" id=\"nephila-data\">";

/// Découpe la page : (avant le bloc, contenu brut du bloc, après le bloc).
fn split_block(html: &str) -> (&str, &str, &str) {
    let start = html.find(OPEN).expect("bloc nephila-data");
    let body = start + OPEN.len();
    let end = body + html[body..].find("</script>").expect("fin du bloc");
    (&html[..start], &html[body..end], &html[end..])
}

fn block(html: &str) -> Value {
    serde_json::from_str(split_block(html).1).expect("bloc JSON valide")
}

/// Décodage DEC-028 : (nœuds, liens) sous la forme de l'export.
fn decode(data: &Value) -> (Vec<Value>, Vec<Value>) {
    assert_eq!(data["version"], 2);
    let nk = data["nodeKinds"].as_array().unwrap();
    let ek = data["edgeKinds"].as_array().unwrap();
    let nodes: Vec<Value> = data["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            json!({"id": n[0], "kind": nk[n[1].as_u64().unwrap() as usize],
                   "short": n[2], "qualified": n[3], "file": n[4],
                   "subkind": n[5], "summary": n[6], "community": n[7]})
        })
        .collect();
    let edges: Vec<Value> = data["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let id = |i: &Value| nodes[i.as_u64().unwrap() as usize]["id"].clone();
            json!({"source": id(&e[0]), "target": id(&e[1]),
                   "kind": ek[e[2].as_u64().unwrap() as usize]})
        })
        .collect();
    (nodes, edges)
}

fn sorted(v: &Value, key: impl Fn(&Value) -> String) -> Vec<Value> {
    let mut a = v.as_array().unwrap().clone();
    a.sort_by_key(key);
    a
}

fn assert_same_graph(html: &str, export: &Value) {
    let (nodes, edges) = decode(&block(html));
    let by_id = |n: &Value| n["id"].as_str().unwrap().to_string();
    let by_edge = |e: &Value| format!("{} {} {}", e["source"], e["target"], e["kind"]);
    assert_eq!(nodes, sorted(&export["nodes"], by_id));
    assert_eq!(edges, sorted(&export["edges"], by_edge));
}

#[test]
fn req025_ac1_default_html_file_is_standalone() {
    let env = Env::built();
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export", "--format", "html"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "repo.html\n");
    let html = fs::read_to_string(cwd.path().join("repo.html")).unwrap();
    assert!(html.starts_with("<!DOCTYPE html>"));
    let (before, _, after) = split_block(&html);
    let outside = format!("{before}{after}");
    assert!(outside.contains("<style>"));
    assert!(outside.contains("<script>"));
    for bad in [
        "<script src",
        "<link",
        "@import",
        "http://",
        "https://",
        "fetch(",
        "import(",
    ] {
        assert!(!outside.contains(bad), "{bad} présent");
    }
}

#[test]
fn req025_ac2_block_decodes_to_the_json_export_and_output_is_honoured() {
    let env = Env::built();
    let export = env.export_json();
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(
        cwd.path(),
        &["export", "--format", "html", "--output", "vue.html"],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "vue.html\n");
    let html = fs::read_to_string(cwd.path().join("vue.html")).unwrap();
    assert_same_graph(&html, &export);
    let data = block(&html);
    assert_eq!(data["name"], "repo");
    assert!(data.get("unresolved").is_none());
    // Même contenu sur stdout.
    let out = env.read_in(cwd.path(), &["export", "--format", "html", "--output", "-"]);
    assert_eq!(text(&out.stdout).trim_end(), html.trim_end());
}

#[test]
fn req025_ac3_hostile_summary_is_escaped_and_round_trips() {
    let env = Env::new();
    let hostile = "</script><script>alert(1)</script> & \u{2028} fin";
    let f = env
        .mock
        .join("core/src/main/java/com/acme/core/OrderService.java.json");
    let fx = json!({"version": 2, "summaries": [
        {"node": "type:com.acme.core.OrderService", "summary": hostile}], "concepts": []});
    fs::write(&f, fx.to_string()).unwrap();
    env.build();
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export", "--format", "html"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let html = fs::read_to_string(cwd.path().join("repo.html")).unwrap();
    let raw = split_block(&html).1;
    for bad in ["</script", "<", ">", "&", "\u{2028}", "\u{2029}"] {
        assert!(!raw.contains(bad), "{bad:?} brut dans le bloc");
    }
    let (nodes, _) = decode(&block(&html));
    let svc = nodes
        .iter()
        .find(|n| n["id"] == "type:com.acme.core.OrderService")
        .unwrap();
    assert_eq!(svc["summary"], hostile);
}

#[test]
fn req025_ac4_unknown_format_is_a_usage_error_and_creates_no_file() {
    let env = Env::built();
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export", "--format", "xml"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(text(&out.stderr).contains("xml"), "{}", text(&out.stderr));
    assert_eq!(fs::read_dir(cwd.path()).unwrap().count(), 0);
}

#[test]
fn req018_ac4_html_export_without_graph_invites_to_build() {
    let env = Env::new();
    let cwd = tempfile::tempdir().unwrap();
    for args in [
        vec!["export", "--format", "html"],
        vec!["export", "--format", "html", "--output", "-"],
        vec!["export", "--format", "html", "--output", "out.html"],
    ] {
        let out = env.read_in(cwd.path(), &args);
        assert!(!out.status.success());
        assert!(
            text(&out.stderr).contains("nephila build"),
            "{}",
            text(&out.stderr)
        );
        assert!(out.stdout.is_empty());
    }
    assert_eq!(fs::read_dir(cwd.path()).unwrap().count(), 0);
    assert!(!env.repo.join(".nephila").exists());
}

/// La fixture des tests JS est le bloc de données décodé du dépôt exemple.
#[test]
fn view_fixture_matches_current_export() {
    let env = Env::built();
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export", "--format", "html"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let html = fs::read_to_string(cwd.path().join("repo.html")).unwrap();
    let mut current = block(&html);
    current["name"] = json!("maven3");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/view/fixtures/maven3.json");
    let want = format!("{}\n", serde_json::to_string(&current).unwrap());
    if std::env::var("NEPHILA_UPDATE_VIEW_FIXTURE").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &want).unwrap();
        return;
    }
    let have = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "fixture tests/view/fixtures/maven3.json obsolète : relancer avec \
         NEPHILA_UPDATE_VIEW_FIXTURE=1 cargo test view_fixture"
    );
}

fn exported_page() -> String {
    let env = Env::built();
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export", "--format", "html"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    fs::read_to_string(cwd.path().join("repo.html")).unwrap()
}

#[test]
fn req039_ac6_page_has_expand_all_button() {
    let html = exported_page();
    assert!(html.contains("<button id=\"expand-all\" type=\"button\""));
    assert!(html.contains(">Tout étendre</button>"));
}

#[test]
fn req039_ac7_button_row_order_and_attributes() {
    let html = exported_page();
    let (before, _, after) = split_block(&html);
    let page = format!("{before}{after}");
    let pos = |needle: &str| {
        page.find(needle)
            .unwrap_or_else(|| panic!("{needle} absent"))
    };
    let (r, x, e, m) = (
        pos("id=\"reset\""),
        pos("id=\"expand-all\""),
        pos("id=\"expand\""),
        pos("id=\"message\""),
    );
    assert!(r < x && x < e && e < m, "ordre des éléments");
    let between = &page[r..m];
    assert!(!between.contains("<section") && !between.contains("</section>"));
    let start = page[..x].rfind('<').unwrap();
    let tag = &page[start..start + page[start..].find('>').unwrap() + 1];
    assert!(tag.starts_with("<button"));
    assert!(tag.contains("id=\"expand-all\""));
    assert!(tag.contains("type=\"button\""));
    assert!(tag.contains(
        "title=\"Affiche tout le graphe, au plus 5000 nœuds (ceux de plus haut degré)\""
    ));
    assert!(!tag.contains("disabled") && !tag.contains("aria-pressed"));
    let ms = page[..m].rfind('<').unwrap();
    let mtag = &page[ms..ms + page[ms..].find('>').unwrap() + 1];
    assert!(mtag.contains("aria-live=\"polite\""));
    let css = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/view/style.css"))
        .unwrap();
    assert!(!css.contains("outline: none") && !css.contains("outline:none"));
}

/// AC-150-2 : numéro de communauté par nœud et `communities` = [nom, taille] de l'export.
#[test]
fn req150_ac2_block_carries_the_communities_of_the_export() {
    let env = Env::built();
    let export = env.export_json();
    let html = exported_page_of(&env);
    let data = block(&html);
    let want: Vec<Value> = export["communities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| json!([c["label"], c["size"]]))
        .collect();
    assert!(!want.is_empty(), "le dépôt exemple a des communautés");
    assert_eq!(data["communities"], Value::Array(want));
    let (nodes, _) = decode(&data);
    for n in &nodes {
        let src = export["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == n["id"])
            .unwrap();
        assert_eq!(n["community"], src["community"], "{}", n["id"]);
        assert!(n["community"].is_u64());
    }
    // Les nœuds portent le numéro en dernier élément.
    assert_eq!(data["nodes"][0].as_array().unwrap().len(), 8);
}

fn exported_page_of(env: &Env) -> String {
    let cwd = tempfile::tempdir().unwrap();
    let out = env.read_in(cwd.path(), &["export", "--format", "html"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    fs::read_to_string(cwd.path().join("repo.html")).unwrap()
}

/// AC-150-5 et AC-150-6 : éléments exacts, ordre dans la page, pas de `outline: none`.
#[test]
fn req150_ac5_ac6_coloring_section_elements_and_order() {
    let html = exported_page();
    let (before, _, after) = split_block(&html);
    let page = format!("{before}{after}");
    for needle in [
        "<section id=\"coloring\">",
        "<h2 id=\"color-title\">Couleur</h2>",
        "role=\"radiogroup\" aria-labelledby=\"color-title\"",
        "<input type=\"radio\" name=\"color\" id=\"color-kind\" value=\"kind\" checked>genre",
        "<input type=\"radio\" name=\"color\" id=\"color-community\" value=\"community\" aria-describedby=\"color-note\">communauté",
        "<div id=\"color-note\"></div>",
        "<ul id=\"legend\" aria-label=\"Légende des communautés\" hidden></ul>",
    ] {
        assert!(page.contains(needle), "{needle} absent");
    }
    let (m, c, g) = (
        page.find("id=\"message\"").unwrap(),
        page.find("id=\"coloring\"").unwrap(),
        page.find("Genres de nœuds").unwrap(),
    );
    assert!(m < c && c < g, "ordre message < coloring < Genres de nœuds");
    for bad in ["http://", "https://", "fetch(", "import(", "innerHTML"] {
        let app = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/view/app.js"))
            .unwrap();
        assert!(!app.contains(bad), "{bad} dans app.js");
    }
    let css = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/view/style.css"))
        .unwrap();
    assert!(!css.contains("outline: none") && !css.contains("outline:none"));
}
