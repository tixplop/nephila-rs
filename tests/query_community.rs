//! `query community` (REQ-148, T-232) : liste, détail, erreurs, aide, sortie fermée, et section
//! des communautés de `summary` (REQ-019, AC-019-7).
#![cfg(unix)]

use assert_cmd::cargo::cargo_bin;
use rusqlite::Connection;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[path = "support/schema8.rs"]
mod schema8;

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
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Env {
    /// Dépôt de deux groupes de classes (deux modules Maven sans lien), non indexé.
    fn unbuilt() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/communities");
        copy_dir(&fx, &repo);
        let config = tmp.path().join("config.toml");
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        Env {
            _tmp: tmp,
            repo,
            config,
        }
    }

    fn built() -> Env {
        let env = Env::unbuilt();
        let o = env.run(&["build", "--no-llm"]);
        assert!(o.status.success(), "{}", text(&o.stderr));
        env
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NO_COLOR");
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let o = self.run(args);
        assert!(o.status.success(), "{args:?} : {}", text(&o.stderr));
        o
    }

    fn lines(&self, args: &[&str]) -> Vec<String> {
        text(&self.ok(args).stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.ok(args).stdout).expect("JSON valide")
    }

    fn db(&self) -> Connection {
        Connection::open(self.repo.join(".nephila/graph.db")).unwrap()
    }

    /// Identifiants des nœuds de la communauté `c`, d'après les tables stockées.
    fn members_in_db(&self, c: i64) -> BTreeSet<String> {
        let db = self.db();
        let mut st = db
            .prepare("SELECT n.qualified FROM node_communities m JOIN nodes n ON n.id = m.node WHERE m.community = ?1")
            .unwrap();
        let rows = st.query_map([c], |r| r.get::<_, String>(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }
}

/// Ligne de communauté : `<id> <nom> (<taille> nœuds, cohésion <c>)`.
fn parse_line(l: &str) -> (u64, u64, String) {
    let (id, rest) = l.split_once(' ').unwrap();
    let open = rest.rfind(" (").unwrap();
    let inner = rest[open + 2..].strip_suffix(')').unwrap();
    let (size, coh) = inner.split_once(" nœuds, cohésion ").unwrap();
    (id.parse().unwrap(), size.parse().unwrap(), coh.to_string())
}

fn class_a(q: &str) -> bool {
    q.starts_with("com.alpha")
}

#[test]
fn req148_ac1_list_has_at_least_two_communities_numbered_from_zero_by_decreasing_size() {
    let env = Env::built();
    let lines = env.lines(&["query", "community"]);
    assert!(lines.len() >= 2, "{lines:?}");
    let parsed: Vec<_> = lines.iter().map(|l| parse_line(l)).collect();
    for (i, p) in parsed.iter().enumerate() {
        assert_eq!(p.0, i as u64, "{lines:?}");
    }
    assert!(parsed.windows(2).all(|w| w[0].1 >= w[1].1), "{lines:?}");
    let total: u64 = parsed.iter().map(|p| p.1).sum();
    let export: Value =
        serde_json::from_slice(&env.ok(&["export", "--output", "-"]).stdout).unwrap();
    assert_eq!(total, export["nodes"].as_array().unwrap().len() as u64);
}

#[test]
fn req148_ac1_detail_json_members_are_the_nodes_of_the_community() {
    let env = Env::built();
    let v = env.json(&["query", "community", "0", "--json"]);
    let got: BTreeSet<String> = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["qualified"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, env.members_in_db(0));
    assert_eq!(v["community"]["size"].as_u64().unwrap() as usize, got.len());
}

#[test]
fn req148_ac1_node_option_gives_the_community_of_a_class_of_group_b_only() {
    let env = Env::built();
    let v = env.json(&["query", "community", "--node", "Ledger", "--json"]);
    let members: Vec<&str> = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["qualified"].as_str().unwrap())
        .collect();
    assert!(members.contains(&"com.beta.Ledger"), "{members:?}");
    assert!(!members.iter().any(|q| class_a(q)), "{members:?}");
}

#[test]
fn req148_ac2_limit_on_the_list_gives_remaining_count() {
    let env = Env::built();
    let all = env.lines(&["query", "community"]).len();
    assert!(all > 2);
    let l = env.lines(&["query", "community", "--limit", "2"]);
    assert_eq!(l.len(), 3, "{l:?}");
    let rest = all - 2;
    let want = if rest == 1 {
        "1 résultat supplémentaire".to_string()
    } else {
        format!("{rest} résultats supplémentaires")
    };
    assert_eq!(l[2], want);
    let j = env.json(&["query", "community", "--limit", "2", "--json"]);
    assert_eq!(j["communities"].as_array().unwrap().len(), 2);
    assert_eq!(j["remaining"], rest);
    let full = env.json(&["query", "community", "--json"]);
    assert_eq!(full["remaining"], 0);
}

#[test]
fn req148_ac2_limit_on_the_members() {
    let env = Env::built();
    let size = parse_line(&env.lines(&["query", "community", "0"])[0]).1 as usize;
    let l = env.lines(&["query", "community", "0", "--limit", "3"]);
    assert_eq!(l.len(), 1 + 3 + 1, "{l:?}");
    assert_eq!(l[4], format!("{} résultats supplémentaires", size - 3));
    let j = env.json(&["query", "community", "0", "--limit", "3", "--json"]);
    assert_eq!(j["members"].as_array().unwrap().len(), 3);
    assert_eq!(j["remaining"], size - 3);
    let all = env.lines(&["query", "community", "0", "--limit", &size.to_string()]);
    assert_eq!(all.len(), 1 + size);
    assert!(!all.last().unwrap().contains("supplémentaire"));
}

#[test]
fn req148_ac2_limit_zero_is_refused() {
    let env = Env::built();
    let o = env.run(&["query", "community", "--limit", "0"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(text(&o.stderr), "erreur: --limit doit valoir au moins 1\n");
}

#[test]
fn req148_ac3_unknown_number_is_no_result() {
    let env = Env::built();
    let o = env.run(&["query", "community", "99"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    assert_eq!(
        text(&o.stderr),
        "erreur: aucun résultat : communauté 99 introuvable (essayez `nephila query community`)\n"
    );
    let big = env.run(&["query", "community", "18446744073709551615"]);
    assert_eq!(big.status.code(), Some(1));
    assert!(text(&big.stderr).contains("communauté 18446744073709551615 introuvable"));
}

#[test]
fn req148_ac3_number_with_leading_zeros_is_written_without_them() {
    let env = Env::built();
    let o = env.run(&["query", "community", "0099"]);
    assert!(text(&o.stderr).contains("communauté 99 introuvable"));
}

#[test]
fn req148_ac3_without_graph_invites_to_build() {
    let env = Env::unbuilt();
    let o = env.run(&["query", "community"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    assert!(
        text(&o.stderr).contains("nephila build"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn req148_ac3_ac8_usage_errors_exit_2() {
    let env = Env::built();
    for bad in ["abc", "-1", "1.5"] {
        let o = env.run(&["query", "community", bad]);
        assert_eq!(o.status.code(), Some(2), "{bad}");
        assert!(o.stdout.is_empty());
        let want = format!(
            "error: invalid value '{bad}' for '[NUMÉRO]': `{bad}` n'est pas un entier positif ou nul"
        );
        assert_eq!(text(&o.stderr).lines().next().unwrap(), want);
    }
    let o = env.run(&["query", "community", "0", "--node", "X"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).contains("cannot be used with"));
}

#[test]
fn req148_ac3_ac8_schema8_graph_is_not_computed_with_or_without_arguments() {
    let env = Env::built();
    schema8::downgrade_to_8(&env.db());
    for args in [
        &["query", "community"][..],
        &["query", "community", "--json"][..],
        &["query", "community", "0"][..],
        &["query", "community", "--node", "Ledger"][..],
    ] {
        let o = env.run(args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(o.stdout.is_empty(), "{args:?}");
        assert_eq!(
            text(&o.stderr),
            "erreur: communautés non calculées : lancez nephila update\n",
            "{args:?}"
        );
    }
}

#[test]
fn req148_ac8_check_stale_warning_comes_first_then_e26() {
    let env = Env::built();
    schema8::downgrade_to_8(&env.db());
    let p = env.repo.join("alpha/src/main/java/com/alpha/Cart.java");
    let mut s = fs::read_to_string(&p).unwrap();
    s.push_str("// modifié, plus long\n");
    fs::write(&p, s).unwrap();
    let o = env.run(&["query", "community", "--check-stale"]);
    assert_eq!(o.status.code(), Some(1));
    let err = text(&o.stderr);
    let l: Vec<&str> = err.lines().collect();
    assert!(
        l[0].starts_with("avertissement: graphe pas à jour"),
        "{err}"
    );
    assert_eq!(
        l[1],
        "erreur: communautés non calculées : lancez nephila update"
    );
}

#[test]
fn req148_ac3_ac8_empty_graph_has_no_community() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("vide");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("NOTES.txt"), "rien\n").unwrap();
    let cfg = tmp.path().join("c.toml");
    fs::write(&cfg, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let env = Env {
        _tmp: tmp,
        repo,
        config: cfg,
    };
    let b = env.run(&["build", "--no-llm"]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    let o = env.run(&["query", "community"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    assert_eq!(
        text(&o.stderr),
        "erreur: aucun résultat : aucune communauté (graphe vide)\n"
    );
    let o = env.run(&["query", "community", "0"]);
    assert!(text(&o.stderr).contains("communauté 0 introuvable"));
    let o = env.run(&["query", "community", "--node", "X"]);
    assert!(text(&o.stderr).contains("nœud « X » introuvable"));
    let s = text(&env.ok(&["summary"]).stdout);
    assert!(s.ends_with("Communautés (0)\n"), "{s}");
}

#[test]
fn req148_ac3_node_errors_are_those_of_neighbors() {
    let env = Env::built();
    let o = env.run(&["query", "community", "--node", "Inexistant"]);
    assert_eq!(o.status.code(), Some(1));
    let n = env.run(&["query", "neighbors", "Inexistant"]);
    assert_eq!(text(&o.stderr), text(&n.stderr));
    let o = env.run(&["query", "community", "--node", "run"]);
    let n = env.run(&["query", "neighbors", "run"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("ambigu"));
    assert_eq!(text(&o.stderr), text(&n.stderr));
}

#[test]
fn req148_ac3_nephila_symlink_is_refused() {
    let env = Env::built();
    let real = env.repo.join("ailleurs");
    fs::rename(env.repo.join(".nephila"), &real).unwrap();
    std::os::unix::fs::symlink(&real, env.repo.join(".nephila")).unwrap();
    let o = env.run(&["query", "community"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    assert_eq!(
        text(&o.stderr),
        "erreur: .nephila est un lien symbolique ; supprimez-le\n"
    );
}

#[test]
fn req148_ac7_line_format_and_detail_header_are_identical() {
    let env = Env::built();
    let list = env.lines(&["query", "community"]);
    for l in &list {
        assert!(!l.starts_with(' '));
        let c = parse_line(l).2;
        assert_eq!(c.len(), 4, "{l}");
        assert!(c.as_bytes()[1] == b'.' && c.bytes().all(|b| b == b'.' || b.is_ascii_digit()));
    }
    let detail = env.lines(&["query", "community", "0"]);
    assert_eq!(detail[0], list[0]);
    for m in &detail[1..] {
        assert!(m.starts_with("  "), "{m}");
    }
    // Chaque membre est la ligne que `query search` écrit pour ce nœud.
    for m in &detail[1..] {
        let line = &m[2..];
        let q = line.split(" (").next().unwrap();
        let search = env.lines(&["query", "search", q]);
        assert!(search.contains(&line.to_string()), "{line} / {search:?}");
    }
}

#[test]
fn req148_ac7_first_member_is_the_pivot_and_members_follow_degree_order() {
    let env = Env::built();
    let j = env.json(&["query", "community", "0", "--json"]);
    let pivot = j["community"]["pivot"].as_str().unwrap().to_string();
    let q = pivot.split_once(':').unwrap().1;
    assert_eq!(j["members"][0]["qualified"], q);
    // Degrés lus dans la base.
    let db = env.db();
    let degree = |qualified: &str| -> i64 {
        db.query_row(
            "SELECT (SELECT COUNT(*) FROM edges WHERE source = n.id) + (SELECT COUNT(*) FROM edges WHERE target = n.id) FROM nodes n WHERE n.qualified = ?1 AND n.id IN (SELECT node FROM node_communities WHERE community = 0)",
            [qualified],
            |r| r.get(0),
        )
        .unwrap()
    };
    let degs: Vec<i64> = j["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| degree(m["qualified"].as_str().unwrap()))
        .collect();
    assert!(degs.windows(2).all(|w| w[0] >= w[1]), "{degs:?}");
}

#[test]
fn req148_ac7_node_gives_the_same_bytes_as_the_number() {
    let env = Env::built();
    let by_node = env.ok(&["query", "community", "--node", "Ledger"]);
    let id = env.json(&["query", "community", "--node", "Ledger", "--json"])["community"]["id"]
        .as_u64()
        .unwrap();
    let by_number = env.ok(&["query", "community", &id.to_string()]);
    assert_eq!(by_node.stdout, by_number.stdout);
    let by_node = env.ok(&["query", "community", "--node", "Ledger", "--limit", "2"]);
    let by_number = env.ok(&["query", "community", &id.to_string(), "--limit", "2"]);
    assert_eq!(by_node.stdout, by_number.stdout);
    assert!(text(&by_node.stdout)
        .lines()
        .last()
        .unwrap()
        .ends_with("résultats supplémentaires"));
    assert!(!text(&by_node.stdout)
        .lines()
        .last()
        .unwrap()
        .starts_with(' '));
}

#[test]
fn req148_ac7_json_shapes() {
    let env = Env::built();
    let list = env.json(&["query", "community", "--json"]);
    let keys = |v: &Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&list), ["communities", "remaining"]);
    for c in list["communities"].as_array().unwrap() {
        assert_eq!(keys(c), ["cohesion", "id", "label", "pivot", "size"]);
    }
    let detail = env.json(&["query", "community", "0", "--json"]);
    assert_eq!(keys(&detail), ["community", "members", "remaining"]);
    let search = env.json(&["query", "search", "Ledger", "--json"]);
    let want = keys(&search["results"][0]);
    for m in detail["members"].as_array().unwrap() {
        assert_eq!(keys(m), want);
    }
    // La cohésion JSON n'est pas arrondie ; le texte l'est à deux décimales.
    let c0 = list["communities"][0]["cohesion"].as_f64().unwrap();
    let t0 = &env.lines(&["query", "community"])[0];
    assert!(t0.ends_with(&format!("cohésion {c0:.2})")), "{t0}");
}

#[test]
fn req148_ac4_summary_text_and_json_carry_the_communities() {
    let env = Env::built();
    let list = env.lines(&["query", "community"]);
    let out = text(&env.ok(&["summary"]).stdout);
    let lines: Vec<&str> = out.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("Communautés ("))
        .unwrap();
    assert_eq!(lines[at], format!("Communautés ({})", list.len()));
    assert!(lines[at - 1].starts_with("  ") || lines[at - 1] == "Thèmes");
    let short: Vec<String> = list
        .iter()
        .take(10)
        .map(|l| {
            let (id, size, _) = parse_line(l);
            let name = l[id.to_string().len() + 1..l.rfind(" (").unwrap()].to_string();
            format!("  {id} {name} ({size} nœuds)")
        })
        .collect();
    assert_eq!(
        &lines[at + 1..],
        &short.iter().map(String::as_str).collect::<Vec<_>>()[..]
    );
    let j = env.json(&["summary", "--json"]);
    assert_eq!(j["communities"]["total"], list.len());
    assert!(j["communities"]["top"].as_array().unwrap().len() <= 10);
}

#[test]
fn req148_ac4_summary_shows_only_ten_communities_when_there_are_more() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    // 12 groupes de deux classes sans lien entre les groupes.
    for g in 0..12 {
        let d = repo.join(format!("g{g}/src"));
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join(format!("A{g}.java")),
            format!("package g{g};\n\npublic class A{g} {{\n    B{g} b;\n    public void run() {{ b.run(); }}\n}}\n"),
        )
        .unwrap();
        fs::write(
            d.join(format!("B{g}.java")),
            format!("package g{g};\n\npublic class B{g} {{\n    public void run() {{ }}\n}}\n"),
        )
        .unwrap();
    }
    let cfg = tmp.path().join("c.toml");
    fs::write(&cfg, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let env = Env {
        _tmp: tmp,
        repo,
        config: cfg,
    };
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let total = env.lines(&["query", "community"]).len();
    assert!(total > 10, "{total}");
    let out = env.lines(&["summary"]);
    let at = out
        .iter()
        .position(|l| l.starts_with("Communautés ("))
        .unwrap();
    assert_eq!(out[at], format!("Communautés ({total})"));
    assert_eq!(out.len() - at - 1, 10);
    let j = env.json(&["summary", "--json"]);
    assert_eq!(j["communities"]["top"].as_array().unwrap().len(), 10);
}

#[test]
fn req019_ac7_summary_of_a_schema8_graph_says_not_computed() {
    let env = Env::built();
    schema8::downgrade_to_8(&env.db());
    let o = env.ok(&["summary"]);
    let out = text(&o.stdout);
    assert!(
        out.ends_with("Communautés\n  communautés non calculées (lancez nephila update)\n"),
        "{out}"
    );
    let j = env.json(&["summary", "--json"]);
    assert!(j["communities"].is_null());
    assert!(j.as_object().unwrap().contains_key("communities"));
}

#[test]
fn req019_ac7_summary_text_before_the_section_is_the_stored_content() {
    let env = Env::built();
    let out = text(&env.ok(&["summary"]).stdout);
    let head: Vec<&str> = out
        .lines()
        .take_while(|l| !l.starts_with("Communautés"))
        .collect();
    assert_eq!(head[0], "Modules (3)");
    assert!(head.contains(&"Nœuds les plus reliés"));
    assert_eq!(*head.last().unwrap(), "  aucun thème (graphe non enrichi)");
}

#[test]
fn req148_ac5_no_escape_byte_and_same_output_with_no_color() {
    let env = Env::built();
    for args in [
        &["query", "community"][..],
        &["query", "community", "0"][..],
        &["summary"][..],
    ] {
        let a = env.ok(args);
        let mut c = env.command(args);
        c.env("NO_COLOR", "1");
        let b = c.output().unwrap();
        assert_eq!(a.stdout, b.stdout, "{args:?}");
        assert!(!a.stdout.contains(&0x1b) && !a.stderr.contains(&0x1b));
    }
}

#[test]
fn req148_ac5_names_with_control_characters_are_escaped() {
    let env = Env::unbuilt();
    let key = "evil\u{1b}]52;c;ZWNobyBwd25lZA==\u{7}key";
    fs::write(
        env.repo.join("data.json"),
        serde_json::json!({ key: "x" }).to_string(),
    )
    .unwrap();
    assert!(env.run(&["build", "--no-llm"]).status.success());
    let n = env.lines(&["query", "community"]).len();
    let mut all = String::new();
    for i in 0..n {
        let o = env.ok(&["query", "community", &i.to_string()]);
        assert!(
            !o.stdout.contains(&0x1b) && !o.stdout.contains(&0x07),
            "{i}"
        );
        all.push_str(&text(&o.stdout));
    }
    assert!(
        all.contains("evil\\u{001B}]52;c;ZWNobyBwd25lZA==\\u{0007}key"),
        "{all}"
    );
    // `--json` garde la valeur brute.
    let mut raw = String::new();
    for i in 0..n {
        raw.push_str(&text(
            &env.ok(&["query", "community", &i.to_string(), "--json"])
                .stdout,
        ));
    }
    assert!(raw.contains("\\u001b]52"), "{raw}");
}

#[test]
fn req148_ac5_closed_stdout_gives_e17() {
    let env = Env::built();
    for args in [
        &["query", "community"][..],
        &["query", "community", "0"][..],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let o = env
            .command(args)
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        let err = text(&o.stderr);
        assert!(
            err.starts_with("erreur: écriture sur stdout impossible : "),
            "{err:?}"
        );
        assert_eq!(err.matches('\n').count(), 1, "{err:?}");
    }
}

#[test]
fn req148_ac5_reader_closing_after_10_bytes_gives_e17() {
    // Un hub appelé par 12 000 méthodes : une communauté de plus de 256 KiB de sortie.
    let env = Env::unbuilt();
    let mut src =
        String::from("package com.big;\n\npublic class Big {\n    public void hub() { }\n");
    for i in 0..12000 {
        src.push_str(&format!("    public void method{i}() {{ hub(); }}\n"));
    }
    src.push_str("}\n");
    let dir = env.repo.join("big/src/main/java/com/big");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Big.java"), src).unwrap();
    let b = env.run(&["build", "--no-llm"]);
    assert!(b.status.success(), "{}", text(&b.stderr));
    let args = ["query", "community", "0", "--limit", "100000"];
    let full = env.run(&args);
    assert!(full.status.success());
    assert!(full.stdout.len() > 256 * 1024, "{}", full.stdout.len());
    let mut child = env
        .command(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut head = vec![0u8; 10];
    std::io::Read::read_exact(&mut stdout, &mut head).unwrap();
    drop(stdout);
    let o = child.wait_with_output().unwrap();
    assert_eq!(head, full.stdout[..10]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
    let err = text(&o.stderr);
    assert!(
        err.starts_with("erreur: écriture sur stdout impossible : "),
        "{err:?}"
    );
    assert_eq!(err.matches('\n').count(), 1, "{err:?}");
    assert!(!err.contains("panicked") && !err.contains('\u{1b}'));
}

#[test]
fn req148_ac9_query_help_lists_community_between_path_and_ask() {
    let o = Command::new(cargo_bin("nephila"))
        .args(["query", "--help"])
        .output()
        .unwrap();
    let t = text(&o.stdout);
    let pos = |w: &str| {
        t.find(&format!("\n  {w} "))
            .unwrap_or_else(|| panic!("{w} : {t}"))
    };
    assert!(
        pos("path") < pos("community") && pos("community") < pos("ask"),
        "{t}"
    );
    assert!(
        t.contains("Communautés du graphe (sous-systèmes calculés sans LLM par build et update)")
    );
}

#[test]
fn req148_ac9_community_help_texts_and_order() {
    let o = Command::new(cargo_bin("nephila"))
        .args(["query", "community", "--help"])
        .output()
        .unwrap();
    let h = text(&o.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let order = [
        "[NUMÉRO]",
        "--node <NŒUD>",
        "--limit <LIMIT>",
        "--json",
        "--check-stale",
    ];
    let mut at = h.find("Arguments:").unwrap();
    for w in order {
        let i = h[at..].find(w).unwrap_or_else(|| panic!("{w} : {h}")) + at;
        at = i + w.len();
    }
    for t in [
        "Usage: nephila query community [OPTIONS] [NUMÉRO]",
        "[NUMÉRO] Numéro de la communauté à détailler, 0 étant la plus grande (sans numéro ni --node : liste des communautés)",
        "--node <NŒUD> Détaille la communauté de ce nœud, désigné par son nom ou son identifiant comme pour query neighbors (incompatible avec NUMÉRO)",
        "--limit <LIMIT> Nombre maximal de communautés ou de membres affichés (défaut : 50)",
        "--check-stale Avertit sur stderr si le graphe n'est pas à jour, d'après la taille et la date de modification des fichiers (parcourt le dépôt sans lire les fichiers déjà indexés ; défaut : aucune vérification ; vérification par le contenu : nephila status)",
    ] {
        assert!(h.contains(t), "{t} absent de : {h}");
    }
}

#[test]
fn req148_other_queries_do_not_mention_communities() {
    let env = Env::built();
    for args in [
        &["query", "search", "Ledger"][..],
        &["query", "neighbors", "Ledger"][..],
        &["query", "path", "Ledger", "Entry"][..],
    ] {
        let o = env.ok(args);
        assert!(!text(&o.stdout).contains("ommunaut"), "{args:?}");
    }
}
