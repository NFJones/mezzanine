//! Consumed X11 channel relay to an independently authenticated frontend stream.
//!
//! Exact-route preface validation belongs to channel admission. This layer bounds
//! setup consumption, checks the negotiated fake cookie before exposing any bytes,
//! and uses fresh direction-local codecs. Real-cookie substitution and local X
//! dialing remain attaching-client responsibilities. No tasks, retries or IPC
//! publication are introduced. Cancellation drops the channel reset/permit owner;
//! normal half-close preserves the reverse direction until EOF or route stop.

use super::*;
use crate::runtime::x11::{X11Cookie, X11IrohDecoder, X11IrohEncoder, X11SetupProgress};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

impl AuthenticatedX11Channel {
    /// Relays one authenticated channel to a caller-owned frontend byte stream.
    /// The caller supplies this exact connection's immutable codec and admitted
    /// offer cookie, never proof from an unrelated session. Setup decoding and
    /// initial delivery share a finite deadline. Application lifetime is bounded
    /// by caller cancellation, route stop and stream EOF, not a guessed idle timer.
    /// Errors are payload-free and consume ownership without replaying bytes.
    pub(super) async fn relay<S>(
        self,
        frontend: S,
        compression: IrohCompressionPolicy,
        fake_cookie: &X11Cookie,
        setup_budget: Duration,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        if !(Duration::from_millis(100)..=Duration::from_secs(120)).contains(&setup_budget) {
            return Err(MezError::invalid_args(
                "outbound X11 relay setup deadline invalid",
            ));
        }
        self.relay_until(
            frontend,
            compression,
            fake_cookie,
            tokio::time::Instant::now() + setup_budget,
        )
        .await
    }

    /// Uses an already validated operation's absolute setup deadline. Positive
    /// remainders need not meet the configuration minimum; expiry rejects before
    /// setup bytes are exposed. Established application relay is not timed here.
    pub(in crate::host::outbound_frontend::setup::connection::initialize::session) async fn relay_until<
        S,
    >(
        mut self,
        mut frontend: S,
        compression: IrohCompressionPolicy,
        fake_cookie: &X11Cookie,
        deadline: tokio::time::Instant,
    ) -> Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        if deadline <= tokio::time::Instant::now() {
            return Err(MezError::invalid_state(
                "outbound X11 relay setup timed out",
            ));
        }
        self._endpoint.frontend_config_root()?;
        let mut decoder = X11IrohDecoder::new(compression)?;
        tokio::time::timeout_at(deadline, async {
            let setup = if decoder.is_raw() {
                read_raw_setup(&mut self.recv).await?
            } else {
                decoder
                    .read_setup(&mut self.recv)
                    .await
                    .map_err(|_| MezError::invalid_state("outbound X11 setup decode unavailable"))?
            };
            let packet = crate::runtime::x11::validate_x11_setup_cookie(&setup, fake_cookie)
                .map_err(|_| MezError::forbidden("outbound X11 setup credential invalid"))?;
            if packet.packet_len != setup.len() {
                return Err(MezError::forbidden(
                    "outbound X11 setup record contains trailing bytes",
                ));
            }
            self._endpoint.frontend_config_root()?;
            frontend.write_all(&setup).await.map_err(|_| {
                MezError::invalid_state("outbound X11 frontend setup write unavailable")
            })?;
            frontend.flush().await.map_err(|_| {
                MezError::invalid_state("outbound X11 frontend setup flush unavailable")
            })
        })
        .await
        .map_err(|_| MezError::invalid_state("outbound X11 relay setup timed out"))??;

        let (mut input, mut output) = tokio::io::split(frontend);
        let mut encoder = X11IrohEncoder::new(compression)?;
        let stopped = self.send.stopped();
        let cancelled = async {
            match stopped.await {
                Ok(Some(_)) | Err(_) => (),
                Ok(None) => std::future::pending::<()>().await,
            }
        };
        let upstream = async {
            encoder
                .relay(&mut input, &mut self.send, None)
                .await
                .map_err(|_| MezError::invalid_state("outbound X11 upstream relay unavailable"))?;
            self.send
                .finish()
                .map_err(|_| MezError::invalid_state("outbound X11 upstream finish unavailable"))?;
            Ok::<_, MezError>(())
        };
        let downstream = async {
            decoder
                .relay(&mut self.recv, &mut output, None)
                .await
                .map_err(|_| MezError::invalid_state("outbound X11 downstream relay unavailable"))
        };
        tokio::pin!(cancelled, upstream, downstream);
        let mut input_done = false;
        let mut output_done = false;
        loop {
            tokio::select! {
                biased;
                () = &mut cancelled => return Ok(()),
                result = &mut upstream, if !input_done => { result?; input_done = true; },
                result = &mut downstream, if !output_done => { result?; output_done = true; },
            }
            if input_done && output_done {
                // Both directions completed normally. SendStream Drop retains
                // its finished tail; reset would instead abandon buffered data.
                self.graceful = true;
                return Ok(());
            }
        }
    }
}

/// Reads exactly one raw setup packet without consuming later application bytes.
/// The fixed parser bounds every requested allocation and the caller owns time.
async fn read_raw_setup<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Zeroizing<Vec<u8>>> {
    let mut setup = Zeroizing::new(Vec::new());
    loop {
        match crate::runtime::x11::parse_x11_setup(&setup)
            .map_err(|_| MezError::forbidden("outbound X11 setup packet invalid"))?
        {
            X11SetupProgress::Complete(_) => return Ok(setup),
            X11SetupProgress::Incomplete { required_len } => {
                if required_len <= setup.len()
                    || required_len > crate::runtime::x11::X11_MAX_SETUP_BYTES
                {
                    return Err(MezError::forbidden("outbound X11 setup size invalid"));
                }
                let start = setup.len();
                setup.resize(required_len, 0);
                reader
                    .read_exact(&mut setup[start..])
                    .await
                    .map_err(|_| MezError::invalid_state("outbound X11 setup packet incomplete"))?;
            }
        }
    }
}

#[cfg(test)]
mod tests;
