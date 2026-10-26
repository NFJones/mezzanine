//! Required source-end replies through the existing control worker/callback lane.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Recognizes one finite frame before synchronous control dispatch.
    pub(super) fn prepare_external_source_end_input(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &ControlConnectionState,
    ) -> Option<(
        String,
        usize,
        Result<Box<crate::runtime::ExternalSourceEndWork>>,
    )> {
        let (body, consumed) = decode_control_frame(input, max_content_length).ok()?;
        let request = crate::control::parse_json_rpc_request(&body).ok()?;
        if request.method != "agent/external/source-end" {
            return None;
        }
        let result = if consumed != input.len() || body.len() > 8192 {
            Err(MezError::invalid_args(
                "external source end requires one bounded control frame",
            ))
        } else {
            self.service
                .prepare_external_source_end(&request, connection)
                .map(Box::new)
        };
        Some((request.id, consumed, result))
    }

    /// Keeps exact connection/reply custody while blocking metadata work runs;
    /// caller loss cannot cancel physical work or create another mutation budget.
    #[allow(
        clippy::too_many_arguments,
        reason = "original bounded control framing and reply custody"
    )]
    pub(super) fn dispatch_external_source_end(
        &mut self,
        id: String,
        prepared: Result<Box<crate::runtime::ExternalSourceEndWork>>,
        connection: ControlConnectionState,
        mut output_prefix: Vec<u8>,
        consumed: usize,
        reply: oneshot::Sender<Result<AsyncControlInputResult>>,
    ) {
        // Logical closure already happened. Maintenance cannot depend on I/O.
        let timer = self.queue_shell_lifecycle_timer_side_effects();
        let work = match prepared {
            Ok(work) => work,
            Err(error) => {
                output_prefix.extend_from_slice(&encode_control_body(
                    &crate::runtime::runtime_json_rpc_error(&id, error.kind(), error.message()),
                ));
                let _ = reply.send(Ok(AsyncControlInputResult {
                    output: output_prefix,
                    consumed,
                    connection,
                    connection_cleanup: None,
                    terminal_lifecycle_flush: None,
                }));
                self.notify_event_delivery();
                return;
            }
        };
        if let Err(error) = timer {
            let body = self
                .service
                .complete_external_source_end(*work, Err(error), &connection);
            output_prefix.extend_from_slice(&encode_control_body(&body));
            let _ = reply.send(Ok(AsyncControlInputResult {
                output: output_prefix,
                consumed,
                connection,
                connection_cleanup: None,
                terminal_lifecycle_flush: None,
            }));
            self.notify_event_delivery();
            return;
        }
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let worker = work.clone();
            #[cfg(test)]
            if let Some((started, release)) = &worker.worker_gate {
                started.notify_one();
                release.notified().await;
            }
            let result = tokio::task::spawn_blocking(move || worker.execute())
                .await
                .map_err(|_| MezError::invalid_state("external source end worker failed"))
                .and_then(|result| result);
            let _ = sender
                .send(AsyncRuntimeRequestEnvelope::new(
                    AsyncRuntimeRequest::CompleteExternalSourceEndInput {
                        work,
                        result,
                        connection,
                        output_prefix,
                        consumed,
                        reply,
                    },
                ))
                .await;
        });
        self.notify_event_delivery();
    }
}
