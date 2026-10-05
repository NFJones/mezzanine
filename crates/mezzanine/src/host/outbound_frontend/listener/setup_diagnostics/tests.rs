//! Static setup reports distinguish incomplete stages from completed delivery.
use super::*;

/// Success must produce no incomplete report, including host-only management.
/// Failed/cancelled-stage evidence is restricted to static labels and timing;
/// later stage changes cannot resurrect an already completed setup report.
#[test]
fn outbound_setup_diagnostics_reports_only_incomplete_stages() {
    let mut diagnostics = SetupDiagnostics::new();
    assert_eq!(diagnostics.report().unwrap().0, "admit");
    for stage in [
        "profile",
        "connect",
        "initialize",
        "initialize-reply",
        "event-preface",
        "host-only-delivery",
        "first-view",
    ] {
        diagnostics.advance(stage);
        let (reported, stage_ms, total_ms) = diagnostics.report().unwrap();
        assert_eq!(reported, stage);
        assert!(stage_ms <= total_ms);
    }
    diagnostics.complete();
    assert!(diagnostics.report().is_none());
    diagnostics.advance("host-only-delivery");
    assert!(diagnostics.report().is_none());
}
