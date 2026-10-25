use std::process::{Command, Output};

use serde_json::Value;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_herald"))
        .args(args)
        .env("HERALD_LOG", "error")
        .output()
        .expect("run herald")
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "invalid JSON ({}): {err}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn demo_flag_autosyncs_and_resets_between_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("herald.toml");
    // Only the flag and an opt-out of semantic search: no accounts needed.
    std::fs::write(&config, "demo = true\n\n[semantic]\nenabled = false\n").expect("write config");
    let config = config.to_str().expect("utf-8 config path");

    let listed = json(&run(&[
        "--config", config, "--json", "list", "--limit", "100",
    ]));
    assert_eq!(listed["total"], 25);
    assert_eq!(listed["messages"].as_array().expect("messages").len(), 25);
    assert!(listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|message| message["is_read"] == false));

    let shown = json(&run(&["--config", config, "--json", "show", "demo-12"]));
    assert_eq!(shown["is_read"], true);
    assert!(shown["body_text"]
        .as_str()
        .unwrap_or_default()
        .contains("测试覆盖率"));
    assert_eq!(
        shown["attachments"].as_array().expect("attachments").len(),
        3
    );

    // Field-scoped search: demo-01 and demo-04 both list lisi in To.
    let to = json(&run(&[
        "--config",
        config,
        "--json",
        "search",
        "lisi@example.com",
        "--field",
        "to",
        "--mode",
        "keyword",
    ]));
    assert_eq!(to["total"], 2);

    // The read flag set by `show` died with that process: demo data resets.
    let unread = json(&run(&[
        "--config", config, "--json", "list", "--unread", "--limit", "100",
    ]));
    assert_eq!(unread["total"], 25);
}
