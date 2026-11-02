use assert_cmd::Command;

fn nephila() -> Command {
    Command::cargo_bin("nephila").unwrap()
}

#[test]
fn req001_ac1_help_lists_commands() {
    let out = nephila()
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(out.stdout).unwrap();
    for c in ["build", "update", "status", "query", "export", "summary"] {
        assert!(text.contains(c), "commande absente de l'aide : {c}");
    }
    assert!(text.contains("--repo"));
}

#[test]
fn req001_ac1_query_help_lists_subcommands() {
    let out = nephila()
        .args(["query", "--help"])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(out.stdout).unwrap();
    for c in ["search", "neighbors", "path", "community", "ask"] {
        assert!(text.contains(c), "sous-commande absente : {c}");
    }
}

#[test]
fn req001_ac3_unknown_command_fails_with_usage_on_stderr() {
    let out = nephila()
        .arg("frobnicate")
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains("Usage"));
}

#[test]
fn req001_ac3_invalid_option_fails_with_usage_on_stderr() {
    let out = nephila()
        .args(["status", "--bogus"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr).unwrap().contains("Usage"));
}

#[test]
fn req001_ac2_error_exits_nonzero_with_prefix_on_stderr_only() {
    let out = nephila()
        .args(["-C", "/nonexistent/nephila-repo"])
        .arg("build")
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .starts_with("erreur:"));
}

// ---- Option --check-stale (REQ-017, AC-017-7, AC-017-8) ----

const CHECK_STALE_HELP: &str = "--check-stale Avertit sur stderr si le graphe n'est pas à jour, d'après la taille et la date de modification des fichiers (parcourt le dépôt sans lire les fichiers déjà indexés ; défaut : aucune vérification ; vérification par le contenu : nephila status)";

fn normalized_help(args: &[&str]) -> String {
    let out = nephila().args(args).arg("--help").assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn req017_ac7_ac8_check_stale_help_of_query_subcommands() {
    for sub in ["search", "neighbors", "path", "community", "ask"] {
        let h = normalized_help(&["query", sub]);
        assert!(h.contains(CHECK_STALE_HELP), "{sub} : {h}");
        let from = h.find("--check-stale").unwrap();
        let entry_end = from + CHECK_STALE_HELP.len();
        assert!(!h[from..entry_end + 20.min(h.len() - entry_end)].contains("[default:"));
        assert!(h.find("--json").unwrap() < from, "{sub}");
        if sub == "ask" {
            assert!(from < h.find("--format-retries").unwrap());
        }
    }
}

#[test]
fn req017_ac7_check_stale_absent_from_other_commands() {
    for cmd in ["status", "build", "update", "export", "summary"] {
        assert!(!normalized_help(&[cmd]).contains("--check-stale"), "{cmd}");
    }
}

// ---- REQ-163 : `--context-chars` et `--verbose` de `query ask` ----

#[test]
fn req163_ac5_ac8_context_chars_usage_errors() {
    for v in ["999", "0", "-1", "1.5", "abc"] {
        let out = nephila()
            .args(["query", "ask", "x", "--context-chars", v])
            .assert()
            .failure()
            .code(2)
            .get_output()
            .clone();
        assert!(out.stdout.is_empty(), "{v}");
        let err = String::from_utf8(out.stderr).unwrap();
        let want = format!(
            "error: invalid value '{v}' for '--context-chars <N>': `{v}` n'est pas un entier d'au moins 1000"
        );
        assert!(err.contains(&want), "{v} : {err}");
    }
    let out = nephila()
        .args(["query", "search", "x", "--context-chars", "2000"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .clone();
    assert!(out.stdout.is_empty());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("unexpected argument '--context-chars'"),
        "{err}"
    );
}

#[test]
fn req163_ac6_ac10_help_of_query_ask() {
    let h = normalized_help(&["query", "ask"]);
    let verbose = "-v, --verbose Configuration effective de l'appel LLM sur stderr, en une ligne (préfixe `trace:`)";
    let chars = "--context-chars <N> Nombre maximal de caractères du contexte envoyé au LLM, au moins 1000 (remplace `max_context_chars` de la section [ask] ; défaut 12000)";
    assert!(h.contains(verbose), "{h}");
    assert!(h.contains(chars), "{h}");
    let pos = |s: &str| h.find(s).unwrap_or_else(|| panic!("{s} : {h}"));
    let order = [
        pos("--json"),
        pos("--check-stale"),
        pos("-v, --verbose"),
        pos("--format-retries <N>"),
        pos("--context-chars <N>"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?} : {h}");
    assert!(!h[pos("--json")..].contains("[default:"), "{h}");
    for bad in ["thinking", "effort", "max_output_tokens", "incompatible"] {
        assert!(!h.contains(bad), "{bad} : {h}");
    }
    for sub in ["search", "neighbors", "path", "community"] {
        assert!(
            !normalized_help(&["query", sub]).contains("--context-chars"),
            "{sub}"
        );
    }
}

// ---- REQ-184, REQ-185 : `--dry-run` de `build` et `update` ----

#[test]
fn req184_ac1_dry_run_with_no_llm_is_a_usage_error() {
    for cmd in ["build", "update"] {
        for args in [["--dry-run", "--no-llm"], ["--no-llm", "--dry-run"]] {
            let dir = tempfile::tempdir().unwrap();
            let out = nephila()
                .arg("-C")
                .arg(dir.path())
                .arg(cmd)
                .args(args)
                .assert()
                .failure()
                .code(2)
                .get_output()
                .clone();
            assert!(out.stdout.is_empty(), "{cmd}");
            let err = String::from_utf8(out.stderr).unwrap();
            assert!(
                err.contains("error: the argument '--dry-run' cannot be used with '--no-llm'")
                    || err
                        .contains("error: the argument '--no-llm' cannot be used with '--dry-run'"),
                "{cmd} : {err}"
            );
            assert!(!dir.path().join(".nephila").exists(), "{cmd}");
        }
    }
}

#[test]
fn req184_ac1_dry_run_alone_is_accepted() {
    for cmd in ["build", "update"] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# A\n").unwrap();
        nephila()
            .env("NEPHILA_CONFIG", dir.path().join("absent.toml"))
            .arg("-C")
            .arg(dir.path())
            .args([cmd, "--dry-run"])
            .assert()
            .success();
    }
}

#[test]
fn req185_ac1_dry_run_help_of_build_and_update() {
    let want = "--dry-run Estime les appels, octets et jetons de l'enrichissement, sans rien envoyer au LLM (graphe écrit comme avec --no-llm ; incompatible avec --no-llm)";
    for cmd in ["build", "update"] {
        let text = normalized_help(&[cmd]);
        assert!(text.contains(want), "{cmd} : {text}");
        let at = |s: &str| text.find(s).unwrap();
        assert!(at("--enrich-code") < at("--dry-run"), "{cmd}");
        assert!(at("--dry-run") < at("-v, --verbose"), "{cmd}");
    }
}

#[test]
fn req184_dry_run_sans_cle_ecrit_bilan_puis_estimation_sur_stdout() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "# A\n\ntexte\n").unwrap();
    let conf = dir.path().join("c.toml");
    std::fs::write(
        &conf,
        "provider = \"openai\"\nmodel = \"m\"\nbase_url = \"http://127.0.0.1:1/v1\"\napi_key_env = \"NEPHILA_CLE_ABSENTE_184\"\n",
    )
    .unwrap();
    let out = nephila()
        .env("NEPHILA_CONFIG", &conf)
        .env_remove("NEPHILA_CLE_ABSENTE_184")
        .arg("-C")
        .arg(dir.path())
        .args(["build", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].ends_with("enrichissement : estimé sans envoi"),
        "{text}"
    );
    assert_eq!(
        lines[1],
        "estimation : 1 fichiers à enrichir dont 0 envoyés seuls et 0 sans appel"
    );
    assert!(
        lines[2].starts_with("estimation minimale : 1 appels, "),
        "{text}"
    );
    assert!(lines[3].ends_with("(3 essais par appel)"), "{text}");
    assert_eq!(lines.len(), 4, "{text}");
}

// ---- REQ-186, REQ-187, REQ-188 : `max_calls` et `--max-calls` ----

const MAX_CALLS_HELP_TEXT: &str = "--max-calls <N> Nombre maximal d'appels d'enrichissement de cette commande, nouveaux essais de format compris ; les fichiers restants attendent le prochain nephila update (remplace `max_calls` de la configuration ; défaut : aucun plafond)";

struct CapEnv {
    tmp: tempfile::TempDir,
}

impl CapEnv {
    fn new(config_extra: Option<&str>) -> CapEnv {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo")).unwrap();
        std::fs::create_dir_all(tmp.path().join("mock")).unwrap();
        if let Some(extra) = config_extra {
            std::fs::write(
                tmp.path().join("config.toml"),
                format!("provider = \"mock\"\nmodel = \"mock\"\n{extra}"),
            )
            .unwrap();
        }
        CapEnv { tmp }
    }

    fn repo(&self) -> std::path::PathBuf {
        self.tmp.path().join("repo")
    }

    fn doc(&self, name: &str) {
        std::fs::write(self.repo().join(name), "# Titre\n\ntexte\n").unwrap();
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut c = nephila();
        c.env("NEPHILA_CONFIG", self.tmp.path().join("config.toml"))
            .env("NEPHILA_MOCK_DIR", self.tmp.path().join("mock"))
            .env_remove("NEPHILA_MOCK_LOG")
            .env_remove("NO_COLOR")
            .arg("-C")
            .arg(self.repo())
            .args(args);
        c.output().unwrap()
    }
}

fn err_of(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn req186_ac1_ac11_valeur_invalide_de_l_option() {
    for cmd in ["build", "update"] {
        for v in ["0", "-1", "x", "1.5", "99999999999999999999"] {
            let env = CapEnv::new(Some(""));
            env.doc("a.md");
            let o = env.run(&[cmd, &format!("--max-calls={v}")]);
            assert_eq!(o.status.code(), Some(2), "{cmd} {v}");
            assert!(o.stdout.is_empty(), "{cmd} {v}");
            let want = format!(
                "error: invalid value '{v}' for '--max-calls <N>': `{v}` n'est pas un entier strictement positif"
            );
            let err = err_of(&o);
            assert!(err.contains(&want), "{cmd} {v} : {err}");
            assert!(!err.contains("unexpected argument"), "{cmd} {v} : {err}");
            assert!(!env.repo().join(".nephila").exists(), "{cmd} {v}");
            // forme séparée par une espace
            let o = env.run(&[cmd, "--max-calls", v]);
            assert_eq!(o.status.code(), Some(2), "{cmd} {v}");
            assert!(err_of(&o).contains(&want), "{cmd} {v}");
        }
    }
}

#[test]
fn req186_ac12_configuration_invalide() {
    for shown in ["0", "-3", "\"5\"", "1.5", "true"] {
        let env = CapEnv::new(Some(&format!("max_calls = {shown}\n")));
        env.doc("a.md");
        let o = env.run(&["build"]);
        assert_eq!(o.status.code(), Some(1), "{shown}");
        let cfg = env.tmp.path().join("config.toml");
        assert_eq!(
            err_of(&o),
            format!(
                "erreur: configuration invalide ({}) : clé « max_calls » doit être un entier strictement positif (reçu : {shown})\n",
                cfg.display()
            ),
            "{shown}"
        );
        assert!(!env.repo().join(".nephila").exists(), "{shown}");
        // sans LLM, la configuration n'est pas lue
        let o = env.run(&["build", "--no-llm"]);
        assert_eq!(o.status.code(), Some(0), "{shown}");
        assert!(!err_of(&o).contains("erreur:"), "{shown}");
    }
}

#[test]
fn req186_ac1_max_calls_refuse_dans_le_fichier_de_projet() {
    let env = CapEnv::new(Some(""));
    env.doc("a.md");
    std::fs::write(env.repo().join(".nephila.toml"), "max_calls = 5\n").unwrap();
    let o = env.run(&["build"]);
    assert_eq!(o.status.code(), Some(1));
    let err = err_of(&o);
    assert!(err.contains("clé inconnue `max_calls`"), "{err}");
    assert!(!env.repo().join(".nephila").exists());
}

#[test]
fn req188_ac1_ac4_aide_de_max_calls() {
    for cmd in ["build", "update"] {
        let h = normalized_help(&[cmd]);
        assert!(h.contains(MAX_CALLS_HELP_TEXT), "{cmd} : {h}");
        let at = h.find("--max-calls <N>").unwrap();
        assert!(h.find("--format-retries <N>").unwrap() < at, "{cmd}");
        assert!(at < h.find("--exclude <MOTIF>").unwrap(), "{cmd}");
        let entry_end = at + MAX_CALLS_HELP_TEXT.len();
        assert!(
            !h[at..entry_end + 20.min(h.len() - entry_end)].contains("[default:"),
            "{cmd}"
        );
        assert!(
            !h[h.find("Filtres :").unwrap()..].contains("max_calls"),
            "{cmd}"
        );
    }
    for args in [
        vec!["status"],
        vec!["query", "ask"],
        vec!["export"],
        vec!["summary"],
    ] {
        assert!(!normalized_help(&args).contains("--max-calls"), "{args:?}");
    }
}

#[test]
fn req187_ac4_ligne_config_et_priorite_de_l_option() {
    let config_line = |o: &std::process::Output| -> String {
        err_of(o)
            .lines()
            .find(|l| l.contains("config: llm="))
            .unwrap_or_else(|| panic!("{}", err_of(o)))
            .to_string()
    };
    let env = CapEnv::new(Some("max_calls = 3\n"));
    env.doc("a.md");
    let o = env.run(&["build", "--verbose"]);
    assert_eq!(o.status.code(), Some(0), "{}", err_of(&o));
    assert!(
        config_line(&o).ends_with(" format_retries=2 max_calls=3"),
        "{}",
        config_line(&o)
    );
    let o = env.run(&["update", "--verbose", "--max-calls", "7"]);
    assert!(
        config_line(&o).ends_with(" format_retries=2 max_calls=7"),
        "{}",
        config_line(&o)
    );

    let env = CapEnv::new(Some(""));
    env.doc("a.md");
    let o = env.run(&["build", "--verbose"]);
    assert!(
        config_line(&o).ends_with(" format_retries=2"),
        "{}",
        config_line(&o)
    );
    assert!(!config_line(&o).contains("max_calls"));
    let o = env.run(&["update", "--verbose", "--max-calls", "7"]);
    assert!(
        config_line(&o).ends_with(" max_calls=7"),
        "{}",
        config_line(&o)
    );

    // sans LLM, l'option est sans effet
    let o = env.run(&["build", "--no-llm", "--verbose", "--max-calls", "3"]);
    let err = err_of(&o);
    assert!(err.contains("config: llm inactif (--no-llm)"), "{err}");
    assert!(!err.contains("max_calls"), "{err}");
}

#[test]
fn req187_ac5_rien_a_enrichir_ou_sans_llm_sans_avertissement() {
    let env = CapEnv::new(Some(""));
    env.doc("a.md");
    assert_eq!(env.run(&["build"]).status.code(), Some(0));
    let o = env.run(&["update", "--max-calls", "1"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(!err_of(&o).contains("avertissement"), "{}", err_of(&o));
    assert!(!err_of(&o).contains("plafond"), "{}", err_of(&o));

    let env = CapEnv::new(Some(""));
    env.doc("a.md");
    let plain = env.run(&["build", "--no-llm"]);
    let env2 = CapEnv::new(Some(""));
    env2.doc("a.md");
    let capped = env2.run(&["build", "--no-llm", "--max-calls", "1"]);
    assert_eq!(plain.status.code(), capped.status.code());
    assert_eq!(plain.stdout, capped.stdout);
    assert_eq!(plain.stderr, capped.stderr);
    assert!(!err_of(&capped).contains("avertissement"));
}

// ---- REQ-197 : `--accept-project-model` ----

const ACCEPT_PROJECT_MODEL_HELP_TEXT: &str = "--accept-project-model Accepte le modèle de .nephila.toml pour cette commande, même s'il fait réenrichir plus de 50 fichiers déjà enrichis (voir `model_override` de la configuration ; défaut : refus au-delà de 50)";

#[test]
fn req197_ac1_ac4_aide_de_accept_project_model() {
    for cmd in ["build", "update"] {
        let h = normalized_help(&[cmd]);
        assert!(h.contains(ACCEPT_PROJECT_MODEL_HELP_TEXT), "{cmd} : {h}");
        let at = h.find("--accept-project-model").unwrap();
        assert!(h.find("--max-calls <N>").unwrap() < at, "{cmd}");
        assert!(at < h.find("--exclude <MOTIF>").unwrap(), "{cmd}");
        let end = at + ACCEPT_PROJECT_MODEL_HELP_TEXT.len();
        assert!(
            !h[at..end + 20.min(h.len() - end)].contains("[default:"),
            "{cmd}"
        );
    }
    for args in [
        vec!["status"],
        vec!["query", "ask"],
        vec!["export"],
        vec!["summary"],
    ] {
        assert!(
            !normalized_help(&args).contains("--accept-project-model"),
            "{args:?}"
        );
    }
}

#[test]
fn req197_ac4_option_refusee_hors_build_et_update() {
    for args in [
        vec!["status", "--accept-project-model"],
        vec!["query", "ask", "--accept-project-model", "x"],
        vec!["export", "--accept-project-model"],
        vec!["summary", "--accept-project-model"],
    ] {
        let o = nephila().args(&args).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(
            err.contains("error: unexpected argument '--accept-project-model' found"),
            "{args:?} : {err}"
        );
    }
}

// ---- REQ-218 : `--batch` ----

const BATCH_HELP_TEXT: &str = "--batch Confie l'enrichissement au mode lot du fournisseur : moins cher, mais jusqu'à 24 h d'attente ; nephila update --batch reprend après Ctrl-C (sans rapport avec --batch-files, qui règle les fichiers par appel ; incompatible avec --no-llm)";

#[test]
fn req218_ac2_aide_de_batch() {
    for cmd in ["build", "update"] {
        let h = normalized_help(&[cmd]);
        assert!(h.contains(BATCH_HELP_TEXT), "{cmd} : {h}");
        let at = h.find(BATCH_HELP_TEXT).unwrap();
        let after_accept = h.find("--accept-project-model").unwrap();
        assert!(after_accept < at, "{cmd}");
        assert!(at < h.find("--exclude <MOTIF>").unwrap(), "{cmd}");
        // Aucune autre option entre `--accept-project-model` et `--batch`.
        let between = &h[after_accept..at];
        assert_eq!(between.matches(" --").count(), 0, "{cmd} : {between}");
        let end = at + BATCH_HELP_TEXT.len();
        assert!(
            !h[at..end + 20.min(h.len() - end)].contains("[default:"),
            "{cmd}"
        );
        assert!(h.contains("--batch-files <N> Nombre maximal de fichiers par appel d'enrichissement (remplace `batch_files` de la configuration)"), "{cmd}");
    }
}
