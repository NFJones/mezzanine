//! Host initialization diagnostics preserve static stages and success silence.
use super::*;

/// Timing reports contain only caller-selected static labels and elapsed values.
/// A slow stage is observable without relaxing admission budgets, while complete
/// publication removes incomplete evidence even if the operation was slow.
#[test]
fn host_initialize_diagnostics_distinguishes_slow_and_completed_stages() {
    let mut diagnostics = InitializeDiagnostics::new();
    assert_eq!(diagnostics.incomplete_report().unwrap().0, "trust");
    for stage in [
        "routing",
        "actor-admission",
        "lease-settlement",
        "reply-publication",
    ] {
        diagnostics.advance(stage);
        assert_eq!(diagnostics.incomplete_report().unwrap().0, stage);
    }
    let now = std::time::Instant::now();
    diagnostics.started = now - std::time::Duration::from_secs(2);
    diagnostics.stage_started = now - std::time::Duration::from_millis(200);
    let (stage, stage_ms, total_ms) = diagnostics.slow_report().unwrap();
    assert_eq!(stage, "reply-publication");
    assert!(stage_ms >= 200 && total_ms >= 2000 && stage_ms <= total_ms);
    diagnostics.complete();
    assert!(diagnostics.incomplete_report().is_none());
    diagnostics.advance("routing");
    assert!(diagnostics.incomplete_report().is_none());
}
