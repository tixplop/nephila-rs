//! Exemples de configuration `examples/config.toml` et `examples/nephila.toml` confrontés au code
//! (REQ-041, DEC-051).
#![allow(non_snake_case)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use nephila::config::{
    Config, ProjectSettings, Provider, DEFAULT_BATCH_FILES, DEFAULT_CONCURRENCY,
    DEFAULT_FORMAT_RETRIES, DEFAULT_MAX_CHUNK_CHARS, DEFAULT_TIMEOUT_SECS, PROJECT_CAPS,
    PROJECT_SETTINGS,
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
    assert_eq!(active.len(), 10, "options communes réactivées : {active:?}");
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

#[test]
fn REQ042_AC13_readme_decrit_les_reglages_de_projet() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "### Fichier de projet `.nephila.toml`");
    assert!(sec.contains(".nephila.toml"));
    for k in [
        "model",
        "max_chunk_chars",
        "concurrency",
        "timeout_secs",
        "batch_files",
        "format_retries",
    ] {
        assert!(sec.contains(k), "réglage absent de la section : {k}");
    }
    for needle in [
        "configuration personnelle",
        "provider",
        "base_url",
        "api_key_env",
        "tls_ca_cert",
        "tls_insecure",
        "--no-llm",
        "graphe à jour",
        "nephila build",
        "build --verbose",
        "examples/nephila.toml",
        "au plus 64",
    ] {
        assert!(
            sec.contains(needle),
            "texte absent de la section : {needle}"
        );
    }
    assert!(sec.contains("option > fichier de projet > configuration personnelle > défaut"));
    assert!(!readme.contains("configuration globale"));
    // AC-042-13 modifié : réenrichissement par `update` ou `build` après un changement de modèle.
    assert!(
        sec.contains("réenrichi") && sec.contains("nephila update"),
        "le réenrichissement par update ou build après un changement de modèle n'est pas décrit"
    );
    assert!(
        !sec.contains("Changer de modèle ne réenrichit rien"),
        "texte périmé : le changement de modèle réenrichit"
    );
}

#[test]
fn REQ050_AC9_readme_decrit_la_portee_de_l_enrichissement() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Enrichissement en lots");
    assert!(sec.contains(
        "Par défaut, seuls les documents Markdown sont enrichis ; enrich_code = true (configuration personnelle) ou --enrich-code (une commande) enrichit aussi le code source et les fichiers de build et de données."
    ));
    for needle in [
        "enrich_code",
        "--enrich-code",
        "conservés",
        "fichiers à enrichir",
        "max_chunk_chars",
        "concurrency",
    ] {
        assert!(
            sec.contains(needle),
            "texte absent de la section : {needle}"
        );
    }
    let line = readme
        .lines()
        .find(|l| l.starts_with("| 0.18 |"))
        .expect("ligne 0.18 absente du tableau des versions");
    assert!(line.contains("Rupture : seuls les documents Markdown sont enrichis par défaut"));
    assert!(line.contains("enrich_code = true"));
    assert!(line.contains("--enrich-code"));
    assert!(line.contains("0.17"));
    let at = |needle: &str| readme.find(needle).unwrap();
    assert!(at("| 0.18 |") < at("| 0.17.5 |"));
    for needle in ["V=v0.24.0", "git tag v0.24.0 && git push origin v0.24.0"] {
        assert!(readme.contains(needle), "absent : {needle}");
    }
    assert!(!readme.contains("0.17.6"));
    // Plus de vérification préalable de la clé (REQ-054).
    assert!(!readme.contains("/v1/models"));
    assert!(!readme.contains("verify_key"));
    assert!(!readme.contains("la vérification de la clé"));
    let verbose = readme_section(&readme, "### Traces (`--verbose`)");
    for needle in [
        "enrichissements conservés",
        "llm enrich attente",
        "enrich_code",
    ] {
        assert!(verbose.contains(needle), "trace absente : {needle}");
    }
    let retries = readme_section(&readme, "## Réponses non conformes et nouveaux essais");
    for needle in [
        "429",
        "503",
        "529",
        "retry-after",
        "60 s",
        "4 nouveaux essais",
    ] {
        assert!(retries.contains(needle), "nouveaux essais HTTP : {needle}");
    }
    let dev = readme_section(&readme, "## Développement");
    assert!(dev.contains("NEPHILA_RETRY_BASE_MS"));
    assert!(dev.contains("chaque appel échoue en « clé refusée »"));
    let cfg = readme_section(&readme, "## Configuration du LLM");
    assert!(cfg.contains("| `enrich_code` |"));
    assert!(
        cfg.contains("| `batch_files` | Fichiers par appel d'enrichissement, entier > 0 | 10 |")
    );
    let upgrade = readme_section(&readme, "## Mise à niveau");
    assert!(upgrade.contains("schéma 5"));
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
    assert_eq!(count, 5);
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
fn REQ042_AC21_readme_table_des_plafonds() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "### Fichier de projet `.nephila.toml`");
    let lines: Vec<&str> = sec.lines().collect();
    let head = lines
        .iter()
        .position(|l| *l == "| Réglage | Plafond dans `.nephila.toml` |")
        .expect("en-tête de table absent");
    assert_eq!(lines[head + 1], "|---|---|");
    for (i, (key, cap)) in PROJECT_CAPS.iter().enumerate() {
        assert_eq!(lines[head + 2 + i], format!("| `{key}` | {cap} |"));
    }
    assert!(!lines[head + 2..head + 7]
        .iter()
        .any(|l| l.contains("`model`")));
    assert!(lines[head + 7].is_empty());
    assert_eq!(lines[head + 8], "Ces plafonds ne valent que pour .nephila.toml : la configuration personnelle et les options --batch-files et --format-retries n'en ont pas. Un dépôt cloné ne peut ainsi imposer ni plus de fils, ni des appels plus longs ou plus gros, ni plus d'essais.");
}

#[test]
fn REQ042_AC23_readme_ligne_0_17() {
    let readme = repo_file("README.md");
    let lines: Vec<&str> = readme.lines().collect();
    let expected = "| 0.17 | Plafonds des réglages entiers de `.nephila.toml` (rupture : une valeur au-dessus est refusée ; pour corriger le fichier, donnez au réglage une valeur au plus égale à son plafond, donné par la table de la section « Fichier de projet `.nephila.toml` », ou déplacez le réglage dans votre configuration personnelle, qui n'a pas de plafond mais vaut pour tous vos dépôts ; les options `--batch-files` et `--format-retries` n'ont pas de plafond non plus, mais ne corrigent pas un fichier refusé) |";
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("| 0.17 |")).count(),
        1
    );
    let i = lines
        .iter()
        .position(|l| *l == expected)
        .expect("ligne 0.17 absente");
    assert!(lines[i + 1].starts_with("| 0.16.1 |"));
}

#[test]
fn REQ048_AC11_readme_second_remede() {
    let readme = repo_file("README.md");
    let expected = "Deux remèdes : `exclude` dans [`.nephila.toml`](#fichier-de-projet-nephilatoml), qui retire aussi sa structure du graphe ; ou une valeur plus grande de `max_chunk_chars` (dans `.nephila.toml`, au plus 1000000, ou dans la configuration personnelle), qui réduit le nombre de morceaux, donc d'appels, mais vaut pour tous les fichiers et allonge chaque requête. `max_chunk_chars` ne suffit pas pour un fichier de plus de 7500 nœuds (50 appels de 150 nœuds), comme une classe générée de 20 000 méthodes : seul `exclude` règle alors le cas.";
    let lines: Vec<&str> = readme
        .lines()
        .filter(|l| {
            l.contains("trop d'appels d'enrichissement (134 nécessaires, au plus 50 par fichier)")
        })
        .collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].ends_with(expected), "{}", lines[0]);
    assert!(!readme.contains("Seul remède"));
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
    assert!(project.lines().any(|l| l
        == concat!(
            "# Changer model fait réenrichir les fichiers déjà enrichis, par nephila update ",
            "comme par nephila build ; les autres réglages ne réenrichissent rien."
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
fn REQ017_AC1_readme_0_19_requetes_sans_parcours_du_depot() {
    let readme = repo_file("README.md");
    for expected in [
        "Les requêtes (`query`) ne parcourent pas le dépôt : elles répondent d'après le graphe, même s'il est en retard sur les fichiers. Avec `--check-stale`, elles le vérifient d'abord et le signalent sur stderr (`avertissement: graphe pas à jour : 1 fichier modifié`).",
        "Toutes les requêtes acceptent `--json` et `--check-stale`.",
        "**Graphe en retard** : par défaut, une requête ne lit que le graphe et ne dit pas s'il est en retard sur les fichiers. Avant une série de requêtes, `nephila status` le vérifie en comparant les contenus, et `nephila update` le remet à jour. Pour vérifier au moment de la requête, ajoutez `--check-stale` : la requête parcourt d'abord le dépôt et compare la taille et la date de modification de chaque fichier à celles du graphe, sans lire les fichiers déjà indexés (les filtres de `.nephila.toml` s'appliquent). Elle répond ensuite normalement ; si des fichiers diffèrent, stderr commence par `avertissement: graphe pas à jour : <n> fichiers modifiés`, où `<n>` compte les fichiers ajoutés, supprimés, modifiés et en attente. Ce compte est approché, et `nephila status` peut en donner un autre :",
        "- un fichier seulement touché (date changée, contenu identique, après un aller-retour entre deux branches git par exemple) est compté ; `nephila update` enregistre sa nouvelle date et l'avertissement disparaît ;",
        "- un fichier réécrit à taille égale, avec sa date d'origine restaurée, n'est pas compté ;",
        "- un `package.json` d'un graphe 0.9, à réanalyser en fichier de build, n'est pas compté.",
        "Les requêtes avec --check-stale ne comparent que la taille et la date (voir [`nephila query`](#nephila-query)).",
        "- **Vers 0.19** : les requêtes n'avertissent plus d'un graphe en retard. Un script qui s'appuyait sur l'avertissement ajoute --check-stale à ses requêtes, ou lance nephila status avant elles.",
        "Rupture : les requêtes ne parcourent plus le dépôt et n'avertissent plus d'un graphe en retard par défaut ; --check-stale rétablit l'avertissement, calculé sur la taille et la date des fichiers.",
        "V=v0.24.0 ",
        "git tag v0.24.0 && git push origin v0.24.0",
    ] {
        assert!(readme.contains(expected), "absent : {expected}");
    }
    assert!(!readme
        .contains("Si le graphe est en retard sur les fichiers, elles le signalent sur stderr"));
    let lines: Vec<&str> = readme.lines().collect();
    let i19 = lines
        .iter()
        .position(|l| l.starts_with("| 0.19 |"))
        .expect("ligne 0.19 absente");
    let i18 = lines
        .iter()
        .position(|l| l.starts_with("| 0.18 |"))
        .expect("ligne 0.18 absente");
    assert_eq!(i19 + 1, i18);
    let i192 = lines
        .iter()
        .position(|l| l.starts_with("| 0.19.2 |"))
        .expect("ligne 0.19.2 absente");
    assert_eq!(i192 + 1, i19);
    let i20 = lines
        .iter()
        .position(|l| l.starts_with("| 0.20 |"))
        .expect("ligne 0.20 absente");
    assert_eq!(i20 + 1, i192);
    let v19 = readme.find("- **Vers 0.19**").expect("puce Vers 0.19");
    let v18 = readme.find("- **Vers 0.18**").expect("puce Vers 0.18");
    assert!(v19 < v18);
}

#[test]
fn REQ055_AC1_readme_0_19_migration_sur_place() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Mise à niveau");
    assert!(sec.contains("schéma 6"));
    assert!(sec.contains("sans appel LLM"));
    let row = readme
        .lines()
        .find(|l| l.starts_with("| 0.19 |"))
        .expect("ligne 0.19");
    for w in ["query ask", "neighbors", "path", "schéma 6"] {
        assert!(row.contains(w), "{w}");
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
    let project = example("nephila.toml");
    for k in ["effort", "thinking", "max_output_tokens"] {
        assert!(!project.contains(k), "{k} dans nephila.toml");
    }
}

#[test]
fn REQ058_AC12_readme_options_llm_table_et_phrase() {
    let readme = repo_file("README.md");
    let lines: Vec<&str> = readme.lines().collect();
    for k in [
        "effort",
        "thinking",
        "max_output_tokens",
        "max_completion_tokens",
        "reasoning_effort",
        "output_config",
    ] {
        assert!(readme.contains(k), "absent : {k}");
    }
    let rows = [
        "| `max_output_tokens` | `max_tokens` | `max_completion_tokens` | sans effet |",
        "| `thinking = true` | `\"thinking\": {\"type\": \"adaptive\"}` | aucun champ propre (voir `effort`) | sans effet |",
        "| `effort` (avec `thinking = true` seulement) | `output_config.effort` | `reasoning_effort` | sans effet |",
    ];
    let first = lines
        .iter()
        .position(|l| *l == rows[0])
        .expect("première ligne de la table absente");
    assert_eq!(
        lines[first - 2],
        "| Clé | `anthropic` | `openai` | `mock` |"
    );
    assert_eq!(lines[first - 1], "|---|---|---|---|");
    assert_eq!(lines[first + 1], rows[1]);
    assert_eq!(lines[first + 2], rows[2]);
    assert_eq!(lines[first + 3], "");
    assert_eq!(
        lines[first + 4],
        "Avec thinking = false, aucun champ de réflexion n'est envoyé ; un modèle qui réfléchit toujours le fait quand même. Pour limiter sa réflexion : thinking = true et effort = \"low\". Un modèle qui ne prend pas en charge ces champs refuse la requête (HTTP 400). Chez anthropic, la réflexion adaptative demande un modèle Claude 4.6 ou suivant : claude-sonnet-4-5, le modèle des exemples, la refuse. Ces trois clés ne sont lues que dans la configuration personnelle : .nephila.toml les refuse."
    );
    let expected = "| 0.20 | Options LLM thinking, effort et max_output_tokens dans la configuration personnelle. Rupture : une limite de sortie de 16384 jetons est envoyée par défaut, à anthropic (au lieu de 8192) et désormais à openai ; un modèle ou un serveur qui ne l'accepte pas refuse la requête (HTTP 400) : baissez max_output_tokens. Une réponse tronquée conseille aussi d'augmenter max_output_tokens. |";
    assert_eq!(lines.iter().filter(|l| **l == expected).count(), 1);
    let sec = readme_section(&readme, "## Configuration du LLM");
    for w in ["16384", "HTTP 400", "\"low\""] {
        assert!(sec.contains(w), "section sans : {w}");
    }
    assert!(!readme.contains("- **Vers 0.20**"));
    let cfg = readme
        .lines()
        .filter(|l| l.starts_with("| `thinking` |") || l.starts_with("| `effort` |"))
        .count();
    assert_eq!(cfg, 2);
}

#[test]
fn REQ058_AC15_readme_textes_de_la_reponse_tronquee_revises() {
    let readme = repo_file("README.md");
    for expected in [
        "avertissement: db/schema.sql : réponse tronquée par la limite de jetons ; réduisez max_chunk_chars ou batch_files, ou augmentez max_output_tokens",
        "erreur: réponse tronquée par la limite de jetons ; posez une question plus ciblée ou augmentez max_output_tokens",
    ] {
        assert!(readme.lines().any(|l| l == expected), "absent : {expected}");
    }
}

#[test]
fn REQ065_AC1_readme_0_21_stockage_allege() {
    let readme = repo_file("README.md");
    let row = "| 0.21 | Stockage SQLite allégé : contexte d'imports stocké une fois par fichier, index inutiles retirés, cache SQLite agrandi à chaque ouverture en écriture, écritures regroupées, build neuf sans recalcul répété des nœuds. Même graphe qu'en 0.20 ; le schéma 7 est migré sur place. |";
    let bullet = "- **Vers 0.21** : le graphe passe au schéma 7 sur place : le premier `nephila build` ou `nephila update` le migre sans appel LLM et sans perdre les résumés ; les requêtes, `status`, `export` et `summary` lisent un graphe 0.18 à 0.20 tel quel. La place libérée n'est rendue au disque que par `sqlite3 .nephila/graph.db 'VACUUM'`. Un binaire 0.20 refuse ensuite ce graphe : pour revenir en arrière, supprimez `.nephila/` (le build suivant réenrichit alors tous les fichiers de la portée).";
    let lines: Vec<&str> = readme.lines().collect();
    assert_eq!(lines.iter().filter(|l| **l == row).count(), 1);
    let i21 = lines.iter().position(|l| *l == row).unwrap();
    let i20 = lines
        .iter()
        .position(|l| l.starts_with("| 0.20 |"))
        .expect("ligne 0.20 absente");
    assert_eq!(i21 + 1, i20);
    let sec = readme_section(&readme, "## Mise à niveau");
    let b = sec.find(bullet).expect("puce Vers 0.21 absente");
    let v19 = sec.find("- **Vers 0.19**").expect("puce Vers 0.19");
    assert!(b < v19);
    assert_eq!(sec[b..v19].trim_end(), bullet);
}

#[test]
fn REQ066_AC1_readme_0_22_mise_a_jour_incrementale() {
    let readme = repo_file("README.md");
    let row = "| 0.22 | Mise à jour incrémentale : `update` ne résout plus que les références touchées par les fichiers changés et ne relit plus toute la table des liens ; résumé global calculé sans relire le graphe ; une seule résolution complète par commande avec LLM. Même graphe qu'en 0.21 ; le schéma 8 est migré sur place. |";
    let bullet = "- **Vers 0.22** : le graphe passe au schéma 8 sur place : le premier `nephila build` ou `nephila update` le migre sans appel LLM et sans perdre les résumés, et la première résolution qui suit est complète ; les requêtes, `status`, `export` et `summary` lisent un graphe 0.18 à 0.21 tel quel. Un binaire 0.21 refuse ensuite ce graphe : pour revenir en arrière, supprimez `.nephila/` (le build suivant réenrichit alors tous les fichiers de la portée).";
    let lines: Vec<&str> = readme.lines().collect();
    assert_eq!(lines.iter().filter(|l| **l == row).count(), 1);
    let i22 = lines.iter().position(|l| *l == row).unwrap();
    let i21 = lines
        .iter()
        .position(|l| l.starts_with("| 0.21 |"))
        .expect("ligne 0.21 absente");
    assert_eq!(i22 + 1, i21);
    let sec = readme_section(&readme, "## Mise à niveau");
    let b = sec.find(bullet).expect("puce Vers 0.22 absente");
    let v21 = sec.find("- **Vers 0.21**").expect("puce Vers 0.21");
    assert!(b < v21);
    assert_eq!(sec[b..v21].trim_end(), bullet);
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
fn REQ071_AC11_readme_option_jobs() {
    let readme = repo_file("README.md");
    // Les textes de la 0.23 restent dans la puce et la ligne de ce numéro (la 0.24 les suit).
    let version = "0.23";
    let lines: Vec<&str> = readme.lines().collect();
    for cmd in ["build ", "update"] {
        let prefix = format!("nephila {cmd}");
        let n = lines
            .iter()
            .filter(|l| {
                l.starts_with(&prefix)
                    && l.contains(" [--enrich-code] [--jobs <N>] [--batch-files <N>] ")
            })
            .count();
        assert_eq!(n, 1, "ligne d'usage de {cmd}");
    }
    let jobs = "| `--jobs <N>` | Nombre de fils du parcours du dépôt et de l'extraction, de 1 à 256 ; défaut : nombre de cœurs disponibles (au plus 256 ; 4 si le système ne le donne pas). Pas de clé de configuration ; indépendant de `concurrency`, qui ne règle que les appels LLM. Quel que soit N, `build` et `update` gardent en mémoire, du parcours à l'extraction, jusqu'à 256 Mio de contenus lus ; au-delà, l'extraction relit les fichiers |";
    assert_eq!(lines.iter().filter(|l| **l == jobs).count(), 1);
    let ij = lines.iter().position(|l| *l == jobs).unwrap();
    assert!(lines[ij + 1].starts_with("| `--batch-files <N>` |"));
    // REQ136_AC2 : AC-136-2, le plafond écrit est celui du code.
    let before = jobs.split(" Mio").next().unwrap();
    let digits: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert_eq!(
        digits.parse::<usize>().unwrap(),
        nephila::scan::RETAIN_BUDGET / (1024 * 1024)
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("| `--jobs <N>` |"))
            .count(),
        1
    );
    let conc =
        "| `concurrency` | Appels LLM simultanés ; sans effet sur l'extraction, réglée par `--jobs` | 4 |";
    assert_eq!(lines.iter().filter(|l| **l == conc).count(), 1);
    assert!(readme.contains("; le nombre de fils annoncé au début de la phase structure est celui de `--jobs` (au plus le nombre de fichiers), celui de l'enrichissement vient de `concurrency`"));
    let sec = readme_section(&readme, "## Mise à niveau");
    let start = format!("- **Vers {version}** : le parcours du dépôt et l'extraction utilisent par défaut un fil par cœur disponible");
    let b = sec.find(&start).expect("puce Vers 0.23 absente");
    let v22 = sec.find("**Vers 0.22**").expect("puce Vers 0.22");
    assert!(b < v22);
    assert!(sec[b..v22].contains("ajoutez --jobs 1 à nephila build et nephila update."));
    let row = lines
        .iter()
        .find(|l| l.starts_with(&format!("| {version} |")))
        .expect("ligne de version");
    assert!(row.contains("Option --jobs de build et update : nombre de fils du parcours du dépôt et de l'extraction, par défaut le nombre de cœurs disponibles ; concurrency ne règle plus que les appels LLM."));
    assert!(row.contains("arcours du dépôt et empreintes en parallèle, fichier lu une seule fois pour son empreinte et son analyse, écriture SQLite allégée, même graphe qu'en 0.22 quel que soit le nombre de fils"));
    let i23 = lines.iter().position(|l| l == row).unwrap();
    assert!(lines[i23 + 1].starts_with("| 0.22 |"));
    assert!(!readme.contains("Appels simultanés |"));
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
fn REQ079_AC7_readme_sous_section_exclusions_par_defaut() {
    let readme = repo_file("README.md");
    assert!(readme.contains("Par défaut, `nephila` analyse tout ce qu'il prend en charge, hors des [exclusions par défaut](#exclusions-par-défaut) (sorties de build, dépendances copiées, réglages d'IDE, JavaScript minifié)."));
    let sec = readme_section(&readme, "### Exclusions par défaut");
    for needle in [
        "Sans réglage, `nephila` n'analyse pas ces chemins, à tout niveau du dépôt (`web/dist/` comme `dist/`) :",
        "| Exclu par défaut | Contenu habituel |",
        "| `dist/`, `out/` | sorties de build |",
        "| `coverage/` | rapports de couverture des tests |",
        "| `.next/`, `.nuxt/` | sorties de Next.js et de Nuxt |",
        "| `vendor/` | dépendances copiées dans le dépôt |",
        "| `.idea/` | réglages des IDE JetBrains |",
        "| `*.min.js` | JavaScript minifié |",
        "Le nom doit être exact, casse comprise : `distrib/`, `outils/` et `Dist/` sont analysés, comme `app.min.mjs`. Ces exclusions ne sont pas des filtres : elles ne comptent pas dans « fichiers exclus par les filtres », et `!motif` ne les annule pas.",
        "Si l'un de ces dossiers contient votre code (un package Java `out` ou `vendor`, par exemple), désactivez-les toutes dans `.nephila.toml` :",
        "```toml\nexclude_defaults = false\n```",
        "Ces chemins sont alors analysés comme les autres (types pris en charge, `.gitignore`, filtres). Pour en garder certains exclus, ajoutez-les à `exclude` : `exclude = [\"dist/\", \"*.min.js\"]`. Il n'y a pas d'option de ligne de commande : la clé est lue par toutes les commandes qui parcourent le dépôt, `status` compris. `target/`, `build/`, `.gradle/`, `node_modules/` et les fichiers de verrouillage restent toujours exclus.",
    ] {
        assert!(sec.contains(needle), "absent : {needle}");
    }
    let at = |needle: &str| readme.find(needle).unwrap();
    assert!(at("### Exclusions par défaut") < at("### Motifs"));
    assert!(at("## Filtrer l'analyse") < at("### Exclusions par défaut"));
    assert!(readme.contains("Les filtres (`exclude`, `exclude_types`), `exclude_defaults` (voir [Exclusions par défaut](#exclusions-par-défaut)) et les six réglages sont facultatifs"));
}

#[test]
fn REQ079_AC7_readme_puce_vers_024_et_ligne_du_tableau() {
    let readme = repo_file("README.md");
    assert_eq!(cargo_minor(), "0.24");
    let sec = readme_section(&readme, "## Mise à niveau");
    let bullet = "- **Vers 0.24** : les dossiers dist/, out/, coverage/, .next/, .nuxt/, vendor/ et .idea/, à tout niveau, et les fichiers *.min.js sont exclus par défaut. nephila status liste comme supprimé chaque fichier du graphe qu'ils contiennent, et le premier nephila update les retire, sans appel LLM. Pour les garder : exclude_defaults = false dans .nephila.toml.";
    let b = sec.find(bullet).expect("puce Vers 0.24 absente");
    let ren = sec.find("- **Renommage `rkg`").expect("puce du renommage");
    let v23 = sec.find("- **Vers 0.23**").expect("puce Vers 0.23");
    assert!(ren < b && b < v23);
    assert_eq!(sec[b..v23].trim_end(), bullet);
    let lines: Vec<&str> = readme.lines().collect();
    let i = lines
        .iter()
        .position(|l| l.starts_with("| 0.24 |"))
        .expect("ligne 0.24");
    assert!(lines[i + 1].starts_with("| 0.23 |"));
    let row = lines[i];
    assert!(row.starts_with("| 0.24 | Rupture : dist/, out/, coverage/, .next/, .nuxt/, vendor/, .idea/ et *.min.js ne sont plus analysés par défaut ; exclude_defaults = false dans .nephila.toml les rétablit."));
    assert!(row.contains("Export JSON écrit au fil de la lecture du graphe, sans recalculer les références non résolues ; même document qu'en 0.23 pour un même graphe."));
    for w in [
        "déduplications linéaires",
        "un parser par fil",
        "export en flux sans nouvelle résolution",
        "profil release optimisé",
        "banc de performance",
    ] {
        assert!(row.contains(w), "ligne 0.24 sans : {w}");
    }
    // REQ-102, REQ-103 : collision de noms après conversion, phrase en fin de cellule.
    assert!(
        row.contains(
            "Deux noms identiques après conversion : le fichier déjà indexé reste retenu, un fichier illisible est ignoré et laisse la place au suivant, que l'avertissement nomme par son nom réel. Les déclarations imbriquées"
        ),
        "ligne 0.24 : phrase de collision absente avant la phrase d'imbrication"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("| 0.24.1 |")),
        "aucune ligne 0.24.1"
    );
}

#[test]
fn req104_req105_readme_declarations_imbriquees_ignorees() {
    let readme = repo_file("README.md");
    assert!(readme.contains("déclarations imbriquées au-delà de 1000 niveaux ignorées"));
    let row = readme
        .lines()
        .find(|l| l.starts_with("| 0.24 |"))
        .expect("ligne 0.24");
    assert!(
        row.contains("Les déclarations imbriquées au-delà de 1000 niveaux sont ignorées avec un avertissement, le reste du fichier est indexé ; les avertissements de l'analyse sont écrits triés par chemin, quel que soit --jobs."),
        "{row}"
    );
    assert!(readme.contains(
        "avertissement: <fichier> : déclarations imbriquées au-delà de 1000 niveaux ignorées"
    ));
    assert!(!readme.lines().any(|l| l.starts_with("| 0.24.1 |")));
    for banned in ["profondeur", "récursion"] {
        for l in readme.lines().filter(|l| l.contains("imbriquées")) {
            assert!(!l.contains(banned), "{l}");
        }
    }
}

#[test]
fn REQ080_AC1_readme_paragraphe_de_l_export_en_flux() {
    let readme = repo_file("README.md");
    let json = "**Format JSON** : objet avec `version`, `nodes`, `edges` et `unresolved`.";
    let para = "Le document est écrit au fil de la lecture du graphe, sans le charger en mémoire. Si l'export échoue en cours de route (disque plein, sortie fermée par le lecteur), la commande s'arrête sur une ligne erreur: et un code non nul : le fichier commencé est supprimé, y compris un export précédent du même nom, et avec --output - stdout peut contenir un début de document. Seul le code de sortie 0 garantit un document complet.";
    let sec = readme_section(&readme, "## Export et vue HTML");
    let a = sec.find(json).expect("phrase du format JSON");
    let b = sec.find(para).expect("paragraphe de l'export en flux");
    assert!(a < b);
    assert_eq!(sec[a + json.len()..b].trim(), "");
}

#[test]
fn REQ082_AC4_readme_commande_du_banc_et_lignes_banc() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Développement");
    let cmd = "cargo nextest run --release --cargo-quiet --color never --test-threads 1 --run-ignored only --success-output final perf_banc";
    assert!(sec.lines().any(|l| l.starts_with(cmd)), "commande du banc");
    for needle in [
        "banc: <commande> --no-llm : parcours",
        "structure",
        "résolution",
        "médiane de 3",
        "banc: fixture",
    ] {
        assert!(sec.contains(needle), "lecture des lignes banc : {needle}");
    }
}

#[test]
fn REQ086_AC5_readme_sortie_standard_fermee() {
    let readme = repo_file("README.md");
    let codes = readme_section(&readme, "## Codes de sortie");
    let prev = "stdout ne contient que le résultat.";
    let para = "Si stdout est fermée avant la fin de la sortie (lecteur arrêté, | head), toute commande s'arrête sur une seule ligne erreur: écriture sur stdout impossible : <cause> et le code 1, sans rien écrire de plus. build et update ont déjà enregistré le graphe : seul leur bilan manque, et nephila status donne l'état. Une commande qui n'avait rien à écrire sur stdout garde son propre message.";
    let a = codes.find(prev).expect("phrase précédente");
    let b = codes.find(para).expect("paragraphe de la sortie fermée");
    assert_eq!(codes[a + prev.len()..b].trim(), "");

    let export = readme_section(&readme, "## Export et vue HTML");
    assert!(export.contains("Seul le code de sortie 0 garantit un document complet. Avec --output <fichier>, si seule la ligne du chemin ne peut être écrite sur stdout, le fichier, complet, est gardé, avec la même erreur et le code 1."));

    let row = readme
        .lines()
        .find(|l| l.starts_with("| 0.24 |"))
        .expect("ligne 0.24");
    assert!(row.starts_with("| 0.24 | Rupture : "));
    assert!(row.contains("banc de performance. Sortie standard fermée par le lecteur (head, par exemple) : toutes les commandes s'arrêtent sur une ligne erreur: écriture sur stdout impossible et le code 1, sans panique. Graphe à résoudre : nephila status l'annonce"));
    assert!(!readme.lines().any(|l| l.starts_with("| 0.24.1 |")));
}

#[test]
fn REQ088_REQ089_REQ090_readme_graphe_a_resoudre_erreur_et_trace() {
    let readme = repo_file("README.md");
    for expected in [
        "graphe à résoudre : lancez nephila update",
        "erreur: résolution : <cause> ; lancez nephila build pour reconstruire le graphe",
        " (incrémentale, <N> références réévaluées)",
        "un graphe d'une version antérieure à la 0.22 est annoncé « graphe à résoudre » jusqu'au premier `nephila update`",
    ] {
        assert!(readme.contains(expected), "absent : {expected}");
    }
    let status = readme_section(&readme, "### `nephila status`");
    assert!(status.contains("graphe à résoudre : lancez nephila update"));
    let codes = readme_section(&readme, "## Codes de sortie");
    assert!(codes.contains(
        "erreur: résolution : <cause> ; lancez nephila build pour reconstruire le graphe"
    ));
    let traces = readme_section(&readme, "### Traces (`--verbose`)");
    assert!(traces.contains(" (incrémentale, <N> références réévaluées)"));
    assert!(traces.contains("(complète)"));
    for banned in ["corrompu", "invalide", "périmé"] {
        for l in status.lines().chain(traces.lines()) {
            assert!(
                !l.contains(banned) || !l.contains("résol"),
                "mot interdit : {l}"
            );
        }
    }
    let row = readme
        .lines()
        .find(|l| l.starts_with("| 0.24 |"))
        .expect("ligne 0.24");
    assert!(row.contains(" Graphe à résoudre : nephila status l'annonce (graphe à résoudre : lancez nephila update) après une commande interrompue ou une passe de résolution en échec (erreur: résolution : <cause> ; lancez nephila build pour reconstruire le graphe) ; --verbose dit si la passe est complète ou incrémentale."), "ligne 0.24 : {row}");
    assert!(!readme.lines().any(|l| l.starts_with("| 0.24.1 |")));
}

#[test]
fn REQ091_REQ092_REQ093_REQ094_REQ095_REQ096_REQ097_REQ041_readme_causes_d_arret_et_essais_http() {
    let readme = repo_file("README.md");
    for expected in [
        "requête refusée par <fournisseur> (HTTP <code>) sur <url>",
        "corrigez base_url",
        "fournisseur saturé : relancez nephila update plus tard ou réduisez concurrency",
        "délais dépassés répétés : augmentez timeout_secs ou vérifiez le fournisseur",
        "llm enrich découpe",
        "après lecture interrompue",
        "requête redirigée",
        "politique: délais dépassés consécutifs",
    ] {
        assert!(readme.contains(expected), "absent : {expected}");
    }
    assert!(!readme.contains("tout le lot échoue"));
    for banned in [" rejetée", " coupée", " coupées"] {
        assert!(!readme.contains(banned), "mot interdit : {banned}");
    }
    for l in readme.lines() {
        assert!(
            !l.contains(" timeout ") && !l.contains(" un timeout"),
            "timeout en prose : {l}"
        );
    }
    let row = readme
        .lines()
        .find(|l| l.starts_with("| 0.24 |"))
        .expect("ligne 0.24");
    assert!(row.ends_with(" |"));
    let tail = row.rsplit("complète ou incrémentale.").next().unwrap();
    for expected in [
        "requête refusée",
        "redirection",
        "délais dépassés répétés",
        "redirections ne sont pas suivies",
    ] {
        assert!(tail.contains(expected), "ligne 0.24, absent : {expected}");
    }
    assert!(!readme.lines().any(|l| l.starts_with("| 0.24.1 |")));
    let http = readme_section(&readme, "### Nouveaux essais HTTP");
    for expected in [
        "requête refusée",
        "redirection",
        "saturé",
        "lecture interrompue",
        "un seul nouvel envoi",
    ] {
        assert!(http.contains(expected), "section HTTP, absent : {expected}");
    }
    let tronq = readme_section(&readme, "### Réponse tronquée par la limite de jetons");
    let que_faire = tronq
        .lines()
        .find(|l| l.starts_with("Que faire"))
        .expect("Que faire");
    assert!(!que_faire.contains("batch_files") && !que_faire.contains("--batch-files"));
    assert!(que_faire.contains("max_chunk_chars") && que_faire.contains("max_output_tokens"));
    assert!(row.contains("`erreur: requête refusée par <fournisseur> (HTTP <code>) sur <url>`"));
    assert!(row.contains(
        "`erreur: <fournisseur> redirige (HTTP <code>) vers <adresse> ; corrigez base_url`"
    ));
    assert!(!row.contains("trois délais dépassés répétés"));
    let lots = readme_section(&readme, "## Enrichissement en lots");
    assert!(lots.contains("llm enrich découpe"));
}

#[test]
fn REQ111_AC1_readme_documente_les_fils_refuses_par_le_systeme() {
    let readme = repo_file("README.md");
    for expected in [
        "parcours du dépôt impossible : ",
        "création des fils d'analyse impossible : ",
        "--jobs 1",
        "fils démarrés sur",
        "exclusions par défaut : ",
        "schéma: migration",
    ] {
        assert!(readme.contains(expected), "absent : {expected}");
    }
    let row = readme
        .lines()
        .find(|l| l.starts_with("| 0.24 |"))
        .expect("ligne 0.24");
    assert!(row.ends_with(" |"));
    let tail = row.rsplit("quel que soit --jobs.").next().unwrap();
    for expected in [
        "refusé par le système",
        "plafond de processus",
        "ligne `erreur:`",
        "--check-stale",
        "graphe pas à jour",
        "première de stderr",
    ] {
        assert!(tail.contains(expected), "ligne 0.24, absent : {expected}");
    }
    assert!(!readme.lines().any(|l| l.starts_with("| 0.24.1 |")));
    for banned in ["thread", "worker"] {
        for l in readme.lines().filter(|l| l.contains("impossible : ")) {
            assert!(!l.contains(banned), "mot interdit {banned} : {l}");
        }
        assert!(!tail.contains(banned), "mot interdit {banned} : {tail}");
    }
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

#[test]
fn REQ116_REQ117_REQ123_readme_ci_dependabot_audit_et_chaine_epinglee() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Développement");
    for needle in [
        ".github/dependabot.yml", // AC-116-2
        "git ls-remote --tags https://github.com/",
        "refs/heads/stable",
        "cargo deny check advisories", // AC-117-4
        "rust-toolchain.toml",         // AC-123-4
    ] {
        assert!(sec.contains(needle), "section Développement : {needle}");
    }
}

/// AC-124-4 : `cargo test` passe aussi, nextest reste la référence.
#[test]
fn REQ124_AC4_readme_cargo_test_passe_aussi() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Développement");
    assert!(
        sec.contains(
            "`cargo test` passe aussi (tests en parallèle, sans limite de temps par test) ; \
             `cargo nextest run` reste la commande de référence, celle de la CI."
        ),
        "phrase `cargo test` absente de la section Développement"
    );
    assert!(!readme.contains("seul nextest"), "« seul nextest » présent");
}

#[test]
fn REQ131_AC131_5_readme_traces_enrichissements_illisibles() {
    let readme = repo_file("README.md");
    let traces = readme_section(&readme, "### Traces (`--verbose`)");
    let expected = "`, <r> enrichissements conservés`), suivi, quand des enrichissements conservés ne se lisent plus (graphe écrit par une version plus récente, données abîmées), de leur nombre (`, <M> enrichissements illisibles écartés` ; ces fichiers sont enrichis de nouveau) ; le nombre de fils annoncé";
    assert!(
        traces.contains(expected),
        "phrase absente de la section Traces"
    );
}

const THINKING_BLOCK: &str = "# Demande au modèle de réfléchir avant de répondre. Booléen ; défaut : false. true : nephila
# envoie à « anthropic » \"thinking\": {\"type\": \"adaptive\"} et \"output_config\": {\"effort\": ...},
# et à « openai » \"reasoning_effort\" ; le niveau est celui de effort. false : aucun champ de
# réflexion n'est envoyé, mais un modèle qui réfléchit toujours (certains modèles Claude, les
# modèles de raisonnement d'OpenAI) le fait quand même, à son niveau par défaut ; pour limiter
# sa réflexion, mettez thinking à true et effort à \"low\". Un modèle qui ne prend pas en charge
# la réflexion refuse la requête (HTTP 400) : ses fichiers restent à enrichir. Chez « anthropic »,
# la réflexion adaptative demande un modèle Claude 4.6 ou suivant : claude-sonnet-4-5, le modèle
# de cet exemple, la refuse. La réflexion n'est jamais affichée ni enregistrée. Sans effet avec
# « mock ». Lue ici seulement :
# .nephila.toml la refuse.
# thinking = false";

const EFFORT_BLOCK: &str = "# Niveau de la réflexion demandée quand thinking vaut true. Texte : \"low\", \"medium\" ou \"high\",
# en minuscules ; défaut : \"low\". Envoyé à « anthropic » dans \"output_config\" et à « openai »
# dans \"reasoning_effort\". Sans effet quand thinking vaut false (rien n'est envoyé) et avec
# « mock ». Un niveau plus haut donne des réponses plus lentes et plus coûteuses, et sa
# réflexion compte dans max_output_tokens. À \"medium\" ou \"high\", augmentez timeout_secs
# (600, par exemple) : un appel plus long échoue en délai dépassé, et trois délais dépassés
# de suite arrêtent l'envoi. Lue ici seulement :
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
fn REQ135_AC2_readme_ligne_effort_de_la_table_des_cles() {
    let readme = repo_file("README.md");
    let wanted = "| `effort` | `\"low\"`, `\"medium\"` ou `\"high\"` : niveau de la réflexion. N'a d'effet qu'avec `thinking = true`. À `\"medium\"` ou `\"high\"`, les appels durent plus longtemps : augmentez `timeout_secs` (600, par exemple), car trois délais dépassés de suite arrêtent l'envoi ; configuration personnelle seulement | `\"low\"` |";
    let found: Vec<&str> = readme
        .lines()
        .filter(|l| l.starts_with("| `effort` |"))
        .collect();
    assert_eq!(found, vec![wanted]);
}

const CONFIG_BULLET: &str = "- la configuration (fournisseur, modèle, `enrich_code`, `thinking`, `effort`, `max_output_tokens`, `concurrency`, `timeout_secs`, `max_chunk_chars`, `batch_files`, `format_retries`, ou `llm inactif`), complétée de ` tls_ca_cert=<chemin résolu>` si cette clé est donnée et de ` tls_insecure` si elle vaut `true` ; quand un réglage vient de `.nephila.toml`, la ligne se termine par ` (.nephila.toml : <réglages>)` ;";

fn without_timestamp(line: &str) -> String {
    let start = line.find('[').expect("horodatage");
    let end = line[start..].find("s] ").expect("fin d'horodatage") + start + 3;
    format!("{}{}", &line[..start], &line[end..])
}

#[test]
fn REQ134_AC1_readme_traces_puce_de_la_configuration_et_ligne_d_exemple() {
    let readme = repo_file("README.md");
    assert_eq!(readme.lines().filter(|l| *l == CONFIG_BULLET).count(), 1);
    let examples: Vec<&str> = readme
        .lines()
        .filter(|l| l.starts_with("trace: ") && l.contains("config: llm="))
        .collect();
    assert_eq!(
        examples,
        vec!["trace: [0.000s] config: llm=mock model=m-projet enrich_code=false thinking=false effort=low max_output_tokens=16384 concurrency=4 timeout_secs=300 max_chunk_chars=60000 batch_files=1 format_retries=1 (.nephila.toml : model,format_retries)"]
    );
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
fn REQ132_AC1_readme_attente_nouveaux_essais() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "### Nouveaux essais HTTP");
    assert!(sec.contains("(15 s d'attente au plus par appel sans `retry-after` ; jusqu'à 60 s par nouvel essai quand le serveur le demande)"));
    assert!(!readme.contains("(15 s d'attente au plus par appel)"));
}

#[test]
fn REQ137_AC1_readme_tests_non_utf8_macos() {
    let readme = repo_file("README.md");
    let sec = readme_section(&readme, "## Développement");
    let para = "Sous macOS, les tests des noms de fichiers non UTF-8 (`tests/non_utf8_names.rs` et ceux de `src/scan.rs`) passent sans rien vérifier : APFS refuse de créer ces noms, et chaque test s'arrête aussitôt. Seule la CI Linux les exécute vraiment ; une modification du traitement de ces noms se vérifie sous Linux.";
    let lines: Vec<&str> = sec.lines().collect();
    let ip = lines
        .iter()
        .position(|l| *l == para)
        .expect("paragraphe absent");
    let nx = lines
        .iter()
        .position(|l| l.starts_with("Les tests s'exécutent avec [cargo-nextest]"))
        .unwrap();
    let fence_open = (nx..lines.len())
        .find(|&i| lines[i].starts_with("```"))
        .unwrap();
    let fence_close = (fence_open + 1..lines.len())
        .find(|&i| lines[i].starts_with("```"))
        .unwrap();
    let banc = lines
        .iter()
        .position(|l| l.starts_with("Le banc de performance mesure"))
        .unwrap();
    assert!(fence_close < ip && ip < banc);
}
