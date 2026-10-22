//! Appels d'enrichissement par groupes de 150 nœuds (REQ-047) : faux serveur, LLM simulé,
//! traces et progression.

use assert_cmd::cargo::cargo_bin;
use nephila::store::Store;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

const BIG: &str = "src/Big.java";

/// Classe Java `class` de `k` méthodes `m0()` … : `k + 1` nœuds.
fn java(class: &str, k: usize) -> String {
    let mut s = format!("public class {class} {{\n");
    for i in 0..k {
        s.push_str(&format!("    void m{i}() {{}}\n"));
    }
    s.push_str("}\n");
    s
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn summary_json(entries: &[(&str, &str)]) -> String {
    let items: Vec<serde_json::Value> = entries
        .iter()
        .map(|(n, s)| serde_json::json!({"node": n, "summary": s}))
        .collect();
    serde_json::json!({"version": 2, "summaries": items, "concepts": []}).to_string()
}

struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    log: PathBuf,
    config: PathBuf,
    server: Option<FakeServer>,
}

impl Env {
    /// Dépôt de `files` `(chemin relatif, contenu)` ; `config` = lignes du fichier de
    /// configuration personnelle ; `server` : faux serveur (alors `base_url` y pointe).
    fn new(files: &[(&str, String)], config: &str, server: Option<FakeServer>) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        fs::create_dir_all(&mock).unwrap();
        for (path, content) in files {
            let p = repo.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        let mut cfg = config.to_string();
        if let Some(s) = &server {
            cfg.push_str(&format!("base_url = \"{}\"\n", s.base_url));
        }
        let config = tmp.path().join("config.toml");
        fs::write(&config, cfg).unwrap();
        Env {
            log: tmp.path().join("mock.log"),
            _tmp: tmp,
            repo,
            mock,
            config,
            server,
        }
    }

    fn mock(files: &[(&str, String)], extra: &str) -> Env {
        let cfg = format!(
            "provider = \"mock\"\nenrich_code = true\nmodel = \"mock\"\nconcurrency = 1\n{extra}"
        );
        Env::new(files, &cfg, None)
    }

    fn http(provider: &str, files: &[(&str, String)], extra: &str, server: FakeServer) -> Env {
        let cfg = format!(
            "provider = \"{provider}\"\nenrich_code = true\nmodel = \"m\"\napi_key_env = \"NEPHILA_T103_KEY\"\nconcurrency = 1\n{extra}"
        );
        Env::new(files, &cfg, Some(server))
    }

    fn run(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut c = Command::new(cargo_bin("nephila"));
        c.arg("-C").arg(&self.repo).args(args);
        for v in [
            "NEPHILA_MOCK_FAIL_AFTER",
            "NEPHILA_MOCK_DELAY_MS",
            "NEPHILA_MOCK_INVALID_KEY",
            "NEPHILA_MOCK_TIMEOUT_FILES",
            "NEPHILA_MOCK_KEY_REVOKED_AFTER",
            "NEPHILA_MOCK_MALFORMED",
            "NO_COLOR",
        ] {
            c.env_remove(v);
        }
        c.env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .env("NEPHILA_MOCK_LOG", &self.log)
            .env("NEPHILA_T103_KEY", "sk-test");
        for (k, v) in extra {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// Identifiants `type:` et `method:` du fichier, triés, relevés dans le graphe de
    /// `build --no-llm` (le graphe est reconstruit par le `build` suivant).
    fn node_ids(&self, path: &str) -> Vec<String> {
        let o = self.run(&["build", "--no-llm"], &[]);
        assert!(o.status.success(), "{}", text(&o).1);
        let mut v: Vec<String> = Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .filter(|n| n.file.as_deref() == Some(path))
            .map(|n| n.id)
            .filter(|id| id.starts_with("type:") || id.starts_with("method:"))
            .collect();
        v.sort();
        v
    }

    fn fixture(&self, path: &str, call: usize, entries: &[(&str, &str)]) {
        let f = self.mock.join(format!("{path}.{call}.json"));
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, summary_json(entries)).unwrap();
    }

    fn summary_of(&self, id: &str) -> Option<String> {
        Store::open_readonly(&self.repo)
            .unwrap()
            .nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.id == id)
            .and_then(|n| n.summary)
    }

    fn enrich_posts(&self, provider: &str) -> Vec<String> {
        let path = if provider == "anthropic" {
            "/v1/messages"
        } else {
            "/v1/chat/completions"
        };
        self.server
            .as_ref()
            .unwrap()
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == path)
            .map(|r| r.body)
            .collect()
    }
}

/// Message utilisateur d'une requête : la chaîne JSON qui contient `Nœuds du fichier :`.
fn user_prompt(body: &str) -> String {
    fn find(v: &serde_json::Value) -> Option<String> {
        match v {
            serde_json::Value::String(s) if s.contains("Nœuds du fichier :") => Some(s.clone()),
            serde_json::Value::Array(a) => a.iter().find_map(find),
            serde_json::Value::Object(o) => o.values().find_map(find),
            _ => None,
        }
    }
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    find(&v).unwrap_or_else(|| panic!("aucun prompt d'enrichissement dans {body}"))
}

/// Identifiants listés (`- <id>`) après chaque `Nœuds du fichier :` du prompt, dans l'ordre.
fn listed_ids(prompt: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_list = false;
    for line in prompt.lines() {
        if line.starts_with("Nœuds du fichier :") {
            in_list = true;
        } else if in_list {
            match line.strip_prefix("- ") {
                Some(id) => out.push(id.to_string()),
                None => in_list = false,
            }
        }
    }
    out
}

/// Contenu du fichier d'un prompt de fichier seul (après `Contenu :\n`).
fn content_of(prompt: &str) -> &str {
    prompt.split_once("\nContenu :\n").unwrap().1
}

fn reply(provider: &str, text: &str) -> (u16, String) {
    let v = if provider == "anthropic" {
        serde_json::json!({"content":[{"type":"text","text":text}],"stop_reason":"end_turn"})
    } else {
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]})
    };
    (200, v.to_string())
}

fn llm_lines<'a>(stderr: &'a str, kind: &str, path: &str) -> Vec<&'a str> {
    let key = format!("llm enrich {kind} {path} ");
    stderr.lines().filter(|l| l.contains(&key)).collect()
}

fn groups_of(ids: &[String]) -> Vec<&[String]> {
    ids.chunks(150).collect()
}

#[test]
fn req047_ac3_http_trois_requetes_de_150_nœuds_au_plus() {
    for provider in ["openai", "anthropic"] {
        let src = java("Big", 400);
        let probe = Env::mock(&[(BIG, src.clone())], "");
        let ids = probe.node_ids(BIG);
        assert_eq!(ids.len(), 401);
        let groups = groups_of(&ids);
        assert_eq!(groups.len(), 3);
        let mut responses = Vec::new();
        for (g, group) in groups.iter().enumerate() {
            let s = format!("Résumé du groupe numéro {g}");
            responses.push(reply(provider, &summary_json(&[(&group[0], &s)])));
        }
        let env = Env::http(
            provider,
            &[(BIG, src.clone())],
            "",
            FakeServer::start(responses),
        );
        let o = env.run(&["build"], &[]);
        let (out, err) = text(&o);
        assert_eq!(o.status.code(), Some(0), "{provider} {out} {err}");
        let posts = env.enrich_posts(provider);
        assert_eq!(posts.len(), 3, "{provider}");
        let mut seen: Vec<String> = Vec::new();
        let mut first_ids: Vec<String> = Vec::new();
        for body in &posts {
            let prompt = user_prompt(body);
            assert!(
                content_of(&prompt).contains(&src),
                "{provider}: contenu entier"
            );
            let listed = listed_ids(&prompt);
            assert!(
                !listed.is_empty() && listed.len() <= 150,
                "{provider} {}",
                listed.len()
            );
            first_ids.push(listed[0].clone());
            seen.extend(listed);
        }
        assert_eq!(seen.len(), 401, "{provider}: doublons");
        let set: BTreeSet<&String> = seen.iter().collect();
        let expected: BTreeSet<&String> = ids.iter().collect();
        assert_eq!(set, expected, "{provider}");
        for (g, group) in groups.iter().enumerate() {
            assert_eq!(
                env.summary_of(&group[0]),
                Some(format!("Résumé du groupe numéro {g}")),
                "{provider} groupe {g}"
            );
        }
        let o = env.run(&["query", "search", "Résumé du groupe", "--json"], &[]);
        let (out, _) = text(&o);
        assert!(o.status.success());
        for g in 0..3 {
            assert!(
                out.contains(&format!("Résumé du groupe numéro {g}")),
                "{provider} {out}"
            );
        }
    }
}

#[test]
fn req047_ac4_mock_journal_traces_fixture_et_pannes() {
    let src = java("Big", 400);
    let env = Env::mock(&[(BIG, src)], "");
    let ids = env.node_ids(BIG);
    let first_group_id = ids[0].clone();
    // Journal et traces : trois appels dans l'ordre.
    let o = env.run(&["build", "--verbose"], &[]);
    let (_, err) = text(&o);
    assert!(o.status.success(), "{err}");
    let expected: Vec<String> = (1..=3).map(|i| format!("{BIG}\t{i}")).collect();
    assert_eq!(env.log_lines(), expected);
    let departs = llm_lines(&err, "départ", BIG);
    assert_eq!(departs.len(), 3, "{err}");
    let mut counts = BTreeSet::new();
    for (i, l) in departs.iter().enumerate() {
        assert!(l.contains(&format!("[{}/3]", i + 1)), "{l}");
        counts.insert(
            l.rsplit_once(" car.")
                .unwrap()
                .0
                .rsplit(' ')
                .next()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(counts.len(), 1, "même nombre de caractères : {counts:?}");

    // Fixture du troisième appel qui résume un nœud du premier groupe : appliquée.
    let _ = fs::remove_file(&env.log);
    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    env.fixture(
        BIG,
        3,
        &[(&first_group_id, "Résumé venu du troisième appel")],
    );
    let o = env.run(&["build"], &[]);
    let (out, err) = text(&o);
    assert!(o.status.success(), "{err}");
    assert!(!err.contains("écarté"), "{err}");
    assert!(out.contains("0 fichiers à enrichir"), "{out}");
    assert_eq!(
        env.summary_of(&first_group_id).as_deref(),
        Some("Résumé venu du troisième appel")
    );

    // Appel 1 non conforme une fois : renvoyé, puis les suivants.
    let _ = fs::remove_file(&env.log);
    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    let o = env.run(
        &["build"],
        &[("NEPHILA_MOCK_MALFORMED", &format!("{BIG}:1"))],
    );
    assert!(o.status.success(), "{}", text(&o).1);
    let expected: Vec<String> = [1, 1, 2, 3].iter().map(|i| format!("{BIG}\t{i}")).collect();
    assert_eq!(env.log_lines(), expected);

    // Délai dépassé : un seul appel, le fichier reste à enrichir.
    let _ = fs::remove_file(&env.log);
    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    let o = env.run(&["build"], &[("NEPHILA_MOCK_TIMEOUT_FILES", BIG)]);
    let (out, _) = text(&o);
    assert!(!o.status.success());
    assert_eq!(env.log_lines(), vec![format!("{BIG}\t1")]);
    assert!(out.contains("1 fichiers à enrichir"), "{out}");
}

/// Limite de morceau (littérale) et indice de la première méthode du morceau 2, relevés sur le binaire 0.17.5.
const AC5_LIMIT: usize = 1946;
const AC5_SPLIT: usize = 107;

#[test]
fn req047_ac5_deux_morceaux_une_liste_par_morceau() {
    // 199 méthodes + le type = 200 nœuds ; limite de morceau à la moitié du fichier environ.
    let src = java("Big", 199);
    let limit = AC5_LIMIT;
    let extra = format!("max_chunk_chars = {limit}\n");
    let probe = Env::mock(&[(BIG, src.clone())], &extra);
    let ids = probe.node_ids(BIG);
    assert_eq!(ids.len(), 200);
    let o = probe.run(&["build"], &[]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out} {err}");
    let expected: Vec<String> = (1..=2).map(|i| format!("{BIG}\t{i}")).collect();
    assert_eq!(probe.log_lines(), expected);
    assert!(out.contains("0 fichiers à enrichir"), "{out}");

    // Même dépôt vers le faux serveur : contenus et listes de chaque requête.
    let mut responses = Vec::new();
    responses.extend((0..2).map(|_| reply("openai", &summary_json(&[]))));
    let env = Env::http(
        "openai",
        &[(BIG, src)],
        &extra,
        FakeServer::start(responses),
    );
    let o = env.run(&["build"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let prompts: Vec<String> = env
        .enrich_posts("openai")
        .iter()
        .map(|b| user_prompt(b))
        .collect();
    assert_eq!(prompts.len(), 2);
    for (i, p) in prompts.iter().enumerate() {
        assert!(
            p.contains(&format!("(morceau {})", i + 1)),
            "appel {}",
            i + 1
        );
    }
    let lists: Vec<Vec<String>> = prompts.iter().map(|p| listed_ids(p)).collect();
    let literal = |range: std::ops::Range<usize>| -> Vec<String> {
        let mut v: Vec<String> = range.map(|i| format!("method:Big.m{i}")).collect();
        v.push("type:Big".to_string());
        v.sort();
        v
    };
    assert_eq!(lists[0], literal(0..AC5_SPLIT), "liste de la requête 1");
    assert_eq!(lists[1], literal(AC5_SPLIT..199), "liste de la requête 2");
    for l in &lists {
        assert!(l.len() <= 150);
    }
    let all: Vec<String> = literal(0..199);
    assert_eq!(all.len(), 200);
    let big = ids.iter().find(|i| i.starts_with("type:")).unwrap();
    assert!(lists[0].contains(big) && lists[1].contains(big));
    let methods: BTreeSet<&String> = lists[0]
        .iter()
        .filter(|i| i.starts_with("method:"))
        .collect();
    assert!(lists[1].iter().all(|i| !methods.contains(i)));
    let union: BTreeSet<&String> = lists.iter().flatten().collect();
    assert_eq!(union, all.iter().collect::<BTreeSet<_>>());
    let ids_set: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(ids_set, all.iter().collect::<BTreeSet<_>>());
    assert_ne!(content_of(&prompts[0]), content_of(&prompts[1]));
    assert!(content_of(&prompts[0]).starts_with("public class Big {"));
}

#[test]
fn req047_ac6_lots_de_150_nœuds_au_plus() {
    let files: Vec<(String, String)> = (0..4)
        .map(|i| (format!("src/F{i}.java"), java(&format!("F{i}"), 59)))
        .collect();
    let refs: Vec<(&str, String)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    let env = Env::mock(&refs, "batch_files = 4\n");
    let o = env.run(&["build"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let lines = env.log_lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    for l in &lines {
        assert_eq!(l.split('\t').next().unwrap().split(';').count(), 2, "{l}");
    }

    // Faux serveur : chaque requête de lot liste au plus 150 identifiants en tout.
    let lot = |a: &str, b: &str| {
        let v = serde_json::json!({"version": 3, "files": [
            {"file": a, "summaries": [], "concepts": []},
            {"file": b, "summaries": [], "concepts": []}]});
        reply("openai", &v.to_string())
    };
    let server = FakeServer::start(vec![
        lot("src/F0.java", "src/F1.java"),
        lot("src/F2.java", "src/F3.java"),
    ]);
    let env = Env::http("openai", &refs, "batch_files = 4\n", server);
    let o = env.run(&["build"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let posts = env.enrich_posts("openai");
    assert_eq!(posts.len(), 2);
    for body in &posts {
        let ids = listed_ids(&user_prompt(body));
        assert_eq!(ids.len(), 120, "deux fichiers de 60 nœuds");
        assert!(ids.len() <= 150);
    }
}

#[test]
fn req047_ac8_aucune_mention_des_groupes() {
    let env = Env::mock(&[(BIG, java("Big", 400))], "");
    let ids = env.node_ids(BIG);
    for (i, g) in groups_of(&ids).iter().enumerate() {
        env.fixture(BIG, i + 1, &[(&g[0], "Résumé conforme")]);
    }
    let o = env.run(&["build"], &[]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{err}");
    assert_eq!(err, "", "stderr vide");
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(
        out.trim_end()
            .ends_with(", 0 fichiers en attente, 0 fichiers à enrichir"),
        "{out}"
    );
    // Graphe neuf : un build sans changement n'enrichit plus rien (REQ-049).
    fs::remove_dir_all(env.repo.join(".nephila")).unwrap();
    let verbose = env.run(&["build", "--verbose"], &[]);
    let (vout, verr) = text(&verbose);
    assert!(verbose.status.success(), "{verr}");
    assert!(verr.contains("llm enrich départ"), "{verr}");
    for s in [&out, &err, &vout, &verr] {
        assert!(
            !s.contains("groupe") && !s.contains("nœuds demandés"),
            "{s}"
        );
    }
}

#[test]
fn req047_ac9_six_lignes_llm_enrich_et_fichier_seul_hors_lots() {
    let env = Env::mock(&[(BIG, java("Big", 400))], "");
    let o = env.run(&["build", "--verbose"], &[]);
    let (_, err) = text(&o);
    assert!(o.status.success(), "{err}");
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("llm enrich") && l.contains(BIG))
        .collect();
    assert_eq!(lines.len(), 6, "{err}");
    let mut chars = BTreeSet::new();
    for i in 1..=3 {
        let d = lines[(i - 1) * 2];
        let f = lines[(i - 1) * 2 + 1];
        assert!(
            d.contains(&format!("llm enrich départ {BIG} [{i}/3] ")),
            "{d}"
        );
        assert!(d.ends_with(" car."), "{d}");
        chars.insert(
            d.rsplit_once(' ')
                .unwrap()
                .0
                .rsplit(' ')
                .next()
                .unwrap()
                .to_string(),
        );
        assert!(
            f.contains(&format!("llm enrich fin {BIG} [{i}/3] ok (")),
            "{f}"
        );
    }
    assert_eq!(chars.len(), 1, "{chars:?}");
    assert!(lines.iter().all(|l| !l.contains("lot ")), "{lines:?}");

    // Fichier de 200 nœuds et deux petits fichiers en lot.
    let files = [
        (BIG, java("Big", 199)),
        ("src/S0.java", java("S0", 2)),
        ("src/S1.java", java("S1", 3)),
    ];
    let env = Env::mock(&files, "batch_files = 8\n");
    let o = env.run(&["build", "--verbose"], &[]);
    let (_, err) = text(&o);
    assert!(o.status.success(), "{err}");
    let departs = llm_lines(&err, "départ", BIG);
    assert_eq!(departs.len(), 2, "{err}");
    assert!(
        departs[0].contains("[1/2]") && departs[1].contains("[2/2]"),
        "{departs:?}"
    );
    let lot_departs: Vec<&str> = err
        .lines()
        .filter(|l| l.contains("llm enrich départ lot"))
        .collect();
    assert!(
        !lot_departs.is_empty(),
        "les petits fichiers partent en lot : {err}"
    );
    assert!(
        lot_departs.iter().all(|l| !l.contains(BIG)),
        "{lot_departs:?}"
    );
}

#[test]
fn req047_ac10_progression_en_fichiers() {
    let env = Env::mock(&[(BIG, java("Big", 400))], "");
    let o = env.run(&["build", "--show-progress"], &[]);
    let (_, err) = text(&o);
    assert!(o.status.success(), "{err}");
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    assert_eq!(
        lines,
        [
            "progression: enrichissement 0 % (0/1 fichiers)",
            "progression: enrichissement 100 % (1/1 fichiers)"
        ]
    );

    let files = [
        (BIG, java("Big", 400)),
        ("src/S0.java", java("S0", 1)),
        ("src/S1.java", java("S1", 1)),
        ("src/S2.java", java("S2", 1)),
    ];
    let env = Env::mock(&files, "");
    let o = env.run(&["build", "--show-progress"], &[]);
    let (_, err) = text(&o);
    assert!(o.status.success(), "{err}");
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("progression: enrichissement"))
        .collect();
    assert!((2..=5).contains(&lines.len()), "{lines:?}");
    assert!(
        lines.iter().all(|l| l.ends_with("/4 fichiers)")),
        "{lines:?}"
    );
    assert!(lines[0].ends_with("(0/4 fichiers)"), "{lines:?}");
    assert!(
        lines.last().unwrap().ends_with("(4/4 fichiers)"),
        "{lines:?}"
    );
}

#[test]
fn req047_ac11_une_seule_ligne_par_fichier() {
    let env = Env::mock(&[(BIG, java("Big", 400))], "");
    let o = env.run(
        &["build"],
        &[("NEPHILA_MOCK_MALFORMED", &format!("{BIG}:9"))],
    );
    let (_, err) = text(&o);
    assert!(!o.status.success());
    let warns: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("avertissement:") && l.contains(BIG))
        .collect();
    assert_eq!(warns.len(), 1, "{err}");
    assert!(warns[0].ends_with(" (3 essais sur 3)"), "{}", warns[0]);

    // Un élément inconnu dans chacun des appels 1 et 3 : une ligne pour le fichier.
    let env = Env::mock(&[(BIG, java("Big", 400))], "");
    env.fixture(BIG, 1, &[("method:Big.inconnu1", "x")]);
    env.fixture(BIG, 3, &[("method:Big.inconnu3", "y")]);
    let o = env.run(&["build"], &[]);
    let (_, err) = text(&o);
    assert!(o.status.success(), "{err}");
    let prefix = format!("avertissement: {BIG} : 2 élément(s) d'enrichissement écarté(s) : ");
    let warns: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("avertissement:"))
        .collect();
    assert_eq!(warns.len(), 1, "{err}");
    assert!(warns[0].starts_with(&prefix), "{}", warns[0]);
}

/// `n` caractères de remplissage (lettres et espaces), en lignes de 49 caractères au plus.
fn filler(n: usize) -> String {
    let base: Vec<char> = "Lorem ipsum dolor sit amet consectetur adipiscing elit "
        .chars()
        .collect();
    let mut out = String::new();
    let mut i = 0;
    while out.chars().count() < n {
        let room = n - out.chars().count();
        let len = room.min(50);
        for _ in 0..len - 1 {
            out.push(base[i % base.len()]);
            i += 1;
        }
        out.push('\n');
    }
    out
}

/// Titres `head` suivis de remplissage, pour un total de exactement `total` caractères.
fn block(head: &str, total: usize) -> String {
    let used = head.chars().count();
    format!("{head}{}", filler(total - used))
}

const GUIDE: &str = "docs/guide.md";

#[test]
fn req047_ac15_sections_markdown_au_morceau_de_leur_titre() {
    let m1 = block(
        "# Guide\n\n## Installation rapide\n\n## Configuration du fournisseur\n\n",
        3000,
    );
    let m2 = block(
        "## Usage avancé\n\nExemples complets\n-----------------\n\n## Dépannage\n\n",
        3000,
    );
    let m3 = block("## Dépannage\n\n```\n# Installation rapide\n```\n\n", 2800);
    let src = format!("{m1}{m2}{m3}");
    assert!(!src.contains("guide") && !src.contains("dépannage"));
    let parts = nephila::chunk::split(&src, 3000);
    assert_eq!(parts.len(), 3, "morceaux");
    assert!(parts[0].contains("# Guide") && parts[0].contains("## Configuration"));
    assert!(parts[1].contains("## Usage avancé") && parts[1].contains("Exemples complets"));
    assert!(parts[2].contains("```\n# Installation rapide"));

    let extra = "max_chunk_chars = 3000\n";
    let mut responses = Vec::new();
    let answers = [
        (
            "document:docs/guide.md",
            "Résumé du document guide numéro 1",
        ),
        (
            "section:docs/guide.md#dépannage",
            "Résumé de dépannage numéro 2",
        ),
        (
            "section:docs/guide.md#dépannage-1",
            "Résumé de dépannage numéro 3",
        ),
    ];
    for (id, s) in answers {
        responses.push(reply("openai", &summary_json(&[(id, s)])));
    }
    let env = Env::http(
        "openai",
        &[(GUIDE, src)],
        extra,
        FakeServer::start(responses),
    );
    let o = env.run(&["build"], &[]);
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out} {err}");
    assert!(out.contains("0 fichiers à enrichir"), "{out}");
    let prompts: Vec<String> = env
        .enrich_posts("openai")
        .iter()
        .map(|b| user_prompt(b))
        .collect();
    assert_eq!(prompts.len(), 3);
    let expected: [&[&str]; 3] = [
        &[
            "document:docs/guide.md",
            "section:docs/guide.md#configuration-du-fournisseur",
            "section:docs/guide.md#guide",
            "section:docs/guide.md#installation-rapide",
        ],
        &[
            "section:docs/guide.md#dépannage",
            "section:docs/guide.md#exemples-complets",
            "section:docs/guide.md#usage-avancé",
        ],
        &["section:docs/guide.md#dépannage-1"],
    ];
    let titles: [&[&str]; 3] = [
        &[
            "# Guide",
            "## Installation rapide",
            "## Configuration du fournisseur",
        ],
        &["## Usage avancé", "Exemples complets", "## Dépannage"],
        &["## Dépannage"],
    ];
    for (k, p) in prompts.iter().enumerate() {
        assert!(
            p.contains(&format!("(morceau {})", k + 1)),
            "requête {}",
            k + 1
        );
        assert_eq!(listed_ids(p), expected[k], "liste de la requête {}", k + 1);
        for t in titles[k] {
            assert!(
                content_of(p).contains(&format!("{t}\n")),
                "{t} dans {}",
                k + 1
            );
        }
    }
    let o = env.run(&["query", "search", "Résumé", "--json"], &[]);
    let (out, _) = text(&o);
    assert!(o.status.success());
    for s in [
        "document guide numéro 1",
        "dépannage numéro 2",
        "dépannage numéro 3",
    ] {
        assert!(out.contains(s), "{s} : {out}");
    }

    let mock = Env::mock(
        &[(GUIDE, fs::read_to_string(env.repo.join(GUIDE)).unwrap())],
        extra,
    );
    let o = mock.run(&["build"], &[]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o).1);
    let log: Vec<String> = (1..=3).map(|i| format!("{GUIDE}\t{i}")).collect();
    assert_eq!(mock.log_lines(), log);
}
