use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_keeppeek"))
        .args(args)
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn help_version_and_configuration_keep_plain_output_with_a_dashboard_request() {
    for args in [
        &["--tui", "always", "--help"][..],
        &["--tui", "always", "--version"],
        &["--tui", "always", "config", "--help"],
    ] {
        let output = run(args);
        assert!(output.status.success(), "{args:?}");
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.contains(&0x1b));
    }
    let help = String::from_utf8(run(&["--help"]).stdout).unwrap();
    assert!(help.contains("--tui") && help.contains("auto") && help.contains("never"));
}

#[test]
fn the_foreground_cli_rejects_an_unknown_dashboard_mode() {
    let output = run(&["--tui", "invalid-dashboard-setting"]);
    assert_eq!(output.status.code(), Some(2));
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("--tui") && error.contains("always"));
}
