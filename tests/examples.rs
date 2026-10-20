//! Exemples de configuration `examples/config.toml` et `examples/nephila.toml` confrontés au code
//! (REQ-041, DEC-051).
#![allow(non_snake_case)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use nephila::config::{
    Config, ProjectSettings, Provider, DEFAULT_BATCH_FILES, DEFAULT_CONCURRENCY,
    DEFAULT_FORMAT_RETRIES, DEFAULT_MAX_CHUNK_CHARS, DEFAULT_TIMEOUT_SECS, PROJECT_SETTINGS,
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

/// Clés écrites dans le texte (lignes actives ou désactivées), dans l'ordre, sans doublon.
fn keys_written(text: &str) -> BTreeSet<String> {
    text.lines().filter_map(key_of).map(String::from).collect()
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
            Some(rest) if key_of(l).is_some() => rest.to_string(),
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
    let reference = parse(&text);
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
    assert_eq!(parse(&join(&replaced)), reference);
    // Les lignes réactivées existent bien (le test ne passe pas à vide).
    let active: Vec<&str> = activated
        .iter()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| key_of(l))
        .collect();
    assert_eq!(active.len(), 6, "options communes réactivées : {active:?}");
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
    assert!(text.contains(
        "# Un dépôt peut remplacer six réglages par son fichier .nephila.toml (model, max_chunk_chars, concurrency, timeout_secs, batch_files, format_retries) ; les autres clés ne sont lues qu'ici."
    ));
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
    let written: Vec<(&str, String)> = activated
        .iter()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let key = key_of(l)?;
            PROJECT_SETTINGS
                .contains(&key)
                .then(|| (key, l.split_once(" = ").unwrap().1.trim().to_string()))
        })
        .collect();
    assert_eq!(written.len(), PROJECT_SETTINGS.len());
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
