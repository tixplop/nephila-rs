//! Exemples de configuration `examples/config.toml` et `examples/nephila.toml` confrontés au code
//! (REQ-041, DEC-051).
#![allow(non_snake_case)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use nephila::config::{
    AskOverrides, Config, ProjectSettings, Provider, ASK_CAPS, ASK_SETTINGS, DEFAULT_BATCH_FILES,
    DEFAULT_CONCURRENCY, DEFAULT_FORMAT_RETRIES, DEFAULT_MAX_CHUNK_CHARS, DEFAULT_TIMEOUT_SECS,
    PROJECT_CAPS, PROJECT_SETTINGS,
};
use nephila::filter::{FileType, Filters, ProjectFile, ProjectFilters};

const MOCK_SOURCE: &str = include_str!("../src/llm/mock.rs");

fn example(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} illisible : {e}", p.display()))
}

fn is_key(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

/// Clé écrite par la ligne (`<clé> =` ou `# <clé> =`), le cas échéant.
fn key_of(line: &str) -> Option<&str> {
    let body = line.strip_prefix("# ").unwrap_or(line);
    let (key, _) = body.split_once(" =")?;
    is_key(key).then_some(key)
}

/// Ligne qui ouvre la section `[ask]`, désactivée dans les exemples (REQ-164).
const ASK_HEADER: &str = "# [ask]";

/// Clés du premier niveau écrites dans le texte (lignes actives ou désactivées), sans doublon,
/// plus `ask` quand la ligne `# [ask]` est écrite ; les clés qui la suivent sont celles de la
/// section (`ask_keys_written`).
fn keys_written(text: &str) -> BTreeSet<String> {
    let mut keys: BTreeSet<String> = text
        .lines()
        .take_while(|l| *l != ASK_HEADER)
        .filter_map(key_of)
        .map(String::from)
        .collect();
    if text.lines().any(|l| l == ASK_HEADER) {
        keys.insert("ask".to_string());
    }
    keys
}

/// Clés écrites après la ligne `# [ask]`, c'est-à-dire dans la section.
fn ask_keys_written(text: &str) -> BTreeSet<String> {
    text.lines()
        .skip_while(|l| *l != ASK_HEADER)
        .filter_map(key_of)
        .map(String::from)
        .collect()
}

/// Clés acceptées par le chargeur, extraites de son erreur sur une clé inconnue (DEC-051).
fn accepted_keys() -> BTreeSet<String> {
    let err = Config::parse(
        "provider = \"mock\"\nzz_inconnue = 1\n",
        Path::new("config.toml"),
    )
    .expect_err("une clé inconnue doit être refusée")
    .to_string();
    let tail = err
        .split_once("expected one of")
        .unwrap_or_else(|| panic!("libellé d'erreur inattendu : {err}"))
        .1;
    let tail = tail.lines().next().unwrap_or("");
    let keys: BTreeSet<String> = tail
        .split('`')
        .skip(1)
        .step_by(2)
        .map(String::from)
        .collect();
    assert!(!keys.is_empty(), "aucune clé extraite de : {err}");
    keys
}

/// Lignes de `# --- <titre> ---` (incluse) jusqu'à la ligne `# --- ` suivante (exclue).
fn section(text: &str, title: &str) -> Vec<String> {
    let head = format!("# --- {title} ---");
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line == head {
            inside = true;
        } else if inside && line.starts_with("# --- ") {
            break;
        }
        if inside {
            out.push(line.to_string());
        }
    }
    assert!(!out.is_empty(), "section absente : {title}");
    out
}

/// `# <clé> = ` devient `<clé> = ` ; les autres lignes restent telles quelles.
fn reactivate(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| match l.strip_prefix("# ") {
            Some(rest) if key_of(l).is_some() || l == ASK_HEADER => rest.to_string(),
            _ => l.clone(),
        })
        .collect()
}

fn parse(text: &str) -> Config {
    Config::parse_with(text, Path::new("config.toml"), &|_| None)
        .unwrap_or_else(|e| panic!("{e}\n---\n{text}"))
}

fn join(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

const PROVIDER_SECTIONS: [(&str, Provider); 5] = [
    ("Anthropic", Provider::Anthropic),
    ("OpenAI", Provider::OpenAi),
    ("Modèle local (Ollama)", Provider::OpenAi),
    (
        "Autre serveur compatible OpenAI (llama.cpp, vLLM, LM Studio)",
        Provider::OpenAi,
    ),
    ("LLM simulé", Provider::Mock),
];

const COMMON: &str = "Options communes";

#[test]
fn REQ041_AC1_exemple_accepte_avec_anthropic_et_defauts() {
    let c = parse(&example("config.toml"));
    assert_eq!(c.provider, Provider::Anthropic);
    assert_eq!(c.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
    assert_eq!(c.base_url, None);
    assert_eq!(c.max_chunk_chars, DEFAULT_MAX_CHUNK_CHARS);
    assert_eq!(c.concurrency, DEFAULT_CONCURRENCY);
    assert_eq!(c.timeout_secs, DEFAULT_TIMEOUT_SECS);
    assert_eq!(c.batch_files, DEFAULT_BATCH_FILES);
    assert_eq!(c.format_retries, DEFAULT_FORMAT_RETRIES);
    assert_eq!(c.tls_ca_cert, None);
    assert!(!c.tls_insecure);
}

#[test]
fn REQ041_AC2_cles_de_l_exemple_egales_aux_cles_du_chargeur() {
    let written = keys_written(&example("config.toml"));
    let accepted = accepted_keys();
    let missing: Vec<_> = accepted.difference(&written).collect();
    let unknown: Vec<_> = written.difference(&accepted).collect();
    assert!(
        missing.is_empty(),
        "clés absentes de l'exemple : {missing:?}"
    );
    assert!(
        unknown.is_empty(),
        "clés refusées par le chargeur : {unknown:?}"
    );
}

#[test]
fn REQ041_AC3_chaque_section_fournisseur_seule_est_acceptee() {
    let text = example("config.toml");
    for (title, provider) in PROVIDER_SECTIONS {
        let lines = reactivate(&section(&text, title));
        let c = parse(&join(&lines));
        assert_eq!(c.provider, provider, "section {title}");
        if title.contains("Ollama") || title.starts_with("Autre serveur") {
            let url = c.base_url.expect("base_url de la section");
            assert!(!url.trim_end_matches('/').ends_with("/v1"), "{url}");
        }
    }
}

#[test]
fn REQ041_AC4_valeurs_par_defaut_documentees_egales_aux_valeurs_reelles() {
    let text = example("config.toml");
    // `effort_explicit` dit seulement si la clé est écrite (REQ-157) : la valeur, elle, est comparée.
    let reference = Config {
        effort_explicit: true,
        ..parse(&text)
    };
    let common = section(&text, COMMON);
    let without_tls_ca: Vec<String> = common
        .iter()
        .filter(|l| !l.starts_with("# tls_ca_cert ="))
        .cloned()
        .collect();
    let activated = reactivate(&without_tls_ca);
    let replaced: Vec<String> = text
        .lines()
        .map(String::from)
        .flat_map(|l| if common.contains(&l) { None } else { Some(l) })
        .chain(activated.clone())
        .collect();
    assert_eq!(
        Config {
            effort_explicit: true,
            ..parse(&join(&replaced))
        },
        reference
    );
    // Les lignes réactivées existent bien (le test ne passe pas à vide).
    let active: Vec<&str> = activated
        .iter()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| key_of(l))
        .collect();
    assert_eq!(active.len(), 13, "options communes réactivées : {active:?}");
}

#[test]
fn REQ041_AC4_tls_ca_cert_reactivee_avec_un_pem_est_acceptee() {
    let text = example("config.toml");
    let line = section(&text, COMMON)
        .into_iter()
        .find(|l| l.starts_with("# tls_ca_cert ="))
        .expect("ligne tls_ca_cert");
    let active = line.trim_start_matches("# ").to_string();
    let rel = active
        .split('"')
        .nth(1)
        .expect("chemin d'exemple entre guillemets")
        .to_string();
    let dir = tempfile::tempdir().unwrap();
    let pem = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tls/ca.pem");
    let target = dir.path().join(&rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(pem, &target).unwrap();
    let config_path = dir.path().join("config.toml");
    let with_ca = format!("{text}\n{active}\n");
    let c = Config::parse_with(&with_ca, &config_path, &|_| None).unwrap();
    assert_eq!(c.tls_ca_cert.as_deref(), Some(target.as_path()));
    assert!(!c.tls_ca_certs.is_empty());
}

/// Bloc de commentaires contigu qui précède la ligne `index`, plus son commentaire de fin.
fn doc_of(lines: &[&str], index: usize) -> String {
    let mut block = Vec::new();
    let mut i = index;
    while i > 0 && lines[i - 1].starts_with('#') {
        block.push(lines[i - 1]);
        i -= 1;
    }
    if let Some((_, trailing)) = lines[index].split_once(" #") {
        block.push(trailing);
    }
    block.join("\n").to_lowercase()
}

#[test]
fn REQ041_AC5_chaque_cle_est_documentee() {
    let text = example("config.toml");
    let lines: Vec<&str> = text.lines().collect();
    for key in keys_written(&text) {
        // `ask` est la section : ses clés sont documentées une à une (AC-164-1).
        if key == "ask" {
            continue;
        }
        let index = lines
            .iter()
            .position(|l| key_of(l) == Some(key.as_str()))
            .unwrap();
        let doc = doc_of(&lines, index);
        assert!(
            doc.contains("défaut :") || doc.contains("obligatoire"),
            "clé {key} sans « défaut : » ni « obligatoire » :\n{doc}"
        );
        for (k, option) in [
            ("batch_files", "--batch-files"),
            ("format_retries", "--format-retries"),
            ("enrich_code", "--enrich-code"),
        ] {
            if key == k {
                assert!(doc.contains(option), "clé {k} sans {option} :\n{doc}");
            }
        }
    }
}

/// Noms `NEPHILA_MOCK_<NOM>` cités dans `text`.
fn mock_vars(text: &str) -> BTreeSet<String> {
    const PREFIX: &str = "NEPHILA_MOCK_";
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        let len = after
            .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .unwrap_or(after.len());
        let name = after[..len].trim_end_matches('_');
        if !name.is_empty() {
            out.insert(format!("{PREFIX}{name}"));
        }
        rest = &after[len..];
    }
    out
}

#[test]
fn REQ041_AC6_variables_nommees() {
    let text = example("config.toml");
    for var in ["NEPHILA_CONFIG", "XDG_CONFIG_HOME"] {
        assert!(text.contains(var), "variable absente : {var}");
    }
    let in_code = mock_vars(MOCK_SOURCE);
    assert!(
        in_code.len() >= 8,
        "variables du code non extraites : {in_code:?}"
    );
    let cited = mock_vars(&text);
    let missing: Vec<_> = in_code.difference(&cited).collect();
    let unknown: Vec<_> = cited.difference(&in_code).collect();
    assert!(
        missing.is_empty(),
        "variables non documentées : {missing:?}"
    );
    assert!(
        unknown.is_empty(),
        "variables absentes du code : {unknown:?}"
    );
}

fn project_example_lines() -> Vec<String> {
    example("nephila.toml").lines().map(String::from).collect()
}

fn type_names_of_example() -> BTreeSet<String> {
    project_example_lines()
        .iter()
        .filter_map(|l| {
            let rest = l.strip_prefix("#   ")?;
            let (name, tail) = rest.split_once("  ")?;
            (is_key(name) && !tail.trim().is_empty()).then(|| name.to_string())
        })
        .collect()
}

fn repo_with_project_file(content: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("docs")).unwrap();
    fs::write(repo.join("README.md"), "# Projet\n\nUn texte.\n").unwrap();
    fs::write(repo.join("docs/guide.md"), "# Guide\n").unwrap();
    fs::write(repo.join(".nephila.toml"), content).unwrap();
    (tmp, repo)
}

#[test]
fn REQ041_AC7_fichier_de_projet_exemple_ne_filtre_rien() {
    let text = example("nephila.toml");
    let (tmp, repo) = repo_with_project_file(&text);
    assert_eq!(ProjectFilters::load(&repo).unwrap(), Filters::default());
    let loaded = ProjectFile::load(&repo).unwrap();
    assert_eq!(loaded.filters, Filters::default());
    assert_eq!(loaded.settings, ProjectSettings::default());
    let out = Command::cargo_bin("nephila")
        .unwrap()
        .env("NEPHILA_CONFIG", tmp.path().join("absent.toml"))
        .arg("-C")
        .arg(&repo)
        .args(["build", "--no-llm"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("build terminé"), "bilan absent : {stdout}");
    assert!(!stdout.contains("exclus"), "{stdout}");
}

#[test]
fn REQ041_AC7_lignes_reactivees_donnent_les_filtres_ecrits() {
    let lines = project_example_lines();
    let activated = join(&reactivate(&lines));
    let (_tmp, repo) = repo_with_project_file(&activated);
    let f = ProjectFilters::load(&repo).unwrap();
    assert_eq!(f.patterns, vec!["docs/legacy/", "*.min.js"]);
    assert_eq!(f.types, BTreeSet::from([FileType::Json]));
}

#[test]
fn REQ041_AC7_types_cites_egaux_aux_types_du_produit() {
    let in_code: BTreeSet<String> = FileType::ALL.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(in_code.len(), 15);
    assert_eq!(type_names_of_example(), in_code);
}

fn repo_file(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{} illisible : {e}", p.display()))
}

#[test]
fn REQ041_AC8_readme_renvoie_aux_deux_exemples() {
    let readme = repo_file("README.md");
    assert!(readme.contains("examples/config.toml"));
    assert!(readme.contains("examples/nephila.toml"));
    assert!(readme.contains("config.example.toml"));
    assert!(readme.contains("nephila.example.toml"));
}

#[test]
fn REQ041_AC8_etape_d_empaquetage_copie_les_deux_exemples() {
    let release = repo_file(".github/workflows/release.yml");
    let has = |src: &str, dst: &str| {
        release.lines().any(|l| {
            let l = l.trim();
            l.starts_with("cp ") && l.contains(src) && l.contains(dst)
        })
    };
    assert!(has("examples/config.toml", "config.example.toml"));
    assert!(has("examples/nephila.toml", "nephila.example.toml"));
}

const FILTERS_TITLE: &str = "# --- Filtres ---";
const SETTINGS_TITLE: &str = "# --- Réglages LLM du dépôt ---";
const REFUSED_LINE: &str = "# Refusées ici (configuration personnelle seulement) : provider, base_url, api_key_env, tls_ca_cert, tls_insecure.";

/// Ligne `# Priorité :` attendue, selon `scope` ("ce fichier" ou ".nephila.toml du dépôt").
fn priority_line(key: &str, first: &str) -> String {
    match key {
        "batch_files" => format!("# Priorité : --batch-files > {first}."),
        "format_retries" => format!("# Priorité : --format-retries > {first}."),
        _ => format!("# Priorité : {first}."),
    }
}

fn position_of(lines: &[&str], wanted: &str) -> usize {
    lines
        .iter()
        .position(|l| *l == wanted)
        .unwrap_or_else(|| panic!("ligne absente : {wanted}"))
}

fn priority_lines_of(doc: &str) -> Vec<String> {
    doc.lines()
        .filter(|l| l.starts_with("# priorité :"))
        .map(String::from)
        .collect()
}

/// Comme `doc_of`, sans la mise en minuscules (comparaison exacte).
fn exact_doc_of(lines: &[&str], index: usize) -> Vec<String> {
    let mut block = Vec::new();
    let mut i = index;
    while i > 0 && lines[i - 1].starts_with('#') {
        block.push(lines[i - 1].to_string());
        i -= 1;
    }
    block
}

#[test]
fn REQ041_AC9_exemple_de_projet_titres_et_lignes_dans_l_ordre() {
    let text = example("nephila.toml");
    let lines: Vec<&str> = text.lines().collect();
    assert!(text.contains(
        "Copié tel quel, ce fichier n'a aucun effet : toutes les clés sont désactivées."
    ));
    let filters = position_of(&lines, FILTERS_TITLE);
    let settings = position_of(&lines, SETTINGS_TITLE);
    assert!(filters < settings);
    let at = |prefix: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("ligne absente : {prefix}"))
    };
    let exclude = at("# exclude =");
    let types = at("# exclude_types =");
    assert!(filters < exclude && exclude < types && types < settings);
    let refused = position_of(&lines, REFUSED_LINE);
    assert!(refused > settings);
    let reason = lines[refused + 1];
    assert!(reason.starts_with("# Raison : "), "{reason}");
    let mut previous = refused;
    for key in PROJECT_SETTINGS {
        let index = at(&format!("# {key} = "));
        assert!(index > previous, "{key} hors ordre");
        previous = index;
    }
}

#[test]
fn REQ041_AC9_exemple_de_projet_lignes_de_priorite_par_bloc() {
    let text = example("nephila.toml");
    let lines: Vec<&str> = text.lines().collect();
    for key in PROJECT_SETTINGS {
        let index = lines
            .iter()
            .position(|l| key_of(l) == Some(key) && l.starts_with("# "))
            .unwrap();
        let block = exact_doc_of(&lines, index);
        let expected = priority_line(key, "ce fichier > configuration personnelle > défaut");
        let found: Vec<&String> = block
            .iter()
            .filter(|l| l.starts_with("# Priorité :"))
            .collect();
        assert_eq!(found, vec![&expected], "bloc de {key}");
        // La ligne de priorité est juste avant la ligne désactivée.
        assert_eq!(lines[index - 1], expected, "bloc de {key}");
        let doc = block.join("\n").to_lowercase();
        assert!(doc.contains("configuration personnelle"), "{key}");
        assert!(doc.contains("remplace"), "{key} : dit qu'il remplace");
    }
}

#[test]
fn REQ041_AC9_exemple_de_config_lignes_de_priorite() {
    let text = example("config.toml");
    let lines: Vec<&str> = text.lines().collect();
    assert!(text.contains(concat!(
        "# Un dépôt peut remplacer sept réglages par son fichier .nephila.toml (model, summary_language,\n",
        "# max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries) ; les autres clés ne\n",
        "# sont lues qu'ici."
    )));
    for key in PROJECT_SETTINGS {
        let index = lines.iter().position(|l| key_of(l) == Some(key)).unwrap();
        let block = exact_doc_of(&lines, index);
        let expected = priority_line(key, ".nephila.toml du dépôt > ce fichier > défaut");
        let found: Vec<&String> = block
            .iter()
            .filter(|l| l.starts_with("# Priorité :"))
            .collect();
        assert_eq!(found, vec![&expected], "bloc de {key}");
        assert!(
            !doc_of(&lines, index).contains("option, puis"),
            "ancienne mention de {key}"
        );
    }
    for key in [
        "provider",
        "base_url",
        "api_key_env",
        "tls_ca_cert",
        "tls_insecure",
    ] {
        let index = lines.iter().position(|l| key_of(l) == Some(key)).unwrap();
        assert!(
            priority_lines_of(&doc_of(&lines, index)).is_empty(),
            "bloc de {key} avec une ligne de priorité"
        );
    }
}

#[test]
fn REQ041_AC9_aucun_exemple_ne_dit_configuration_globale() {
    for name in ["config.toml", "nephila.toml"] {
        assert!(!example(name).contains("configuration globale"), "{name}");
    }
}

/// Clés acceptées par le lecteur du fichier de projet, extraites de son erreur E-2.
fn project_accepted_keys() -> BTreeSet<String> {
    let (_tmp, repo) = repo_with_project_file("zz_inconnue = 1\n");
    let err = ProjectFile::load(&repo)
        .expect_err("clé inconnue")
        .to_string();
    let tail = err
        .split_once("clés permises :")
        .unwrap_or_else(|| panic!("libellé d'erreur inattendu : {err}"))
        .1;
    let keys: BTreeSet<String> = tail
        .split(',')
        .map(|k| k.trim().trim_end_matches(')').to_string())
        .filter(|k| !k.is_empty())
        .collect();
    assert!(keys.len() >= 8, "clés extraites de : {err}");
    keys
}

#[test]
fn REQ042_AC12_cles_de_l_exemple_egales_aux_cles_du_lecteur() {
    let written = keys_written(&example("nephila.toml"));
    let accepted = project_accepted_keys();
    assert_eq!(written, accepted);
}

#[test]
fn REQ042_AC12_lignes_reactivees_donnent_les_reglages_ecrits() {
    let lines = project_example_lines();
    let activated = reactivate(&lines);
    let (_tmp, repo) = repo_with_project_file(&join(&activated));
    let file = ProjectFile::load(&repo).unwrap();
    // Filtres : ceux d'AC-041-7.
    let reference = {
        let (_t, r) = repo_with_project_file(&join(&reactivate(
            &lines
                .iter()
                .filter(|l| !PROJECT_SETTINGS.iter().any(|k| key_of(l) == Some(*k)))
                .cloned()
                .collect::<Vec<_>>(),
        )));
        ProjectFile::load(&r).unwrap().filters
    };
    assert_eq!(file.filters, reference);
    assert_eq!(file.filters.patterns, vec!["docs/legacy/", "*.min.js"]);
    // Réglages : égaux aux valeurs écrites.
    // Premier niveau seulement : la section `[ask]` a ses propres clés (AC-164-2).
    let written: Vec<(&str, String)> = activated
        .iter()
        .take_while(|l| *l != "[ask]")
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let key = key_of(l)?;
            PROJECT_SETTINGS
                .contains(&key)
                .then(|| (key, l.split_once(" = ").unwrap().1.trim().to_string()))
        })
        .collect();
    assert_eq!(written.len(), PROJECT_SETTINGS.len());
    for (key, text) in &written {
        if let Some(cap) = nephila::config::project_cap(key) {
            let n: u64 = text.parse().unwrap();
            assert!(n <= cap, "{key} = {n} dépasse le plafond {cap}");
        }
    }
    let value = |k: &str| written.iter().find(|(n, _)| *n == k).unwrap().1.clone();
    let s = &file.settings;
    assert_eq!(s.model.as_deref(), Some(value("model").trim_matches('"')));
    assert_eq!(
        s.max_chunk_chars.map(|v| v.to_string()),
        Some(value("max_chunk_chars"))
    );
    assert_eq!(
        s.concurrency.map(|v| v.to_string()),
        Some(value("concurrency"))
    );
    assert_eq!(
        s.timeout_secs.map(|v| v.to_string()),
        Some(value("timeout_secs"))
    );
    assert_eq!(
        s.batch_files.map(|v| v.to_string()),
        Some(value("batch_files"))
    );
    assert_eq!(
        s.format_retries.map(|v| v.to_string()),
        Some(value("format_retries"))
    );
}

#[test]
fn REQ042_AC12_blocs_de_config_citent_le_fichier_de_projet() {
    let text = example("config.toml");
    let lines: Vec<&str> = text.lines().collect();
    for key in PROJECT_SETTINGS {
        let index = lines.iter().position(|l| key_of(l) == Some(key)).unwrap();
        assert!(
            doc_of(&lines, index).contains(".nephila.toml"),
            "bloc de {key} sans .nephila.toml"
        );
    }
}

/// Section du README qui commence au titre donné et finit au titre suivant de même niveau ou supérieur.
fn readme_section(readme: &str, title: &str) -> String {
    let level = title.chars().take_while(|c| *c == '#').count();
    let mut out = Vec::new();
    let mut inside = false;
    let mut in_fence = false;
    for line in readme.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
        }
        let depth = line.chars().take_while(|c| *c == '#').count();
        let is_title = !in_fence && depth > 0 && line.chars().nth(depth) == Some(' ');
        if line == title {
            inside = true;
            continue;
        }
        if inside && is_title && depth <= level {
            break;
        }
        if inside {
            out.push(line);
        }
    }
    assert!(!out.is_empty(), "section absente : {title}");
    out.join("\n")
}

const CAPS_HEADER: &str = "# Plafonds : ici seulement, chaque réglage entier a un plafond (ligne « Plafond dans ce fichier ») ; la configuration personnelle et les options n'en ont pas.";

#[test]
fn REQ042_AC20_lignes_de_plafond_de_l_exemple() {
    let text = example("nephila.toml");
    let lines: Vec<&str> = text.lines().collect();
    let reason = lines
        .iter()
        .position(|l| l.starts_with("# Raison : "))
        .unwrap();
    assert_eq!(lines[reason + 1], CAPS_HEADER);
    for (key, cap) in PROJECT_CAPS {
        let index = lines.iter().position(|l| key_of(l) == Some(key)).unwrap();
        assert!(lines[index - 1].starts_with("# Priorité : "), "{key}");
        assert_eq!(
            lines[index - 2],
            format!("# Plafond dans ce fichier : au plus {cap}."),
            "{key}"
        );
    }
    let count = lines
        .iter()
        .filter(|l| l.starts_with("# Plafond dans ce fichier :"))
        .count();
    assert_eq!(count, 5 + ASK_CAPS.len());
    for (key, cap) in ASK_CAPS {
        let at = lines.iter().position(|l| *l == ASK_HEADER).unwrap();
        let index = at
            + lines[at..]
                .iter()
                .position(|l| key_of(l) == Some(key))
                .unwrap();
        assert!(lines[index - 1].starts_with("# Priorité : "), "ask.{key}");
        assert_eq!(
            lines[index - 2],
            format!("# Plafond dans ce fichier : au plus {cap}."),
            "ask.{key}"
        );
    }
    let model = lines
        .iter()
        .position(|l| key_of(l) == Some("model"))
        .unwrap();
    assert!(!lines[model - 3..model]
        .iter()
        .any(|l| l.starts_with("# Plafond dans ce fichier :")));
    for l in &lines {
        assert!(!l.contains("maximum"), "{l}");
        assert!(!l.contains("4294967295"));
    }
}

#[test]
fn REQ041_AC10_bloc_d_enrich_code_de_l_exemple_de_config() {
    let text = example("config.toml");
    let lines: Vec<&str> = text.lines().collect();
    let at = |wanted: &str| position_of(&lines, wanted);
    let title = at("# --- Options communes ---");
    let key = at("# enrich_code = false");
    let next = lines
        .iter()
        .position(|l| l.starts_with("# max_chunk_chars ="))
        .unwrap();
    assert!(title < key && key < next);
    let block = exact_doc_of(&lines, key);
    for wanted in [
        "# Priorité : --enrich-code > ce fichier > défaut.",
        "# .nephila.toml la refuse.",
    ] {
        assert!(
            block.iter().any(|l| l == wanted),
            "ligne absente : {wanted}"
        );
    }
    assert!(!block.join("\n").contains(".nephila.toml du dépôt"));
}

/// Texte des commentaires mis bout à bout : « # » retiré, lignes jointes par une espace.
fn comments_joined(text: &str) -> String {
    let words: Vec<&str> = text
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix('#'))
        .map(str::trim)
        .collect();
    words.join(" ")
}

#[test]
fn REQ041_AC10_textes_des_exemples_sur_les_enrichissements_conserves() {
    let project = example("nephila.toml");
    assert!(project.contains(concat!(
        "# Changer model ou summary_language fait réenrichir les fichiers déjà enrichis, par nephila update\n",
        "# comme par nephila build ; les autres réglages ne réenrichissent rien.\n"
    )));
    let project_flat = comments_joined(&project);
    for absent in ["ne réenrichit aucun fichier", "enrich_code"] {
        assert!(!project_flat.contains(absent), "présent : {absent}");
    }
    let config = example("config.toml");
    for wanted in [
        "#     d'environnement qui la contient. Elle n'est pas vérifiée à l'avance : une clé refusée est",
        "# Enrichissements conservés : un fichier déjà enrichi n'est renvoyé au LLM que si son contenu",
        "# batch_files = 10",
        "# plus 3 appels pour une même requête ; 0 = aucun nouvel essai. Ces essais ne portent que sur le",
        "# jamais retentés. Remplacé, pour une commande, par",
    ] {
        assert!(config.lines().any(|l| l == wanted), "ligne absente : {wanted}");
    }
    let config_flat = comments_joined(&config);
    for absent in [
        "la vérifie par un appel",
        "la vérification de clé",
        "Les erreurs réseau, les délais dépassés et les clés refusées ne sont jamais retentés",
    ] {
        assert!(!config_flat.contains(absent), "présent : {absent}");
    }
}

#[test]
fn REQ041_AC11_blocs_des_options_llm_de_l_exemple_de_config() {
    let text = example("config.toml");
    let lines: Vec<&str> = text.lines().collect();
    let at = |wanted: &str| position_of(&lines, wanted);
    let title = at("# --- Options communes ---");
    let enrich = at("# enrich_code = false");
    let thinking = at("# thinking = false");
    let effort = at("# effort = \"low\"");
    let max_out = at("# max_output_tokens = 16384");
    let chunk = lines
        .iter()
        .position(|l| l.starts_with("# max_chunk_chars ="))
        .unwrap();
    assert!(title < enrich);
    assert_eq!(lines[enrich + 1], "");
    assert!(enrich < thinking && thinking < effort && effort < max_out && max_out < chunk);
    assert_eq!(lines[thinking + 1], "");
    assert_eq!(lines[effort + 1], "");
    assert_eq!(lines[max_out + 1], "");
    for (key, extra) in [
        (thinking, vec!["adaptive", "reasoning_effort"]),
        (effort, vec![]),
        (max_out, vec!["max_tokens", "max_completion_tokens"]),
    ] {
        let block = exact_doc_of(&lines, key);
        let joined = block.join("\n");
        assert!(joined.contains("défaut :"), "défaut : absent");
        assert!(block.iter().any(|l| l == "# .nephila.toml la refuse."));
        assert!(!joined.contains(".nephila.toml du dépôt"));
        assert!(!joined.contains("--"));
        for w in extra {
            assert!(joined.contains(w), "absent : {w}");
        }
    }
    // Au premier niveau seulement : la section `[ask]`, en fin de fichier, les permet (REQ-162).
    let project = example("nephila.toml");
    let first_level = project.split(ASK_HEADER).next().unwrap();
    for k in ["effort", "thinking", "max_output_tokens"] {
        assert!(!first_level.contains(k), "{k} dans nephila.toml");
    }
}

fn cargo_minor() -> String {
    let toml = repo_file("Cargo.toml");
    let v = toml
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("version de Cargo.toml");
    v.split('.').take(2).collect::<Vec<_>>().join(".")
}

fn block_ending_at(text: &str, last: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .position(|l| l.trim_end() == last)
        .unwrap_or_else(|| panic!("ligne `{last}` absente"));
    let mut start = end;
    while start > 0 && lines[start - 1].starts_with('#') && !lines[start - 1].trim().eq("#") {
        start -= 1;
    }
    lines[start..=end]
        .iter()
        .map(|l| l.trim_end().to_string())
        .collect()
}

#[test]
fn REQ041_AC13_blocs_de_concurrency_des_exemples() {
    let personal = [
        "# Nombre d'appels LLM simultanés. Entier > 0. Défaut : 4. Sans effet sur l'extraction : ses",
        "# fils, et ceux du parcours du dépôt, se règlent par l'option --jobs de build et update",
        "# (défaut : nombre de cœurs disponibles), qui n'a pas de clé de configuration.",
        "# Un dépôt peut le remplacer par son .nephila.toml.",
        "# Priorité : .nephila.toml du dépôt > ce fichier > défaut.",
        "# concurrency = 4",
    ];
    let project = [
        "# Nombre d'appels LLM simultanés. Entier > 0. Remplace la valeur de la configuration personnelle.",
        "# Sans effet sur l'extraction : ses fils se règlent par l'option --jobs, qui n'a pas de clé.",
        "# Plafond dans ce fichier : au plus 64.",
        "# Priorité : ce fichier > configuration personnelle > défaut.",
        "# concurrency = 2",
    ];
    let a = example("config.toml");
    let b = example("nephila.toml");
    assert_eq!(block_ending_at(&a, "# concurrency = 4"), personal);
    assert_eq!(block_ending_at(&b, "# concurrency = 2"), project);
    for text in [&a, &b] {
        for bad in ["borne aussi", "fils d'analyse", "fils de l'analyse"] {
            assert!(!text.contains(bad), "{bad}");
        }
        for l in text.lines() {
            assert!(!l.starts_with("# jobs =") && !l.starts_with("jobs ="));
        }
    }
}

#[test]
fn REQ071_AC6_exemples_et_readme_sans_fils_d_analyse() {
    let readme = repo_file("README.md");
    for (name, text) in [
        ("config.toml", example("config.toml")),
        ("nephila.toml", example("nephila.toml")),
        ("README.md", readme),
    ] {
        // E-25 (REQ-112) garde son texte exact : seul « création des fils d'analyse impossible : » est permis.
        let text = text.replace("création des fils d'analyse impossible : ", "");
        for bad in ["borne aussi", "fils d'analyse"] {
            assert!(!text.contains(bad), "{name} : {bad}");
        }
    }
}

#[test]
fn req076_ac2_readme_prerequis_rust_1_99() {
    let readme =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .unwrap();
    let expected = "Prérequis : Rust 1.99 ou plus récent (`cargo`). Aucune dépendance système : SQLite est intégré au binaire.";
    assert!(
        readme.lines().any(|l| l == expected),
        "ligne absente : {expected}"
    );
    assert!(!readme.contains("une chaîne Rust stable récente"));
}

const EXCLUDE_DEFAULTS_BLOCK: &str = "# Exclusions par défaut : les dossiers dist/, out/, coverage/, .next/, .nuxt/, vendor/ et .idea/,
# à tout niveau du dépôt, et les fichiers *.min.js ne sont pas analysés. Booléen ; défaut : true.
# false : ces chemins sont analysés comme les autres fichiers (types pris en charge, .gitignore,
# exclude et exclude_types) ; utile quand l'un de ces dossiers contient votre code. Pour en
# garder certains exclus, ajoutez-les à exclude. Ce ne sont pas des filtres : ils ne comptent
# pas dans « fichiers exclus par les filtres », et !motif ne les réinclut pas. Pas d'option de
# ligne de commande : la clé vaut pour toutes les commandes, status compris.
# exclude_defaults = false
";

#[test]
fn REQ079_AC7_exemple_de_projet_entete_filtres() {
    let text = example("nephila.toml");
    assert!(text.contains(
        "# Un filtre s'ajoute à ce que nephila ignore déjà (.gitignore, target/, build/, .gradle/, .git/,
# .nephila/, node_modules/) et à ce qu'il exclut par défaut (voir exclude_defaults) ; il ne peut
# jamais réinclure un fichier ignoré ou exclu par défaut.
"
    ));
    assert!(!text.contains("# .nephila/) et ne peut jamais réinclure un fichier ignoré."));
}

/// Graphe construit sans LLM sur un dépôt dont `dist/app.js` est du code : l'export le cite ou non.
fn exports_dist_with(project_file: &str) -> bool {
    let (tmp, repo) = repo_with_project_file(project_file);
    fs::create_dir_all(repo.join("dist")).unwrap();
    fs::write(
        repo.join("dist/app.js"),
        "export function f() { return 1; }\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = Command::cargo_bin("nephila")
            .unwrap()
            .env("NEPHILA_CONFIG", tmp.path().join("absent.toml"))
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&["build", "--no-llm"]);
    run(&["export", "--format", "json", "--output", "-"]).contains("dist/app.js")
}

#[test]
fn REQ079_AC7_exemple_de_projet_exclude_defaults_reactivee_analyse_dist() {
    let text = example("nephila.toml");
    assert!(!exports_dist_with(&text), "copié tel quel : dist/ exclu");
    let activated = text.replace("# exclude_defaults = false", "exclude_defaults = false");
    assert_ne!(activated, text);
    let (_tmp, repo) = repo_with_project_file(&activated);
    ProjectFile::load(&repo).expect("ligne réactivée acceptée");
    assert!(exports_dist_with(&activated), "dist/ analysé");
}

#[test]
fn REQ079_AC7_exemple_de_projet_bloc_exclude_defaults() {
    let text = example("nephila.toml");
    assert!(text.contains(EXCLUDE_DEFAULTS_BLOCK));
    let n = text
        .lines()
        .filter(|l| l.starts_with("# exclude_defaults ="))
        .count();
    assert_eq!(n, 1);
    let at = |needle: &str| text.find(needle).unwrap();
    assert!(at("# exclude_types = [") < at("# Exclusions par défaut :"));
    assert!(at("# exclude_defaults = false") < at("# --- Réglages LLM du dépôt ---"));
    assert!(!text.contains("exclude_defaults = true"));
}

#[test]
fn REQ122_AC1_readme_installe_avec_locked() {
    let readme = repo_file("README.md");
    assert!(
        !readme.contains("cargo install --path ."),
        "README : `cargo install --path .` sans --locked"
    );
    for title in ["## Démarrage rapide", "### Depuis les sources"] {
        let sec = readme_section(&readme, title);
        assert!(
            sec.contains("cargo install --locked --path ."),
            "{title} : `cargo install --locked --path .` absent"
        );
    }
}

const THINKING_BLOCK: &str = "# Demande au modèle de réfléchir avant de répondre. Booléen ; défaut : false. true : nephila
# envoie à « anthropic » \"thinking\": {\"type\": \"adaptive\"} et à « openai » \"reasoning_effort\",
# même sans clé effort ; le niveau est celui de effort. false : aucun champ \"thinking\" n'est
# envoyé, mais un modèle qui réfléchit toujours (certains modèles Claude, les modèles de
# raisonnement d'OpenAI) le fait quand même ; effort règle aussi son niveau (voir ci-dessous).
# Un modèle qui ne prend pas en charge la réflexion refuse la requête (HTTP 400) : ses fichiers
# restent à enrichir. Chez « anthropic », la réflexion adaptative demande un modèle Claude 4.6
# ou suivant : claude-sonnet-4-5, le modèle de cet exemple, la refuse. La réflexion n'est
# jamais affichée ni enregistrée. Sans effet avec « mock ». Lue ici seulement :
# .nephila.toml la refuse.
# thinking = false";

const EFFORT_BLOCK: &str =
    "# Niveau d'effort demandé au modèle, avec ou sans thinking : il vaut pour toute la réponse,
# réflexion comprise. Texte : \"low\", \"medium\", \"high\" ou \"default\", en minuscules ; défaut :
# \"low\". Envoyé à « anthropic » dans \"output_config\", sauf quand thinking vaut false et que le
# modèle est sorti avant Claude Opus 4.5, comme claude-sonnet-4-5, le modèle de cet exemple :
# ces modèles refusent le champ. Envoyé à « openai » dans \"reasoning_effort\" si cette clé est
# écrite ou si thinking vaut true ; un modèle sans raisonnement (gpt-4o-mini, celui de la
# section OpenAI) le refuse. \"default\" n'envoie rien, et le modèle garde son niveau par défaut :
# c'est le remède quand un serveur ou un modèle refuse la requête (HTTP 400) à cause de ce
# champ. Sans effet avec « mock ». Un niveau plus haut donne des réponses plus lentes et plus
# coûteuses, et sa réflexion compte dans max_output_tokens.
# À \"medium\" ou \"high\", augmentez timeout_secs (600, par exemple) : un appel plus long échoue
# en délai dépassé, et trois délais dépassés de suite arrêtent l'envoi. Lue ici seulement :
# .nephila.toml la refuse.
# effort = \"low\"";

fn example_block_ending_at(key_line: &str) -> String {
    let text = example("config.toml");
    let lines: Vec<&str> = text.lines().collect();
    let at = position_of(&lines, key_line);
    let mut block = exact_doc_of(&lines, at);
    block.reverse();
    block.push(lines[at].to_string());
    block.join("\n")
}

#[test]
fn REQ133_AC1_bloc_thinking_de_l_exemple_de_config_au_texte_exact() {
    assert_eq!(
        example_block_ending_at("# thinking = false"),
        THINKING_BLOCK
    );
}

#[test]
fn REQ133_AC3_modele_de_l_exemple_reste_claude_sonnet_4_5() {
    assert!(example("config.toml")
        .lines()
        .any(|l| l == "model = \"claude-sonnet-4-5\""));
}

#[test]
fn REQ135_AC1_bloc_effort_de_l_exemple_de_config_au_texte_exact() {
    assert_eq!(example_block_ending_at("# effort = \"low\""), EFFORT_BLOCK);
}

#[test]
fn REQ157_AC9_AC13_blocs_thinking_et_effort_de_l_exemple() {
    let thinking = example_block_ending_at("# thinking = false");
    let effort = example_block_ending_at("# effort = \"low\"");
    assert_eq!(thinking.lines().count(), 11);
    assert_eq!(effort.lines().count(), 14);
    for needle in [
        "aucun champ \"thinking\"",
        "effort règle aussi son niveau (voir ci-dessous)",
        "4.6",
        "claude-sonnet-4-5",
    ] {
        assert!(thinking.contains(needle), "thinking : {needle}");
    }
    for bad in [
        "aucun champ de réflexion",
        "mettez thinking à true et effort à \"low\"",
    ] {
        assert!(!thinking.contains(bad), "thinking : {bad}");
    }
    assert_eq!(
        effort.lines().next().unwrap(),
        "# Niveau d'effort demandé au modèle, avec ou sans thinking : il vaut pour toute la réponse,"
    );
    for needle in [
        "\"low\", \"medium\", \"high\" ou \"default\"",
        "sorti avant Claude Opus 4.5",
        "gpt-4o-mini",
        "\"default\" n'envoie rien",
        "claude-sonnet-4-5",
        "timeout_secs",
        "trois délais dépassés",
    ] {
        assert!(effort.contains(needle), "effort : {needle}");
    }
    assert!(!effort.contains("Sans effet quand thinking vaut false"));
    assert!(effort
        .lines()
        .any(|l| l.starts_with("# À \"medium\" ou \"high\", augmentez timeout_secs")));
    for l in thinking.lines().chain(effort.lines()) {
        assert!(l.chars().count() <= 100, "ligne trop longue : {l}");
    }
}

#[test]
fn REQ157_AC10_AC14_readme_effort_sans_thinking() {
    let readme = repo_file("README.md");
    let conf = readme_section(&readme, "## Configuration du LLM");
    let key_row = conf
        .lines()
        .find(|l| l.starts_with("| `effort` |") && !l.contains("output_config"))
        .expect("ligne effort de la table des clés");
    assert!(key_row.contains("\"default\""), "{key_row}");
    assert!(!key_row.contains("avec `thinking = true`"), "{key_row}");
    assert!(conf.contains("effort = \"default\""));
    assert!(!readme.contains("(avec `thinking = true` seulement)"));
    let rows = table_rows(
        &conf,
        "| Clé | `anthropic` | `openai` | `mock` |",
        "|---|---|---|---|",
    );
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(cell(rows[3], 0), "`structured_output`");
    assert_eq!(cell(rows[0], 0), "`max_output_tokens`");
    assert_eq!(cell(rows[1], 0), "`thinking = true`");
    assert_eq!(cell(rows[2], 0), "`effort`");
    assert!(cell(rows[1], 2).contains("reasoning_effort"), "{}", rows[1]);
    assert!(
        cell(rows[2], 1).contains("output_config.effort"),
        "{}",
        rows[2]
    );
    assert!(cell(rows[2], 1).contains("Opus 4.5"), "{}", rows[2]);
    assert!(
        cell(rows[2], 1).contains("claude-sonnet-4-5"),
        "{}",
        rows[2]
    );
    assert!(cell(rows[2], 2).contains("reasoning_effort"), "{}", rows[2]);
    assert!(!rows[2].contains("thinking = true"), "{}", rows[2]);
    let stop = readme_section(&readme, "### Arrêt de l'envoi");
    let stop_rows = table_rows(&stop, "| Cause | Quand | Que faire |", "|---|---|---|");
    let refused = stop_rows
        .iter()
        .find(|r| cell(r, 0) == "Requête refusée")
        .expect("ligne Requête refusée");
    for needle in [
        "`output_config`",
        "`reasoning_effort`",
        "`effort = \"default\"`",
        "`max_output_tokens`",
    ] {
        assert!(cell(refused, 2).contains(needle), "{refused} : {needle}");
    }
    let upgrade = readme_section(&readme, "## Mise à niveau");
    let first = upgrade
        .lines()
        .find(|l| l.starts_with("- **Vers "))
        .expect("première puce");
    assert!(first.starts_with("- **Vers 0.26** :"), "{first}");
    for needle in [
        "output_config",
        "effort = \"default\"",
        "requête refusée",
        "nephila update",
    ] {
        assert!(first.contains(needle), "{first} : {needle}");
    }
}

fn without_timestamp(line: &str) -> String {
    let start = line.find('[').expect("horodatage");
    let end = line[start..].find("s] ").expect("fin d'horodatage") + start + 3;
    format!("{}{}", &line[..start], &line[end..])
}

#[test]
fn REQ134_AC2_ligne_config_d_exemple_du_readme_est_la_sortie_reelle() {
    let (tmp, repo) = repo_with_project_file("model = \"m-projet\"\nformat_retries = 1\n");
    let config = tmp.path().join("config.toml");
    fs::write(
        &config,
        "provider = \"mock\"\nbatch_files = 1\nformat_retries = 1\n",
    )
    .unwrap();
    let mock = tmp.path().join("mock");
    fs::create_dir_all(&mock).unwrap();
    let out = Command::cargo_bin("nephila")
        .unwrap()
        .env("NEPHILA_CONFIG", &config)
        .env("NEPHILA_MOCK_DIR", &mock)
        .arg("-C")
        .arg(&repo)
        .args(["build", "--verbose"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let real = stderr
        .lines()
        .find(|l| l.contains("config: llm="))
        .expect("ligne config");
    let readme = repo_file("README.md");
    let example = readme
        .lines()
        .find(|l| l.starts_with("trace: ") && l.contains("config: llm="))
        .expect("exemple du README");
    assert_eq!(without_timestamp(real), without_timestamp(example));
}

#[test]
fn REQ158_AC1_version_0_26_0() {
    assert_eq!(cargo_minor(), "0.26");
    let toml = repo_file("Cargo.toml");
    assert!(toml.lines().any(|l| l == "version = \"0.26.0\""));
    let lock = repo_file("Cargo.lock");
    assert!(lock.contains("name = \"nephila-rs\"\nversion = \"0.26.0\""));
    let readme = repo_file("README.md");
    for needle in ["V=v0.26.0", "git tag v0.26.0 && git push origin v0.26.0"] {
        assert!(readme.contains(needle), "absent : {needle}");
    }
    assert!(!readme.contains("v0.25.3"));
}

/// Titres du README (niveau 1 et plus), hors blocs de code, tels qu'écrits.
fn headings(readme: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in readme.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        let depth = line.chars().take_while(|c| *c == '#').count();
        if !in_fence && depth > 0 && line.chars().nth(depth) == Some(' ') {
            out.push(line.to_string());
        }
    }
    out
}

/// Lignes du README hors blocs de code.
fn prose_lines(readme: &str) -> Vec<&str> {
    let mut in_fence = false;
    let mut out = Vec::new();
    for line in readme.lines() {
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push(line);
        }
    }
    out
}

/// Ancre d'un titre à la manière de GitHub.
fn github_anchor(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

#[test]
fn github_anchor_calcule_les_ancres_de_reference() {
    for (title, anchor) in [
        ("Filtrer l'analyse", "filtrer-lanalyse"),
        (
            "Fichier de projet `.nephila.toml`",
            "fichier-de-projet-nephilatoml",
        ),
        ("Arrêt de l'envoi", "arrêt-de-lenvoi"),
        (
            "LLM simulé (`provider = \"mock\"`)",
            "llm-simulé-provider--mock",
        ),
    ] {
        assert_eq!(github_anchor(title), anchor, "{title}");
    }
}

const PLAN: [&str; 32] = [
    "# Nephila",
    "## Sommaire",
    "## Démarrage rapide",
    "## Installation",
    "### Binaires précompilés",
    "### Depuis les sources",
    "## Commandes",
    "### `nephila build` et `nephila update`",
    "### `nephila status`",
    "### `nephila query`",
    "### `nephila summary`",
    "### Traces et progression",
    "## Usage par un agent",
    "## Codes de sortie",
    "## Filtrer l'analyse",
    "### Exclusions par défaut",
    "### Motifs et types",
    "### Fichier de projet `.nephila.toml`",
    "## Configuration du LLM",
    "### Certificats TLS",
    "## Enrichissement",
    "### Lots et gros fichiers",
    "### Échecs et nouveaux essais",
    "### Arrêt de l'envoi",
    "## Export et vue HTML",
    "## Communautés",
    "## Ce que l'analyse extrait",
    "## Limites connues",
    "## Mise à niveau",
    "## Développement",
    "### LLM simulé (`provider = \"mock\"`)",
    "## Licence",
];

#[test]
fn REQ155_AC1_readme_condense() {
    let size = fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .unwrap()
        .len();
    assert!(size <= 47_000, "README : {size} octets");
    let readme = repo_file("README.md");
    assert!(!readme.contains("| Version | Nouveautés |"));
    assert!(!readme.lines().any(|l| l.starts_with("| 0.")));
    let upgrade = readme_section(&readme, "## Mise à niveau");
    assert!(!upgrade.contains("rkg"));
    for d in 0..=7 {
        let bad = format!("Vers 0.1{d}");
        assert!(!upgrade.contains(&bad), "absent attendu : {bad}");
    }
    for bad in ["Vers 0.9", "Depuis 0.2"] {
        assert!(!upgrade.contains(bad), "absent attendu : {bad}");
    }
}

#[test]
fn REQ155_AC2_sommaire_sections_et_liens() {
    let readme = repo_file("README.md");
    let all = headings(&readme);
    let level2: Vec<&String> = all.iter().filter(|h| h.starts_with("## ")).collect();
    let expected2: Vec<&str> = PLAN
        .iter()
        .copied()
        .filter(|h| h.starts_with("## "))
        .collect();
    assert_eq!(level2, expected2);
    let after: Vec<&str> = expected2[1..].to_vec();
    let wanted: Vec<String> = after
        .iter()
        .map(|h| {
            let t = &h[3..];
            format!("- [{t}](#{})", github_anchor(t))
        })
        .collect();
    let found: Vec<String> = readme_section(&readme, "## Sommaire")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(found, wanted);
    let anchors: BTreeSet<String> = all
        .iter()
        .map(|h| github_anchor(h.trim_start_matches('#').trim_start()))
        .collect();
    for line in prose_lines(&readme) {
        let mut rest = line;
        while let Some(i) = rest.find("](#") {
            rest = &rest[i + 3..];
            let end = rest.find(')').expect("lien interne non fermé");
            assert!(
                anchors.contains(&rest[..end]),
                "ancre sans titre : #{}",
                &rest[..end]
            );
            rest = &rest[end..];
        }
    }
}

#[test]
fn REQ155_AC4_essentiel_pour_l_utilisateur() {
    let readme = repo_file("README.md");
    for needle in [
        "SHA256SUMS",
        ".nephila/",
        ".gitignore",
        "NEPHILA_CONFIG",
        "$XDG_CONFIG_HOME/nephila/config.toml",
        "~/.config/nephila/config.toml",
        "en attente",
        "à enrichir",
        "graphe à résoudre : lancez nephila update",
        "exclude_defaults = false",
        "trop d'appels d'enrichissement",
        "`exclude`",
        "`max_chunk_chars`",
        "réponse tronquée par la limite de jetons",
        "retry-after",
        "tls_ca_cert",
        "tls_insecure",
    ] {
        assert!(readme.contains(needle), "absent : {needle}");
    }
}

#[test]
fn REQ155_AC5_essentiel_pour_un_agent() {
    let readme = repo_file("README.md");
    let agent = readme_section(&readme, "## Usage par un agent");
    for needle in [
        "nephila status --json",
        "a_jour",
        "nephila update",
        "nephila query search --json",
        "nephila query neighbors --json",
        "nephila query path --json",
        "nephila query community",
        "--check-stale",
        "nephila summary --json",
        "nephila export --output -",
    ] {
        assert!(agent.contains(needle), "Usage par un agent : {needle}");
    }
    let codes = readme_section(&readme, "## Codes de sortie");
    for needle in [
        "| 0 |",
        "| 1 |",
        "| 2 |",
        "| 130 |",
        "erreur:",
        "avertissement:",
        "stderr",
        "stdout",
    ] {
        assert!(codes.contains(needle), "Codes de sortie : {needle}");
    }
}

#[test]
fn REQ155_AC10_plan_des_titres() {
    let readme = repo_file("README.md");
    assert_eq!(headings(&readme), PLAN);
}

#[test]
fn REQ155_AC11_usage_par_un_agent_en_trois_etapes() {
    let readme = repo_file("README.md");
    let agent = readme_section(&readme, "## Usage par un agent");
    let numbered: Vec<&str> = agent
        .lines()
        .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_digit()) && l.contains(". "))
        .collect();
    assert_eq!(numbered.len(), 3, "{numbered:?}");
    assert_eq!(
        numbered[0],
        "1. `nephila status --json` : si `a_jour` vaut `false`, lancer `nephila update`."
    );
    assert!(numbered[1].starts_with("2. "));
    assert!(numbered[1].contains("nephila query community --json --node"));
    assert!(numbered[2].starts_with("3. "));
    for needle in ["genre:nom", "--check-stale", "nephila <commande> --help"] {
        assert!(agent.contains(needle), "absent : {needle}");
    }
}

/// Lignes de table qui suivent la ligne d'en-tête `header` et sa ligne `separator`.
fn table_rows<'a>(section: &'a str, header: &str, separator: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = section.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == header)
        .unwrap_or_else(|| panic!("en-tête absent : {header}"));
    assert_eq!(lines[at + 1], separator);
    lines[at + 2..]
        .iter()
        .copied()
        .take_while(|l| l.starts_with('|'))
        .collect()
}

fn cell(row: &str, n: usize) -> &str {
    row.split('|').nth(n + 1).expect("cellule").trim()
}

#[test]
fn REQ155_AC12_table_d_arret_de_l_envoi() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "### Arrêt de l'envoi");
    let rows = table_rows(&sec, "| Cause | Quand | Que faire |", "|---|---|---|");
    let causes = [
        ("Clé refusée", "`api_key_env`"),
        ("Requête refusée", "`max_output_tokens`"),
        ("Redirection", "`base_url`"),
        ("Fournisseur saturé", "`concurrency`"),
        ("Panne réseau", "`base_url`"),
        ("Délais dépassés", "`timeout_secs`"),
    ];
    assert_eq!(rows.len(), 6, "{rows:?}");
    for (row, (cause, action)) in rows.iter().zip(causes) {
        assert_eq!(cell(row, 0), cause);
        assert!(cell(row, 2).contains(action), "{row} : {action}");
    }
    for needle in ["erreur:", "nephila update", "à enrichir"] {
        assert!(sec.contains(needle), "absent : {needle}");
    }
    assert!(!sec
        .lines()
        .any(|l| l.starts_with("erreur:") || l.starts_with("- `erreur:")));
}

#[test]
fn REQ155_AC13_mise_a_niveau_en_six_actions() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Mise à niveau");
    let bullets: Vec<&str> = sec.lines().filter(|l| l.starts_with("- **Vers ")).collect();
    let wanted = [
        ("- **Vers 0.26** :", "output_config"),
        ("- **Vers 0.25** :", ".nephila/"),
        ("- **Vers 0.24** :", "exclude_defaults = false"),
        ("- **Vers 0.23** :", "--jobs 1"),
        ("- **Vers 0.19** :", "--check-stale"),
        ("- **Vers 0.18** :", "enrich_code = true"),
    ];
    assert_eq!(bullets.len(), 6, "{bullets:?}");
    for (bullet, (prefix, needle)) in bullets.iter().zip(wanted) {
        assert!(bullet.starts_with(prefix), "{bullet}");
        assert!(bullet.contains(needle), "{bullet} : {needle}");
    }
    for needle in ["effort = \"default\"", "requête refusée", "nephila update"] {
        assert!(bullets[0].contains(needle), "{} : {needle}", bullets[0]);
    }
    assert!(sec.contains("https://github.com/tixplop/nephila-rs/releases"));
    assert!(sec.contains("Un graphe 0.18 à 0.24 est migré sur place"));
}

#[test]
fn REQ155_AC14_options_de_build_en_une_ligne() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "### `nephila build` et `nephila update`");
    let rows = table_rows(&sec, "| Option | Effet |", "|---|---|");
    let options = [
        "`--no-llm`",
        "`--enrich-code`",
        "`--dry-run`",
        "`-v, --verbose`",
        "`--show-progress`",
        "`--jobs <N>`",
        "`--batch-files <N>`",
        "`--format-retries <N>`",
        "`--max-calls <N>`",
        "`--accept-project-model`",
        "`--exclude <MOTIF>`",
        "`--exclude-type <TYPES>`",
    ];
    assert_eq!(rows.len(), 12, "{rows:?}");
    for (row, option) in rows.iter().zip(options) {
        assert_eq!(cell(row, 0), option);
        assert!(row.chars().count() <= 160, "{row}");
    }
    assert!(rows[4].contains("incompatible avec `--verbose`"));
    assert!(rows[5].contains("`concurrency`"));
    for needle in [
        "en attente",
        "à enrichir",
        "erreur: indexation déjà en cours sur ce dépôt",
    ] {
        assert!(sec.contains(needle), "absent : {needle}");
    }
    assert!(!sec.contains("Mio"));
}

#[test]
fn REQ155_AC15_traces_et_progression_sans_enumeration() {
    let readme = repo_file("README.md");
    let traces: Vec<&str> = readme
        .lines()
        .filter(|l| l.starts_with("trace: "))
        .collect();
    assert!(traces.len() <= 4, "{traces:?}");
    assert_eq!(
        traces.iter().filter(|l| l.contains("config: llm=")).count(),
        1
    );
    assert!(!readme.lines().any(|l| l.starts_with("progression: ")));
    let sec = readme_section(&readme, "### Traces et progression");
    for needle in [
        "progression: <phase> <p> % (<k>/<n> fichiers)",
        "grep '^progression:'",
        "incompatibles",
        "Les traces ne contiennent jamais de contenu de fichier, de prompt, de réponse du LLM ni de clé d'API.",
    ] {
        assert!(sec.contains(needle), "absent : {needle}");
    }
    for bad in [
        "politique: arrêt de l'envoi",
        "llm enrich attente",
        "llm enrich découpe",
    ] {
        assert!(!sec.contains(bad), "présent : {bad}");
    }
}

#[test]
fn REQ155_AC16_fichier_de_projet_et_etats() {
    let readme = repo_file("README.md");
    let project = readme_section(&readme, "### Fichier de projet `.nephila.toml`");
    for needle in [
        "Clés permises : les filtres `exclude`, `exclude_types` et `exclude_defaults`, et les sept réglages `model`, `summary_language`, `max_chunk_chars`, `concurrency`, `timeout_secs`, `batch_files` et `format_retries`",
        "Priorité : option > fichier de projet > configuration personnelle > défaut.",
        "| Réglage | Plafond dans `.nephila.toml` |",
    ] {
        assert!(project.contains(needle), "absent : {needle}");
    }
    let status = readme_section(&readme, "### `nephila status`");
    assert!(status.contains("graphe à résoudre : lancez nephila update"));
    assert!(status.contains("nephila build"));
    let codes = readme_section(&readme, "## Codes de sortie");
    assert!(codes.contains("erreur: écriture sur stdout impossible : <cause>"));
}

#[test]
fn REQ155_AC17_tables_et_liens_lisibles() {
    let readme = repo_file("README.md");
    let lines = prose_lines(&readme);
    let mut i = 0;
    while i < lines.len() {
        if lines[i].starts_with('|') {
            let second = lines.get(i + 1).copied().unwrap_or("");
            assert!(
                !second.is_empty() && second.chars().all(|c| c == '|' || c == '-'),
                "table sans ligne d'en-tête : {}",
                lines[i]
            );
            while i < lines.len() && lines[i].starts_with('|') {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    for line in &lines {
        let mut rest = *line;
        while let Some(i) = rest.find("](") {
            let start = rest[..i].rfind('[').expect("lien sans crochet ouvrant");
            let text = &rest[start + 1..i];
            assert!(
                !["ici", "ce lien", "cette section"].contains(&text),
                "texte de lien : {text}"
            );
            rest = &rest[i + 2..];
        }
    }
    assert!(!readme.contains("!["));
}

// --- REQ-155 AC-155-3 : cohérence du README avec le code (DEC-125) ---

/// Sortie standard de `nephila <args>`.
fn help_of(args: &[&str]) -> String {
    let out = Command::cargo_bin("nephila")
        .unwrap()
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?} a échoué");
    String::from_utf8(out.stdout).unwrap()
}

/// Sous-commandes de la section `Commands:` d'une aide, `help` exclu.
fn subcommands_of(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|l| *l != "Commands:")
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(String::from)
        .collect()
}

/// Options longues des lignes d'options d'une aide, `--help` et `--version` exclus.
fn long_options_of(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|l| *l != "Options:")
        .skip(1)
        .take_while(|l| l.starts_with(' '))
        .filter(|l| l.trim_start().starts_with('-'))
        .flat_map(|l| {
            let spec = l.trim_start().split("  ").next().unwrap_or("");
            spec.split(|c: char| c.is_whitespace() || c == ',')
                .filter(|w| w.starts_with("--"))
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .filter(|o| o != "--help" && o != "--version")
        .collect()
}

#[test]
fn REQ155_AC3a_sous_commandes() {
    let readme = repo_file("README.md");
    let table = readme_section(&readme, "## Commandes");
    let rows: Vec<&str> = table.lines().filter(|l| l.starts_with("| ")).collect();
    let top = subcommands_of(&help_of(&["--help"]));
    let query = subcommands_of(&help_of(&["query", "--help"]));
    assert!(top.len() >= 6 && query.len() >= 5, "{top:?} {query:?}");
    for (prefix, names) in [("nephila", top), ("nephila query", query)] {
        for name in names {
            let start = format!("| `{prefix} {name}");
            assert!(
                rows.iter().any(|r| r.starts_with(&start)),
                "sous-commande absente de la table de `## Commandes` : {prefix} {name}"
            );
        }
    }
}

#[test]
fn REQ155_AC3b_options_longues() {
    let readme = repo_file("README.md");
    let mut commands: Vec<Vec<&str>> = ["build", "update", "status", "summary", "export"]
        .iter()
        .map(|c| vec![*c])
        .collect();
    for q in ["search", "neighbors", "path", "community", "ask"] {
        commands.push(vec!["query", q]);
    }
    for mut args in commands {
        let label = args.join(" ");
        args.push("--help");
        let options = long_options_of(&help_of(&args));
        assert!(!options.is_empty(), "aucune option lue pour {label}");
        for option in options {
            assert!(
                readme.contains(&option),
                "option absente du README : {option} (nephila {label})"
            );
        }
    }
}

#[test]
fn REQ155_AC3c_cles_de_configuration() {
    let readme = repo_file("README.md");
    let llm = readme_section(&readme, "## Configuration du LLM");
    for key in accepted_keys() {
        let start = format!("| `{key}` |");
        assert!(
            llm.lines().any(|l| l.starts_with(&start)),
            "clé absente de la table de `## Configuration du LLM` : {key}"
        );
    }
    let project = readme_section(&readme, "### Fichier de projet `.nephila.toml`");
    for key in project_accepted_keys() {
        // `ask` est la section : le README l'écrit `[ask]`.
        let cited = if key == "ask" {
            "`[ask]`".to_string()
        } else {
            format!("`{key}`")
        };
        assert!(
            project.contains(&cited),
            "clé absente de `### Fichier de projet` : {key}"
        );
    }
    // Les clés de `[ask]` (AC-164-3) : dans le paragraphe **Section `[ask]`**.
    let paragraph = llm
        .lines()
        .find(|l| l.starts_with("**Section `[ask]`**"))
        .expect("paragraphe Section [ask]");
    for key in ASK_SETTINGS {
        assert!(
            paragraph.contains(&format!("`{key}`")),
            "clé de [ask] absente du paragraphe : {key}"
        );
    }
}

#[test]
fn REQ155_AC3d_types_de_fichiers() {
    let readme = repo_file("README.md");
    let section = readme_section(&readme, "### Motifs et types");
    for t in FileType::ALL {
        let start = format!("| `{}` |", t.name());
        assert!(
            section.lines().any(|l| l.starts_with(&start)),
            "type absent de la table de `### Motifs et types` : {}",
            t.name()
        );
    }
}

#[test]
fn REQ155_AC3e_plafonds() {
    let readme = repo_file("README.md");
    let section = readme_section(&readme, "### Fichier de projet `.nephila.toml`");
    for (key, cap) in PROJECT_CAPS {
        let row = format!("| `{key}` | {cap} |");
        assert!(
            section.lines().any(|l| l == row),
            "ligne de plafond absente : {row}"
        );
    }
    for (key, cap) in ASK_CAPS {
        let row = format!("| `ask.{key}` | {cap} |");
        assert!(
            section.lines().any(|l| l == row),
            "ligne de plafond absente : {row}"
        );
    }
}

#[test]
fn REQ155_AC3f_variables_du_llm_simule() {
    let readme = repo_file("README.md");
    let section = readme_section(&readme, "### LLM simulé (`provider = \"mock\"`)");
    let in_code = mock_vars(MOCK_SOURCE);
    let cited = mock_vars(&section);
    assert!(in_code.len() >= 8, "{in_code:?}");
    let missing: Vec<_> = in_code.difference(&cited).collect();
    let unknown: Vec<_> = cited.difference(&in_code).collect();
    assert!(
        missing.is_empty(),
        "variables du code absentes du README : {missing:?}"
    );
    assert!(
        unknown.is_empty(),
        "variables du README inconnues du code : {unknown:?}"
    );
    let dev = readme_section(&readme, "## Développement");
    assert!(
        dev.contains("NEPHILA_RETRY_BASE_MS"),
        "NEPHILA_RETRY_BASE_MS absente de `## Développement`"
    );
}

#[test]
fn REQ155_AC3g_memoire_en_mio() {
    let readme = repo_file("README.md");
    let expected = nephila::scan::RETAIN_BUDGET / (1024 * 1024);
    for (at, _) in readme.match_indices(" Mio") {
        let digits: String = readme[..at]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert_eq!(
            digits,
            expected.to_string(),
            "nombre de Mio du README différent de RETAIN_BUDGET"
        );
    }
}

#[test]
fn REQ164_AC1_section_ask_de_la_configuration_personnelle() {
    let text = example("config.toml");
    let written = ask_keys_written(&text);
    // `structured_output` : clé de la configuration personnelle seulement (REQ-204).
    let accepted: BTreeSet<String> = ASK_SETTINGS
        .iter()
        .chain(["structured_output"].iter())
        .map(|k| k.to_string())
        .collect();
    assert_eq!(written, accepted);
    // Chaque clé a sa ligne désactivée, et la ligne `# [ask]` est la dernière de ce niveau.
    assert_eq!(text.lines().filter(|l| *l == ASK_HEADER).count(), 1);
    // Lignes réactivées : acceptées, et lues telles qu'écrites.
    let section = reactivate(&section(&text, "Réglages de query ask"));
    let c = parse(&format!("provider = \"mock\"\n{}", join(&section)));
    assert_eq!(c.model, "mock");
    assert_eq!(
        c.ask,
        AskOverrides {
            model: Some("claude-opus-5-5".into()),
            thinking: Some(true),
            effort: Some(nephila::config::Effort::Medium),
            max_output_tokens: Some(4096),
            structured_output: Some(nephila::config::StructuredOutput::Off),
            max_context_chars: Some(30000),
            max_seeds: Some(30),
            max_nodes: Some(120),
        }
    );
}

#[test]
fn REQ164_AC2_section_ask_du_fichier_de_projet() {
    let text = example("nephila.toml");
    let accepted: BTreeSet<String> = ASK_SETTINGS.iter().map(|k| k.to_string()).collect();
    assert_eq!(ask_keys_written(&text), accepted);
    let lines = project_example_lines();
    let (_tmp, repo) = repo_with_project_file(&join(&reactivate(&lines)));
    let file = ProjectFile::load(&repo).unwrap();
    let a = &file.settings.ask;
    assert_eq!(a.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(a.thinking, Some(true));
    assert_eq!(a.effort, Some(nephila::config::Effort::Medium));
    assert_eq!(a.max_output_tokens, Some(4096));
    assert_eq!(a.max_context_chars, Some(30000));
    assert_eq!(a.max_seeds, Some(30));
    assert_eq!(a.max_nodes, Some(120));
    // Les valeurs écrites sont sous les plafonds, qui viennent d'ASK_CAPS.
    for (key, cap) in ASK_CAPS {
        let value: u64 = match key {
            "max_output_tokens" => a.max_output_tokens.map(u64::from),
            "max_context_chars" => a.max_context_chars.map(|v| v as u64),
            "max_seeds" => a.max_seeds.map(|v| v as u64),
            _ => a.max_nodes.map(|v| v as u64),
        }
        .unwrap();
        assert!(value <= cap, "{key} = {value} dépasse {cap}");
    }
}

#[test]
fn REQ165_AC16_readme_traces_sentence() {
    let readme = repo_file("README.md");
    let new = "chaque appel LLM avec son résultat, ses jetons et ses nouveaux essais, découpes, politique d'arrêt, bilan des appels et des jetons (que `query ask -v` écrit aussi). stdout est inchangé";
    assert_eq!(readme.matches(new).count(), 1);
    assert!(!readme.contains("avec son résultat et ses nouveaux essais"));
    assert_eq!(readme.matches("config: llm=").count(), 1);
    assert!(!readme
        .lines()
        .any(|l| l.starts_with("trace:") && l.contains("appels: ")));
    let size = fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .unwrap()
        .len();
    assert!(size <= 47_000, "README : {size} octets");
}

#[test]
fn REQ168_AC1_reenrichissement_a_la_mise_a_niveau() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Mise à niveau");
    let first = sec
        .lines()
        .find(|l| l.starts_with("- **Vers "))
        .expect("puce de tête");
    let lead = "- **Vers 0.26** : le premier `nephila build` ou `nephila update` réenrichit une fois chaque fichier de la portée. ";
    assert!(first.starts_with(lead), "{first}");
    assert!(first.contains("`effort` (défaut"), "{first}");
    assert!(
        first.find("réenrichit").unwrap() < first.find("`effort`").unwrap(),
        "{first}"
    );
    for needle in ["output_config", "effort = \"default\"", "requête refusée"] {
        assert!(first.contains(needle), "{first} : {needle}");
    }
    // Une phrase s'ajoute à la fin (REQ-197, item 39) ; celles qui précèdent ne bougent pas.
    assert!(
        first.contains("puis lancez `nephila update`. Si le `.nephila.toml`"),
        "{first}"
    );
    assert_eq!(
        sec.lines().filter(|l| l.starts_with("- **Vers ")).count(),
        6
    );
    let v25 = sec
        .lines()
        .find(|l| l.starts_with("- **Vers 0.25** :"))
        .unwrap();
    assert!(v25.contains("sans appel LLM"));

    assert!(!sec.contains("sans appel LLM et sans perdre"));
    assert!(sec.contains("Un graphe 0.18 à 0.24 est migré sur place"));
    assert!(sec
        .contains("par le premier `nephila update` ou `nephila build`, sans perdre les résumés."));

    let enr = readme_section(&readme, "## Enrichissement");
    let para = enr.split("### Lots et gros fichiers").next().unwrap();
    assert!(para.contains(
        "tant que son contenu, le modèle, la langue des résumés et le prompt de la version de nephila ne changent pas"
    ));
    assert!(para.contains(
        "changer `model` ou `summary_language` réenrichit une fois chaque fichier de la portée"
    ));
    assert!(!para.contains("tant que son contenu et le modèle"));

    let size = fs::metadata(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .unwrap()
        .len();
    assert!(size <= 47_000, "README : {size} octets");
}

// ---- REQ-174 : `summary_language` dans les exemples et le README ----

const LANG_BLOCK_PROJECT: &str = "\
# Langue des résumés et des noms de concepts pour ce dépôt. Nom de langue de 1 à 40 lettres,
# espaces, traits d'union ou apostrophes. Remplace la langue de la configuration personnelle
# (défaut : \"français\") ; la casse compte. La changer réenrichit les fichiers déjà enrichis.
# Priorité : ce fichier > configuration personnelle > défaut.
# summary_language = \"anglais\"
";

const LANG_BLOCK_CONFIG: &str = "\
# Langue des résumés et des noms de concepts. Nom de langue de 1 à 40 lettres, espaces, traits
# d'union ou apostrophes (\"anglais\", \"portugais du Brésil\") ; défaut : \"français\".
# Elle est écrite telle quelle dans la consigne envoyée au modèle, sans traduction : la casse
# compte (\"Français\" et \"français\" sont deux langues). Une seule langue pour tout le dépôt
# garde un vocabulaire commun : les concepts se regroupent et query search les retrouve.
# Changer de langue réenrichit une fois les fichiers déjà enrichis, comme changer de modèle.
# Les identifiants et les termes techniques restent tels quels.
# Un dépôt peut la remplacer par son .nephila.toml.
# Priorité : .nephila.toml du dépôt > ce fichier > défaut.
# summary_language = \"français\"
";

#[test]
fn REQ174_AC4_AC9_blocs_exacts_et_places_de_summary_language() {
    let project = example("nephila.toml");
    let config = example("config.toml");
    assert!(project.contains(&format!(
        "# model = \"claude-sonnet-4-5\"\n\n{LANG_BLOCK_PROJECT}\n# Taille maximale"
    )));
    assert!(config.contains(&format!(
        "# enrich_code = false\n\n{LANG_BLOCK_CONFIG}\n# Demande au modèle"
    )));
    for (name, text) in [("nephila.toml", &project), ("config.toml", &config)] {
        let lines: Vec<&str> = text.lines().collect();
        let at = |p: &str| lines.iter().position(|l| l.starts_with(p)).unwrap();
        let lang = at("# summary_language = ");
        if name == "nephila.toml" {
            assert!(at("# model = ") < lang && lang < at("# max_chunk_chars = "));
        } else {
            assert!(at("# enrich_code = ") < lang && lang < at("# thinking = "));
        }
        // Seule la ligne désactivée commence par `# summary_language =`.
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("# summary_language ="))
                .count(),
            1,
            "{name}"
        );
        assert!(keys_written(text).contains("summary_language"), "{name}");
        // Lignes de cette évolution : 100 caractères au plus.
        let block = if name == "nephila.toml" {
            LANG_BLOCK_PROJECT
        } else {
            LANG_BLOCK_CONFIG
        };
        for l in block.lines() {
            assert!(l.chars().count() <= 100, "{l}");
        }
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("# Plafond dans ce fichier :"))
                .count(),
            if name == "nephila.toml" { 9 } else { 0 }
        );
    }
    assert_eq!(
        project
            .lines()
            .filter(|l| l.starts_with("# Plafond dans ce fichier :"))
            .count(),
        9
    );
    // Phrases réécrites, anciennes absentes.
    assert!(project.contains(concat!(
        "# Changer model ou summary_language fait réenrichir les fichiers déjà enrichis, par nephila update\n",
        "# comme par nephila build ; les autres réglages ne réenrichissent rien.\n"
    )));
    assert!(!project.contains("# Changer model fait réenrichir les fichiers"));
    assert!(config.contains(concat!(
        "# Un dépôt peut remplacer sept réglages par son fichier .nephila.toml (model, summary_language,\n",
        "# max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries) ; les autres clés ne\n",
        "# sont lues qu'ici.\n"
    )));
    assert!(config.contains(concat!(
        "# Enrichissements conservés : un fichier déjà enrichi n'est renvoyé au LLM que si son contenu\n",
        "# change, ou si le modèle, summary_language (ici ou dans .nephila.toml) ou le prompt de la\n",
        "# version de nephila changent ; les autres réglages ne réenrichissent rien.\n"
    )));
    for old in [
        "six réglages",
        "si le modèle change (ici ou dans .nephila.toml)",
    ] {
        assert!(!config.contains(old), "présent : {old}");
    }
    for l in [
        "# Un dépôt peut remplacer sept réglages par son fichier .nephila.toml (model, summary_language,",
        "# max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries) ; les autres clés ne",
        "# change, ou si le modèle, summary_language (ici ou dans .nephila.toml) ou le prompt de la",
    ] {
        assert!(l.chars().count() <= 100, "{l}");
    }
    // Les lignes réactivées sont acceptées.
    let lines: Vec<String> = project.lines().map(String::from).collect();
    let activated = join(&reactivate(&lines));
    let (_tmp, repo) = repo_with_project_file(&activated);
    let settings = ProjectFile::load(&repo).unwrap().settings;
    assert_eq!(settings.summary_language.as_deref(), Some("anglais"));
    let common: Vec<String> = section(&config, COMMON)
        .into_iter()
        .filter(|l| !l.starts_with("# tls_ca_cert ="))
        .collect();
    let activated = join(&reactivate(&common));
    assert_eq!(
        parse(&format!("provider = \"mock\"\n{activated}")).summary_language,
        "français"
    );
}

#[test]
fn REQ174_AC5_AC10_readme_lignes_et_phrases_de_summary_language() {
    let readme = repo_file("README.md");
    let table = readme_section(&readme, "## Configuration du LLM");
    let lines: Vec<&str> = table.lines().collect();
    let model = lines
        .iter()
        .position(|l| l.starts_with("| `model` |"))
        .unwrap();
    assert_eq!(
        lines[model + 1],
        "| `summary_language` | Langue des résumés et des noms de concepts, en toutes lettres (`\"anglais\"`) ; aussi dans `.nephila.toml` | `\"français\"` |"
    );
    let project = readme_section(&readme, "### Fichier de projet `.nephila.toml`");
    assert!(
        project.contains("et les sept réglages `model`, `summary_language`, `max_chunk_chars`, ")
    );
    assert!(project.contains(
        "- **Changer `model` ou `summary_language` réenrichit une fois** chaque fichier de la portée ; les autres réglages ne réenrichissent rien."
    ));
    assert!(readme.contains(
        "Un fichier garde ses enrichissements tant que son contenu, le modèle, la langue des résumés et le prompt de la version de nephila ne changent pas : un second `build` sans changement ne fait aucun appel, et changer `model` ou `summary_language` réenrichit une fois chaque fichier de la portée."
    ));
    assert!(readme.contains(
        "- **Vers 0.26** : le premier `nephila build` ou `nephila update` réenrichit une fois chaque fichier de la portée. Les résumés y sont écrits en `summary_language` (défaut `\"français\"`) : pour une autre langue, écrivez la clé avant cette commande. `effort` (défaut"
    ));
    assert!(readme.contains(
        "trace: [0.000s] config: llm=mock model=m-projet enrich_code=false summary_language=\"français\" thinking=false effort=low"
    ));
    for old in [
        "les six réglages",
        "**Changer `model` réenrichit une fois**",
    ] {
        assert!(!readme.contains(old), "présent : {old}");
    }
    assert_eq!(readme.matches("config: llm=").count(), 1);
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
}

/// AC-185-2 : textes de V-012 pour `--dry-run`, ligne de table après `--enrich-code`.
#[test]
fn req185_ac2_readme_dry_run() {
    let readme = repo_file("README.md");
    let row = "| `--dry-run` | Estime l'enrichissement sans rien envoyer ; structure écrite comme avec `--no-llm`, incompatible avec lui |";
    let para = "Avec `--dry-run`, des lignes `estimation` suivent le bilan : fichiers à enrichir selon la configuration, appels, octets et jetons au minimum (un essai par appel) et au maximum (tous les nouveaux essais de format), fichiers au plafond. Aucune clé d'API n'est lue. Jetons à ±30 % ; hors estimation : nouveaux essais HTTP, lots coupés en deux, réflexion, cache.";
    assert!(readme.contains(row) && readme.contains(para));
    let enrich = readme.find("| `--enrich-code` |").unwrap();
    let at = readme.find(row).unwrap();
    assert!(enrich < at);
    assert!(at < readme.find("| `-v, --verbose` |").unwrap());
    let section = readme_section(&readme, "### `nephila build` et `nephila update`");
    let table = section.lines().filter(|l| l.starts_with("| `-")).count();
    assert_eq!(table, 12, "{section}");
    assert!(readme.find("Un seul `build` ou `update`").unwrap() < readme.find(para).unwrap());
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
}

// ---- REQ-188 : `max_calls` dans l'exemple et le README ----

#[test]
fn REQ188_AC2_AC5_section_max_calls_de_l_exemple() {
    let text = example("config.toml");
    let lines: Vec<&str> = text.lines().collect();
    let at = |wanted: &str| position_of(&lines, wanted);
    let title = at("# --- Plafond d'appels ---");
    assert!(at("# tls_insecure = false") < title);
    assert!(title < at("# --- Réglages de query ask ---"));
    let key = at("# max_calls = 500");
    assert_eq!(
        lines[key - 1],
        "# Priorité : --max-calls > ce fichier > défaut."
    );
    assert!(title < key && key < at("# --- Réglages de query ask ---"));
    let common = &lines[at("# --- Options communes ---")..title];
    assert!(!common.iter().any(|l| l.contains("max_calls")));
    assert!(!example("nephila.toml").contains("max_calls"));
    // décommentée, la ligne est acceptée
    let enabled = text.replace("# max_calls = 500", "max_calls = 500");
    assert_eq!(parse(&enabled).max_calls, Some(500));
}

#[test]
fn REQ188_AC3_AC6_textes_du_readme() {
    let readme = repo_file("README.md");
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
    let has_after = |first: &str, second: &str| {
        let a = readme
            .find(first)
            .unwrap_or_else(|| panic!("absent : {first}"));
        assert_eq!(readme[a..].lines().nth(1), Some(second), "{first}");
    };
    has_after(
        "| `format_retries` | Nouveaux essais d'une réponse non conforme, entier ≥ 0 | 2 |",
        "| `max_calls` | Plafond d'appels d'enrichissement par commande, nouveaux essais de format compris ; configuration personnelle seulement | aucun |",
    );
    has_after(
        "| `--format-retries <N>` | Nouveaux essais d'une réponse non conforme, pour cette commande |",
        "| `--max-calls <N>` | Plafond d'appels d'enrichissement, pour cette commande |",
    );
    let sec = readme_section(&readme, "### Arrêt de l'envoi");
    assert!(sec.trim_start().starts_with(
        "Huit causes arrêtent l'envoi au LLM. Les six de la table sont des erreurs :"
    ));
    let causes = table_rows(&sec, "| Cause | Quand | Que faire |", "|---|---|---|");
    assert_eq!(causes.len(), 6, "{causes:?}");
    assert!(sec.contains("\nLe plafond `max_calls` (ou `--max-calls`) est la septième cause, et pas une erreur : il borne les appels d'une commande, nouveaux essais de format compris. Quand il en faudrait un de plus, plus rien ne part, les appels en cours se terminent, et une ligne `avertissement: plafond de <n> appels atteint : …` compte les fichiers laissés à enrichir. Code 0 si rien d'autre n'échoue : chaque `nephila update` enrichit la tranche suivante.\n"));
    assert!(readme.contains(
        "\n| 0 | Succès, même si seul le plafond `max_calls` laisse des fichiers à enrichir |\n"
    ));
    assert!(readme.contains("- **à enrichir** : la structure du fichier est dans le graphe, mais le LLM ne l'a pas enrichie (réponse non conforme ou tronquée, trop d'appels, plafond `max_calls`, erreur, `--no-llm`, fichier modifié depuis) ; code 1 avec le LLM actif, 0 sans lui ou s'il est laissé par le plafond ; `nephila update` le reprend.\n"));
}

#[test]
fn REQ192_AC3_ctrl_c_dans_le_readme() {
    let readme = repo_file("README.md");
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
    let codes = readme_section(&readme, "## Codes de sortie");
    let rows = table_rows(&codes, "| Code | Signification |", "|---|---|");
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert_eq!(
        rows[3],
        "| 130 | Interrompu par Ctrl-C (SIGINT) pendant l'enrichissement ; `nephila update` reprend les fichiers à enrichir |"
    );
    let sec = readme_section(&readme, "### Arrêt de l'envoi");
    let para = "Un Ctrl-C (SIGINT) pendant l'enrichissement de `build` ou `update` est la huitième cause : plus rien ne part, les appels en cours se terminent et sont enregistrés, et une ligne `avertissement: Ctrl-C reçu : …` l'annonce. La commande finit ensuite comme après une erreur, bilan compris, sur `erreur: envoi interrompu : Ctrl-C reçu : …` et le code 130 ; `nephila update` reprend les fichiers à enrichir. Un second Ctrl-C quitte aussitôt, code 130 : ni bilan, ni attente, les réponses en cours sont perdues, le graphe reste lisible. Hors de l'enrichissement, Ctrl-C arrête la commande sur-le-champ.";
    assert_eq!(sec.trim_end().lines().last(), Some(para));
    let before = sec.trim_end().lines().rev().nth(1);
    assert_eq!(before, Some(""));
    assert!(!readme.contains("la seule voulue"));
    for args in [&["build", "--help"][..], &["update", "--help"][..]] {
        assert!(!help_of(args).contains("Ctrl-C"), "{args:?}");
    }
}

// ---- REQ-195, REQ-197 : `model_override` dans les exemples et le README ----

const MODEL_OVERRIDE_BLOCK: &str = r#"# Ce que fait nephila quand le .nephila.toml d'un dépôt impose un modèle (model, ou model de
# sa section [ask]). Texte : "allow", "warn" ou "deny" ; défaut : "warn".
#   "allow" : le modèle du dépôt s'applique, sans message.
#   "warn"  : le modèle du dépôt s'applique ; quand il fait réenrichir des fichiers déjà enrichis
#             avec un autre modèle, build et update le disent par un avertissement qui donne leur
#             nombre, avant tout appel. Au-delà de 50 fichiers, ils refusent, sans rien envoyer
#             ni écrire, sauf avec l'option --accept-project-model (pour cette commande) ;
#             --dry-run ne refuse jamais. query ask n'est pas concerné.
#   "deny"  : le modèle du dépôt est ignoré partout (build, update, --dry-run, query ask) : le
#             vôtre s'applique ; les autres réglages du dépôt restent.
# Un dépôt cloné ne peut ainsi pas faire réenrichir votre graphe, avec votre clé d'API, par un
# modèle de son choix sans que vous le sachiez. Lue ici seulement :
# .nephila.toml la refuse.
# model_override = "warn""#;

const MODEL_BLOCK_PROJECT: &str = "\
# Nom du modèle utilisé pour ce dépôt. Texte non vide. Remplace le modèle de la configuration
# personnelle, qui peut aussi l'ignorer ou demander une confirmation (clé model_override) : par
# défaut, s'il fait réenrichir plus de 50 fichiers déjà enrichis avec un autre modèle, build et
# update refusent sans l'option --accept-project-model.
# Priorité : ce fichier > configuration personnelle > défaut.
# model = \"claude-sonnet-4-5\"";

const MODEL_ASK_LINE: &str = "# Modèle qui répond aux questions. Texte non vide. Absente : le modèle de l'enrichissement.\n# Ignorée, comme model ci-dessus, si la configuration personnelle dit model_override = \"deny\".\n";

#[test]
fn REQ197_AC2_AC5_bloc_model_override_de_l_exemple_personnel() {
    let config = example("config.toml");
    assert!(
        config.contains(&format!(
            "# tls_insecure = false\n\n{MODEL_OVERRIDE_BLOCK}\n\n# --- Plafond d'appels ---"
        )),
        "bloc absent ou mal placé"
    );
    assert_eq!(MODEL_OVERRIDE_BLOCK.lines().count(), 14);
    for l in MODEL_OVERRIDE_BLOCK.lines() {
        assert!(l.chars().count() <= 100, "{l}");
    }
    assert_eq!(
        config
            .lines()
            .filter(|l| l.starts_with("# model_override"))
            .count(),
        1
    );
    // AC-041-2 : la clé est écrite ; AC-041-4 : réactivée, elle vaut le défaut.
    assert!(keys_written(&config).contains("model_override"));
    let common = section(&config, COMMON);
    assert!(common.iter().any(|l| l == "# model_override = \"warn\""));
    let enabled = config.replace("# model_override = \"warn\"", "model_override = \"warn\"");
    assert_eq!(
        parse(&enabled).model_override,
        nephila::config::ModelOverride::Warn
    );
    for needle in [
        "\"allow\"",
        "\"warn\"",
        "\"deny\"",
        "50",
        "--accept-project-model",
    ] {
        assert!(MODEL_OVERRIDE_BLOCK.contains(needle), "{needle}");
    }
}

#[test]
fn REQ197_AC2_AC5_blocs_de_model_dans_l_exemple_du_depot() {
    let project = example("nephila.toml");
    assert!(project.contains(MODEL_BLOCK_PROJECT));
    assert!(project.contains(MODEL_ASK_LINE));
    assert!(!project.lines().any(|l| l.starts_with("# model_override")));
    assert!(!keys_written(&project).contains("model_override"));
    // Décommenté, le fichier reste accepté par le lecteur du fichier de projet.
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = project.lines().map(String::from).collect();
    fs::write(dir.path().join(".nephila.toml"), join(&reactivate(&lines))).unwrap();
    ProjectFile::load(dir.path()).unwrap();
}

#[test]
fn REQ197_AC3_AC6_readme_model_override() {
    let readme = repo_file("README.md");
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
    let table = readme_section(&readme, "## Configuration du LLM");
    let lines: Vec<&str> = table.lines().collect();
    let lang = lines
        .iter()
        .position(|l| l.starts_with("| `summary_language` |"))
        .unwrap();
    assert_eq!(
        lines[lang + 1],
        "| `model_override` | Modèle imposé par `.nephila.toml` (`model`, `[ask].model`) : `\"allow\"` l'accepte, `\"warn\"` aussi mais avertit et refuse au-delà de 50 fichiers à réenrichir sans `--accept-project-model`, `\"deny\"` l'ignore ; configuration personnelle seulement | `\"warn\"` |"
    );
    assert!(lines[lang + 2].starts_with("| `api_key_env` |"));
    let options = readme.lines().collect::<Vec<_>>();
    let mc = options
        .iter()
        .position(|l| l.starts_with("| `--max-calls <N>` |"))
        .unwrap();
    assert_eq!(
        options[mc + 1],
        "| `--accept-project-model` | Accepte le modèle de `.nephila.toml` même s'il réenrichit plus de 50 fichiers, pour cette commande |"
    );
    assert!(options[mc + 2].starts_with("| `--exclude <MOTIF>` |"));
    let bullet = "- **Changer `model` ou `summary_language` réenrichit une fois** chaque fichier de la portée ; les autres réglages ne réenrichissent rien. Un modèle imposé ici (`model`, `[ask].model`) suit `model_override` de la configuration personnelle : par défaut (`\"warn\"`), `build` et `update` l'annoncent par un avertissement qui compte les fichiers déjà enrichis à refaire, et refusent au-delà de 50 sans `--accept-project-model` ; `\"deny\"` l'ignore partout, `query ask` compris ; `\"allow\"` l'accepte sans message. Pour vérifier les valeurs effectives : `nephila build --verbose` (ligne `config`).";
    assert_eq!(
        readme
            .lines()
            .filter(|l| l
                .starts_with("- **Changer `model` ou `summary_language` réenrichit une fois**"))
            .collect::<Vec<_>>(),
        vec![bullet]
    );
    let vers = readme
        .lines()
        .find(|l| l.starts_with("- **Vers 0.26**"))
        .unwrap();
    assert!(vers.ends_with(" Si le `.nephila.toml` d'un dépôt impose un modèle qui ferait réenrichir plus de 50 fichiers déjà enrichis, `build` et `update` refusent : ajoutez `--accept-project-model`, ou `model_override = \"allow\"` dans la configuration personnelle."));
    assert!(vers.contains("puis lancez `nephila update`. Si le"));
}

#[test]
fn REQ199_AC10_readme_fichier_absent_d_une_reponse_de_lot() {
    let readme = repo_file("README.md");
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
    let section = readme_section(&readme, "### Échecs et nouveaux essais");
    let bullets: Vec<&str> = section.lines().filter(|l| l.starts_with("- ")).collect();
    let tronquee = bullets
        .iter()
        .position(|l| l.starts_with("- **Réponse tronquée**"))
        .unwrap();
    assert_eq!(
        bullets[tronquee + 1],
        "- **Fichier absent d'une réponse de lot** : renvoyé une fois, avec les autres absents du même lot ; encore absent, il échoue (`absent de la réponse du lot`)."
    );
    assert!(bullets[tronquee + 2].starts_with("- **Ce qui reste**"));
    assert_eq!(bullets.len(), 5, "{bullets:?}");
}

/// AC-200-9 : puce « Réponse non conforme » du README.
#[test]
fn req200_ac9_readme_non_conforming_reply_bullet() {
    let readme = repo_file("README.md");
    let section = readme_section(&readme, "### Échecs et nouveaux essais");
    let bullets: Vec<&str> = section
        .lines()
        .filter(|l| l.starts_with("- **Réponse non conforme**"))
        .collect();
    assert_eq!(
        bullets,
        ["- **Réponse non conforme** : renvoyée aussitôt, jusqu'à `format_retries` fois (défaut 2) ; encore non conforme, un lot de 4 fichiers ou plus est découpé une fois en deux, chaque moitié avec ses propres essais ; l'échec final nomme le fichier avec `(<k> essais sur <t>)` ; si le modèle échoue souvent, changez-le ou réduisez `batch_files`."]
    );
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
}

const STRUCTURED_BLOCK: &str = "# Schéma JSON envoyé avec chaque appel d'enrichissement : le serveur l'impose à la réponse, ce
# qui évite des réponses non conformes et leurs nouveaux essais. Texte : \"auto\", \"on\" ou \"off\" ;
# défaut : \"auto\".
#   \"auto\" : envoyé au serveur public du fournisseur, jamais quand base_url est écrite (serveur
#            local ou compatible) ; refusé par le serveur (HTTP 400 qui cite format ou schema),
#            la requête repart sans lui, et la suite de la commande s'en passe.
#   \"on\"   : toujours envoyé, même avec base_url (Ollama et vLLM l'acceptent) ; un refus arrête
#            l'envoi, comme toute requête refusée.
#   \"off\"  : jamais envoyé ; seule la consigne du prompt décrit la réponse.
# Envoyé à « anthropic » dans \"output_config\" et à « openai » dans \"response_format\". Sans
# effet avec « mock ». Le changer ne réenrichit rien. Sans effet sur query ask, qui a sa clé
# dans la section [ask]. Lue ici seulement :
# .nephila.toml la refuse.
# structured_output = \"auto\"";

const STRUCTURED_ASK_BLOCK: &str = "# Schéma JSON imposé à la réponse de query ask. Texte : \"auto\", \"on\" ou \"off\", mêmes effets
# qu'au premier niveau. Absente : \"off\", même si structured_output est écrite au premier
# niveau : à activer ici pour query ask. Lue ici seulement :
# .nephila.toml la refuse.
# structured_output = \"off\"";

/// AC-205-1, AC-205-3 : blocs exacts de `structured_output` dans l'exemple personnel.
#[test]
fn req205_ac1_ac3_blocs_structured_output_de_l_exemple_personnel() {
    let config = example("config.toml");
    assert_eq!(STRUCTURED_BLOCK.lines().count(), 14);
    assert_eq!(STRUCTURED_ASK_BLOCK.lines().count(), 5);
    assert_eq!(config.matches(STRUCTURED_BLOCK).count(), 1);
    assert_eq!(config.matches(STRUCTURED_ASK_BLOCK).count(), 1);
    assert!(config.contains(&format!(
        "# max_output_tokens = 16384\n\n{STRUCTURED_BLOCK}\n\n# Taille maximale, en caractères, d'un morceau"
    )));
    assert!(config.contains(&format!(
        "# max_output_tokens = 4096\n\n{STRUCTURED_ASK_BLOCK}\n\n# Nombre maximal de caractères du contexte"
    )));
    assert_eq!(
        config
            .lines()
            .filter(|l| l.starts_with("# structured_output ="))
            .count(),
        2
    );
    for l in STRUCTURED_BLOCK.lines().chain(STRUCTURED_ASK_BLOCK.lines()) {
        assert!(l.chars().count() <= 100, "{l}");
    }
    // AC-041-2 : clés de l'exemple = clés du chargeur, au premier niveau et dans [ask].
    assert!(keys_written(&config).contains("structured_output"));
    assert!(ask_keys_written(&config).contains("structured_output"));
    // AC-041-4 : réactivée avec les autres options communes, la ligne ne change rien.
    let common = section(&config, COMMON);
    assert!(common.iter().any(|l| l == "# structured_output = \"auto\""));
    let enabled = config
        .replace("\n# [ask]\n", "\n[ask]\n")
        .replace(
            "# structured_output = \"auto\"",
            "structured_output = \"auto\"",
        )
        .replace(
            "# structured_output = \"off\"",
            "structured_output = \"off\"",
        );
    let c = parse(&enabled);
    assert_eq!(c.structured_output, nephila::config::StructuredOutput::Auto);
    assert_eq!(
        c.ask.structured_output,
        Some(nephila::config::StructuredOutput::Off)
    );
    // Le fichier du dépôt ne la contient pas.
    assert!(!example("nephila.toml").contains("structured_output"));
}

/// AC-205-2 : lignes exactes des items 42 à 44 du README, une fois, et taille.
#[test]
fn req205_ac2_readme_lignes_structured_output() {
    let readme = repo_file("README.md");
    assert!(readme.len() <= 47_000, "README : {} octets", readme.len());
    let lines: Vec<&str> = readme.lines().collect();
    let count = |l: &str| lines.iter().filter(|x| **x == l).count();
    assert_eq!(count("| `structured_output` | Schéma JSON imposé aux réponses d'enrichissement : `\"auto\"` l'envoie sans `base_url` et, s'il est refusé (HTTP 400), s'en passe pour la commande ; `\"on\"` l'envoie toujours, et un refus arrête l'envoi ; `\"off\"` jamais ; configuration personnelle seulement | `\"auto\"` |"), 1);
    let pos = lines
        .iter()
        .position(|l| l.starts_with("| `effort` | `output_config.effort`"))
        .unwrap();
    assert_eq!(
        lines[pos + 1],
        "| `structured_output` | `output_config.format` | `response_format` | sans effet |"
    );
    assert_eq!(
        count("| `structured_output` | `output_config.format` | `response_format` | sans effet |"),
        1
    );
    let phrase = "bornent le contexte envoyé. Dans la configuration personnelle seulement, `structured_output` (défaut `\"off\"`, même si la clé du premier niveau est écrite) impose aussi le schéma JSON à la réponse de `query ask`. Priorité :";
    assert_eq!(readme.matches(phrase).count(), 1);
}
