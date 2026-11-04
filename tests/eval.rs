//! Banc d'évaluation : corpus figé, questions, passe et rapport (REQ-220 à REQ-222, DEC-172).

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Empreinte du banc (AC-220-6) ; à recalculer si le corpus ou les questions changent.
const CORPUS_HASH: &str = "b0a0628e94ba50f54ae238502bb1ded346aff8e69285de00aab6f2fdc4bbaea1";

#[allow(dead_code)]
#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::FakeServer;

fn eval_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval")
}

/// Fichiers sous `dir`, chemins relatifs avec `/`, triés.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = format!("{prefix}{}", e.file_name().to_str().unwrap());
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), &format!("{name}/"), out);
            } else {
                out.push(name);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

fn corpus_files() -> Vec<String> {
    files_under(&eval_dir().join("corpus"))
}

fn questions() -> Vec<Value> {
    let raw = fs::read_to_string(eval_dir().join("questions.json")).unwrap();
    serde_json::from_str::<Value>(&raw)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// BLAKE3 de la suite `<chemin>\0<contenu>\0` du corpus trié, puis de `questions.json`.
fn bench_hash(corpus: &Path, questions: &Path) -> String {
    let mut h = blake3::Hasher::new();
    for f in files_under(corpus) {
        h.update(f.as_bytes());
        h.update(b"\0");
        h.update(&fs::read(corpus.join(&f)).unwrap());
        h.update(b"\0");
    }
    h.update(b"questions.json\0");
    h.update(&fs::read(questions).unwrap());
    h.update(b"\0");
    h.finalize().to_hex().to_string()
}

fn corpus_hash() -> String {
    bench_hash(
        &eval_dir().join("corpus"),
        &eval_dir().join("questions.json"),
    )
}

/// Dépôt de test : copie du corpus sous `TMPDIR`, configuration `mock`, dossier de fixtures.
struct Env {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    mock: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mock = tmp.path().join("mock");
        let config = tmp.path().join("config.toml");
        for f in corpus_files() {
            let to = repo.join(&f);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(eval_dir().join("corpus").join(&f), to).unwrap();
        }
        fs::create_dir_all(&mock).unwrap();
        fs::write(&config, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
        Env {
            _tmp: tmp,
            repo,
            mock,
            config,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env("NEPHILA_CONFIG", &self.config)
            .env("NEPHILA_MOCK_DIR", &self.mock)
            .output()
            .unwrap()
    }

    /// Graphe `--no-llm` du corpus, puis son export.
    fn build_and_export(&self) -> Vec<Value> {
        let b = self.run(&["build", "--no-llm"]);
        assert_eq!(b.status.code(), Some(0));
        assert!(
            b.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&b.stderr)
        );
        let e = self.run(&["export", "--output", "-"]);
        assert!(e.status.success());
        let v: Value = serde_json::from_slice(&e.stdout).unwrap();
        v["nodes"].as_array().unwrap().clone()
    }
}

fn ext_count(files: &[String], ext: &str) -> usize {
    files.iter().filter(|f| f.ends_with(ext)).count()
}

#[test]
fn req220_ac1_corpus_composition() {
    let files = corpus_files();
    assert!((30..=50).contains(&files.len()), "{} fichiers", files.len());
    for (ext, min) in [
        (".java", 8),
        (".kt", 3),
        (".scala", 1),
        (".groovy", 1),
        ("pom.xml", 1),
        (".js", 3),
        (".ts", 3),
        (".md", 4),
        (".sql", 4),
    ] {
        assert!(ext_count(&files, ext) >= min, "{ext} : moins de {min}");
    }
}

/// Premier indice sensible trouvé dans `text` (casse ignorée), sinon `None`.
fn sensitive(text: &str) -> Option<String> {
    let t = text.to_lowercase();
    for w in [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "credential",
        "privatekey",
        "private_key",
        "private-key",
    ] {
        if t.contains(w) {
            return Some(w.to_string());
        }
    }
    for (i, _) in t.match_indices("begin ") {
        let run: String = t[i + 6..]
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == ' ')
            .collect();
        if run.contains("private") {
            return Some("BEGIN … PRIVATE".into());
        }
    }
    let b = t.as_bytes();
    for (i, c) in b.iter().enumerate() {
        let around = i > 0 && i + 1 < b.len() && b[i - 1].is_ascii_alphanumeric();
        if *c == b'@' && around && b[i + 1].is_ascii_alphanumeric() {
            return Some("adresse e-mail".into());
        }
    }
    for scheme in ["http://", "https://"] {
        for (i, _) in t.match_indices(scheme) {
            let host: String = t[i + scheme.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
                .collect();
            if host != "exemple.org" && !host.ends_with(".exemple.org") {
                return Some(format!("URL {host}"));
            }
        }
    }
    None
}

#[test]
fn req220_ac2_no_sensitive_data() {
    // Le détecteur lui-même.
    for bad in [
        "mot de passe : PassWord",
        "Secret",
        "x_api-key",
        "-----BEGIN RSA PRIVATE KEY-----",
        "a@b.fr",
        "http://exemple.com/x",
        "https://exemple.org.evil.net",
    ] {
        assert!(sensitive(bad).is_some(), "{bad}");
    }
    assert!(sensitive("voir https://exemple.org/a et https://www.exemple.org").is_none());

    let files = corpus_files();
    for f in &files {
        let content = fs::read_to_string(eval_dir().join("corpus").join(f)).unwrap();
        assert_eq!(sensitive(&content), None, "{f}");
        assert_eq!(sensitive(f), None, "{f}");
        let name = f.rsplit('/').next().unwrap();
        assert!(name != ".nephila.toml" && name != ".env", "{f}");
    }
    let q = fs::read_to_string(eval_dir().join("questions.json")).unwrap();
    assert_eq!(sensitive(&q), None, "questions.json");
}

#[test]
fn req220_ac3_graph_covers_every_file_and_dry_run_counts_them() {
    let env = Env::new();
    let nodes = env.build_and_export();
    for f in corpus_files() {
        let covered = nodes.iter().any(|n| {
            n["file"] == f.as_str()
                && !n["id"].as_str().unwrap().starts_with("package:")
                && !n["id"].as_str().unwrap().starts_with("key:")
        });
        assert!(covered, "aucun nœud pour {f}");
    }
    let o = env.run(&["build", "--dry-run", "--enrich-code"]);
    assert_eq!(o.status.code(), Some(0));
    let want = format!(
        "estimation : {} fichiers à enrichir dont 0 envoyés seuls et 0 sans appel",
        corpus_files().len()
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(stdout.lines().any(|l| l == want), "{stdout}");
}

#[test]
fn req220_ac4_questions_are_well_formed_and_cover_families() {
    let env = Env::new();
    let nodes = env.build_and_export();
    let file_of = |q: &str| {
        nodes
            .iter()
            .find(|n| n["qualified"] == q)
            .unwrap_or_else(|| panic!("{q} absent de l'export"))["file"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let qs = questions();
    assert!((20..=30).contains(&qs.len()), "{} questions", qs.len());
    let mut ids = std::collections::HashSet::new();
    let mut families = [0usize; 4];
    for q in &qs {
        assert!(ids.insert(q["id"].as_str().unwrap().to_string()));
        assert!(!q["question"].as_str().unwrap().trim().is_empty());
        let expected = q["expected"].as_array().unwrap();
        assert!(!expected.is_empty() && expected.len() <= 4);
        let mut fam = [false; 4];
        for e in expected {
            let f = file_of(e.as_str().unwrap());
            let i = match f.rsplit('.').next().unwrap() {
                "java" | "kt" | "scala" | "groovy" | "xml" => 0,
                "js" | "ts" => 1,
                "md" => 2,
                "sql" => 3,
                other => panic!("famille inconnue : {other}"),
            };
            fam[i] = true;
        }
        for i in 0..4 {
            families[i] += fam[i] as usize;
        }
    }
    for (i, n) in families.iter().enumerate() {
        assert!(*n >= 2, "famille {i} : {n} questions");
    }
}

#[test]
fn req220_ac5_expected_nodes_are_reachable_by_ask() {
    let env = Env::new();
    env.build_and_export();
    for q in questions() {
        let expected = q["expected"].clone();
        let fixture = serde_json::json!({"found": true, "answer": "ok", "nodes": expected});
        fs::write(env.mock.join("_ask.json"), fixture.to_string()).unwrap();
        let o = env.run(&["query", "ask", "--json", q["question"].as_str().unwrap()]);
        let id = &q["id"];
        assert_eq!(
            o.status.code(),
            Some(0),
            "{id} : {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let got: Vec<&str> = v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["qualified"].as_str().unwrap())
            .collect();
        let want: Vec<&str> = expected
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert_eq!(got, want, "{id}");
    }
}

#[test]
fn req220_ac6_corpus_hash_is_frozen() {
    assert_eq!(corpus_hash(), CORPUS_HASH);
}

// ---------------------------------------------------------------------------------------------
// Passe d'évaluation (REQ-221, REQ-222)
// ---------------------------------------------------------------------------------------------

type Res<T> = Result<T, String>;

/// Réglages d'une passe.
struct Params {
    /// Configuration nephila de la passe (`NEPHILA_EVAL_CONFIG`).
    config: PathBuf,
    /// Dossier du corpus, copié avant la passe.
    corpus: PathBuf,
    /// Fichier des questions.
    questions: PathBuf,
    /// Chemin du rapport.
    out: PathBuf,
    /// Variables d'environnement ajoutées aux commandes (le simulé, la clé d'API).
    env: Vec<(String, String)>,
    /// Configuration du juge (`NEPHILA_EVAL_JUDGE_CONFIG`), absente : pas de juge.
    judge_config: Option<PathBuf>,
}

/// Réglages tirés de l'environnement (`NEPHILA_EVAL_CONFIG` obligatoire, `NEPHILA_EVAL_OUT`,
/// `NEPHILA_EVAL_JUDGE_CONFIG`).
fn params_from_env(get: &dyn Fn(&str) -> Option<String>) -> Res<Params> {
    let config = get("NEPHILA_EVAL_CONFIG")
        .filter(|v| !v.is_empty())
        .ok_or("NEPHILA_EVAL_CONFIG manque : chemin d'une configuration nephila")?;
    let out = match get("NEPHILA_EVAL_OUT").filter(|v| !v.is_empty()) {
        Some(o) => PathBuf::from(o),
        None => Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/eval")
            .join(format!("{}.json", env!("CARGO_PKG_VERSION"))),
    };
    Ok(Params {
        config: PathBuf::from(config),
        corpus: eval_dir().join("corpus"),
        questions: eval_dir().join("questions.json"),
        out,
        env: vec![],
        judge_config: get("NEPHILA_EVAL_JUDGE_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
    })
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Nombre écrit dans le rapport : entier s'il l'est, sinon arrondi à 4 décimales.
fn num(x: f64) -> Value {
    if x.fract() == 0.0 {
        Value::from(x as i64)
    } else {
        Value::from((x * 10_000.0).round() / 10_000.0)
    }
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 / b as f64
    }
}

/// Texte des lignes de trace de `stderr`, horodatage `trace: [<s>.<mmm>s] ` retiré.
fn trace_lines(stderr: &str) -> impl Iterator<Item = &str> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("trace: ["))
        .filter_map(|l| l.split_once("s] ").map(|(_, rest)| rest))
}

/// Libellé en tête de `rest`, jusqu'au `]` de `[i/n]` compris.
fn label_of(rest: &str) -> &str {
    rest.find("] ").map_or(rest, |i| &rest[..=i])
}

/// `(first_calls, first_try_ok)` : lignes `llm enrich départ` sans `(essai `, et celles dont la
/// ligne `fin` suivante de même libellé est `ok`.
fn parse_calls(stderr: &str) -> (u64, u64) {
    let mut pending: HashMap<&str, VecDeque<bool>> = HashMap::new();
    let (mut first, mut ok) = (0, 0);
    for l in trace_lines(stderr) {
        if let Some(rest) = l.strip_prefix("llm enrich départ ") {
            let is_first = !rest.contains("(essai ");
            first += is_first as u64;
            pending
                .entry(label_of(rest))
                .or_default()
                .push_back(is_first);
        } else if let Some(rest) = l.strip_prefix("llm enrich fin ") {
            let label = label_of(rest);
            let was_first = pending.get_mut(label).and_then(|q| q.pop_front());
            let result = rest[label.len()..].split_whitespace().next();
            ok += (was_first == Some(true) && result == Some("ok")) as u64;
        }
    }
    (first, ok)
}

struct Totals {
    calls: u64,
    format_retries: u64,
    http_sends: u64,
    /// Entrée (cache compris), cache, sortie ; `None` : jetons inconnus.
    tokens: Option<[u64; 3]>,
}

fn number_before(s: &str, suffix: &str) -> u64 {
    let i = s.find(suffix).unwrap_or(0);
    s[..i]
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn number_after(s: &str, prefix: &str) -> u64 {
    s.find(prefix)
        .map(|i| {
            s[i + prefix.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Lit une ligne `appels: …` (sans horodatage).
fn parse_totals(line: &str) -> Totals {
    let tokens = if line.contains("jetons : inconnus") {
        None
    } else {
        Some([
            number_after(line, "entrée "),
            number_after(line, "dont cache "),
            number_after(line, "sortie "),
        ])
    };
    Totals {
        calls: number_before(line, " appels,"),
        format_retries: number_before(line, " nouveaux essais"),
        http_sends: number_before(line, " envois HTTP"),
        tokens,
    }
}

fn tokens_json(t: Option<[u64; 3]>) -> Value {
    match t {
        Some([i, c, o]) => serde_json::json!({"input": i, "cache": c, "output": o}),
        None => Value::Null,
    }
}

/// Mots des résumés : nombre, moyenne, médiane, rang le plus proche à 90 %, maximum.
fn word_stats(summaries: &[String]) -> Value {
    let mut w: Vec<u64> = summaries
        .iter()
        .map(|s| s.split_whitespace().count() as u64)
        .collect();
    w.sort_unstable();
    let n = w.len();
    if n == 0 {
        return serde_json::json!({"count": 0, "mean": 0, "median": 0, "p90": 0, "max": 0});
    }
    let mean = w.iter().sum::<u64>() as f64 / n as f64;
    let median = (w[(n - 1) / 2] + w[n / 2]) as f64 / 2.0;
    let rank = (n * 9).div_ceil(10).max(1);
    serde_json::json!({
        "count": n, "mean": num(mean), "median": num(median),
        "p90": w[rank - 1], "max": w[n - 1]
    })
}

/// Mesures de l'enrichissement (sans `exit_code`), d'après l'export et stderr de `build --verbose`.
fn enrich_metrics(export: &Value, stderr: &str) -> Value {
    let (mut requested, mut covered) = (0u64, 0u64);
    let mut summaries = Vec::new();
    for n in export["nodes"].as_array().map_or(&[][..], |a| &a[..]) {
        let id = n["id"].as_str().unwrap_or("");
        if n["file"].is_null() || id.starts_with("package:") || id.starts_with("key:") {
            continue;
        }
        requested += 1;
        if let Some(s) = n["summary"].as_str().filter(|s| !s.is_empty()) {
            covered += 1;
            summaries.push(s.to_string());
        }
    }
    let (first_calls, first_ok) = parse_calls(stderr);
    let totals = trace_lines(stderr)
        .filter(|l| l.starts_with("appels:"))
        .last()
        .map(parse_totals);
    let (calls, retries, sends, tokens) = totals.map_or((0, 0, 0, None), |t| {
        (t.calls, t.format_retries, t.http_sends, t.tokens)
    });
    serde_json::json!({
        "nodes_requested": requested,
        "nodes_covered": covered,
        "coverage": num(ratio(covered, requested)),
        "first_calls": first_calls,
        "first_try_ok": first_ok,
        "first_try_rate": num(ratio(first_ok, first_calls)),
        "calls": calls,
        "format_retries": retries,
        "http_sends": sends,
        "tokens": tokens_json(tokens),
        "summary_words": word_stats(&summaries),
    })
}

/// Recall et précision d'une question : attendus cités / attendus, attendus cités / cités.
fn recall_precision(expected: &[String], cited: &[String]) -> (f64, f64) {
    let hits = expected.iter().filter(|e| cited.contains(e)).count() as u64;
    (
        ratio(hits, expected.len() as u64),
        ratio(hits, cited.len() as u64),
    )
}

/// Résultat d'un `query ask --json --verbose`.
struct AskRun {
    code: i32,
    stdout: Vec<u8>,
    stderr: String,
}

/// Mesures de `query ask` : une exécution par question, dans l'ordre du fichier.
fn ask_metrics(questions: &[Value], runs: &[AskRun]) -> Value {
    let (mut ok, mut without, mut not_found, mut error) = (0u64, 0u64, 0u64, 0u64);
    let (mut calls, mut retries, mut sends) = (0u64, 0u64, 0u64);
    let mut tokens: Option<[u64; 3]> = None;
    let (mut sum_recall, mut sum_precision) = (0.0, 0.0);
    let mut details = Vec::new();
    for (q, run) in questions.iter().zip(runs) {
        let expected: Vec<String> = q["expected"].as_array().map_or(vec![], |a| {
            a.iter()
                .filter_map(|e| e.as_str())
                .map(String::from)
                .collect()
        });
        let mut cited: Vec<String> = serde_json::from_slice::<Value>(&run.stdout)
            .ok()
            .and_then(|v| v["nodes"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|n| n["qualified"].as_str().map(String::from))
            .collect();
        cited.sort();
        let status = if run.code == 0 && !cited.is_empty() {
            ok += 1;
            "ok"
        } else if run.code == 0 && serde_json::from_slice::<Value>(&run.stdout).is_ok() {
            without += 1;
            "without_citation"
        } else if run.code != 0
            && last_line(run.stderr.as_bytes()) == "erreur: pas trouvé dans le graphe"
        {
            not_found += 1;
            "not_found"
        } else {
            error += 1;
            "error"
        };
        if status != "ok" {
            cited.clear();
        }
        let (recall, precision) = recall_precision(&expected, &cited);
        sum_recall += recall;
        sum_precision += precision;
        if let Some(t) = trace_lines(&run.stderr)
            .filter(|l| l.starts_with("appels:"))
            .last()
            .map(parse_totals)
        {
            calls += t.calls;
            retries += t.format_retries;
            sends += t.http_sends;
            if let Some(k) = t.tokens {
                let sum = tokens.get_or_insert([0; 3]);
                for i in 0..3 {
                    sum[i] += k[i];
                }
            }
        }
        details.push(serde_json::json!({
            "id": q["id"], "status": status, "cited": cited,
            "recall": num(recall), "precision": num(precision),
        }));
    }
    let n = questions.len() as f64;
    let mean = |x: f64| if n == 0.0 { 0.0 } else { x / n };
    serde_json::json!({
        "questions": questions.len(),
        "recall": num(mean(sum_recall)),
        "precision": num(mean(sum_precision)),
        "ok": ok, "without_citation": without, "not_found": not_found, "error": error,
        "calls": calls, "format_retries": retries, "http_sends": sends,
        "tokens": tokens_json(tokens),
        "details": details,
    })
}

/// Nœuds demandés et couverts de l'export : `(id, résumé, fichier)`.
fn covered_nodes(export: &Value) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for n in export["nodes"].as_array().map_or(&[][..], |a| &a[..]) {
        let id = n["id"].as_str().unwrap_or("");
        if id.starts_with("package:") || id.starts_with("key:") {
            continue;
        }
        if let (Some(file), Some(s)) = (n["file"].as_str(), n["summary"].as_str()) {
            if !s.is_empty() {
                out.push((id.to_string(), s.to_string(), file.to_string()));
            }
        }
    }
    out
}

const JUDGE_QUESTION: &str = "Note de 1 à 5 ce résumé d'un élément de code : 5 = exact, utile et concis ; 1 = faux ou vide. Mets la note seule, un chiffre, dans « answer », et une liste vide dans « nodes ».";

/// Contexte d'un appel du juge : identifiant, résumé, 6 000 premiers caractères du fichier.
fn judge_context(id: &str, summary: &str, file_text: &str) -> String {
    let head: String = file_text.chars().take(6000).collect();
    format!("Élément : {id}\nRésumé : {summary}\nFichier :\n{head}")
}

/// Juge : note de 1 à 5 de chacun des 20 résumés de plus petit `BLAKE3(id)`.
fn judge(
    llm: &dyn nephila::llm::Llm,
    summaries: &[(String, String, String)],
    corpus: &Path,
) -> Value {
    let mut sample: Vec<(blake3::Hash, &(String, String, String))> = summaries
        .iter()
        .map(|s| (blake3::hash(s.0.as_bytes()), s))
        .collect();
    sample.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    sample.truncate(20);
    let mut details = Vec::new();
    for (_, (id, summary, file)) in &sample {
        let text = fs::read(corpus.join(file))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let score = match llm.ask(JUDGE_QUESTION, &judge_context(id, summary, &text)) {
            Ok(nephila::llm::AskReply::Found { answer, .. }) => {
                let a = answer.trim();
                a.parse::<u8>()
                    .ok()
                    .filter(|n| (1..=5).contains(n) && a.bytes().all(|b| b.is_ascii_digit()))
            }
            _ => None,
        };
        details.push((id.clone(), score));
    }
    details.sort();
    let scores: Vec<u64> = details.iter().filter_map(|d| d.1.map(u64::from)).collect();
    let mean = if scores.is_empty() {
        Value::Null
    } else {
        num(scores.iter().sum::<u64>() as f64 / scores.len() as f64)
    };
    serde_json::json!({
        "model": llm.model(),
        "sample": details.len(),
        "mean": mean,
        "unreadable": details.len() - scores.len(),
        "details": details.iter()
            .map(|(node, score)| serde_json::json!({"node": node, "score": score}))
            .collect::<Vec<_>>(),
    })
}

fn last_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(stderr vide)")
        .to_string()
}

/// Une passe : `build --enrich-code --verbose`, `export`, les questions, le juge, rapport écrit
/// en une fois.
fn run_pass(p: &Params) -> Res<Value> {
    if !p.config.is_file() {
        return Err(format!(
            "NEPHILA_EVAL_CONFIG : fichier absent : {}",
            p.config.display()
        ));
    }
    let judge_llm = match &p.judge_config {
        None => None,
        Some(path) => Some(
            nephila::config::Config::load_from(path)
                .and_then(|c| nephila::llm::build_llm(&c))
                .map_err(|e| format!("NEPHILA_EVAL_JUDGE_CONFIG : {}: {e}", path.display()))?,
        ),
    };
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let repo = tmp.path().join("repo");
    copy_tree(&p.corpus, &repo);
    let nephila = |args: &[&str]| {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("NEPHILA_CONFIG", &p.config)
            .envs(p.env.iter().cloned())
            .output()
            .map_err(|e| format!("nephila n'a pas pu être lancé : {e}"))
    };
    let v = Command::new(cargo_bin("nephila"))
        .arg("--version")
        .output()
        .map_err(|e| e.to_string())?;
    let version = String::from_utf8_lossy(&v.stdout)
        .split_whitespace()
        .last()
        .unwrap_or("")
        .to_string();

    let build = nephila(&["build", "--enrich-code", "--verbose"])?;
    let stderr = String::from_utf8_lossy(&build.stderr).into_owned();
    let code = build.status.code().unwrap_or(-1);
    let mut export = serde_json::json!({"nodes": []});
    let mut failure = None;
    if code == 0 {
        let e = nephila(&["export", "--output", "-"])?;
        if e.status.success() {
            export = serde_json::from_slice(&e.stdout).map_err(|e| format!("export : {e}"))?;
        } else {
            failure = Some(format!("export a échoué : {}", last_line(&e.stderr)));
        }
    } else {
        failure = Some(format!(
            "build a échoué (code {code}) : {}",
            last_line(&build.stderr)
        ));
    }

    let config_line = trace_lines(&stderr).find(|l| l.starts_with("config: llm="));
    let field = |key: &str| {
        config_line
            .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix(key)))
            .map_or(Value::Null, Value::from)
    };
    let mut enrich = enrich_metrics(&export, &stderr);
    enrich["exit_code"] = code.into();
    let questions: Value =
        serde_json::from_slice(&fs::read(&p.questions).map_err(|e| e.to_string())?)
            .map_err(|e| format!("questions illisibles : {e}"))?;
    let (mut ask, mut judged) = (Value::Null, Value::Null);
    if failure.is_none() {
        let list = questions.as_array().cloned().unwrap_or_default();
        let mut runs = Vec::new();
        for q in &list {
            let o = nephila(&[
                "query",
                "ask",
                "--json",
                "--verbose",
                q["question"].as_str().unwrap_or(""),
            ])?;
            runs.push(AskRun {
                code: o.status.code().unwrap_or(-1),
                stdout: o.stdout,
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            });
        }
        ask = ask_metrics(&list, &runs);
        if let Some(llm) = &judge_llm {
            judged = judge(llm.as_ref(), &covered_nodes(&export), &p.corpus);
        }
    }
    let report = serde_json::json!({
        "schema": 1,
        "version": version,
        "provider": field("llm="),
        "model": field("model="),
        "prompt_versions": {
            "code": nephila::llm::enrich::CODE_PROMPT_VERSION,
            "build": nephila::llm::enrich::BUILD_PROMPT_VERSION,
            "data": nephila::llm::enrich::DATA_PROMPT_VERSION,
            "markdown": nephila::llm::enrich::MARKDOWN_PROMPT_VERSION,
            "ask": nephila::llm::ask::ASK_PROMPT_VERSION,
        },
        "corpus": {
            "hash": bench_hash(&p.corpus, &p.questions),
            "files": files_under(&p.corpus).len(),
            "questions": questions.as_array().map_or(0, Vec::len),
        },
        "enrich": enrich,
        "ask": ask,
        "judge": judged,
    });
    if let Some(dir) = p.out.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    let tmp_out = PathBuf::from(format!("{}.tmp", p.out.display()));
    fs::write(&tmp_out, text + "\n").map_err(|e| format!("{} : {e}", tmp_out.display()))?;
    fs::rename(&tmp_out, &p.out).map_err(|e| format!("{} : {e}", p.out.display()))?;
    match failure {
        Some(f) => Err(f),
        None => Ok(report),
    }
}

/// Passe réelle, lancée à la main par la commande `eval` du plan : appels payants.
#[test]
#[ignore]
fn eval_passe() {
    let params = params_from_env(&|k| std::env::var(k).ok()).unwrap_or_else(|e| panic!("{e}"));
    let report = run_pass(&params).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("rapport : {}\n{report:#}", params.out.display());
}

// ---- Tests de la passe --------------------------------------------------------------------

/// Fixtures du simulé pour chaque nœud demandé du corpus, tirées d'un export `--no-llm`.
fn write_fixtures(env: &Env, extra: &str) {
    for n in env.build_and_export() {
        let id = n["id"].as_str().unwrap();
        let Some(file) = n["file"].as_str() else {
            continue;
        };
        if id.starts_with("package:") || id.starts_with("key:") {
            continue;
        }
        let path = env.mock.join(format!("{file}.json"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut fixture: Value = fs::read_to_string(&path)
            .map(|t| serde_json::from_str(&t).unwrap())
            .unwrap_or_else(|_| serde_json::json!({"version": 2, "summaries": [], "concepts": []}));
        let short = n["short"].as_str().unwrap();
        fixture["summaries"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "node": id, "summary": format!("Résumé de {short} {extra}")
            }));
        fs::write(path, fixture.to_string()).unwrap();
    }
}

fn mock_params(env: &Env, out: &Path) -> Params {
    Params {
        config: env.config.clone(),
        corpus: eval_dir().join("corpus"),
        questions: eval_dir().join("questions.json"),
        out: out.to_path_buf(),
        env: vec![("NEPHILA_MOCK_DIR".into(), env.mock.to_str().unwrap().into())],
        judge_config: None,
    }
}

/// Petit corpus de documents `a.md`, `b.md`, `c.md` et fichier de questions vide.
struct Mini {
    tmp: tempfile::TempDir,
}

impl Mini {
    fn new() -> Mini {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("corpus")).unwrap();
        for n in ["a", "b", "c"] {
            fs::write(
                tmp.path().join(format!("corpus/{n}.md")),
                format!("# {n}\n\nTexte {n}.\n"),
            )
            .unwrap();
        }
        fs::write(tmp.path().join("questions.json"), "[]").unwrap();
        Mini { tmp }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.tmp.path().join(p)
    }

    fn params(&self, config: &str, env: Vec<(String, String)>) -> Params {
        fs::write(self.path("config.toml"), config).unwrap();
        fs::create_dir_all(self.path("mock")).unwrap();
        let mut all = vec![(
            "NEPHILA_MOCK_DIR".to_string(),
            self.path("mock").to_str().unwrap().to_string(),
        )];
        all.extend(env);
        Params {
            config: self.path("config.toml"),
            corpus: self.path("corpus"),
            questions: self.path("questions.json"),
            out: self.path("out/report.json"),
            env: all,
            judge_config: None,
        }
    }
}

const MINI_MOCK: &str = "provider = \"mock\"\nmodel = \"mock\"\nbatch_files = 1\nconcurrency = 1\n";

#[test]
fn req221_ac1_eval_passe_is_ignored_and_has_a_long_timeout() {
    let exe = std::env::current_exe().unwrap();
    let list = Command::new(&exe)
        .args(["--list", "--ignored"])
        .output()
        .unwrap();
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(listed.contains("eval_passe: test"), "{listed}");
    // Lancé sans `--ignored`, il n'est pas exécuté.
    let run = Command::new(&exe)
        .args(["eval_passe", "--exact"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{out}");
    assert!(
        out.contains("1 ignored") && out.contains("0 passed"),
        "{out}"
    );
    // Ni le filtre `perf`, ni le binaire `perf`.
    assert!(!"eval_passe".contains("perf"));
    assert_eq!(Path::new(file!()).file_stem().unwrap(), "eval");

    let toml: toml::Value = toml::from_str(
        &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".config/nextest.toml"))
            .unwrap(),
    )
    .unwrap();
    let profile = &toml["profile"]["default"];
    let timeout = |v: &toml::Value| {
        (
            v["slow-timeout"]["period"].as_str().unwrap().to_string(),
            v["slow-timeout"]["terminate-after"].as_integer().unwrap(),
        )
    };
    assert_eq!(timeout(profile), ("60s".to_string(), 1));
    let by_filter: HashMap<&str, (String, i64)> = profile["overrides"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["filter"].as_str().unwrap(), timeout(o)))
        .collect();
    assert_eq!(by_filter["test(/^eval_/)"], ("1800s".to_string(), 1));
    assert_eq!(by_filter["kind(lib) | kind(bin)"], ("10s".to_string(), 1));
    assert_eq!(by_filter["test(/^perf_/)"], ("300s".to_string(), 1));
    assert_eq!(by_filter.len(), 3);
}

#[test]
fn req221_ac2_missing_variable_or_file_fails_before_any_process() {
    let err = params_from_env(&|_| None).err().unwrap();
    assert!(err.contains("NEPHILA_EVAL_CONFIG"), "{err}");

    let mini = Mini::new();
    let log = mini.path("mock.log");
    let mut p = mini.params(MINI_MOCK, vec![]);
    p.env
        .push(("NEPHILA_MOCK_LOG".into(), log.to_str().unwrap().into()));
    p.config = mini.path("absent.toml");
    let err = run_pass(&p).err().unwrap();
    assert!(err.contains("NEPHILA_EVAL_CONFIG"), "{err}");
    assert!(!log.exists() && !p.out.exists());
}

#[test]
fn req221_ac3_report_has_the_exact_shape() {
    let env = Env::new();
    write_fixtures(&env, "");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("nouveau/dossier/rapport.json");
    let report = run_pass(&mock_params(&env, &out)).unwrap();
    let written: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    assert_eq!(written, report);
    let mut keys: Vec<&String> = written.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "ask",
            "corpus",
            "enrich",
            "judge",
            "model",
            "prompt_versions",
            "provider",
            "schema",
            "version"
        ]
    );
    assert_eq!(written["schema"], 1);
    assert_eq!(written["provider"], "mock");
    assert_eq!(written["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(written["corpus"]["hash"], CORPUS_HASH);
    assert_eq!(written["corpus"]["files"], corpus_files().len());
    assert_eq!(written["corpus"]["questions"], questions().len());
    let pv = &written["prompt_versions"];
    assert_eq!(pv["code"], nephila::llm::enrich::CODE_PROMPT_VERSION);
    assert_eq!(pv["ask"], nephila::llm::ask::ASK_PROMPT_VERSION);
    assert_eq!(written["ask"]["questions"], questions().len());
    assert!(written["judge"].is_null());
    let enrich = &written["enrich"];
    assert_eq!(enrich["exit_code"], 0);
    assert!(enrich["nodes_covered"].as_u64().unwrap() > 0);
    assert_eq!(enrich["coverage"], 1);

    // Chemin par défaut : `<manifeste>/target/eval/<version>.json`.
    let p =
        params_from_env(&|k| (k == "NEPHILA_EVAL_CONFIG").then(|| "c.toml".to_string())).unwrap();
    assert_eq!(
        p.out,
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/eval")
            .join(format!("{}.json", env!("CARGO_PKG_VERSION")))
    );
    let p = params_from_env(&|k| match k {
        "NEPHILA_EVAL_CONFIG" => Some("c.toml".into()),
        "NEPHILA_EVAL_OUT" => Some("/x/r.json".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(p.out, PathBuf::from("/x/r.json"));
}

#[test]
fn req221_ac4_two_passes_give_identical_reports() {
    let env = Env::new();
    write_fixtures(&env, "");
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.json"), tmp.path().join("b.json"));
    run_pass(&mock_params(&env, &a)).unwrap();
    run_pass(&mock_params(&env, &b)).unwrap();
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
}

#[test]
fn req221_ac5_failed_build_stops_before_any_question() {
    let env = Env::new();
    write_fixtures(&env, "");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("r.json");
    let log = tmp.path().join("mock.log");
    let mut p = mock_params(&env, &out);
    p.env.push(("NEPHILA_MOCK_INVALID_KEY".into(), "1".into()));
    p.env
        .push(("NEPHILA_MOCK_LOG".into(), log.to_str().unwrap().into()));
    let err = run_pass(&p).err().unwrap();
    assert!(
        err.contains("build a échoué") && err.contains("clé d'API refusée"),
        "{err}"
    );
    let report: Value = serde_json::from_slice(&fs::read(&out).unwrap()).unwrap();
    assert_ne!(report["enrich"]["exit_code"], 0);
    assert!(report["ask"].is_null() && report["judge"].is_null());
    let journal = fs::read_to_string(&log).unwrap_or_default();
    assert!(!journal.contains("_ask"), "{journal}");
}

#[test]
fn req221_ac6_report_keeps_no_answer_and_no_key() {
    let mini = Mini::new();
    fs::write(
        mini.path("questions.json"),
        r#"[{"id":"Q01","question":"Que dit a.md ?","expected":["a.md"]}]"#,
    )
    .unwrap();
    let chat = |content: &str| {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": content},
                "finish_reason": "stop"}]
        })
        .to_string()
    };
    let enrich = chat(
        r#"{"version":2,"summaries":[{"node":"document:a.md","summary":"SECRET_REPONSE_42"}],"concepts":[]}"#,
    );
    let ask = chat(r#"{"found":true,"answer":"SECRET_REPONSE_42","nodes":[]}"#);
    let judged = chat(r#"{"found":true,"answer":"SECRET_REPONSE_42","nodes":[]}"#);
    let mut replies: Vec<(u16, String)> = (0..3).map(|_| (200, enrich.clone())).collect();
    replies.push((200, ask));
    replies.push((200, judged));
    let server = FakeServer::start(replies);
    let config = format!(
        "provider = \"openai\"\nmodel = \"m\"\napi_key_env = \"NEPHILA_EVAL_TEST_KEY\"\nbase_url = \"{}\"\nbatch_files = 1\nconcurrency = 1\n",
        server.base_url
    );
    let mut p = mini.params(
        &config,
        vec![("NEPHILA_EVAL_TEST_KEY".into(), "sk-test-SECRET".into())],
    );
    // Le juge parle au même serveur, sans clé.
    fs::write(
        mini.path("judge.toml"),
        format!(
            "provider = \"openai\"\nmodel = \"juge\"\nbase_url = \"{}\"\n",
            server.base_url
        ),
    )
    .unwrap();
    p.judge_config = Some(mini.path("judge.toml"));
    let report = run_pass(&p).unwrap();
    assert_eq!(
        report["enrich"]["nodes_covered"], 1,
        "le résumé a bien circulé"
    );
    let a = &report["ask"];
    assert_eq!(
        a["ok"].as_u64().unwrap() + a["without_citation"].as_u64().unwrap(),
        1,
        "{a}"
    );
    assert_eq!(a["calls"], 1, "la réponse d'ask a bien circulé");
    assert_eq!(report["judge"]["sample"], 1);
    assert_eq!(
        report["judge"]["unreadable"], 1,
        "la réponse du juge a bien circulé"
    );
    let text = fs::read_to_string(&p.out).unwrap();
    assert!(!text.contains("SECRET_REPONSE_42") && !text.contains("sk-test-SECRET"));
}

/// Stderr fixe où deux fils se croisent (AC-222-1).
const CROSSING: &str = "\
trace: [0.001s] llm enrich départ a [1/1] 10 car.
trace: [0.001s] llm enrich départ b [1/1] 10 car.
trace: [0.002s] llm enrich fin b [1/1] ok (1 résumés, 0 concepts) 3 ms
trace: [0.003s] llm enrich fin a [1/1] erreur 3 ms : réponse LLM non conforme au schéma
trace: [0.003s] llm enrich départ a [1/1] 10 car. (essai 2/3)
trace: [0.004s] llm enrich fin a [1/1] ok (1 résumés, 0 concepts) 3 ms
trace: [0.005s] llm enrich départ lot c +1 [1/1] 20 car.
trace: [0.006s] llm enrich fin lot c +1 [1/1] tronquée 3 ms : réponse tronquée
";

#[test]
fn req222_ac1_first_try_counts_crossing_threads() {
    assert_eq!(parse_calls(CROSSING), (3, 1));
    let m = enrich_metrics(&serde_json::json!({"nodes": []}), CROSSING);
    assert_eq!(m["first_calls"], 3);
    assert_eq!(m["first_try_ok"], 1);
    assert_eq!(m["first_try_rate"], 0.3333);
}

#[test]
fn req222_ac2_mock_malformed_costs_one_retry() {
    let mini = Mini::new();
    let p = mini.params(
        MINI_MOCK,
        vec![("NEPHILA_MOCK_MALFORMED".into(), "a.md:1".into())],
    );
    let e = &run_pass(&p).unwrap()["enrich"];
    assert_eq!(e["first_calls"], 3);
    assert_eq!(e["first_try_ok"], 2);
    assert_eq!(e["calls"], 4);
    assert_eq!(e["format_retries"], 1);
    assert_eq!(e["http_sends"], 4);
    assert!(e["tokens"].is_null());
}

#[test]
fn req222_ac3_tokens_come_from_the_calls_line() {
    let mini = Mini::new();
    let reply = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content":
            r#"{"version": 2, "summaries": [], "concepts": []}"#}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1000, "prompt_tokens_details": {"cached_tokens": 200},
                  "completion_tokens": 50}
    })
    .to_string();
    let server = FakeServer::start((0..3).map(|_| (200, reply.clone())).collect());
    let config = format!(
        "provider = \"openai\"\nmodel = \"m\"\napi_key_env = \"NEPHILA_EVAL_TEST_KEY\"\nbase_url = \"{}\"\nbatch_files = 1\nconcurrency = 1\n",
        server.base_url
    );
    let p = mini.params(
        &config,
        vec![("NEPHILA_EVAL_TEST_KEY".into(), "sk-test".into())],
    );
    let e = &run_pass(&p).unwrap()["enrich"];
    assert_eq!(e["calls"], 3);
    assert_eq!(
        e["tokens"],
        serde_json::json!({"input": 3000, "cache": 600, "output": 150})
    );
    // Lecture d'une ligne `appels:` sans jetons connus.
    let t = parse_totals(
        "appels: 2 appels, 1 nouveaux essais de format, 4 envois HTTP, jetons : inconnus",
    );
    assert_eq!(
        (t.calls, t.format_retries, t.http_sends, t.tokens),
        (2, 1, 4, None)
    );
}

#[test]
fn req222_ac4_coverage_and_word_statistics() {
    let export = serde_json::json!({"nodes": [
        {"id": "type:A", "file": "A.java", "summary": "un deux"},
        {"id": "method:A.m", "file": "A.java", "summary": "trois"},
        {"id": "type:B", "file": "B.java", "summary": ""},
        {"id": "type:C", "file": "C.java", "summary": null},
        {"id": "package:p", "file": "A.java", "summary": "ignoré"},
        {"id": "key:k", "file": "pom.xml", "summary": "ignoré"},
        {"id": "concept:x", "file": null, "summary": "ignoré"},
    ]});
    let m = enrich_metrics(&export, "");
    assert_eq!(m["nodes_requested"], 4);
    assert_eq!(m["nodes_covered"], 2);
    assert_eq!(m["coverage"], 0.5);
    assert_eq!(m["summary_words"]["count"], 2);
    // Sans ligne `appels:` : tout à 0 et jetons nuls.
    assert_eq!(
        (m["calls"].clone(), m["first_calls"].clone()),
        (0.into(), 0.into())
    );
    assert!(m["tokens"].is_null());

    let words =
        |n: &[usize]| -> Vec<String> { n.iter().map(|k| vec!["mot"; *k].join(" ")).collect() };
    assert_eq!(
        word_stats(&words(&[1, 2, 3, 4, 10])),
        serde_json::json!({"count": 5, "mean": 4, "median": 3, "p90": 10, "max": 10})
    );
    assert_eq!(
        word_stats(&[]),
        serde_json::json!({"count": 0, "mean": 0, "median": 0, "p90": 0, "max": 0})
    );
    let none = enrich_metrics(&serde_json::json!({"nodes": []}), "");
    assert_eq!(none["coverage"], 0);
    assert_eq!(none["first_try_rate"], 0);
    assert_eq!(none["nodes_requested"], 0);
}

#[test]
fn req222_ac5_mock_without_fixture_covers_nothing() {
    let env = Env::new();
    let tmp = tempfile::tempdir().unwrap();
    let report = run_pass(&mock_params(&env, &tmp.path().join("r.json"))).unwrap();
    let e = &report["enrich"];
    assert_eq!(e["exit_code"], 0);
    assert_eq!(e["coverage"], 0);
    assert_eq!(e["nodes_covered"], 0);
    assert!(e["nodes_requested"].as_u64().unwrap() > 0);
    assert_eq!(e["first_try_rate"], 1);
}

// ---------------------------------------------------------------------------------------------
// Mesures de `query ask` (REQ-223)
// ---------------------------------------------------------------------------------------------

#[test]
fn req223_ac1_recall_and_precision_on_fixed_cases() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        recall_precision(&v(&["A", "B"]), &v(&["A", "C"])),
        (0.5, 0.5)
    );
    assert_eq!(
        recall_precision(&v(&["A", "B"]), &v(&["A", "B"])),
        (1.0, 1.0)
    );
    assert_eq!(recall_precision(&v(&["A", "B"]), &v(&[])), (0.0, 0.0));
}

fn ask_fixture(env: &Env, json: &str) {
    fs::write(env.mock.join("_ask.json"), json).unwrap();
}

#[test]
fn req223_ac2_cited_node_is_ok_and_hidden_node_is_without_citation() {
    let env = Env::new();
    ask_fixture(
        &env,
        r#"{"found":true,"answer":"ok","nodes":["org.exemple.mediatheque.service.EmpruntService"]}"#,
    );
    let tmp = tempfile::tempdir().unwrap();
    let report = run_pass(&mock_params(&env, &tmp.path().join("r.json"))).unwrap();
    let d = report["ask"]["details"].as_array().unwrap();
    assert_eq!(d.len(), questions().len());
    assert_eq!(d[0]["id"], "Q01");
    assert_eq!(d[0]["status"], "ok");
    assert!(d[0]["recall"].as_f64().unwrap() > 0.0);
    assert_eq!(
        d[0]["cited"],
        serde_json::json!(["org.exemple.mediatheque.service.EmpruntService"])
    );
    let hidden = d
        .iter()
        .find(|q| q["status"] == "without_citation")
        .expect("une question dont le contexte ne montre pas le nœud");
    assert_eq!(hidden["recall"], 0);
    assert_eq!(hidden["precision"], 0);
    assert_eq!(hidden["cited"], serde_json::json!([]));
    let a = &report["ask"];
    assert_eq!(
        a["ok"].as_u64().unwrap()
            + a["without_citation"].as_u64().unwrap()
            + a["not_found"].as_u64().unwrap()
            + a["error"].as_u64().unwrap(),
        questions().len() as u64
    );
    assert!(a["recall"].as_f64().unwrap() > 0.0);
}

#[test]
fn req223_ac3_not_found_and_errors_do_not_stop_the_pass() {
    let env = Env::new();
    ask_fixture(&env, r#"{"found":false}"#);
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("r.json");
    let report = run_pass(&mock_params(&env, &out)).unwrap();
    let a = &report["ask"];
    let n = questions().len();
    assert_eq!(a["not_found"], n);
    assert_eq!(
        (a["recall"].clone(), a["precision"].clone()),
        (0.into(), 0.into())
    );
    assert!(out.is_file());

    let mut p = mock_params(&env, &out);
    p.env
        .push(("NEPHILA_MOCK_MALFORMED".into(), "_ask:99".into()));
    let report = run_pass(&p).unwrap();
    let a = &report["ask"];
    assert_eq!(a["error"], n);
    assert!(a["calls"].as_u64().unwrap() > n as u64, "{a}");
    assert_eq!(a["recall"], 0);
    assert!(out.is_file());
}

#[test]
fn req223_ac4_ask_calls_and_tokens_are_summed() {
    let env = Env::new();
    env.build_and_export();
    let reply = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content":
            r#"{"found":true,"answer":"ok","nodes":[]}"#}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 900, "prompt_tokens_details": {"cached_tokens": 100},
                  "completion_tokens": 40}
    })
    .to_string();
    let server = FakeServer::start((0..20).map(|_| (200, reply.clone())).collect());
    fs::write(
        &env.config,
        format!(
            "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"{}\"\n",
            server.base_url
        ),
    )
    .unwrap();
    let qs: Vec<Value> = questions().into_iter().take(20).collect();
    assert_eq!(qs.len(), 20);
    let runs: Vec<AskRun> = qs
        .iter()
        .map(|q| {
            let o = env.run(&[
                "query",
                "ask",
                "--json",
                "--verbose",
                q["question"].as_str().unwrap(),
            ]);
            AskRun {
                code: o.status.code().unwrap(),
                stdout: o.stdout,
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            }
        })
        .collect();
    let a = ask_metrics(&qs, &runs);
    assert_eq!(a["calls"], 20, "{a}");
    assert_eq!(
        a["tokens"],
        serde_json::json!({"input": 18000, "cache": 2000, "output": 800})
    );
}

// ---------------------------------------------------------------------------------------------
// Juge (REQ-224)
// ---------------------------------------------------------------------------------------------

fn judge_inputs(n: usize) -> Vec<(String, String, String)> {
    (0..n)
        .rev()
        .map(|i| {
            (
                format!("type:T{i}"),
                format!("Résumé {i}"),
                "absent.java".to_string(),
            )
        })
        .collect()
}

fn judge_mock(dir: &Path, reply: &str) -> nephila::llm::mock::MockLlm {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("_ask.json"), reply).unwrap();
    nephila::llm::mock::MockLlm::new(nephila::llm::mock::MockOptions {
        dir: Some(dir.to_path_buf()),
        ..Default::default()
    })
}

#[test]
fn req224_ac1_no_judge_config_means_no_judge_call() {
    let env = Env::new();
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("mock.log");
    let mut p = mock_params(&env, &tmp.path().join("r.json"));
    p.env
        .push(("NEPHILA_MOCK_LOG".into(), log.to_str().unwrap().into()));
    let report = run_pass(&p).unwrap();
    assert!(report["judge"].is_null());
    let asks = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("_ask\t"))
        .count();
    assert!(asks <= questions().len(), "{asks} appels _ask");
}

#[test]
fn req224_ac2_sample_is_the_twenty_smallest_hashes() {
    let tmp = tempfile::tempdir().unwrap();
    let llm = judge_mock(
        &tmp.path().join("m"),
        r#"{"found":true,"answer":"4","nodes":[]}"#,
    );
    let inputs = judge_inputs(25);
    let j = judge(&llm, &inputs, tmp.path());
    assert_eq!(
        (
            j["sample"].clone(),
            j["mean"].clone(),
            j["unreadable"].clone()
        ),
        (20.into(), 4.into(), 0.into())
    );
    let mut by_hash: Vec<_> = inputs
        .iter()
        .map(|i| (blake3::hash(i.0.as_bytes()), i.0.clone()))
        .collect();
    by_hash.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut want: Vec<String> = by_hash.into_iter().take(20).map(|x| x.1).collect();
    want.sort();
    let got: Vec<String> = j["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["node"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, want);
    assert_eq!(
        judge(&llm, &inputs, tmp.path()),
        j,
        "mêmes nœuds à chaque lancement"
    );
    assert_eq!(judge(&llm, &judge_inputs(3), tmp.path())["sample"], 3);
}

#[test]
fn req224_ac3_unreadable_answers_have_no_score() {
    let tmp = tempfile::tempdir().unwrap();
    for (i, reply) in [
        r#"{"found":true,"answer":"très bien","nodes":[]}"#,
        r#"{"found":true,"answer":"0","nodes":[]}"#,
        r#"{"found":true,"answer":"6","nodes":[]}"#,
        r#"{"found":true,"answer":"4.5","nodes":[]}"#,
        r#"{"found":false}"#,
    ]
    .iter()
    .enumerate()
    {
        let llm = judge_mock(&tmp.path().join(format!("m{i}")), reply);
        let j = judge(&llm, &judge_inputs(2), tmp.path());
        assert_eq!(j["unreadable"], 2, "{reply}");
        assert!(j["mean"].is_null(), "{reply}");
        assert!(j["details"][0]["score"].is_null());
    }
    let llm = nephila::llm::mock::MockLlm::new(nephila::llm::mock::MockOptions {
        invalid_key: true,
        ..Default::default()
    });
    let j = judge(&llm, &judge_inputs(2), tmp.path());
    assert_eq!(j["unreadable"], 2);
    assert!(j["mean"].is_null());

    let llm = judge_mock(
        &tmp.path().join("blancs"),
        r#"{"found":true,"answer":" 3 ","nodes":[]}"#,
    );
    let j = judge(&llm, &judge_inputs(2), tmp.path());
    assert_eq!(
        (j["mean"].clone(), j["unreadable"].clone()),
        (3.into(), 0.into())
    );
}

#[test]
fn req224_ac4_missing_judge_config_fails_before_any_call() {
    let mini = Mini::new();
    let log = mini.path("mock.log");
    let mut p = mini.params(MINI_MOCK, vec![]);
    p.env
        .push(("NEPHILA_MOCK_LOG".into(), log.to_str().unwrap().into()));
    p.judge_config = Some(mini.path("juge-absent.toml"));
    let err = run_pass(&p).err().unwrap();
    assert!(err.contains("NEPHILA_EVAL_JUDGE_CONFIG"), "{err}");
    assert!(!log.exists() && !p.out.exists());
    let got = params_from_env(&|k| match k {
        "NEPHILA_EVAL_CONFIG" => Some("c.toml".into()),
        "NEPHILA_EVAL_JUDGE_CONFIG" => Some("j.toml".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(got.judge_config, Some(PathBuf::from("j.toml")));
}

#[test]
fn req224_ac5_context_holds_id_summary_and_at_most_6000_chars() {
    let text = "é".repeat(7000);
    let c = judge_context("type:T", "Un résumé", &text);
    assert!(c.starts_with("Élément : type:T\nRésumé : Un résumé\nFichier :\n"));
    let file = c.split_once("Fichier :\n").unwrap().1;
    assert_eq!(file.chars().count(), 6000);
    assert!(file.chars().all(|ch| ch == 'é'));
    let short = judge_context("i", "r", "abc");
    assert!(short.ends_with("Fichier :\nabc"));
}

// ---------------------------------------------------------------------------------------------
// Documentation du banc (REQ-225)
// ---------------------------------------------------------------------------------------------

#[test]
fn req225_ac1_bench_readme_names_variables_and_report_keys() {
    let text = fs::read_to_string(eval_dir().join("README.md")).unwrap();
    for w in [
        "NEPHILA_EVAL_CONFIG",
        "NEPHILA_EVAL_OUT",
        "NEPHILA_EVAL_JUDGE_CONFIG",
        "eval_passe",
        "schema",
        "version",
        "provider",
        "model",
        "prompt_versions",
        "corpus",
        "enrich",
        "ask",
        "judge",
    ] {
        assert!(text.contains(&format!("`{w}`")), "{w}");
    }
    let lower = text.to_lowercase();
    assert!(lower.contains("appels réels") && lower.contains("payant"));
}
