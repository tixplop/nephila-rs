//! `query search`, résolution de nom et avertissement « pas à jour »
//! (REQ-013, REQ-017, AC-001-4, AC-020-2).

use assert_cmd::cargo::cargo_bin;
use rkg::query::lookup::{self, Lookup};
use rkg::store::Store;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const SERVICE: &str = "core/OrderService.java";
const REPO_JAVA: &str = "core/OrderRepository.java";
const OTHER: &str = "other/Other.java";

struct Env {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
    log: PathBuf,
}

fn decl(kind: &str, name: &str, extra: &str) -> String {
    format!("{{\"kind\":\"{kind}\",\"name\":\"{name}\"{extra}}}")
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        let config = tmp.path().join("config.toml");
        let log = tmp.path().join("mock.log");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        let env = Env {
            tmp,
            repo,
            mock,
            config,
            log,
        };
        let sum = |s: &str| format!(",\"summary\":\"{s}\"");
        env.file(
            SERVICE,
            "class OrderService {}\n",
            &[
                decl("package", "com.acme.core", ""),
                decl(
                    "type",
                    "com.acme.core.OrderService",
                    &format!(
                        ",\"subkind\":\"class\"{}",
                        sum("Valide et enregistre les commandes")
                    ),
                ),
                decl("method", "com.acme.core.OrderService.place", ""),
            ],
        );
        env.file(
            REPO_JAVA,
            "interface OrderRepository {}\n",
            &[
                decl("package", "com.acme.core", ""),
                decl(
                    "type",
                    "com.acme.core.OrderRepository",
                    ",\"subkind\":\"interface\"",
                ),
                decl("method", "com.acme.core.OrderRepository.save", ""),
            ],
        );
        env.file(
            OTHER,
            "class Other {}\n",
            &[
                decl("package", "com.acme.other", ""),
                decl("type", "com.acme.other.Order", ",\"subkind\":\"class\""),
                decl(
                    "type",
                    "com.acme.other.PreOrderHook",
                    ",\"subkind\":\"class\"",
                ),
                decl(
                    "type",
                    "com.acme.other.Billing",
                    &format!(
                        ",\"subkind\":\"class\"{}",
                        sum("Gère la FACTURATION par lot")
                    ),
                ),
                decl("method", "com.acme.other.Billing.save", ""),
            ],
        );
        let topics: Vec<String> = std::iter::once(decl("document", "docs/many.md", ""))
            .chain((0..60).map(|i| decl("concept", &format!("topic{i:02}"), "")))
            .collect();
        env.file("docs/many.md", "# Many\n", &topics);
        env
    }

    fn file(&self, path: &str, content: &str, declares: &[String]) {
        let p = self.repo.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).unwrap();
        let m = self.mock.join(format!("{path}.json"));
        fs::create_dir_all(m.parent().unwrap()).unwrap();
        fs::write(
            m,
            format!(
                "{{\"version\":1,\"declares\":[{}],\"references\":[]}}",
                declares.join(",")
            ),
        )
        .unwrap();
    }

    fn built() -> Env {
        let env = Env::new();
        let out = env.cmd(&["build"], &[]).output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        env
    }

    fn cmd(&self, args: &[&str], extra: &[(&str, &str)]) -> Command {
        let mut c = Command::new(cargo_bin("rkg"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("RKG_CONFIG", &self.config)
            .env("RKG_MOCK_DIR", &self.mock)
            .env("RKG_MOCK_LOG", &self.log)
            .env_remove("RKG_MOCK_DELAY_MS")
            .env_remove("RKG_MOCK_FAIL_AFTER");
        for (k, v) in extra {
            c.env(k, v);
        }
        c
    }

    fn q(&self, args: &[&str]) -> Output {
        let mut a = vec!["query", "search"];
        a.extend_from_slice(args);
        self.cmd(&a, &[]).output().unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn lines(o: &Output) -> Vec<String> {
    text(&o.stdout).lines().map(str::to_string).collect()
}

fn json(o: &Output) -> Value {
    assert!(o.status.success(), "{}", text(&o.stderr));
    serde_json::from_slice(&o.stdout).expect("JSON valide")
}

#[test]
fn req013_ac1_search_shows_name_kind_file_and_summary() {
    let env = Env::built();
    let out = env.q(&["OrderService"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let l = lines(&out);
    assert_eq!(l.len(), 2, "{l:?}");
    assert!(l[0].contains("OrderService"), "{}", l[0]);
    assert!(l[0].contains("class"), "{}", l[0]);
    assert!(l[1].contains("OrderService.place") && l[1].contains("method"));
    assert!(l[0].contains(SERVICE), "{}", l[0]);
    assert!(
        l[0].contains("Valide et enregistre les commandes"),
        "{}",
        l[0]
    );
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
}

#[test]
fn req013_ac1_search_is_case_insensitive() {
    let env = Env::built();
    let out = env.q(&["orderSERVICE"]);
    assert!(out.status.success());
    assert_eq!(lines(&out), lines(&env.q(&["OrderService"])));
    assert_eq!(lines(&out).len(), 2);
}

#[test]
fn req013_ac1_search_matches_qualified_name() {
    let env = Env::built();
    let v = json(&env.q(&["acme.other", "--json"]));
    let names: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["qualified"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"com.acme.other.Order"), "{names:?}");
    assert!(!names.contains(&"com.acme.core.OrderService"), "{names:?}");
}

#[test]
fn req013_ac2_keyword_only_in_summary_finds_node() {
    let env = Env::built();
    let out = env.q(&["facturation"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let l = lines(&out);
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].contains("Billing"), "{}", l[0]);
}

#[test]
fn req013_ac1_results_sorted_exact_then_prefix_then_content() {
    let env = Env::built();
    let v = json(&env.q(&["order", "--json"]));
    let names: Vec<&str> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["qualified"].as_str().unwrap())
        .collect();
    let pos = |n: &str| {
        names
            .iter()
            .position(|x| *x == n)
            .unwrap_or_else(|| panic!("{n} absent : {names:?}"))
    };
    let exact = pos("com.acme.other.Order");
    let prefix = pos("com.acme.core.OrderService").max(pos("com.acme.core.OrderRepository"));
    let content = pos("com.acme.other.PreOrderHook");
    assert_eq!(exact, 0, "{names:?}");
    assert!(exact < pos("com.acme.core.OrderRepository"));
    assert!(prefix < content, "{names:?}");
}

#[test]
fn req013_ac3_default_limit_is_50_with_remaining_line() {
    let env = Env::built();
    let out = env.q(&["topic"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let l = lines(&out);
    assert_eq!(l.len(), 51, "{l:?}");
    assert!(l[50].contains("10 résultats supplémentaires"), "{}", l[50]);
}

#[test]
fn req013_ac3_limit_option_changes_limit() {
    let env = Env::built();
    let l = lines(&env.q(&["topic", "--limit", "5"]));
    assert_eq!(l.len(), 6, "{l:?}");
    assert!(l[5].contains("55 résultats supplémentaires"), "{}", l[5]);
    let l = lines(&env.q(&["topic", "--limit", "60"]));
    assert_eq!(l.len(), 60, "{l:?}");
    assert!(!l.iter().any(|x| x.contains("supplémentaires")));
}

#[test]
fn req013_ac3_invalid_limit_and_empty_term_are_errors() {
    let env = Env::built();
    for args in [vec!["topic", "--limit", "0"], vec![""], vec!["   "]] {
        let out = env.q(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty());
        assert!(text(&out.stderr).starts_with("erreur:"), "{args:?}");
    }
}

#[test]
fn req013_ac4_json_has_results_and_remaining() {
    let env = Env::built();
    let v = json(&env.q(&["OrderService", "--json"]));
    assert_eq!(v["remaining"], 0);
    let r = &v["results"].as_array().unwrap()[0];
    assert_eq!(r["kind"], "type");
    assert_eq!(r["subkind"], "class");
    assert_eq!(r["qualified"], "com.acme.core.OrderService");
    assert_eq!(r["file"], SERVICE);
    assert_eq!(r["summary"], "Valide et enregistre les commandes");

    let v = json(&env.q(&["topic", "--json", "--limit", "7"]));
    assert_eq!(v["results"].as_array().unwrap().len(), 7);
    assert_eq!(v["remaining"], 53);
}

#[test]
fn req013_ac5_no_result_is_stderr_and_nonzero() {
    let env = Env::built();
    for extra in [vec![], vec!["--json"]] {
        let mut a = vec!["zzzabsent"];
        a.extend(extra);
        let out = env.q(&a);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(text(&out.stderr).contains("aucun résultat"));
    }
}

#[test]
fn req017_ac1_stale_graph_warns_on_stderr_but_answers() {
    let env = Env::built();
    for p in [SERVICE, REPO_JAVA, OTHER] {
        let path = env.repo.join(p);
        let mut c = fs::read_to_string(&path).unwrap();
        c.push_str("// modifié\n");
        fs::write(path, c).unwrap();
    }
    let out = env.q(&["OrderService"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(lines(&out).len(), 2);
    assert!(
        text(&out.stderr).contains("graphe pas à jour : 3 fichiers modifiés"),
        "{}",
        text(&out.stderr)
    );
    // Avertissement aussi avec --json, sans polluer stdout.
    let out = env.q(&["OrderService", "--json"]);
    assert!(text(&out.stderr).contains("graphe pas à jour : 3 fichiers modifiés"));
    json(&out);
}

#[test]
fn req017_ac1_warning_also_given_when_no_result() {
    let env = Env::built();
    fs::write(env.repo.join("docs/new.md"), "# Nouveau\n").unwrap();
    let out = env.q(&["zzzabsent"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(
        err.contains("graphe pas à jour : 1 fichier modifié"),
        "{err}"
    );
    assert!(err.contains("aucun résultat"), "{err}");
}

#[test]
fn req017_ac2_up_to_date_graph_has_no_warning() {
    let env = Env::built();
    let out = env.q(&["Order"]);
    assert!(out.status.success());
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
}

#[test]
fn req017_ac3_no_graph_invites_to_build() {
    let env = Env::new();
    let out = env.q(&["Order"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(
        err.starts_with("erreur:") && err.contains("rkg build"),
        "{err}"
    );
    assert!(!env.repo.join(".rkg").exists(), "la requête a créé .rkg");
}

#[test]
fn req001_ac4_search_calls_no_llm_and_needs_no_config() {
    let env = Env::built();
    let before = fs::read_to_string(&env.log).unwrap();
    let out = Command::new(cargo_bin("rkg"))
        .arg("-C")
        .arg(&env.repo)
        .args(["query", "search", "Order"])
        .env("RKG_CONFIG", env.repo.join("absent.toml"))
        .env_remove("RKG_MOCK_DIR")
        .env_remove("RKG_MOCK_LOG")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(fs::read_to_string(&env.log).unwrap(), before);
}

#[test]
fn req001_ac2_search_accepts_relative_repo_option() {
    let env = Env::built();
    let out = Command::new(cargo_bin("rkg"))
        .current_dir(env.tmp.path())
        .args(["-C", "repo", "query", "search", "OrderService"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("OrderService"));
}

#[test]
fn req020_ac2_query_search_answers_during_running_build() {
    let env = Env::new();
    for i in 0..12 {
        env.file(
            &format!("docs/filler{i}.md"),
            "# Filler\n",
            &[decl("document", &format!("docs/filler{i}.md"), "")],
        );
    }
    let mut child = env
        .cmd(&["build"], &[("RKG_MOCK_DELAY_MS", "400")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Attend qu'au moins un nœud soit enregistré par le build en cours.
    let start = Instant::now();
    let name = loop {
        assert!(start.elapsed() < Duration::from_secs(20), "aucun nœud");
        if let Ok(store) = Store::open_readonly(&env.repo) {
            if let Some(n) = store.nodes().ok().and_then(|n| n.into_iter().next()) {
                break n.short;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let t = Instant::now();
    let out = env.q(&[&name]);
    let elapsed = t.elapsed();
    let running = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(running, "build déjà terminé");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains(&name));
    assert!(
        elapsed < Duration::from_secs(3),
        "search bloqué : {elapsed:?}"
    );
    assert!(text(&out.stderr).contains("graphe pas à jour"));
}

fn store_of(env: &Env) -> Store {
    Store::open_readonly(&env.repo).unwrap()
}

fn found(l: Lookup) -> String {
    match l {
        Lookup::Found(n) => n.id,
        other => panic!("attendu Found : {other:?}"),
    }
}

#[test]
fn lookup_resolves_short_qualified_and_suffix_names() {
    let env = Env::built();
    let s = store_of(&env);
    let want = "type:com.acme.core.OrderService";
    assert_eq!(found(lookup::resolve(&s, "OrderService").unwrap()), want);
    assert_eq!(
        found(lookup::resolve(&s, "com.acme.core.OrderService").unwrap()),
        want
    );
    assert_eq!(
        found(lookup::resolve(&s, "core.OrderService").unwrap()),
        want
    );
    assert_eq!(found(lookup::resolve(&s, want).unwrap()), want);
    assert_eq!(
        found(lookup::resolve(&s, "OrderRepository.save").unwrap()),
        "method:com.acme.core.OrderRepository.save"
    );
}

#[test]
fn lookup_is_case_sensitive_and_does_not_match_inside_a_segment() {
    let env = Env::built();
    let s = store_of(&env);
    assert_eq!(
        lookup::resolve(&s, "orderservice").unwrap(),
        Lookup::NotFound
    );
    assert_eq!(lookup::resolve(&s, "Service").unwrap(), Lookup::NotFound);
    assert_eq!(lookup::resolve(&s, "").unwrap(), Lookup::NotFound);
}

#[test]
fn lookup_ambiguous_lists_candidates_and_qualified_name_disambiguates() {
    let env = Env::built();
    let s = store_of(&env);
    match lookup::resolve(&s, "save").unwrap() {
        Lookup::Ambiguous(c) => {
            let ids: Vec<&str> = c.iter().map(|n| n.id.as_str()).collect();
            assert_eq!(
                ids,
                [
                    "method:com.acme.core.OrderRepository.save",
                    "method:com.acme.other.Billing.save"
                ]
            );
        }
        other => panic!("{other:?}"),
    }
    let err = lookup::require(&s, "save").unwrap_err().to_string();
    assert!(err.contains("com.acme.core.OrderRepository.save"), "{err}");
    assert!(err.contains("com.acme.other.Billing.save"), "{err}");
    let err = lookup::require(&s, "Nope").unwrap_err().to_string();
    assert!(err.contains("Nope"), "{err}");
    assert_eq!(
        lookup::require(&s, "Billing.save").unwrap().qualified,
        "com.acme.other.Billing.save"
    );
}

#[test]
fn lookup_matches_package_by_trailing_segment() {
    let env = Env::built();
    let s = store_of(&env);
    assert_eq!(
        found(lookup::resolve(&s, "other").unwrap()),
        "package:com.acme.other"
    );
}
