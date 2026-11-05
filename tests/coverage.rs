//! Couverture des nœuds demandés sur la trace `llm enrich fin` (REQ-235, DEC-176) : faux serveur,
//! `--verbose`, `concurrency = 1`.

use assert_cmd::cargo::cargo_bin;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

struct Setup {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    server: FakeServer,
}

fn reply(text: &str) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]})
        .to_string()
}

fn ok_for(summaries: &[String]) -> String {
    let list: Vec<serde_json::Value> = summaries
        .iter()
        .map(|n| serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")}))
        .collect();
    reply(&serde_json::json!({"version": 2, "summaries": list, "concepts": []}).to_string())
}

fn lot_reply(names: &[&str]) -> String {
    let files: Vec<serde_json::Value> = names
        .iter()
        .map(|n| {
            serde_json::json!({
                "file": format!("{n}.java"),
                "summaries": [{"node": format!("type:{n}"), "summary": format!("Classe {n} centrale.")}],
                "concepts": []
            })
        })
        .collect();
    reply(&serde_json::json!({"version": 3, "files": files}).to_string())
}

impl Setup {
    fn new(replies: Vec<String>, extra: &str) -> Setup {
        Setup::with_statuses(replies.into_iter().map(|b| (200, b)).collect(), extra)
    }

    fn with_statuses(replies: Vec<(u16, String)>, extra: &str) -> Setup {
        let server = FakeServer::start(replies);
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            format!(
                "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T325_KEY\"\nconcurrency = 1\n{extra}",
                server.base_url
            ),
        )
        .unwrap();
        Setup {
            _tmp: tmp,
            repo,
            config,
            server,
        }
    }

    fn write(&self, name: &str, text: &str) {
        fs::write(self.repo.join(name), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_T325_KEY", "sk-test")
            .env("NEPHILA_RETRY_BASE_MS", "1")
            .env_remove("NO_COLOR")
            .output()
            .unwrap()
    }

    fn posts(&self) -> usize {
        self.server
            .requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.path == "/v1/chat/completions")
            .count()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Lignes `llm enrich fin …` de la trace.
fn fins(o: &Output) -> Vec<String> {
    err(o)
        .lines()
        .filter(|l| l.starts_with("trace:") && l.contains("llm enrich fin "))
        .map(String::from)
        .collect()
}

/// `class` de `k` méthodes `m0()` … : `k + 1` nœuds.
fn java(class: &str, k: usize) -> String {
    let mut s = format!("public class {class} {{\n");
    for i in 0..k {
        s.push_str(&format!("    void m{i}() {{}}\n"));
    }
    s.push_str("}\n");
    s
}

/// Identifiants de `class` dans l'ordre de la liste envoyée (ordre alphabétique, type en dernier).
fn ids(class: &str, k: usize) -> Vec<String> {
    let mut v: Vec<String> = (0..k).map(|i| format!("method:{class}.m{i}")).collect();
    v.push(format!("type:{class}"));
    v.sort();
    v
}

/// Forme de la ligne `fin` : début jusqu'à `ok (`, puis la parenthèse jusqu'à `)`.
fn matches_fin(line: &str, path: &str, label: &str, paren: &str) -> bool {
    let Some(rest) = line.strip_prefix("trace: [") else {
        return false;
    };
    let Some((time, rest)) = rest.split_once("s] ") else {
        return false;
    };
    let Some((s, ms)) = time.split_once('.') else {
        return false;
    };
    if s.is_empty()
        || !s.chars().all(|c| c.is_ascii_digit())
        || ms.len() != 3
        || !ms.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    let head = format!("llm enrich fin {path} {label} ok ({paren}) ");
    let Some(tail) = rest.strip_prefix(&head) else {
        return false;
    };
    let Some(tail) = tail.strip_suffix(" ms") else {
        return false;
    };
    !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())
}

#[test]
fn req235_ac1_ac13_trace_quatre_sur_neuf() {
    let all = ids("A", 8);
    let s = Setup::new(vec![ok_for(&all[..4])], "");
    s.write("A.java", &java("A", 8));
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    let f = fins(&o);
    assert_eq!(f.len(), 1, "{}", err(&o));
    assert!(
        matches_fin(
            &f[0],
            "A.java",
            "[1/1]",
            "4 résumés, 0 concepts, 4/9 nœuds résumés"
        ),
        "{}",
        f[0]
    );
    assert!(!err(&o).contains("avertissement:"), "{}", err(&o));
    assert!(!err(&o).contains("couverture") && !err(&o).contains("nœuds demandés"));
    let line = out(&o);
    assert!(
        line.starts_with("build terminé : ")
            && line
                .trim_end()
                .ends_with(", 0 fichiers à enrichir, 4/9 nœuds résumés"),
        "{line}"
    );
    assert_eq!(line.matches("nœuds résumés").count(), 1, "{line}");
    // Sans --verbose, aucune trace.
    let s = Setup::new(vec![ok_for(&all[..4])], "");
    s.write("A.java", &java("A", 8));
    let o = s.run(&["build"]);
    assert!(!err(&o).contains("nœuds résumés"), "{}", err(&o));
}

#[test]
fn req235_ac13_jetons_rapportes_gardent_le_debut() {
    let all = ids("A", 8);
    let text = serde_json::json!({"version": 2, "summaries": [{"node": all[0], "summary": "R."}], "concepts": []}).to_string();
    let body = serde_json::json!({
        "choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    })
    .to_string();
    let s = Setup::new(vec![body], "");
    s.write("A.java", &java("A", 8));
    let o = s.run(&["build", "--verbose"]);
    let f = fins(&o);
    assert_eq!(f.len(), 1, "{}", err(&o));
    let head = "llm enrich fin A.java [1/1] ok (1 résumés, 0 concepts, 1/9 nœuds résumés) ";
    let rest = f[0].split_once("s] ").unwrap().1;
    let tail = rest.strip_prefix(head).unwrap_or_else(|| panic!("{rest}"));
    assert!(tail.contains(" ms (jetons : "), "{rest}");
}

#[test]
fn req235_ac2_resumes_hors_liste_ou_vides_ne_comptent_pas() {
    let all = ids("A", 8);
    // 2 ordinaires, 1 coupé par le plafond, 1 hors liste, 1 vide, 1 `N/A` : 3 comptent dans `<n>`.
    let long = (0..90)
        .map(|i| format!("mot{i:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    let list = serde_json::json!([
        {"node": all[0], "summary": "Un."},
        {"node": all[1], "summary": "Deux."},
        {"node": "method:A.hors", "summary": "Hors liste."},
        {"node": all[2], "summary": ""},
        {"node": all[3], "summary": "N/A"},
        {"node": all[4], "summary": long},
    ]);
    let body =
        reply(&serde_json::json!({"version": 2, "summaries": list, "concepts": []}).to_string());
    let s = Setup::new(vec![body], "");
    s.write("A.java", &java("A", 8));
    let o = s.run(&["build", "--verbose"]);
    let f = fins(&o);
    assert_eq!(f.len(), 1, "{}", err(&o));
    assert!(f[0].contains("nœuds résumés)"), "{}", f[0]);
    assert!(f[0].contains(", 3/9 nœuds résumés)"), "{}", f[0]);
    assert!(
        out(&o)
            .trim_end()
            .ends_with(", 0 fichiers à enrichir, 3/9 nœuds résumés"),
        "{}",
        out(&o)
    );
}

#[test]
fn req235_ac3_deux_appels_de_150_et_50() {
    let all = ids("B", 199);
    let (l1, l2) = (&all[..150], &all[150..]);
    // Réponse 1 : 100 de sa liste et 10 de la liste 2. Réponse 2 : 30, dont 5 de ces 10.
    let mut r1: Vec<String> = l1[..100].to_vec();
    r1.extend(l2[..10].iter().cloned());
    let mut r2: Vec<String> = l2[..5].to_vec();
    r2.extend(l2[10..35].iter().cloned());
    let s = Setup::new(vec![ok_for(&r1), ok_for(&r2)], "");
    s.write("B.java", &java("B", 199));
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.posts(), 2);
    let f = fins(&o);
    assert_eq!(f.len(), 2, "{}", err(&o));
    assert!(
        matches_fin(
            &f[0],
            "B.java",
            "[1/2]",
            "110 résumés, 0 concepts, 100/150 nœuds résumés"
        ),
        "{}",
        f[0]
    );
    assert!(
        matches_fin(
            &f[1],
            "B.java",
            "[2/2]",
            "30 résumés, 0 concepts, 30/50 nœuds résumés"
        ),
        "{}",
        f[1]
    );
    assert!(
        out(&o)
            .trim_end()
            .ends_with(", 0 fichiers à enrichir, 135/200 nœuds résumés"),
        "{}",
        out(&o)
    );
}

#[test]
fn req235_ac4_lot_trois_sur_quatre_puis_renvoi() {
    let s = Setup::new(
        vec![lot_reply(&["A", "B", "C"]), ok_for(&["type:D".to_string()])],
        "batch_files = 4\n",
    );
    for n in ["A", "B", "C", "D"] {
        s.write(&format!("{n}.java"), &format!("public class {n} {{}}\n"));
    }
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    let f = fins(&o);
    assert_eq!(f.len(), 2, "{}", err(&o));
    assert!(
        matches_fin(
            &f[0],
            "lot A.java +3",
            "[1/1]",
            "3 résumés, 0 concepts, 3/4 nœuds résumés"
        ),
        "{}",
        f[0]
    );
    assert!(
        matches_fin(
            &f[1],
            "D.java",
            "[1/1]",
            "1 résumés, 0 concepts, 1/1 nœuds résumés"
        ),
        "{}",
        f[1]
    );
    assert!(
        out(&o)
            .trim_end()
            .ends_with(", 0 fichiers à enrichir, 4/4 nœuds résumés"),
        "{}",
        out(&o)
    );
}

#[test]
fn req235_ac6_morceau_a_liste_vide_zero_sur_zero() {
    let none = ok_for(&[]);
    let s = Setup::new(vec![none; 6], "max_chunk_chars = 8\nformat_retries = 0\n");
    s.write("E.md", "# t0\nxxxxxxx\n# t2\n");
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    let f = fins(&o);
    assert!(
        f.iter().any(|l| l.contains(", 0/0 nœuds résumés) ")),
        "{}",
        err(&o)
    );
}

#[test]
fn req235_ac8_appel_conserve_une_seule_ligne_fin() {
    // Premier build : le second appel (50 nœuds) échoue sur un statut 400 et reste à envoyer.
    let all = ids("B", 199);
    let (l1, l2) = (all[..150].to_vec(), all[150..].to_vec());
    let s = Setup::with_statuses(
        vec![(200, ok_for(&l1)), (400, "{}".into()), (200, ok_for(&l2))],
        "",
    );
    s.write("B.java", &java("B", 199));
    let o = s.run(&["build"]);
    assert_eq!(s.posts(), 2, "{}", err(&o));
    let o = s.run(&["update", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.posts(), 3, "{}", err(&o));
    let f = fins(&o);
    assert_eq!(f.len(), 1, "{}", err(&o));
    assert!(f[0].contains(", 50/50 nœuds résumés) "), "{}", f[0]);
    assert!(
        out(&o)
            .trim_end()
            .ends_with(", 0 fichiers à enrichir, 200/200 nœuds résumés"),
        "{}",
        out(&o)
    );
}

#[test]
fn req235_ac8_sous_appel_cinq_sur_cinq_marque_en_dernier() {
    let all = ids("A", 8);
    let items: Vec<String> = all[..4]
        .iter()
        .map(|n| serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")}).to_string())
        .collect();
    let cut = format!(
        r#"{{"version": 2, "summaries": [{}, {{"node": "{}", "summ"#,
        items.join(","),
        all[4]
    );
    let truncated =
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":cut},"finish_reason":"length"}]})
            .to_string();
    let s = Setup::new(vec![truncated, ok_for(&all[4..])], "");
    s.write("A.java", &java("A", 8));
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    let f = fins(&o);
    let sub = f
        .iter()
        .find(|l| l.contains("ok ("))
        .unwrap_or_else(|| panic!("{}", err(&o)));
    assert!(
        sub.contains("ok (5 résumés, 0 concepts, 5/5 nœuds résumés) ")
            && sub.ends_with(" ms (sous-appel 1)"),
        "{sub}"
    );
    assert!(
        out(&o)
            .trim_end()
            .ends_with(", 0 fichiers à enrichir, 9/9 nœuds résumés"),
        "{}",
        out(&o)
    );
    // Seules les fins `ok` portent le segment.
    for l in err(&o).lines().filter(|l| l.contains("nœuds résumés")) {
        assert!(l.contains("llm enrich fin ") && l.contains(" ok ("), "{l}");
    }
}

/// Aucune ligne de stdout ne porte le segment de couverture.
fn no_coverage(o: &Output) {
    assert!(o.status.success(), "{}", err(o));
    assert!(!out(o).contains("nœuds résumés"), "{}", out(o));
}

#[test]
fn req235_ac5_reponse_vide_zero_sur_neuf() {
    let s = Setup::new(vec![ok_for(&[])], "");
    s.write("A.java", &java("A", 8));
    let o = s.run(&["build", "--verbose"]);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(s.posts(), 1);
    let f = fins(&o);
    assert_eq!(f.len(), 1, "{}", err(&o));
    assert!(f[0].contains(", 0/9 nœuds résumés) "), "{}", f[0]);
    assert!(
        out(&o)
            .trim_end()
            .ends_with(", 0 fichiers à enrichir, 0/9 nœuds résumés"),
        "{}",
        out(&o)
    );
    assert!(!err(&o).contains("avertissement:"), "{}", err(&o));
    let u = s.run(&["update"]);
    assert!(u.status.success(), "{}", err(&u));
    assert!(out(&u).contains("graphe à jour"), "{}", out(&u));
    assert_eq!(s.posts(), 1);
}

#[test]
fn req235_ac6_sans_ecriture_d_enrichissement_aucun_segment() {
    let all = ids("A", 8);
    // --no-llm
    let s = Setup::new(vec![], "");
    s.write("A.java", &java("A", 8));
    no_coverage(&s.run(&["build", "--no-llm"]));
    // --dry-run
    let s = Setup::new(vec![], "");
    s.write("A.java", &java("A", 8));
    no_coverage(&s.run(&["build", "--dry-run"]));
    // update à jour, puis second build où tout est réappliqué
    let s = Setup::new(vec![ok_for(&all)], "");
    s.write("A.java", &java("A", 8));
    let first = s.run(&["build"]);
    assert!(out(&first).contains("9/9 nœuds résumés"), "{}", out(&first));
    let up = s.run(&["update"]);
    no_coverage(&up);
    let second = s.run(&["build"]);
    no_coverage(&second);
    assert_eq!(s.posts(), 1);
    // seuls des fichiers sans nœud écrits
    let s = Setup::new(vec![], "");
    s.write("E.java", "package p;\n");
    let o = s.run(&["build"]);
    no_coverage(&o);
    assert_eq!(s.posts(), 0);
}

#[test]
fn req235_ac7_segments_de_portee_et_de_filtres_restent_derniers() {
    let all = ids("A", 8);
    let s = Setup::new(vec![ok_for(&all[..4])], "enrich_exclude = [\"B.java\"]\n");
    s.write("A.java", &java("A", 8));
    s.write("B.java", &java("B", 2));
    s.write("Z.java", &java("Z", 2));
    let o = s.run(&["build", "--exclude", "Z.java"]);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).trim_end().ends_with(
            ", 0 fichiers à enrichir, 4/9 nœuds résumés, 1 fichiers hors portée d'enrichissement, 1 fichiers exclus par les filtres"
        ),
        "{}",
        out(&o)
    );
}

#[test]
fn req235_ac9_fichier_en_echec_ou_laisse_ne_compte_pas() {
    let all = ids("A", 8);
    let s = Setup::new(
        vec![ok_for(&all[..4]), reply("pas de JSON ici")],
        "format_retries = 0\nbatch_files = 1\n",
    );
    s.write("A.java", &java("A", 8));
    s.write("B.java", &java("B", 2));
    let o = s.run(&["build"]);
    assert_eq!(s.posts(), 2, "{}", err(&o));
    assert!(
        out(&o).contains(", 4/9 nœuds résumés") && !out(&o).contains("/12 "),
        "{}",
        out(&o)
    );
    assert_eq!(out(&o).matches("nœuds résumés").count(), 1);
    // Plafond : le fichier est laissé, aucun segment.
    let items: Vec<String> = all[..4]
        .iter()
        .map(|n| serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")}).to_string())
        .collect();
    let cut = format!(
        r#"{{"version": 2, "summaries": [{}, {{"node": "{}", "summ"#,
        items.join(","),
        all[4]
    );
    let truncated =
        serde_json::json!({"choices":[{"message":{"role":"assistant","content":cut},"finish_reason":"length"}]})
            .to_string();
    let s = Setup::new(vec![truncated, ok_for(&all[4..])], "");
    s.write("A.java", &java("A", 8));
    let o = s.run(&["build", "--max-calls", "1"]);
    assert_eq!(s.posts(), 1, "{}", err(&o));
    assert!(!out(&o).contains("nœuds résumés"), "{}", out(&o));
}

#[test]
fn req235_ac10_mode_lot_couverture_ecrite_une_fois_et_collecte_ne_compte_pas() {
    #[allow(dead_code)]
    #[path = "support/batch_server.rs"]
    mod batch_server;
    use batch_server::{BatchServer, Outcome};
    let all = ids("A", 8);
    let text = serde_json::json!({
        "version": 2,
        "summaries": all[..4].iter().map(|n| serde_json::json!({"node": n, "summary": format!("Résumé de {n}.")})).collect::<Vec<_>>(),
        "concepts": []
    })
    .to_string();
    let server = BatchServer::start();
    server.simulate("openai", 0, move |_| Outcome::Text(text.clone()));
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("A.java"), java("A", 8)).unwrap();
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "provider = \"openai\"\nenrich_code = true\nmodel = \"m\"\nbase_url = \"{}\"\napi_key_env = \"NEPHILA_T325_KEY\"\nconcurrency = 1\n",
            server.base_url
        ),
    )
    .unwrap();
    let o = Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(&repo)
        .args(["build", "--batch"])
        .env("NEPHILA_CONFIG", &config)
        .env("NEPHILA_T325_KEY", "sk-test")
        .env("NEPHILA_BATCH_POLL_MS", "5")
        .env("NEPHILA_RETRY_BASE_MS", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o)
            .trim_end()
            .ends_with(", 0 fichiers à enrichir, 4/9 nœuds résumés"),
        "{}",
        out(&o)
    );
}
