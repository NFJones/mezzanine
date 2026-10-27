//! Real interactive Fish payload ownership and fail-closed frame regressions.
//!
//! These tests retain the production PTY and terminal responder. Child output
//! is checked on disk so echoed source cannot masquerade as execution evidence.

use super::*;
use mez_agent::shell::{
    ShellChildArgument, ShellChildLaunch, ShellLaunchArtifact, ShellLaunchArtifactId,
};

#[test]
/// Verifies paced and unpaced structured transactions keep Fish's interactive
/// editor away from command, sidecar and artifact records. A digest corruption
/// must drain through the sentinel without launching the child, while an
/// already queued parent command must survive both successful and failed input.
/// Files written by actual processes distinguish execution from terminal echo.
fn structured_payload_preserves_parent_input_and_rejects_corrupt_frames() {
    let Some(fish) = fish_path_for_tests() else {
        eprintln!("skipping Fish payload ownership test because fish is unavailable");
        return;
    };
    for acknowledged in [false, true] {
        for corrupt in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "mez-fish-payload-{}-{acknowledged}-{corrupt}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let child_result = root.join("child-result");
            let parent_result = root.join("parent-result");
            let compatibility = ManagedFishCompatibility::new(
                MarkerToken::new("33333333333333333333333333333333").unwrap(),
            );
            let mut process = spawn_managed_fish(&fish, &compatibility, &root);
            settle_managed_fish_startup(&mut process);
            let artifact_id = ShellLaunchArtifactId::new("fixture").unwrap();
            let launch = ShellChildLaunch::new_with_artifacts(
                "/bin/sh",
                vec![
                    ShellChildArgument::Literal("-c".into()),
                    ShellChildArgument::Literal(
                        "printf 'child:' > \"$1\"; cat \"$2\" >> \"$1\"".into(),
                    ),
                    ShellChildArgument::Literal("sh".into()),
                    ShellChildArgument::Literal(child_result.to_str().unwrap().into()),
                    ShellChildArgument::MaterializedArtifact(artifact_id.clone()),
                ],
                vec![
                    ShellLaunchArtifact::new(artifact_id, b"inert $(false)".to_vec(), 0o400)
                        .unwrap(),
                ],
            )
            .unwrap();
            let marker = MarkerToken::new("44444444444444444444444444444444").unwrap();
            let input = ShellTransaction::new(
                marker.clone(),
                "payload-turn",
                "payload-agent",
                "p1",
                &fish,
                "true",
            )
            .unwrap()
            .with_input_sidecar(Some("validated sidecar\n".into()))
            .with_child_launch(launch)
            .with_payload_receiver_acknowledgements(acknowledged)
            .render_for_classification_input(ShellClassification::Fish);
            let mut output = process.write_shell_delivery(
                &ShellInputDelivery::generated_source_for_transaction(
                    input.wrapper.as_bytes().to_vec(),
                    marker.as_str(),
                ),
            );
            let start = format!("\x1b]133;C;mez_marker={};", marker.as_str());
            extend_fish_output_until(&mut process, &mut output, |bytes| {
                bytes
                    .windows(start.len())
                    .any(|window| window == start.as_bytes())
            });
            let payload = if corrupt {
                assert!(input.payload.contains("S1D "));
                input.payload.replacen("S1D ", "S1D X", 1)
            } else {
                input.payload.clone()
            };
            if acknowledged {
                output.extend(process.write_shell_delivery(
                    &ShellInputDelivery::receiver_acknowledged(
                        payload.into_bytes(),
                        marker.as_str(),
                        true,
                    ),
                ));
            } else {
                // Exercise the unpaced renderer directly, including on Darwin
                // where the runtime normally negotiates acknowledged delivery.
                process.write_input(payload.as_bytes()).unwrap();
            }
            // Queue ordinary parent input immediately after the authenticated
            // sentinel; the producer must never read ahead into this command.
            process
                .write_input(format!("printf parent > '{}'\n", parent_result.display()).as_bytes())
                .unwrap();
            let status = if corrupt { 1 } else { 0 };
            let end = format!("\x1b]133;D;{status};mez_marker={};", marker.as_str());
            extend_fish_output_until(&mut process, &mut output, |bytes| {
                bytes
                    .windows(end.len())
                    .any(|window| window == end.as_bytes())
                    && parent_result.is_file()
            });
            let transaction_start = output
                .windows(start.len())
                .position(|window| window == start.as_bytes())
                .unwrap();
            let expected_acknowledgements = if acknowledged {
                input
                    .payload
                    .as_bytes()
                    .split_inclusive(|byte| *byte == b'\n')
                    .filter(|record| mez_mux::process::receiver_input_record_requires_ack(record))
                    .count()
            } else {
                0
            };
            assert_eq!(
                output[transaction_start..]
                    .iter()
                    .filter(|byte| **byte == 0x1e)
                    .count(),
                expected_acknowledgements,
                "only logical frame boundaries and ordinary records may be acknowledged"
            );
            assert_eq!(std::fs::read(&parent_result).unwrap(), b"parent");
            if corrupt {
                assert!(!child_result.exists(), "corrupt frame launched a child");
            } else {
                assert_eq!(
                    std::fs::read(&child_result).unwrap(),
                    b"child:inert $(false)"
                );
            }
            process.terminate(Duration::from_millis(100)).unwrap();
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
