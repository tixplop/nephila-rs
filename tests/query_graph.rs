//! `query neighbors` et `query path` (REQ-014, REQ-015, REQ-017 AC-3).

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

fn decl(kind: &str, name: &str, sub: Option<&str>) -> String {
    match sub {
        Some(s) => format!("{{\"kind\":\"{kind}\",\"name\":\"{name}\",\"subkind\":\"{s}\"}}"),
        None => format!("{{\"kind\":\"{kind}\",\"name\":\"{name}\"}}"),
    }
}

fn class(name: &str) -> String {
    decl("type", name, Some("class"))
}

fn method(name: &str) -> String {
    decl("method", name, None)
}

fn rf(source: &str, kind: &str, target: &str) -> String {
    format!("{{\"source\":\"{source}\",\"kind\":\"{kind}\",\"target\":\"{target}\"}}")
}

impl Env {
    fn empty() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        Env {
            repo,
            mock,
            config,
            _tmp: tmp,
        }
    }

    fn file(&self, path: &str, declares: &[String], refs: &[String]) {
        let p = self.repo.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "// source\n").unwrap();
        let m = self.mock.join(format!("{path}.json"));
        fs::create_dir_all(m.parent().unwrap()).unwrap();
        fs::write(
            m,
            format!(
                "{{\"version\":1,\"declares\":[{}],\"references\":[{}]}}",
                declares.join(","),
                refs.join(",")
            ),
        )
        .unwrap();
    }

    fn sources(&self) {
        self.file(
            "core/OrderService.java",
            &[
                class("com.acme.core.OrderService"),
                method("com.acme.core.OrderService.place"),
            ],
            &[
                rf(
                    "type:com.acme.core.OrderService",
                    "imports",
                    "com.acme.core.OrderRepository",
                ),
                rf(
                    "method:com.acme.core.OrderService.place",
                    "calls",
                    "OrderRepository.save",
                ),
            ],
        );
        self.file(
            "core/OrderRepository.java",
            &[
                decl("type", "com.acme.core.OrderRepository", Some("interface")),
                method("com.acme.core.OrderRepository.save"),
            ],
            &[],
        );
        self.file(
            "api/ApiController.java",
            &[
                class("com.acme.api.ApiController"),
                method("com.acme.api.ApiController.create"),
            ],
            &[rf(
                "method:com.acme.api.ApiController.create",
                "calls",
                "OrderService.place",
            )],
        );
        self.file("iso/Island.java", &[class("com.acme.iso.Island")], &[]);
        let mut d = Vec::new();
        for i in 0..12 {
            d.push(class(&format!("com.acme.many.Repo{i:02}")));
            d.push(method(&format!("com.acme.many.Repo{i:02}.save")));
        }
        self.file("many/Repos.java", &d, &[]);
        let mut d = vec![class("com.acme.hub.Hub")];
        let mut r = Vec::new();
        for i in 0..300 {
            d.push(class(&format!("com.acme.hub.T{i:03}")));
            r.push(rf(
                "type:com.acme.hub.Hub",
                "imports",
                &format!("com.acme.hub.T{i:03}"),
            ));
        }
        self.file("hub/Hub.java", &d, &r);
    }

    fn built() -> Env {
        let env = Env::empty();
        env.sources();
        let out = env.run(&["build"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        env
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("rkg"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env_remove("RKG_MOCK_LOG")
            .env_remove("RKG_MOCK_DELAY_MS")
            .env_remove("RKG_MOCK_FAIL_AFTER")
            .output()
            .unwrap()
    }

    fn neighbors(&self, args: &[&str]) -> Output {
        let mut a = vec!["query", "neighbors"];
        a.extend_from_slice(args);
        self.run(&a)
    }

    fn path(&self, args: &[&str]) -> Output {
        let mut a = vec!["query", "path"];
        a.extend_from_slice(args);
        self.run(&a)
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn lines(o: &Output) -> Vec<String> {
    text(&o.stdout).lines().map(str::to_string).collect()
}

fn pos(l: &[String], pat: &str) -> usize {
    l.iter()
        .position(|x| x.contains(pat))
        .unwrap_or_else(|| panic!("« {pat} » absent : {l:#?}"))
}

#[test]
fn req014_ac1_neighbors_split_uses_and_used_by_with_edge_kinds() {
    let env = Env::built();
    let out = env.neighbors(&["OrderService"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    let l = lines(&out);
    let uses = pos(&l, "utilise");
    let used_by = pos(&l, "utilisé par");
    assert!(uses < used_by, "{l:#?}");
    let imp = pos(&l, "com.acme.core.OrderRepository ");
    assert!(
        uses < imp && imp < used_by && l[imp].contains("imports"),
        "{l:#?}"
    );
    let call = pos(&l, "com.acme.core.OrderRepository.save");
    assert!(
        uses < call && call < used_by && l[call].contains("calls"),
        "{l:#?}"
    );
    let by = pos(&l, "com.acme.api.ApiController.create");
    assert!(by > used_by && l[by].contains("calls"), "{l:#?}");
    // Les méthodes contenues ne sont pas des voisins du type.
    assert!(
        !l.iter().any(|x| x.contains("OrderService.place")),
        "{l:#?}"
    );
}

#[test]
fn req014_ac1_neighbors_json_shape() {
    let env = Env::built();
    let out = env.neighbors(&["OrderService", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["remaining"], 0);
    let uses = v["uses"].as_array().unwrap();
    assert_eq!(uses.len(), 2, "{v}");
    assert!(uses
        .iter()
        .any(|e| e["kind"] == "imports" && e["qualified"] == "com.acme.core.OrderRepository"));
    let by = v["used_by"].as_array().unwrap();
    assert_eq!(by.len(), 1, "{v}");
    assert_eq!(by[0]["kind"], "calls");
    assert_eq!(by[0]["qualified"], "com.acme.api.ApiController.create");
}

#[test]
fn req014_ac2_depth_defaults_to_one_and_depth_two_adds_second_level() {
    let env = Env::built();
    let l = lines(&env.neighbors(&["ApiController"]));
    assert!(l.iter().any(|x| x.contains("OrderService.place")), "{l:#?}");
    assert!(
        !l.iter().any(|x| x.contains("OrderRepository.save")),
        "{l:#?}"
    );
    let out = env.neighbors(&["ApiController", "--depth", "2"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let l = lines(&out);
    assert!(l.iter().any(|x| x.contains("OrderService.place")), "{l:#?}");
    assert!(
        l.iter().any(|x| x.contains("OrderRepository.save")),
        "{l:#?}"
    );
    let out = env.neighbors(&["ApiController", "--depth", "1"]);
    assert!(!text(&out.stdout).contains("OrderRepository.save"));
}

#[test]
fn req014_ac2_invalid_depth_and_limit_are_errors() {
    let env = Env::built();
    for args in [
        ["OrderService", "--depth", "0"],
        ["OrderService", "--limit", "0"],
    ] {
        let out = env.neighbors(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty());
        let err = text(&out.stderr);
        assert!(err.starts_with("erreur:") && err.contains(args[1]), "{err}");
    }
}

#[test]
fn req014_ac3_hub_with_300_links_shows_50_and_250_remaining() {
    let env = Env::built();
    let out = env.neighbors(&["Hub"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let l = lines(&out);
    let shown = l.iter().filter(|x| x.contains("com.acme.hub.T")).count();
    assert_eq!(shown, 50, "{l:#?}");
    assert_eq!(l.last().unwrap(), "250 résultats supplémentaires");

    let l = lines(&env.neighbors(&["Hub", "--limit", "299"]));
    assert_eq!(l.last().unwrap(), "1 résultat supplémentaire");
    let l = lines(&env.neighbors(&["Hub", "--limit", "300"]));
    assert!(!l.iter().any(|x| x.contains("supplémentaire")), "{l:?}");

    let out = env.neighbors(&["Hub", "--limit", "7", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["uses"].as_array().unwrap().len(), 7);
    assert_eq!(v["remaining"], 293);
}

#[test]
fn req014_ac4_ambiguous_name_lists_candidates_then_qualified_succeeds() {
    let env = Env::built();
    let out = env.neighbors(&["save"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(err.starts_with("erreur:"), "{err}");
    for q in [
        "com.acme.core.OrderRepository.save",
        "com.acme.many.Repo11.save",
    ] {
        assert!(err.contains(q), "{q} absent : {err}");
    }
    assert_eq!(err.matches(".save").count(), 13, "{err}");
    let out = env.neighbors(&["OrderRepository.save"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("OrderService.place"));
}

#[test]
fn req014_ac5_unknown_node_is_error() {
    let env = Env::built();
    for extra in [vec![], vec!["--json"]] {
        let mut a = vec!["Fantome"];
        a.extend(extra);
        let out = env.neighbors(&a);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let err = text(&out.stderr);
        assert!(
            err.starts_with("erreur:") && err.contains("aucun résultat"),
            "{err}"
        );
    }
}

#[test]
fn req014_node_without_links_reports_no_neighbors() {
    let env = Env::built();
    let out = env.neighbors(&["Island"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("aucun voisin"));
}

#[test]
fn req015_ac1_path_follows_real_direction_of_links() {
    let env = Env::built();
    let out = env.path(&["ApiController", "OrderRepository"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    let l = lines(&out);
    assert_eq!(l.len(), 4, "{l:#?}");
    assert!(
        l[0].starts_with("com.acme.api.ApiController contains com.acme.api.ApiController.create"),
        "{}",
        l[0]
    );
    assert!(
        l[1].contains("ApiController.create calls") && l[1].ends_with("OrderService.place"),
        "{}",
        l[1]
    );
    assert!(
        l[2].contains("OrderService.place calls") && l[2].ends_with("OrderRepository.save"),
        "{}",
        l[2]
    );
    assert!(
        l[3].contains("OrderRepository.save contenu par") && l[3].ends_with("OrderRepository"),
        "{}",
        l[3]
    );
}

#[test]
fn req015_ac1_path_json_gives_steps_with_real_source_and_target() {
    let env = Env::built();
    let out = env.path(&["ApiController", "OrderRepository", "--json"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let steps = v["path"].as_array().unwrap();
    assert_eq!(steps.len(), 4, "{v}");
    let last = &steps[3];
    assert_eq!(last["kind"], "contains");
    assert_eq!(last["reversed"], true);
    assert_eq!(last["source"], "com.acme.core.OrderRepository");
    assert_eq!(last["target"], "com.acme.core.OrderRepository.save");
    assert_eq!(steps[0]["reversed"], false);
    assert_eq!(steps[0]["source"], "com.acme.api.ApiController");
}

#[test]
fn req015_path_is_shortest_and_symmetric_in_length() {
    let env = Env::built();
    let out = env.path(&["OrderRepository", "ApiController"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines(&out).len(), 4);
    let out = env.path(&["OrderService", "OrderService"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("OrderService"));
}

#[test]
fn req015_ac2_no_path_is_error() {
    let env = Env::built();
    for extra in [vec![], vec!["--json"]] {
        let mut a = vec!["ApiController", "Island"];
        a.extend(extra);
        let out = env.path(&a);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(text(&out.stderr).contains("aucun chemin"));
    }
}

#[test]
fn req015_ac3_ambiguous_or_unknown_endpoint_behaves_like_neighbors() {
    let env = Env::built();
    for args in [["save", "ApiController"], ["ApiController", "save"]] {
        let out = env.path(&args);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let err = text(&out.stderr);
        assert!(
            err.contains("ambigu") && err.contains("com.acme.many.Repo03.save"),
            "{err}"
        );
    }
    for args in [["Fantome", "ApiController"], ["ApiController", "Fantome"]] {
        let out = env.path(&args);
        assert!(!out.status.success());
        assert!(text(&out.stderr).contains("aucun résultat"));
    }
    let out = env.path(&["OrderRepository.save", "ApiController"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn req017_ac3_no_graph_invites_to_build_for_neighbors_and_path() {
    let env = Env::empty();
    env.sources();
    for out in [env.neighbors(&["OrderService"]), env.path(&["A", "B"])] {
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        let err = text(&out.stderr);
        assert!(
            err.starts_with("erreur:") && err.contains("rkg build"),
            "{err}"
        );
    }
    assert!(!env.repo.join(".rkg").exists());
}

#[test]
fn req017_ac1_stale_graph_warns_for_neighbors_and_path() {
    let env = Env::built();
    fs::write(env.repo.join("core/OrderService.java"), "// modifié\n").unwrap();
    fs::write(env.repo.join("iso/Island.java"), "// modifié\n").unwrap();
    let out = env.neighbors(&["OrderService"]);
    assert!(out.status.success());
    assert!(text(&out.stderr).contains("graphe pas à jour : 2 fichiers modifiés"));
    let out = env.path(&["ApiController", "OrderRepository"]);
    assert!(out.status.success());
    assert!(text(&out.stderr).contains("graphe pas à jour : 2 fichiers modifiés"));
}

#[test]
fn req014_ac3_limit_applies_to_both_sections_together() {
    let env = Env::built();
    let out = env.neighbors(&["OrderService", "--limit", "2", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["uses"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["used_by"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["remaining"], 1);
    let out = env.neighbors(&["OrderService", "--limit", "3", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["used_by"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["remaining"], 0);
}
