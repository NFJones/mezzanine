//! Version-two private Pi observation framing for parent-owned session handoff.
//!
//! Session spelling and observer epoch are proposals, never daemon authority.
//! A same-user inherited socket carries bounded allowlisted facts only. The
//! parent separately validates transition ordering and obtains fresh authority;
//! this module reads no vendor files, issues no grants, and changes no decisions.

use super::{pi, pi_ipc};
use crate::error::{MezError, Result};
use serde::Deserialize;
use std::os::fd::AsRawFd;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::time::Instant;

/// Finite physical-frame budget including session/epoch envelope overhead.
const MAX_FRAME: usize = 2048;

/// Inert child proposal retained only until parent validation and adoption.
pub(crate) struct Frame {
    pub(crate) session: String,
    pub(crate) epoch: u64,
    pub(crate) fact: pi::Observation,
}

/// Strict envelope rejects duplicate/unknown keys and arbitrary body fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    session: String,
    epoch: u64,
    event: pi_ipc::Event,
}

/// Payload-free failure never includes session text or vendor content.
fn unavailable() -> MezError {
    MezError::invalid_state("Pi session proposal unavailable")
}

impl Frame {
    /// Decodes exact content-free facts without allowing session paths or unsafe
    /// JavaScript integers to become parent ownership or model-visible content.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_FRAME {
            return Err(unavailable());
        }
        let wire: Wire = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
        if wire.epoch == 0 || wire.epoch > 9_007_199_254_740_991 {
            return Err(unavailable());
        }
        let fact = pi_ipc::observation(
            &wire.session,
            &serde_json::to_vec(&wire.event).map_err(|_| unavailable())?,
        )
        .map_err(|_| unavailable())?;
        Ok(Self {
            session: wire.session,
            epoch: wire.epoch,
            fact,
        })
    }
}

/// Sole stream reader. One-byte reads leave the next proposal untouched while
/// a previous session's retirement or reauthorization is awaited by the parent.
pub(crate) struct Reader(tokio::net::UnixStream);

impl Reader {
    /// Authenticates an inherited observation endpoint before reading proposals.
    pub(crate) fn new(stream: tokio::net::UnixStream) -> Result<Self> {
        crate::runtime::authenticated_unix_peer_uid(
            stream.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        )
        .map_err(|_| unavailable())?;
        Ok(Self(stream))
    }

    /// Reads one frame with a non-resetting 250ms deadline after its first byte.
    /// Idle silence is allowed; clean EOF is separate from truncated-frame loss.
    pub(crate) async fn next(&mut self) -> Result<Option<Frame>> {
        let mut bytes = Vec::with_capacity(MAX_FRAME);
        let mut deadline = None;
        loop {
            let mut byte = [0];
            if deadline.is_some_and(|end| Instant::now() >= end) {
                return Err(unavailable());
            }
            let count = if let Some(end) = deadline {
                tokio::time::timeout_at(end, self.0.read(&mut byte))
                    .await
                    .map_err(|_| unavailable())?
            } else {
                self.0.read(&mut byte).await
            }
            .map_err(|_| unavailable())?;
            if deadline.is_some_and(|end| Instant::now() >= end) {
                return Err(unavailable());
            }
            if count == 0 {
                return if bytes.is_empty() {
                    Ok(None)
                } else {
                    Err(unavailable())
                };
            }
            if byte[0] == b'\n' {
                return Frame::decode(&bytes).map(Some);
            }
            if bytes.len() == MAX_FRAME {
                return Err(unavailable());
            }
            if bytes.is_empty() {
                deadline = Some(Instant::now() + Duration::from_millis(250));
            }
            bytes.push(byte[0]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// Explicit supplied package/runtime paths exercise the actual compiled
    /// directory artifact through Pi's loader/runner. Only fake session context
    /// and allowlisted callbacks are used; no provider, tools or user extensions.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "explicit MEZ_PI_NODE and MEZ_PI_PACKAGE required for offline loader"]
    async fn pi_binding_installed_loader_preserves_session_transition_envelopes() {
        use super::super::{installer, pi_artifact, pi_launch};
        let node = std::path::PathBuf::from(std::env::var_os("MEZ_PI_NODE").unwrap());
        let package = std::path::PathBuf::from(std::env::var_os("MEZ_PI_PACKAGE").unwrap());
        assert!(node.is_absolute() && package.is_absolute());
        let root = std::env::temp_dir().join(format!(
            "mez-pi-load-v2-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let artifact = root.join("agent");
        let home = root.join("home");
        std::fs::create_dir(&artifact).unwrap();
        std::fs::create_dir(&home).unwrap();
        installer::plan(
            &artifact,
            &pi_artifact::candidate_manifest(),
            installer::Operation::Install,
        )
        .unwrap()
        .apply()
        .unwrap();
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let launched = pi_launch::spawn(pi_launch::LaunchSpec {
            executable: node.clone(),
            directory: root.clone(),
            arguments: vec![
                repository
                    .join("scripts/qualify-pi-binding.mjs")
                    .into_os_string(),
                package.into_os_string(),
                artifact.into_os_string(),
            ],
            environment: vec![
                ("HOME".into(), home.into_os_string()),
                ("PATH".into(), node.parent().unwrap().as_os_str().into()),
                ("MEZ_PI_OBSERVER_FD".into(), "3".into()),
                ("MEZ_PI_OBSERVER_SESSION".into(), "initial".into()),
                ("MEZ_PI_OBSERVER_PROTOCOL".into(), "2".into()),
            ],
            stdin: std::process::Stdio::null(),
            stdout: std::process::Stdio::null(),
            stderr: std::process::Stdio::null(),
        })
        .unwrap();
        let mut child = launched.child;
        let read = async {
            let mut reader = Reader::new(launched.observer).unwrap();
            let mut seen = Vec::new();
            while let Some(frame) = reader.next().await.unwrap() {
                seen.push((frame.session, frame.epoch, frame.fact));
            }
            seen
        };
        let (seen, exit) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(read, child.wait())
        })
        .await
        .unwrap();
        assert!(exit.unwrap().success());
        assert_eq!(seen.len(), 15);
        for (index, session, epoch, reason) in [
            (0, "initial", 1, "startup"),
            (3, "second", 2, "new"),
            (6, "initial", 3, "resume"),
            (9, "branch", 4, "fork"),
            (12, "branch", 5, "reload"),
        ] {
            assert_eq!(
                seen[index],
                (
                    session.into(),
                    epoch,
                    pi::Observation::SessionStarted { reason }
                )
            );
        }
        assert_eq!(
            seen.last().unwrap().2,
            pi::Observation::SessionShutdown { reason: "quit" }
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Session and epoch envelope cannot import paths, secrets, callback content,
    /// unknown/duplicate fields or unsafe integers. Known facts remain inert.
    #[test]
    fn pi_binding_frames_reject_content_and_ambiguous_identity() {
        let valid = br#"{"session":"bound","epoch":1,"event":{"type":"agent_start"}}"#;
        assert_eq!(Frame::decode(valid).unwrap().fact, pi::Observation::Running);
        for value in [
            r#"{"session":"../private","epoch":1,"event":{"type":"agent_start"}}"#,
            r#"{"session":"bound","epoch":0,"event":{"type":"agent_start"}}"#,
            r#"{"session":"bound","epoch":9007199254740992,"event":{"type":"agent_start"}}"#,
            r#"{"session":"bound","epoch":1,"extra":"secret","event":{"type":"agent_start"}}"#,
            r#"{"session":"bound","session":"other","epoch":1,"event":{"type":"agent_start"}}"#,
            r#"{"session":"bound","epoch":1,"event":{"type":"agent_start","prompt":"secret"}}"#,
            r#"{"session":"bound","epoch":1,"event":{"type":"session_start","reason":"startup","reason":"reload"}}"#,
        ] {
            assert!(Frame::decode(value.as_bytes()).is_err());
        }
    }

    /// Framing preserves adjacent proposals and permits idle input, but stalls
    /// after partial bytes and truncated EOF fail under the shared finite budget.
    #[tokio::test(start_paused = true, flavor = "current_thread")]
    async fn pi_binding_reader_bounds_partial_frames_and_retains_next() {
        let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
        let mut reader = Reader::new(stream).unwrap();
        writer.write_all(b"{\"session\":\"bound\",\"epoch\":1,\"event\":{\"type\":\"agent_start\"}}\n{\"session\":\"bound\",\"epoch\":1,\"event\":{\"type\":\"agent_settled\"}}\n").await.unwrap();
        assert_eq!(
            reader.next().await.unwrap().unwrap().fact,
            pi::Observation::Running
        );
        assert_eq!(
            reader.next().await.unwrap().unwrap().fact,
            pi::Observation::Settled
        );
        writer.write_all(b"{").await.unwrap();
        assert!(reader.next().await.is_err());
        writer.shutdown().await.unwrap();
    }
}
