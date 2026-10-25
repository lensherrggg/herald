use std::path::Path;

use herald_core::config::Config;
use herald_core::demo::SAMPLE_MESSAGES;
use herald_core::model::MessageFilter;
use herald_core::paths::PathLayout;
use herald_core::Herald;

fn layout(dir: &Path) -> PathLayout {
    PathLayout {
        config_path: dir.join("herald.toml"),
        config_dir: dir.to_path_buf(),
        data_dir: dir.to_path_buf(),
        portable: true,
        fallback_reason: None,
    }
}

#[test]
fn demo_respects_configured_export_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exports = dir.path().join("my-exports");
    std::fs::create_dir_all(&exports).unwrap();
    let config = Config::from_toml(&format!(
        "demo = true\n\n[attachments]\nexport_dir = \"{}\"\n",
        exports.to_string_lossy().replace('\\', "/")
    ))
    .expect("demo config");
    let herald = Herald::open_with_config(config, layout(dir.path())).expect("open demo herald");
    assert_eq!(herald.export_base_dir(), exports);
}

#[test]
fn demo_flag_syncs_mock_mail_and_resets_on_exit() {
    // Only the flag is required: no accounts, no database path.
    let config = Config::from_toml("demo = true\n").expect("demo config");
    assert!(config.accounts.is_empty());
    let dir = tempfile::tempdir().expect("tempdir");

    let (database, data_dir) = {
        let herald =
            Herald::open_with_config(config.clone(), layout(dir.path())).expect("open demo herald");
        assert!(herald.demo_active());
        assert_eq!(herald.config.accounts.len(), 1);
        assert_eq!(herald.config.accounts[0].name, "demo");

        let (total, page) = herald
            .list_messages(
                Some("demo"),
                &MessageFilter {
                    limit: 100,
                    ..Default::default()
                },
            )
            .expect("list demo mail");
        assert_eq!(total, SAMPLE_MESSAGES);
        assert_eq!(page.len(), SAMPLE_MESSAGES);
        assert_eq!(
            herald.unread_count(Some("demo")).expect("unread count"),
            SAMPLE_MESSAGES
        );

        // Bodies and attachments arrive with the automatic first sync.
        let detail = herald
            .get_stored_message(Some("demo"), "demo-12")
            .expect("stored detail");
        assert!(detail
            .body_text
            .as_deref()
            .unwrap_or_default()
            .contains("测试覆盖率"));
        assert_eq!(detail.attachments.len(), 3);

        herald
            .set_read(Some("demo"), "demo-01", true)
            .expect("mark read");
        assert_eq!(
            herald.unread_count(Some("demo")).expect("unread count"),
            SAMPLE_MESSAGES - 1
        );

        let database = herald.paths.database_path.clone();
        assert!(database.exists());
        let data_dir = database.parent().expect("demo data dir").to_path_buf();
        (database, data_dir)
    };

    assert!(
        !data_dir.exists(),
        "demo data directory must be removed when the process ends: {}",
        data_dir.display()
    );
    assert!(!database.exists());

    // The next run starts from a clean mailbox and fresh database.
    let herald = Herald::open_with_config(config, layout(dir.path())).expect("reopen demo herald");
    assert_eq!(
        herald.unread_count(Some("demo")).expect("unread count"),
        SAMPLE_MESSAGES
    );
    assert_ne!(
        herald.paths.database_path.parent().expect("demo data dir"),
        data_dir
    );
}
