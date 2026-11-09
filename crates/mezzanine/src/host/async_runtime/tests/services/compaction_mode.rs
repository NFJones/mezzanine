//! Command -> ordered adapter writer -> fresh runtime acceptance. No user
//! configuration, credentials, provider network or fabricated durability ACKs.

use super::super::*;

fn fixture(format: ConfigFormat) -> (RuntimeSessionService, ClientId, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "mez-mode-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let (extension, text) = match format {
        ConfigFormat::Toml => (
            "toml",
            "# authored comment\n[history]\nlines=4321\n[agents]\ncompaction_mode=\"summary\"\n",
        ),
        ConfigFormat::Json => (
            "json",
            r#"{"history":{"lines":4321},"agents":{"compaction_mode":"summary"}}"#,
        ),
        ConfigFormat::Yaml => (
            "yaml",
            "history:\n  lines: 4321\nagents:\n  compaction_mode: summary\n",
        ),
    };
    let path = root.join(format!("config.{extension}"));
    std::fs::write(&path, text).unwrap();
    let mut service = test_service_with_event_log();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".into(),
            path: Some(path.clone()),
            scope: ConfigScope::Primary,
            format,
            trusted: true,
            text: text.into(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("mode", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.use_config_effect_adapter();
    (service, primary, path)
}

fn reopened(path: &Path) -> RuntimeSessionService {
    let mut fresh = test_service_with_event_log();
    fresh
        .replace_config_layers(vec![ConfigLayer {
            name: "fresh-primary".into(),
            path: Some(path.into()),
            format: ConfigFormat::from_path(path).unwrap(),
            scope: ConfigScope::Primary,
            trusted: true,
            text: std::fs::read_to_string(path).unwrap(),
        }])
        .unwrap();
    fresh
}

async fn write(
    handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
) -> crate::host::async_runtime::side_effects::AsyncPersistenceSideEffectServiceReport {
    run_async_persistence_side_effect_service(
        handle,
        AsyncRuntimeSideEffectServiceConfig {
            max_polls: 1,
            drain_limit: 128,
            idle_interval: Duration::from_millis(1),
        },
        |_, _| false,
    )
    .await
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn async_compaction_mode_global_writer_and_restart_all_formats() {
    for format in [ConfigFormat::Toml, ConfigFormat::Json, ConfigFormat::Yaml] {
        let (service, primary, path) = fixture(format);
        let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
            .build()
            .unwrap();
        let client = async {
            for (command, expected) in [
                ("raw_tail", "raw_tail"),
                ("summary", "summary"),
                ("raw_tail", "raw_tail"),
                ("reset", "summary"),
            ] {
                let old = std::fs::read(&path).unwrap();
                let reply = handle
                    .execute_agent_shell_command(
                        primary.clone(),
                        format!("/compaction-mode {command} --global"),
                    )
                    .await
                    .unwrap();
                assert!(reply.contains("changed=true"), "{reply}");
                assert!(reply.contains("Persistence | pending"), "{reply}");
                assert!(!reply.contains("Persistence | durable"), "{reply}");
                assert_eq!(std::fs::read(&path).unwrap(), old);
                let report = write(&handle).await;
                assert_eq!(report.failed, 0);
                assert!(report.completed >= 1);
                let status = handle
                    .execute_agent_shell_command(primary.clone(), "/compaction-mode status".into())
                    .await
                    .unwrap();
                assert!(status.contains("Persistence | durable"), "{status}");
                let fresh = reopened(&path);
                assert_eq!(fresh.agent_compaction_mode().name(), expected);
                assert!(fresh.agent_compaction_mode_override("%1").is_none());
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(text.contains("4321"), "unrelated setting lost: {text}");
                if format == ConfigFormat::Toml {
                    assert!(text.contains("# authored comment"));
                }
            }
            handle.shutdown().await.unwrap();
        };
        let ((), _) = tokio::join!(client, actor.run());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn async_compaction_mode_ordered_revisions_ignore_duplicate_old_receipts() {
    let (mut service, primary, path) = fixture(ConfigFormat::Toml);
    for command in ["raw_tail", "summary"] {
        let reply = service
            .execute_agent_shell_command(&primary, &format!("/compaction-mode {command} --global"))
            .unwrap();
        assert!(reply.contains("Persistence | pending"), "{reply}");
    }
    let effects = service
        .drain_config_persistence_transition()
        .side_effects
        .into_iter()
        .filter(|effect| matches!(effect, RuntimeSideEffect::PersistConfig { .. }))
        .collect::<Vec<_>>();
    assert_eq!(effects.len(), 2);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(vec![effects[0].clone()])
            .await
            .unwrap();
        assert_eq!(write(&handle).await.failed, 0);
        assert_eq!(reopened(&path).agent_compaction_mode().name(), "raw_tail");
        let status = handle
            .execute_agent_shell_command(primary.clone(), "/compaction-mode".into())
            .await
            .unwrap();
        assert!(
            status.contains("Persistence | pending"),
            "old write confirmed newer revision: {status}"
        );
        handle
            .queue_runtime_side_effects(vec![effects[1].clone()])
            .await
            .unwrap();
        assert_eq!(write(&handle).await.failed, 0);
        assert_eq!(reopened(&path).agent_compaction_mode().name(), "summary");
        // Actual old work, not a pathname ACK: one-shot receipt prevents replay.
        handle
            .queue_runtime_side_effects(vec![effects[0].clone(), effects[1].clone()])
            .await
            .unwrap();
        assert_eq!(write(&handle).await.failed, 0);
        assert_eq!(reopened(&path).agent_compaction_mode().name(), "summary");
        let status = handle
            .execute_agent_shell_command(primary.clone(), "/compaction-mode".into())
            .await
            .unwrap();
        assert!(status.contains("Persistence | durable"), "{status}");
        handle.shutdown().await.unwrap();
    };
    let ((), _) = tokio::join!(client, actor.run());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn async_compaction_mode_failed_writer_stays_local_and_new_selection_recovers() {
    let (service, primary, path) = fixture(ConfigFormat::Toml);
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .execute_agent_shell_command(
                primary.clone(),
                "/compaction-mode raw_tail --global".into(),
            )
            .await
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap(); // deterministic refusal, even as root
        assert_eq!(write(&handle).await.failed, 1);
        let failed = handle
            .execute_agent_shell_command(primary.clone(), "/compaction-mode".into())
            .await
            .unwrap();
        assert!(failed.contains("Effective mode | `raw_tail`"), "{failed}");
        assert!(failed.contains("Persistence | failed"), "{failed}");
        assert!(failed.contains("disk unconfirmed"), "{failed}");
        // Identical mutation is a no-op and must not fabricate durability/retry.
        let noop = handle
            .execute_agent_shell_command(
                primary.clone(),
                "/compaction-mode raw_tail --global".into(),
            )
            .await
            .unwrap();
        assert!(noop.contains("changed=false"), "{noop}");
        assert!(noop.contains("Persistence | failed"), "{noop}");
        assert!(
            handle
                .execute_agent_shell_command(primary.clone(), "/status".into())
                .await
                .unwrap()
                .contains("Compaction mode")
        );
        std::fs::remove_dir(&path).unwrap();
        let repaired = handle
            .execute_agent_shell_command(
                primary.clone(),
                "/compaction-mode summary --global".into(),
            )
            .await
            .unwrap();
        assert!(repaired.contains("Persistence | pending"), "{repaired}");
        assert_eq!(write(&handle).await.failed, 0);
        assert_eq!(reopened(&path).agent_compaction_mode().name(), "summary");
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.agent_compaction_mode().name(), "summary");
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
