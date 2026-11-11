use super::*;

/// This parser is deliberately callable without registering a usable command.
/// Every valid operation has one typed, fully parsed result before any effects.
#[test]
fn session_budget_parser_accepts_status_and_all_unique_flag_orders() {
    for args in ["", "status", "  status  ", "'status'"] {
        assert_eq!(
            parse_session_budget_options(args),
            Ok(SessionBudgetCommand::Status)
        );
    }
    let expected = SessionBudgetCommand::Mutate(SessionBudgetMutation {
        global: true,
        input: Some(BudgetOverride::Explicit(TokenLimit::Limited(1_500_000))),
        output: Some(BudgetOverride::Inherit),
    });
    for args in [
        "--global --input 1.5M --output inherit",
        "--global --output inherit --input 1.5M",
        "--input 1.5M --global --output inherit",
        "--output inherit --global --input 1.5M",
        "--input 1.5M --output inherit --global",
        "--output inherit --input '1.5M' --global",
    ] {
        assert_eq!(parse_session_budget_options(args), Ok(expected), "{args}");
    }
    assert_eq!(
        parse_session_budget_options("--output unlimited"),
        Ok(SessionBudgetCommand::Mutate(SessionBudgetMutation {
            output: Some(BudgetOverride::Explicit(TokenLimit::Unlimited)),
            ..SessionBudgetMutation::default()
        }))
    );
    assert_eq!(
        parse_session_budget_options("--input 0"),
        Ok(SessionBudgetCommand::Mutate(SessionBudgetMutation {
            input: Some(BudgetOverride::Explicit(TokenLimit::Limited(0))),
            ..SessionBudgetMutation::default()
        }))
    );
}

#[test]
fn session_budget_parser_rejects_invalid_compounds_without_resplitting_quotes() {
    for args in [
        "status --global",
        "status --input 1",
        "--input 1 status",
        "--global",
        "--input",
        "--output",
        "--global --global --input 1",
        "--input 1 --input 2",
        "--output 1 --output 2",
        "--input=1",
        "--unknown 1",
        "--input 1 2",
        "--input 1 --output -1",
        "--input inherit --output 0.0001K",
        "--input Unlimited",
        "--output INHERIT",
        "--input '1 2'",
        "'--input 1'",
        "--input ''",
        "--input '1",
        "--input 1\\",
        "--input --global",
        "--input 1 --",
        "Status",
        "--input 1\0",
    ] {
        assert_eq!(
            parse_session_budget_options(args),
            Err(SESSION_BUDGET_USAGE),
            "{args:?}"
        );
    }
    assert_eq!(
        parse_session_budget_option_tokens(["--input 1", "--output", "2"]),
        Err(SESSION_BUDGET_USAGE)
    );
    assert_eq!(
        parse_session_budget_option_tokens(["--input", "1 --output 2"]),
        Err(SESSION_BUDGET_USAGE)
    );
}
