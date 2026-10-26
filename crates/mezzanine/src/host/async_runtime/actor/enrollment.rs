//! Synchronous enrollment settlement outside the large async request poll frame.
//!
//! This owner preserves original control framing and reply custody across native
//! observation, actual actor admission and off-actor receipt acceptance (required
//! for cold claims, optional initially). No actor SQLite or ancestry walk occurs;
//! final cold publication additionally checks a constant-time native boot clock.
//! Heap-owned work and a
//! redacted retained reply stay bounded by the enrollment admission pool. Moving
//! these synchronous temporaries out of handle_request follows the existing raw
//! MMP boundary pattern; it does not increase stacks or change execution order.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Test-only synchronous harness around the REAL completion method and
    /// nondroppable queue admission. Returns the original service for its native
    /// fixture's exact cleanup; no process, callback or metadata success is fake.
    #[cfg(test)]
    pub(crate) fn full_timer_lane_enrollment_for_tests(
        service: crate::runtime::RuntimeSessionService,
        work: crate::runtime::ExternalEnrollmentWork,
        result: crate::Result<()>,
        connection: ControlConnectionState,
    ) -> (
        crate::runtime::RuntimeSessionService,
        Option<crate::MezError>,
    ) {
        let (_handle, mut actor) = Self::new(
            service,
            AsyncRuntimeActorConfig {
                side_effect_buffer: 1,
                ..Default::default()
            },
        )
        .unwrap();
        actor
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::Bootstrap,
                    "cold-timer-lane-fixture",
                    1,
                ),
                delay_ms: 1000,
            }])
            .unwrap();
        let (reply, mut received) = oneshot::channel();
        actor.complete_enrollment_input(Box::new(work), result, connection, Vec::new(), 0, reply);
        let error = received.try_recv().unwrap().err();
        (actor.service, error)
    }

    /// Settles one exact phase, arms maintenance even on reply loss, and either
    /// transfers the reply to the original worker lane or publishes it once.
    #[allow(
        clippy::too_many_arguments,
        reason = "exact control framing and reply ownership cross the same actor boundary"
    )]
    pub(super) fn complete_enrollment_input(
        &mut self,
        work: Box<crate::runtime::ExternalEnrollmentWork>,
        result: crate::Result<()>,
        connection: ControlConnectionState,
        mut output_prefix: Vec<u8>,
        consumed: usize,
        reply: oneshot::Sender<crate::Result<AsyncControlInputResult>>,
    ) {
        let publishing = work.is_source_publication();
        let body = if publishing {
            self.service
                .complete_external_source_publication((*work).clone(), result, &connection)
        } else {
            self.service
                .complete_external_enrollment((*work).clone(), result, &connection)
        };
        // Admission is already real; idle maintenance must not wait for optional
        // metadata I/O or depend on an original reply receiver still existing.
        if let Err(error) = self.queue_shell_lifecycle_timer_side_effects() {
            // This exit precedes required-phase transfer. Preserve exact cold
            // failure cleanup even when the timer lane itself cannot admit.
            // Already published bindings and stale registries remain untouched.
            self.service.discard_unpublished_cold(&work);
            let _ = reply.send(Err(error));
            self.notify_event_delivery();
            return;
        }
        let body = if !publishing {
            match self.service.begin_external_source_publication(*work, body) {
                Ok(work) => {
                    let request_id = work.request_id.clone();
                    self.dispatch_external_enrollment_observation(
                        request_id,
                        Ok(Box::new(work)),
                        connection,
                        output_prefix,
                        consumed,
                        reply,
                    );
                    self.notify_event_delivery();
                    return;
                }
                Err(body) => body,
            }
        } else {
            body
        };
        output_prefix.extend_from_slice(&encode_control_body(&body));
        let _ = reply.send(Ok(AsyncControlInputResult {
            output: output_prefix,
            consumed,
            connection,
            connection_cleanup: None,
            terminal_lifecycle_flush: None,
        }));
        self.notify_event_delivery();
    }
}
