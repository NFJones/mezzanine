//! Two ordinary X11 CLI terminals with generated fixture credentials and real hosts.
//!
//! A synthetic xauth executable produces a distinct private test credential;
//! a synthetic local TCP X peer verifies local substitution. The real CLI owns
//! preparation, foreground, dedicated channel supervision and explicit cleanup.
//! Real host proxies and QUIC route authority are used. No physical X server,
//! provider or desktop clipboard is touched. Every process stays fixture-owned.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Encodes a fixed loopback Xauthority selector for the local fixture display.
fn authority(display: &str, cookie: u8) -> Vec<u8> {
    let number = display.strip_prefix("127.0.0.1:").unwrap().as_bytes();
    let mut bytes = 0_u16.to_be_bytes().to_vec();
    for field in [
        &[127_u8, 0, 0, 1][..],
        number,
        b"MIT-MAGIC-COOKIE-1",
        &[cookie; 16],
    ] {
        bytes.extend_from_slice(&(field.len() as u16).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes
}

/// Reports the actual hosted pane proxy using one harmless shell command. This
/// is fixture-only environment evidence, never a broker-supplied local X target.
async fn proxy(terminal: &mut CliTerminal, root: &Path, name: &str) -> Result<(u16, Vec<u8>)> {
    let path = root.join(format!("{name}.report"));
    terminal.input(
        format!(
            "printf '%s\\n%s\\n' \"$DISPLAY\" \"$XAUTHORITY\" > {}\n",
            mez_agent::shell_quote(path.to_str().unwrap())
        )
        .as_bytes(),
    )?;
    let report = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let lines: Vec<_> = text.lines().collect();
                if lines.len() == 2 && lines[0].starts_with("127.0.0.1:") {
                    break (lines[0].to_string(), PathBuf::from(lines[1]));
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("fixture proxy report timed out"))?;
    assert!(report.1.starts_with(root.join("host/x11-sessions")));
    let auth = std::fs::read(&report.1)?;
    let cookie = auth[auth.len().checked_sub(16).unwrap()..].to_vec();
    let display: u16 = report
        .0
        .strip_prefix("127.0.0.1:")
        .unwrap()
        .strip_suffix(".0")
        .unwrap()
        .parse()
        .unwrap();
    Ok((6000_u16.checked_add(display).unwrap(), cookie))
}

/// Delivers one actual proxy application through the real CLI supervisor. The
/// local peer receives only its generated credential and exact application bytes;
/// reverse data survives directional half-close to the hosted application.
async fn application(
    local: &tokio::net::TcpListener,
    proxy: &(u16, Vec<u8>),
    label: &[u8],
) -> Result<()> {
    let mut setup = vec![0; 48];
    setup[0] = b'l';
    setup[2..4].copy_from_slice(&11_u16.to_le_bytes());
    setup[6..8].copy_from_slice(&18_u16.to_le_bytes());
    setup[8..10].copy_from_slice(&16_u16.to_le_bytes());
    setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
    setup[32..48].copy_from_slice(&proxy.1);
    let remote = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", proxy.0))
            .await
            .unwrap();
        stream
            .write_all(&[setup, label.to_vec()].concat())
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, [b"reply:".as_slice(), label].concat());
    };
    let peer = async {
        let (mut stream, _) = local.accept().await.unwrap();
        let mut setup = [0; 48];
        stream.read_exact(&mut setup).await.unwrap();
        crate::runtime::x11::validate_x11_setup_cookie(
            &setup,
            &crate::runtime::x11::X11Cookie::new([52; 16]),
        )
        .unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, label);
        stream
            .write_all(&[b"reply:".as_slice(), label].concat())
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(Box::pin(remote), Box::pin(peer));
    })
    .await
    .map_err(|_| MezError::invalid_state("fixture X application timed out"))?;
    Ok(())
}

/// Opens and fences an established application without either directional FIN.
/// The returned socket pair permits actual overlapping relay cancellation and
/// sibling byte transfer, without relying on timing or queued application input.
async fn active_application(
    local: &tokio::net::TcpListener,
    proxy: &(u16, Vec<u8>),
    label: &[u8],
) -> Result<(tokio::net::TcpStream, tokio::net::TcpStream)> {
    let mut setup = vec![0; 48];
    setup[0] = b'l';
    setup[2..4].copy_from_slice(&11_u16.to_le_bytes());
    setup[6..8].copy_from_slice(&18_u16.to_le_bytes());
    setup[8..10].copy_from_slice(&16_u16.to_le_bytes());
    setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
    setup[32..48].copy_from_slice(&proxy.1);
    let remote = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", proxy.0))
            .await
            .unwrap();
        stream
            .write_all(&[setup, label.to_vec()].concat())
            .await
            .unwrap();
        stream
    };
    let peer = async {
        let (mut stream, _) = local.accept().await.unwrap();
        let mut setup = [0; 48];
        stream.read_exact(&mut setup).await.unwrap();
        crate::runtime::x11::validate_x11_setup_cookie(
            &setup,
            &crate::runtime::x11::X11Cookie::new([52; 16]),
        )
        .unwrap();
        let mut bytes = vec![0; label.len()];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(bytes, label);
        stream
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(Box::pin(remote), Box::pin(peer))
    })
    .await
    .map_err(|_| MezError::invalid_state("fixture active X application timed out"))
}

/// Two actual ordinary new frontends must each forward through their own host
/// route with generated client credentials. First SIGINT retires its channel
/// owners and private artifacts while the second continues input and X traffic.
/// The broker stays live and exclusive, and committed sessions survive. Fixture
/// xauth is not proof of physical X SECURITY issuance or server-side revocation.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for combined X11 process qualification"]
async fn broker_attach_two_cli_x11_terminals_retain_sibling_and_clean_credentials() {
    let executable = PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("trusted fixture binary required"),
    );
    assert!(executable.is_absolute());
    let root = std::env::temp_dir().join(format!("mez-xt-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("home");
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let runtime = root.join("cli-runtime");
    std::fs::create_dir(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = crate::runtime::RuntimeIrohTransportPolicy {
        compression_codecs: vec![crate::runtime::RuntimeIrohCompressionCodec::None],
        x11: crate::runtime::RuntimeIrohX11Policy {
            enabled: true,
            allow_trusted: true,
            max_connections_per_route: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    std::fs::write(paths.default_primary_file(), format!("version = {}\n[transport.iroh]\ncompression_codecs = [\"none\"]\n[transport.iroh.x11]\nenabled = true\nallow_trusted = true\nmax_connections_per_route = 2\n", crate::config::CURRENT_CONFIG_SCHEMA_VERSION)).unwrap();
    let local = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let display = format!(
        "127.0.0.1:{}",
        local
            .local_addr()
            .unwrap()
            .port()
            .checked_sub(6000)
            .unwrap()
    );
    let source = root.join("source");
    let generated = root.join("generated");
    std::fs::write(&source, authority(&display, 17)).unwrap();
    std::fs::write(&generated, authority(&display, 52)).unwrap();
    for file in [&source, &generated] {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = root.join("generated-paths");
    let helper = bin.join("xauth");
    std::fs::write(&helper, format!("#!/bin/sh\nif [ \"$5\" = generate ]; then /bin/cp {} \"$4\"; printf '%s\\n' \"$4\" >> {}; exit 0; fi\nif [ \"$5\" = remove ]; then : > \"$4\"; exit 0; fi\nexit 2\n", mez_agent::shell_quote(generated.to_str().unwrap()), mez_agent::shell_quote(log.to_str().unwrap()))).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host_root = root.join("host");
    let host = HostIrohRuntime::bind(
        &host_root,
        crate::runtime::RuntimeIrohTransportPolicy {
            enabled: true,
            ..policy.clone()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let identity = RemoteClientIdentity::load_or_create(paths.root()).unwrap();
    let endpoint_id = identity.endpoint_id().to_string();
    drop(identity);
    let trust = RemoteTrustStore::under_host_config_root(&host_root).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invitation = trust
        .create_host_invitation(
            host.endpoint_id(),
            RemoteRoleCeiling::Primary,
            RemoteHostRoutingAuthority {
                session_create: true,
                session_list: true,
                session_kill: false,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 2,
                max_live_sessions: 2,
                lease_lifetime_ceiling_seconds: None,
            },
            600,
            now,
        )
        .unwrap();
    let redemption = trust
        .redeem_invitation(
            &invitation.token,
            host.endpoint_id(),
            &endpoint_id,
            "fixture",
            crate::control::RequestedRole::Primary,
            now,
        )
        .unwrap();
    RemoteClientProfileStore::under_config_root(paths.root())
        .save(&RemoteClientProfile {
            name: "fixture".into(),
            server_addr: host.endpoint_addr().unwrap(),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: redemption.device_credential,
        })
        .unwrap();
    let router = HostSessionRouter::new(HostSessionRouterConfig {
        runtime_root: root.join("host-runtime"), owner_uid: crate::runtime::current_effective_uid(), config_root: host_root.clone(),
        config_layers: vec![crate::config::ConfigLayer { name:"fixture-x11".into(), path:None, format:crate::config::ConfigFormat::Toml,
            scope:crate::config::ConfigScope::Primary, trusted:true, text:"[transport.iroh.x11]\nenabled=true\nallow_trusted=true\nmax_connections_per_route=2\n".into() }],
        shell:ResolvedShell::new(PathBuf::from("/bin/sh"),ShellSource::FallbackBinSh), max_sessions:2,max_live_sessions:2,
        default_session_policy:HostDefaultSessionPolicy::MostRecentAttachable,default_lease_lifetime_seconds:0,
    });
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stopped = stop.clone();
    let mut child = None;
    let work = async {
        drop(
            crate::cli::remote::broker::launch::connect_owned(
                &executable,
                &env,
                policy.setup_timeout,
                &mut child,
            )
            .await?,
        );
        let mut first = CliTerminal::spawn_with_x11(
            &executable,
            &home,
            &runtime,
            "x-first",
            Some((&display, &source, &bin)),
        )?;
        first.wait_text("□").await?;
        let first_proxy = proxy(&mut first, &root, "first").await?;
        application(&local, &first_proxy, b"FIRST-X11").await?;
        let mut second = CliTerminal::spawn_with_x11(
            &executable,
            &home,
            &runtime,
            "x-second",
            Some((&display, &source, &bin)),
        )?;
        second.wait_text("□").await?;
        let second_proxy = proxy(&mut second, &root, "second").await?;
        assert_ne!(first_proxy.0, second_proxy.0);
        assert_ne!(first_proxy.1, second_proxy.1);
        application(&local, &second_proxy, b"SECOND-X11").await?;
        let private: Vec<PathBuf> = std::fs::read_to_string(&log)?
            .lines()
            .map(PathBuf::from)
            .collect();
        assert_eq!(private.len(), 2);
        assert!(private.iter().all(|path| path.exists()));
        assert!(first.child.try_wait()?.is_none());
        assert!(second.child.try_wait()?.is_none());
        let (first_app, mut first_local) =
            active_application(&local, &first_proxy, b"FIRST-ACTIVE").await?;
        let (mut second_app, mut second_local) =
            active_application(&local, &second_proxy, b"SECOND-ACTIVE").await?;
        first.interrupt_and_reap().await?;
        let mut retired = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(5),
            first_local.read_to_end(&mut retired),
        )
        .await
        .map_err(|_| {
            MezError::invalid_state("retired CLI must close established local X relay")
        })??;
        assert!(retired.is_empty());
        drop((first_app, first_local));
        assert!(!private[0].parent().unwrap().exists());
        assert!(private[1].exists());
        second_app.write_all(b"active-sibling").await?;
        let mut alive = [0; 14];
        tokio::time::timeout(Duration::from_secs(5), second_local.read_exact(&mut alive))
            .await
            .map_err(|_| MezError::invalid_state("active sibling relay lost input"))??;
        assert_eq!(&alive, b"active-sibling");
        second_local.write_all(b"still-alive").await?;
        let mut response = [0; 11];
        tokio::time::timeout(Duration::from_secs(5), second_app.read_exact(&mut response))
            .await
            .map_err(|_| MezError::invalid_state("active sibling relay lost response"))??;
        assert_eq!(&response, b"still-alive");
        drop((second_app, second_local));
        second.input(b"printf 'X-SIBLING-%s\\n' 'LIVE'\n")?;
        second.wait_text("X-SIBLING-LIVE").await?;
        application(&local, &second_proxy, b"SURVIVING-X11").await?;
        assert!(child.as_mut().unwrap().try_wait()?.is_none());
        second.interrupt_and_reap().await?;
        assert!(!private[1].parent().unwrap().exists());
        assert_eq!(router.snapshots().await?.len(), 2);
        assert!(RemoteClientIdentity::load_or_create(paths.root()).is_err());
        Ok::<(), MezError>(())
    };
    let work = async {
        let result = tokio::time::timeout(Duration::from_secs(60), Box::pin(work)).await;
        stop.notify_one();
        result
    };
    let (served, result) = tokio::join!(
        Box::pin(host.serve_routed(router.clone(), async move { stopped.notified().await })),
        Box::pin(work)
    );
    let shutdown = if let Some(child) = child.as_mut() {
        tokio::time::timeout(Duration::from_secs(10), child.shutdown_for_tests()).await
    } else {
        Ok(Err(MezError::invalid_state("fixture child missing")))
    };
    if let Some(child) = child.as_mut()
        && child.try_wait().unwrap().is_none()
    {
        tokio::time::timeout(Duration::from_secs(5), child.terminate_for_tests())
            .await
            .unwrap()
            .unwrap();
    }
    router
        .shutdown_all(true, Duration::from_secs(5))
        .await
        .unwrap();
    served.unwrap();
    result.unwrap().unwrap();
    assert!(shutdown.unwrap().unwrap().success());
    assert!(!paths.root().join("outbound.sock").exists());
    assert_eq!(std::fs::read(&source).unwrap(), authority(&display, 17));
    let identity = RemoteClientIdentity::load_or_create(paths.root()).unwrap();
    assert_eq!(identity.endpoint_id().to_string(), endpoint_id);
    drop(identity);
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
