use assert_cmd::Command;

fn rkg() -> Command {
    Command::cargo_bin("rkg").unwrap()
}

#[test]
fn req001_ac1_help_lists_commands() {
    let out = rkg().arg("--help").assert().success().get_output().clone();
    let text = String::from_utf8(out.stdout).unwrap();
    for c in ["build", "update", "status", "query", "export", "summary"] {
        assert!(text.contains(c), "commande absente de l'aide : {c}");
    }
    assert!(text.contains("--repo"));
}

#[test]
fn req001_ac1_query_help_lists_subcommands() {
    let out = rkg()
        .args(["query", "--help"])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(out.stdout).unwrap();
    for c in ["search", "neighbors", "path", "ask"] {
        assert!(text.contains(c), "sous-commande absente : {c}");
    }
}

#[test]
fn req001_ac3_unknown_command_fails_with_usage_on_stderr() {
    let out = rkg()
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
    let out = rkg()
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
    let out = rkg()
        .env("RKG_CONFIG", "/nonexistent/rkg-config.toml")
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

#[test]
fn req001_ac2_all_subcommands_parse_and_report_not_implemented() {
    let cases: Vec<Vec<&str>> = vec![
        vec!["update"],
        vec!["-C", ".", "update"],
        vec!["query", "search", "foo", "--limit", "5", "--json"],
        vec!["query", "neighbors", "foo", "--depth", "2"],
        vec!["query", "path", "a", "b"],
        vec!["query", "ask", "pourquoi ?"],
    ];
    for args in cases {
        let out = rkg().args(&args).assert().failure().get_output().clone();
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(
            err.starts_with("erreur:") && err.contains("non implémenté"),
            "{args:?}: {err}"
        );
        assert!(out.stdout.is_empty());
    }
}
