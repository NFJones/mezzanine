//! In-process D-Bus peer regressions for Linux inhibitor wire contracts.

use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UnixStream;
use zbus::connection::Builder as ConnectionBuilder;
use zbus::{Connection, Guid};

use super::logind::LogindInhibitRequest;
use super::screensaver::ScreenSaverInhibitRequest;
use super::{INHIBITION_REASON, NativeLinuxPowerInhibitionApi};
use crate::host::power_inhibition::PowerInhibitionLease;

const SERVER_NAME: &str = ":1.704";
const CLIENT_NAME: &str = ":1.705";
type LogindRequest = (String, String, String, String);

#[derive(Debug)]
struct FakeLogind {
    requests: Arc<Mutex<Vec<LogindRequest>>>,
    inhibitor: Mutex<Option<OwnedFd>>,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl FakeLogind {
    #[zbus(name = "Inhibit")]
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::zvariant::OwnedFd {
        self.requests.lock().unwrap().push((
            what.to_string(),
            who.to_string(),
            why.to_string(),
            mode.to_string(),
        ));
        self.inhibitor
            .lock()
            .unwrap()
            .take()
            .expect("test logind inhibitor is single-use")
            .into()
    }
}

#[derive(Debug, Default)]
struct FakeScreenSaverState {
    inhibit_requests: Vec<(String, String)>,
    uninhibit_cookies: Vec<u32>,
}

#[derive(Debug)]
struct FakeScreenSaver {
    state: Arc<Mutex<FakeScreenSaverState>>,
    cookie: u32,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl FakeScreenSaver {
    #[zbus(name = "Inhibit")]
    fn inhibit(&self, application: &str, reason: &str) -> u32 {
        self.state
            .lock()
            .unwrap()
            .inhibit_requests
            .push((application.to_string(), reason.to_string()));
        self.cookie
    }

    #[zbus(name = "UnInhibit")]
    fn uninhibit(&self, cookie: u32) {
        self.state.lock().unwrap().uninhibit_cookies.push(cookie);
    }
}

fn private_peer<I>(
    api: &NativeLinuxPowerInhibitionApi,
    path: &'static str,
    interface: I,
) -> (Connection, Connection)
where
    I: zbus::object_server::Interface,
{
    api.runtime()
        .unwrap()
        .block_on(async move {
            let guid = Guid::generate();
            let (server_socket, client_socket) = UnixStream::pair()?;
            let server = ConnectionBuilder::unix_stream(server_socket)
                .server(guid)?
                .p2p()
                .unique_name(SERVER_NAME)?
                .serve_at(path, interface)?
                .build();
            let client = ConnectionBuilder::unix_stream(client_socket)
                .p2p()
                .unique_name(CLIENT_NAME)?
                .build();
            let (server, client) = tokio::try_join!(server, client)?;
            Ok::<_, zbus::Error>((server, client))
        })
        .unwrap()
}

fn assert_inhibitor_open(peer: &mut StdUnixStream) {
    let mut byte = [0_u8; 1];
    let error = peer.read(&mut byte).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
}

fn wait_for_inhibitor_close(peer: &mut StdUnixStream) {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut byte = [0_u8; 1];
    loop {
        match peer.read(&mut byte) {
            Ok(0) => return,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "inhibitor descriptor stayed open"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            result => panic!("unexpected inhibitor peer read result: {result:?}"),
        }
    }
}

/// Verifies the production zbus call serializes the exact logind method body,
/// receives an owned Unix descriptor, and closes it only with the lease.
#[test]
fn private_logind_peer_preserves_protocol_and_fd_lifetime() {
    let api = NativeLinuxPowerInhibitionApi::new();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (inhibitor, mut peer) = StdUnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let (_server, client) = private_peer(
        &api,
        super::logind::PATH,
        FakeLogind {
            requests: Arc::clone(&requests),
            inhibitor: Mutex::new(Some(OwnedFd::from(inhibitor))),
        },
    );

    let mut lease = api
        .inhibit_logind_on(
            client,
            Some(SERVER_NAME),
            LogindInhibitRequest::for_active_turn(INHIBITION_REASON),
        )
        .unwrap();

    assert_eq!(
        *requests.lock().unwrap(),
        [(
            "idle".to_string(),
            "Mezzanine".to_string(),
            INHIBITION_REASON.to_string(),
            "block".to_string(),
        )]
    );
    assert_inhibitor_open(&mut peer);
    lease.release().unwrap();
    wait_for_inhibitor_close(&mut peer);
}

/// Verifies the production ScreenSaver call sends the exact string pair,
/// captures the issuing peer owner, and returns the exact cookie on release.
#[test]
fn private_screensaver_peer_pairs_cookie_with_issuing_owner() {
    let api = NativeLinuxPowerInhibitionApi::new();
    let state = Arc::new(Mutex::new(FakeScreenSaverState::default()));
    let (_server, client) = private_peer(
        &api,
        super::screensaver::PATH,
        FakeScreenSaver {
            state: Arc::clone(&state),
            cookie: 0x51a7,
        },
    );

    let mut lease = api
        .inhibit_screensaver_on(
            client,
            Some(SERVER_NAME),
            ScreenSaverInhibitRequest::for_active_turn(INHIBITION_REASON),
        )
        .unwrap();
    lease.release().unwrap();

    let state = state.lock().unwrap();
    assert_eq!(
        state.inhibit_requests,
        [(
            "io.mezzanine.Mez".to_string(),
            INHIBITION_REASON.to_string(),
        )]
    );
    assert_eq!(state.uninhibit_cookies, [0x51a7]);
}
