//! Pure evidence binding and original-journal containment; real HTTP coverage
//! belongs to provider-service budget_receipts::transport.

use super::*;

struct Factory {
    proof: Arc<Mutex<Option<ProviderFinalUsage>>>,
    fault: u8,
}

impl ProviderObservationReceiptFactory for Factory {
    fn usage_finalized(
        &self,
        _: &ProviderWireRequestObservation,
        proof: ProviderFinalUsage,
    ) -> Result<()> {
        *self.proof.lock().unwrap() = Some(proof);
        match self.fault {
            1 => Err(MezError::conflict("fixture final evidence refused")),
            2 => panic!("fixture final evidence callback panicked"),
            _ => Ok(()),
        }
    }

    fn freeze(
        &self,
        _: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        Ok(Arc::new(Receipt))
    }
}

struct Receipt;
impl ProviderObservationReceipt for Receipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn final_unary_evidence_binds_subject_values_presence_and_fences_failed_transfer() {
    for fault in 0..=2 {
        let proof = Arc::new(Mutex::new(None));
        let factory: Arc<dyn ProviderObservationReceiptFactory> = Arc::new(Factory {
            proof: proof.clone(),
            fault,
        });
        let pending = PendingProviderWireObservations::default();
        let (sender, _) = tokio::sync::mpsc::channel(1);
        let observer = ProviderWireRequestObserver::new("conversation", "%1", sender)
            .with_pending_observations(pending.clone())
            .with_budget_receipts(factory.clone())
            .unwrap();
        let request = mez_agent::epoch_compaction::provider::request_from_source(
            &mez_agent::ModelProfile {
                provider: "fixture".into(),
                model: "model".into(),
                ..Default::default()
            },
            "%1",
            "conversation",
            "fixture".into(),
            mez_agent::AllowedActionSet::say_only(),
        );
        let context = ProviderWireObservationContext::new(
            &observer,
            ProviderRequestPurpose::Execution,
            "fixture".into(),
            Ok(None),
        );
        context.plan_wire_attempt(&request, 1, None).unwrap();
        let usage = mez_agent::ModelTokenUsage {
            input_tokens: 5,
            reasoning_tokens: 3,
            cached_input_tokens: Some(2),
            ..Default::default()
        };
        let presence = mez_agent::ModelTokenUsagePresence {
            input: true,
            output: false,
        };
        assert_eq!(
            context.finalize_unary_usage(usage, presence).is_err(),
            fault != 0
        );
        let mut observation = context
            .planned_wire
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .observation
            .clone();
        observation.usage = Some(usage);
        observation.usage_presence = Some(presence);
        {
            let retained = proof.lock().unwrap();
            let retained = retained.as_ref().unwrap();
            assert_eq!(
                retained.terminal_snapshot(&observation).unwrap(),
                UsageSnapshot {
                    input: Some(5),
                    output: None,
                    terminal: true
                }
            );
            let mut foreign = observation.clone();
            foreign.request_id.push_str("-foreign");
            assert!(retained.terminal_snapshot(&foreign).is_err());
            let mut changed = observation.clone();
            changed.usage.as_mut().unwrap().input_tokens += 1;
            assert!(retained.terminal_snapshot(&changed).is_err());
            let mut changed = observation.clone();
            changed.usage_presence.as_mut().unwrap().output = true;
            assert!(retained.terminal_snapshot(&changed).is_err());
        }
        assert!(pending.retain_with_receipt(observation.clone(), Some(&factory)));
        assert_eq!(pending.snapshot(), vec![observation.clone()]);
        assert_eq!(pending.checkpoint(&observation).await.is_err(), fault != 0);
        pending.acknowledge(&observation.request_id);
        assert_eq!(pending.is_empty(), fault == 0);
        if fault != 0 {
            assert_eq!(pending.snapshot(), vec![observation]);
        }
    }
}

#[test]
fn final_unary_chat_shape_requires_all_choices_finished() {
    for body in [
        "invalid",
        "{}",
        r#"{"choices":[]}"#,
        r#"{"choices":[{"finish_reason":null}]}"#,
        r#"{"choices":[{"finish_reason":"stop"},{"finish_reason":null}]}"#,
    ] {
        assert!(!chat_body_is_terminal(body));
    }
    assert!(chat_body_is_terminal(
        r#"{"choices":[{"finish_reason":"stop"},{"finish_reason":"length"}]}"#
    ));
}
