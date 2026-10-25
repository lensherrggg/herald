use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_herald"))
        .args(args)
        .output()
        .expect("run herald")
}

#[test]
fn help_exposes_tui_subcommand() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("tui"), "{text}");
    for command in [
        "init",
        "accounts",
        "sync",
        "list",
        "search",
        "show",
        "status",
        "export-attachment",
        "models",
    ] {
        assert!(text.contains(command), "missing {command}:\n{text}");
    }

    let tui = run(&["tui", "--help"]);
    assert!(tui.status.success());
    let text = String::from_utf8_lossy(&tui.stdout);
    assert!(text.contains("--account"), "{text}");
    assert!(text.contains("终端界面"), "{text}");

    let search = run(&["search", "--help"]);
    assert!(search.status.success());
    let text = String::from_utf8_lossy(&search.stdout);
    for expected in [
        "--mode",
        "auto",
        "keyword",
        "semantic",
        "hybrid",
        "--semantic",
    ] {
        assert!(text.contains(expected), "missing {expected}:\n{text}");
    }
}
