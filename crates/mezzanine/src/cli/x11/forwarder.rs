//! Client-local X server dialing and setup-cookie rewrite.
//!
//! The target is resolved before Iroh setup and cannot be supplied by the
//! server or stream. Each stream independently connects with a bounded
//! deadline, validates the fake setup credential, and substitutes the real
//! client-local credential without changing packet length or byte order.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{MezError, Result};
use crate::runtime::x11::{X11Cookie, X11SetupPacket, rewrite_x11_setup_cookie};

use super::display::{ResolvedX11Display, X11LocalTarget};

/// Erased local X socket used by the stream relay phase.
pub(crate) trait X11LocalIo: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> X11LocalIo for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

/// Boxed Unix or TCP stream connected only to the frozen local target.
pub(crate) type X11LocalStream = Box<dyn X11LocalIo>;

/// Connects one validated stream only to the pre-resolved local X endpoint.
pub(crate) async fn connect_local_x11(
    display: &ResolvedX11Display,
    timeout: Duration,
) -> Result<X11LocalStream> {
    match display.target() {
        X11LocalTarget::Unix(path) => {
            tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path))
                .await
                .map_err(|_| MezError::invalid_state("local X11 socket connection timed out"))?
                .map(|stream| Box::new(stream) as X11LocalStream)
                .map_err(|error| {
                    MezError::invalid_state(format!("local X11 socket connection failed: {error}"))
                })
        }
        X11LocalTarget::Tcp(address) => {
            tokio::time::timeout(timeout, tokio::net::TcpStream::connect(address))
                .await
                .map_err(|_| MezError::invalid_state("local X11 socket connection timed out"))?
                .map(|stream| Box::new(stream) as X11LocalStream)
                .map_err(|error| {
                    MezError::invalid_state(format!("local X11 socket connection failed: {error}"))
                })
        }
    }
}

/// Validates and length-preservingly rewrites one setup packet for the local X
/// server.
pub(crate) fn rewrite_local_x11_setup(
    setup: &mut [u8],
    fake_cookie: &X11Cookie,
    real_cookie: &X11Cookie,
) -> Result<X11SetupPacket> {
    rewrite_x11_setup_cookie(setup, fake_cookie, real_cookie)
        .map_err(|error| MezError::forbidden(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::x11::display::resolve_local_x11_display;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Numeric loopback DISPLAY resolution must dial only the frozen endpoint
    /// and provide a usable bidirectional stream.
    #[tokio::test]
    async fn connects_only_to_frozen_loopback_tcp_target() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port >= 6000);
        let display = resolve_local_x11_display(&format!("127.0.0.1:{}", port - 6000)).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 4];
            stream.read_exact(&mut bytes).await.unwrap();
            bytes
        });

        let mut stream = connect_local_x11(&display, Duration::from_secs(2))
            .await
            .unwrap();
        stream.write_all(b"ping").await.unwrap();
        assert_eq!(server.await.unwrap(), *b"ping");
    }
}
