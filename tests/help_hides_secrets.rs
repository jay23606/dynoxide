//! `--help` names the environment variables a flag reads from, but never
//! prints the value of one that holds a secret.

#![cfg(all(feature = "http-server", feature = "mcp-server", feature = "import"))]

use std::process::Command;

fn dynoxide_bin() -> &'static str {
    env!("CARGO_BIN_EXE_dynoxide")
}

const SENTINEL: &str = "help-must-not-show-this-value";

/// Run `dynoxide <args>` with every secret-bearing variable set to the
/// sentinel, and return stdout and stderr together.
fn help_output(args: &[&str]) -> String {
    let output = Command::new(dynoxide_bin())
        .args(args)
        .env("DYNOXIDE_MCP_AUTH_TOKEN", SENTINEL)
        .output()
        .expect("failed to run dynoxide");
    assert!(
        output.status.success(),
        "`dynoxide {}` should exit 0",
        args.join(" ")
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Every way of asking for a subcommand's help: `--help`, `-h` and
/// `help <sub>`.
fn assert_token_hidden(subcommand: &str) {
    for args in [
        vec![subcommand, "--help"],
        vec![subcommand, "-h"],
        vec!["help", subcommand],
    ] {
        let help = help_output(&args);
        let command = format!("dynoxide {}", args.join(" "));
        // The variable is still documented, so the flag's help text is there
        // to leak from.
        assert!(
            help.contains("DYNOXIDE_MCP_AUTH_TOKEN"),
            "`{command}` should still name DYNOXIDE_MCP_AUTH_TOKEN; got:\n{help}"
        );
        assert!(
            !help.contains(SENTINEL),
            "`{command}` printed the value of DYNOXIDE_MCP_AUTH_TOKEN"
        );
    }
}

// The top-level help lists the subcommands rather than the MCP flags, so it
// has nothing to leak from; this pins that it stays that way.
#[test]
fn root_help_does_not_show_the_mcp_token_value() {
    for args in [vec!["--help"], vec!["-h"], vec!["help"]] {
        let help = help_output(&args);
        assert!(
            !help.contains(SENTINEL),
            "`dynoxide {}` printed the value of DYNOXIDE_MCP_AUTH_TOKEN",
            args.join(" ")
        );
    }
}

#[test]
fn serve_help_hides_the_mcp_token_value() {
    assert_token_hidden("serve");
}

#[test]
fn mcp_help_hides_the_mcp_token_value() {
    assert_token_hidden("mcp");
}

#[test]
fn import_help_hides_the_mcp_token_value() {
    assert_token_hidden("import");
}
