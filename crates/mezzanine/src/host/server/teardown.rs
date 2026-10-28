//! Accepted host-stop sequencing with one deadline and truthful failure reports.
//!
//! Admission is one-way: after draining starts, checkpoint failure cannot reopen
//! this host. Remaining safe stages run through the existing router/supervisor
//! under the original force level and absolute budget. Failed durability stays an
//! error even if runtimes settle. Healthy completion requires every stage and
//! original-connection publication; neither diagnostics nor elapsed time proves
//! native retirement or grants retry authority.

use super::*;
use crate::error::TeardownErrors;

impl HostServer {
    /// Finishes an accepted stop after either an explicit request or serving
    /// failure. It cancels owned connections after bounded admission draining,
    /// attempts required checkpoints and exact runtime shutdown, then publishes
    /// the final verdict within the original authenticated connection deadline.
    pub(super) async fn complete_shutdown<F>(
        &self,
        shutdown: HostShutdownRequest,
        completion: Option<HostShutdownCompletion>,
        mut connections: FuturesUnordered<F>,
        mut failures: TeardownErrors,
    ) -> Result<()>
    where
        F: Future<Output = Option<(HostShutdownRequest, Option<HostShutdownCompletion>)>>,
    {
        let now = tokio::time::Instant::now();
        let deadline = match now.checked_add(self.config.shutdown_timeout) {
            Some(deadline) => deadline,
            None => {
                failures.record(
                    "host shutdown budget",
                    MezError::invalid_args("host shutdown budget is not representable"),
                );
                now
            }
        };
        let completion = completion.map(|mut reply| {
            reply.deadline = reply.deadline.min(deadline);
            reply
        });
        match self.router.start_draining() {
            Err(error) => failures.record("host admission drain", error),
            Ok(()) if tokio::time::Instant::now() >= deadline => {
                failures.record(
                    "host admission drain",
                    MezError::invalid_state(
                        "host shutdown deadline expired before admission drain",
                    ),
                );
            }
            Ok(()) => {
                let draining = tokio::time::timeout_at(deadline, self.router.begin_draining());
                tokio::pin!(draining);
                loop {
                    tokio::select! {
                        result = &mut draining => {
                            match result {
                                Ok(Ok(())) => {},
                                Ok(Err(error)) => failures.record("host admission drain", error),
                                Err(_) => failures.record("host admission drain", MezError::invalid_state("host shutdown deadline expired during admission drain")),
                            }
                            break;
                        }
                        _ = connections.next(), if !connections.is_empty() => {},
                    }
                }
            }
        }
        // Only owned connection futures are cancelled. Runtime/native custody
        // remains in the existing supervisor even if an admitted request failed.
        drop(connections);
        if !shutdown.force {
            let checkpoint = if tokio::time::Instant::now() >= deadline {
                Err(MezError::invalid_state(
                    "host shutdown deadline expired before required checkpoints",
                ))
            } else {
                tokio::time::timeout_at(
                    deadline,
                    Box::pin(self.router.checkpoint_active_local_assignments_strict()),
                )
                .await
                .map_err(|_| {
                    MezError::invalid_state(
                        "host shutdown deadline expired during required checkpoints",
                    )
                })
                .and_then(|result| result.map(|_| ()))
            };
            if let Err(error) = checkpoint {
                failures.record("required local checkpoints", error);
            }
        }
        match self
            .router
            .shutdown_all_until(shutdown.force, deadline)
            .await
        {
            Err(error) => failures.record("host runtime shutdown", error),
            Ok(()) => {
                if let Err(error) = self.router.mark_stopped() {
                    failures.record("host stopped publication", error);
                }
            }
        }
        let result = failures.finish();
        let publication = match completion {
            Some(reply) => reply.publish(shutdown.force, &result).await,
            None => Ok(()),
        };
        match (result, publication) {
            (Err(primary), Err(publication)) => {
                let mut failures = TeardownErrors::default();
                failures.record("host shutdown", primary);
                failures.record("shutdown completion reply", publication);
                failures.finish()
            }
            (Err(error), _) => Err(error),
            (_, Err(error)) => Err(error.with_context("shutdown completion reply")),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}
