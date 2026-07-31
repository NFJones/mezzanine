use super::*;
use base64::Engine;
use std::process::Command;

/// Verifies the pane command reads exported values from its own process
/// environment and reports unset names without shell interpolation.
#[test]
fn pane_environment_command_reads_only_requested_exported_values() {
    let request = PaneEnvironmentRequest::new(vec![
        "MEZ_TEST_PRESENT".to_string(),
        "MEZ_TEST_UNSET".to_string(),
    ])
    .unwrap();
    let command =
        pane_environment_evidence_command(&request, ShellClassification::PosixSh).unwrap();
    assert!(!command.contains("MEZ_TEST_PRESENT="));
    assert!(!command.contains("$MEZ_TEST_PRESENT"));

    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .env("MEZ_TEST_PRESENT", "pane-value with spaces")
        .env_remove("MEZ_TEST_UNSET")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence =
        parse_pane_environment_evidence(&String::from_utf8(output.stdout).unwrap(), &request)
            .unwrap();
    assert_eq!(
        evidence.values.get("MEZ_TEST_PRESENT").map(String::as_str),
        Some("pane-value with spaces")
    );
    assert_eq!(
        evidence.omitted.get("MEZ_TEST_UNSET").map(String::as_str),
        Some("unset")
    );
}

/// Verifies malformed, unsafe, and missing pane records degrade individual
/// names without accepting values outside the exact request.
#[test]
fn pane_environment_parser_restricts_invalid_records() {
    let request = PaneEnvironmentRequest::new(vec![
        "GOOD".to_string(),
        "UNSAFE".to_string(),
        "MISSING".to_string(),
    ])
    .unwrap();
    let wire = serde_json::json!({
        "version": 1,
        "entries": [
            {
                "name": "GOOD",
                "status": "present",
                "value": base64::engine::general_purpose::STANDARD.encode("ok")
            },
            {
                "name": "UNSAFE",
                "status": "present",
                "value": base64::engine::general_purpose::STANDARD.encode("line\nbreak")
            }
        ]
    });
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&wire).unwrap());
    let evidence = parse_pane_environment_evidence(
        &format!("noise\nMEZ_ENVIRONMENT_EVIDENCE_V1\t{encoded}\n"),
        &request,
    )
    .unwrap();
    assert_eq!(evidence.values.get("GOOD").map(String::as_str), Some("ok"));
    assert_eq!(
        evidence.omitted.get("UNSAFE").map(String::as_str),
        Some("unsafe_control")
    );
    assert_eq!(
        evidence.omitted.get("MISSING").map(String::as_str),
        Some("missing_record")
    );
}

/// Verifies request validation rejects duplicate and non-portable names before
/// any pane command can be generated.
#[test]
fn pane_environment_request_rejects_unsafe_names() {
    for names in [
        vec!["DUP".to_string(), "DUP".to_string()],
        vec!["BAD-NAME".to_string()],
        vec!["1BAD".to_string()],
    ] {
        assert!(PaneEnvironmentRequest::new(names).is_err());
    }
}
