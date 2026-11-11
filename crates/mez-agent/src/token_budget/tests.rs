//! Contract coverage independent of runtime/storage; this does not qualify a
//! provider admission path or advertise a working user command.

use super::*;

/// Typed ingress must not bypass the same signed transport ceiling as text.
#[test]
fn token_budget_deserialization_enforces_external_limit_ceiling() {
    for n in [0, MAX_TOKEN_LIMIT] {
        let value = serde_json::json!({"Limited": n});
        assert_eq!(TokenLimit::limited(n), Ok(TokenLimit::Limited(n)));
        assert_eq!(serde_json::to_value(TokenLimit::Limited(n)).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<TokenLimit>(value.clone()).unwrap(),
            TokenLimit::Limited(n)
        );
        assert!(
            serde_json::from_value::<BudgetOverride>(
                serde_json::json!({"Explicit": value.clone()})
            )
            .is_ok()
        );
        assert!(
            serde_json::from_value::<BudgetLimits>(
                serde_json::json!({"input": value, "output": "Unlimited"})
            )
            .is_ok()
        );
    }
    for n in [MAX_TOKEN_LIMIT + 1, u64::MAX] {
        let value = serde_json::json!({"Limited": n});
        assert_eq!(TokenLimit::limited(n), Err(TokenQuantityError::Overflow));
        assert!(serde_json::to_value(TokenLimit::Limited(n)).is_err());
        assert!(serde_json::to_value(BudgetOverride::Explicit(TokenLimit::Limited(n))).is_err());
        assert!(serde_json::from_value::<TokenLimit>(value.clone()).is_err());
        assert!(
            serde_json::from_value::<BudgetOverride>(
                serde_json::json!({"Explicit": value.clone()})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<BudgetLimits>(
                serde_json::json!({"input": value, "output": "Unlimited"})
            )
            .is_err()
        );
    }
}

#[test]
fn token_budget_quantities_are_exact_bounded_decimal_integers() {
    for (text, expected) in [
        ("0", 0),
        ("0001", 1),
        ("1K", 1000),
        ("1k", 1000),
        ("1m", 1_000_000),
        ("1B", 1_000_000_000),
        ("1t", 1_000_000_000_000),
        ("1P", 1_000_000_000_000_000),
        ("1e", 1_000_000_000_000_000_000),
        ("1.5M", 1_500_000),
        ("0.001K", 1),
        ("000.0010000000k", 1),
        ("9.223372036854775807E", MAX_TOKEN_LIMIT),
        ("9223372036854775807", MAX_TOKEN_LIMIT),
        ("0.000000000000000001E", 1),
        ("00000000000000000000000E", 0),
    ] {
        assert_eq!(parse_token_quantity(text), Ok(expected), "{text}");
    }
    for text in [
        "", " ", "1 ", " 1", "+1", "-1", "1e3", "1E+3", "NaN", "inf", "1,000", "1_000", "１", "١",
        "1Ki", "1KB", "1G", "1.0", "1.", ".1K", "1.K", "K", "1..2M", "1\0", "1\n", "1\tK",
    ] {
        assert!(parse_token_quantity(text).is_err(), "{text:?}");
    }
    assert_eq!(
        parse_token_quantity("0.0001K"),
        Err(TokenQuantityError::FractionalToken)
    );
    for text in [
        "9223372036854775808",
        "9.223372036854775808E",
        "10E",
        "18446744073709551615K",
        "18446744073709551616",
        "99999999999999999999999999999999999E",
    ] {
        assert_eq!(
            parse_token_quantity(text),
            Err(TokenQuantityError::Overflow),
            "{text}"
        );
    }
}

#[test]
fn token_budget_exact_and_compact_display_round_trip_without_rounding() {
    for n in [
        0,
        1,
        999,
        1000,
        1001,
        1_500_000,
        999_999_999,
        MAX_TOKEN_LIMIT,
    ] {
        let limit = TokenLimit::Limited(n);
        assert_eq!(TokenLimit::parse(&limit.to_string()), Ok(limit));
        assert_eq!(TokenLimit::parse(&limit.compact()), Ok(limit));
    }
    assert_eq!(TokenLimit::Limited(1_500_000).compact(), "1.5M");
    assert_eq!(TokenLimit::Unlimited.to_string(), "unlimited");
    assert_eq!(TokenLimit::Unlimited.compact(), "unlimited");
    assert!(TokenLimit::parse("Unlimited").is_err());
}

#[test]
fn token_budget_policy_dimensions_precedence_and_revisions_remain_independent() {
    let root = BudgetRootId::new("root").unwrap();
    let revisions = BudgetRevisions {
        policy: 7,
        accounting: 9,
    };
    let used = UsageTotals {
        input: 10,
        output: 20,
    };
    let evaluate = |input, output, hold| {
        BudgetDecision::evaluate(
            root.clone(),
            revisions,
            BudgetLimits { input, output },
            used,
            hold,
        )
    };
    let both = evaluate(TokenLimit::Limited(10), TokenLimit::Limited(19), None);
    assert!(both.input_exhausted && both.output_exhausted);
    assert_eq!(both.revisions, revisions);
    assert_eq!(both.root, root);
    assert!(
        !evaluate(TokenLimit::Unlimited, TokenLimit::Limited(19), None).permits_budget_admission()
    );
    assert!(
        evaluate(TokenLimit::Limited(11), TokenLimit::Limited(21), None).permits_budget_admission()
    );
    assert!(
        evaluate(TokenLimit::Unlimited, TokenLimit::Unlimited, None).permits_budget_admission()
    );
    assert!(
        !evaluate(
            TokenLimit::Unlimited,
            TokenLimit::Unlimited,
            Some(BudgetOperationalHold::AccountingUnavailable)
        )
        .permits_budget_admission()
    );
    assert!(TokenLimit::Limited(0).exhausted(0));
    assert!(!TokenLimit::Unlimited.exhausted(u64::MAX));
    assert_eq!(TokenLimit::Limited(10).remaining(12), Some(0));
    assert_eq!(TokenLimit::Limited(10).excess(12), 2);
    assert_eq!(TokenLimit::Unlimited.remaining(12), None);
    assert_eq!(
        BudgetOverride::Inherit.resolve(TokenLimit::Limited(2)),
        TokenLimit::Limited(2)
    );
    assert_eq!(
        BudgetOverride::Explicit(TokenLimit::Unlimited).resolve(TokenLimit::Limited(2)),
        TokenLimit::Unlimited
    );
    assert!(!BudgetLimits::default().requires_serial_inference());
    assert!(
        BudgetLimits {
            input: TokenLimit::Limited(0),
            ..BudgetLimits::default()
        }
        .requires_serial_inference()
    );
}

/// Stream prefixes/final totals are cumulative, not separate incurred expense.
/// Terminal unknown coverage never destroys a prior-known amount or invents zero.
#[test]
fn token_budget_usage_reconciles_prefix_final_duplicates_and_partial_coverage() {
    let root = BudgetRootId::new("root").unwrap();
    let attempt = BudgetAttemptId::new("attempt").unwrap();
    let mut state = AttemptUsage::new(root.clone(), attempt.clone());
    assert_eq!(state.observed().input, None);
    assert_eq!(
        state.observe(
            &root,
            &attempt,
            UsageSnapshot {
                input: Some(0),
                ..UsageSnapshot::default()
            }
        ),
        Ok(UsageTotals::default())
    );
    assert_eq!(state.observed().input, Some(0));
    assert!(!state.coverage().input_complete);
    let partial = UsageSnapshot {
        input: Some(10),
        output: Some(2),
        terminal: false,
    };
    assert_eq!(
        state.observe(&root, &attempt, partial),
        Ok(UsageTotals {
            input: 10,
            output: 2
        })
    );
    assert_eq!(
        state.observe(&root, &attempt, partial),
        Ok(UsageTotals::default())
    );
    let before = state.clone();
    assert_eq!(
        state.observe(
            &root,
            &attempt,
            UsageSnapshot {
                input: Some(11),
                output: Some(1),
                terminal: false
            }
        ),
        Err(BudgetUsageError::Regression)
    );
    assert_eq!(state, before);
    assert_eq!(
        state.observe(&BudgetRootId::new("other").unwrap(), &attempt, partial),
        Err(BudgetUsageError::Ownership)
    );
    assert_eq!(
        state.observe(&root, &BudgetAttemptId::new("other").unwrap(), partial),
        Err(BudgetUsageError::Ownership)
    );
    let final_snapshot = UsageSnapshot {
        input: None,
        output: Some(5),
        terminal: true,
    };
    assert_eq!(
        state.observe(&root, &attempt, final_snapshot),
        Ok(UsageTotals {
            input: 0,
            output: 3
        })
    );
    assert_eq!(
        state.known(),
        UsageTotals {
            input: 10,
            output: 5
        }
    );
    assert_eq!(
        state.coverage(),
        UsageCoverage {
            input_complete: false,
            output_complete: true
        }
    );
    assert!(state.is_terminal());
    assert_eq!(
        state.observe(&root, &attempt, final_snapshot),
        Ok(UsageTotals::default())
    );
    let before = state.clone();
    assert_eq!(
        state.observe(&root, &attempt, partial),
        Err(BudgetUsageError::Finalized)
    );
    assert_eq!(state, before);
}

#[test]
fn token_budget_reconciliation_checks_lifetime_overflow_before_publishing_either_field() {
    let root = BudgetRootId::new("root").unwrap();
    let attempt = BudgetAttemptId::new("attempt").unwrap();
    let state = AttemptUsage::new(root.clone(), attempt.clone());
    let snapshot = UsageSnapshot {
        input: Some(0),
        output: Some(1),
        terminal: true,
    };
    let totals = UsageTotals {
        input: 12,
        output: u64::MAX,
    };
    assert_eq!(
        state.reconcile(&root, &attempt, snapshot, totals),
        Err(BudgetUsageError::Overflow)
    );
    assert_eq!(state.observed(), UsageSnapshot::default());
    assert!(!state.is_terminal());
    let (settled, totals) = state
        .reconcile(&root, &attempt, snapshot, UsageTotals::default())
        .unwrap();
    assert_eq!(
        totals,
        UsageTotals {
            input: 0,
            output: 1
        }
    );
    assert!(settled.coverage().input_complete);
    assert_eq!(
        settled
            .reconcile(&root, &attempt, snapshot, totals)
            .unwrap()
            .1,
        totals
    );
    assert!(
        UsageTotals {
            input: u64::MAX,
            output: 0
        }
        .checked_add(UsageTotals {
            input: 1,
            output: 1
        })
        .is_err()
    );
}

#[test]
fn token_budget_identities_validate_at_construction_and_deserialization() {
    for text in ["", " ", "a b", "a\n", "a\0"] {
        assert!(BudgetRootId::new(text).is_err());
        assert!(serde_json::from_value::<BudgetAttemptId>(serde_json::json!(text)).is_err());
    }
    assert!(BudgetReceiptId::new("x".repeat(257)).is_err());
    let root = BudgetRootId::new("opaque-root").unwrap();
    assert_eq!(
        serde_json::from_str::<BudgetRootId>(&serde_json::to_string(&root).unwrap()).unwrap(),
        root
    );
}
