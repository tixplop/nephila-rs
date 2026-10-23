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

    fn file(&self, path: &str, content: &str) {
        let p = self.repo.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
    }

    fn sources(&self) {
        self.file(
            "core/OrderService.java",
            "package com.acme.core;\n\nimport com.acme.core.OrderRepository;\n\npublic class OrderService {\n    private OrderRepository repo;\n\n    public void place() {\n        repo.save();\n    }\n}\n",
        );
        self.file(
            "core/OrderRepository.java",
            "package com.acme.core;\n\npublic interface OrderRepository {\n    void save();\n}\n",
        );
        self.file(
            "api/ApiController.java",
            "package com.acme.api;\n\npublic class ApiController {\n    private com.acme.core.OrderService service;\n\n    public void create() {\n        service.place();\n    }\n}\n",
        );
        self.file("iso/Island.java", "public class Island {\n}\n");
        let mut many = String::from("package com.acme.many;\n\n");
        for i in 0..12 {
            many.push_str(&format!(
                "class Repo{i:02} {{\n    void save() {{}}\n}}\n\n"
            ));
        }
        self.file("many/Repos.java", &many);
        let mut hub = String::from("package com.acme.hub;\n\n");
        for i in 0..300 {
            hub.push_str(&format!("import com.acme.hub.T{i:03};\n"));
        }
        hub.push_str("\npublic class Hub {\n}\n");
        self.file("hub/Hub.java", &hub);
        // Chaque type d'un fichier importe tous les imports de ce fichier : un type par fichier.
        for i in 0..300 {
            self.file(
                &format!("hub/T{i:03}.java"),
                &format!("package com.acme.hub;\n\npublic class T{i:03} {{\n}}\n"),
            );
        }
    }

    fn built() -> Env {
        let env = Env::empty();
        env.sources();
        let out = env.run(&["build"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        env
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env_remove("NEPHILA_MOCK_LOG")
            .env_remove("NEPHILA_MOCK_DELAY_MS")
            .env_remove("NEPHILA_MOCK_FAIL_AFTER")
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
    // Appelé par ApiController.create et contenu par son package.
    let by = v["used_by"].as_array().unwrap();
    assert_eq!(by.len(), 2, "{v}");
    let caller = by.iter().find(|e| e["kind"] == "calls").unwrap();
    assert_eq!(caller["qualified"], "com.acme.api.ApiController.create");
    assert!(by
        .iter()
        .any(|e| e["kind"] == "contains" && e["qualified"] == "com.acme.core"));
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
    // 300 imports + le package qui contient Hub.
    assert_eq!(l.last().unwrap(), "251 résultats supplémentaires");

    let l = lines(&env.neighbors(&["Hub", "--limit", "300"]));
    assert_eq!(l.last().unwrap(), "1 résultat supplémentaire");
    let l = lines(&env.neighbors(&["Hub", "--limit", "301"]));
    assert!(!l.iter().any(|x| x.contains("supplémentaire")), "{l:?}");

    let out = env.neighbors(&["Hub", "--limit", "7", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["uses"].as_array().unwrap().len(), 7);
    assert_eq!(v["remaining"], 294);
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
            err.starts_with("erreur:") && err.contains("nephila build"),
            "{err}"
        );
    }
    assert!(!env.repo.join(".nephila").exists());
}

#[test]
fn req017_ac1_stale_graph_warns_for_neighbors_and_path() {
    let env = Env::built();
    fs::write(env.repo.join("core/OrderService.java"), "// modifié\n").unwrap();
    fs::write(env.repo.join("iso/Island.java"), "// modifié\n").unwrap();
    let out = env.neighbors(&["OrderService", "--check-stale"]);
    assert!(out.status.success());
    assert!(text(&out.stderr).contains("graphe pas à jour : 2 fichiers modifiés"));
    let out = env.path(&["ApiController", "OrderRepository", "--check-stale"]);
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
    assert_eq!(v["remaining"], 2);
    let out = env.neighbors(&["OrderService", "--limit", "3", "--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["used_by"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["remaining"], 1);
}

#[test]
fn req015_path_equal_length_ties_pick_smallest_junction_id() {
    let env = Env::empty();
    // Deux chemins de même longueur entre Src et Dst : par MidB ou par MidA.
    for (name, imports) in [
        ("Src", &["MidB", "MidA"][..]),
        ("MidA", &["Dst"][..]),
        ("MidB", &["Dst"][..]),
        ("Dst", &[][..]),
    ] {
        // Un paquet par classe : le nœud de paquet ne crée pas de raccourci.
        let mut s = format!("package com.tie.{};\n\n", name.to_lowercase());
        for i in imports {
            s.push_str(&format!("import com.tie.{}.{i};\n", i.to_lowercase()));
        }
        s.push_str(&format!("\npublic class {name} {{\n}}\n"));
        env.file(&format!("tie/{name}.java"), &s);
    }
    assert!(env.run(&["build"]).status.success());
    for _ in 0..3 {
        let out = env.path(&["Src", "Dst"]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let l = lines(&out);
        assert_eq!(l.len(), 2, "{l:#?}");
        assert!(l[0].contains("com.tie.mida.MidA"), "{l:#?}");
        assert!(l[1].contains("com.tie.mida.MidA"), "{l:#?}");
    }
}
