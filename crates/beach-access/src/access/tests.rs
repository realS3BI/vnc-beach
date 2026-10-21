use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};

fn worker() -> (Worker, Receiver<Event>) {
    let (tx, rx) = channel();
    (
        Worker {
            tx,
            cancel: Arc::new(Cancel::default()),
            repaint: Arc::new(|| {}),
            known_hosts: None,
        },
        rx,
    )
}

/// The fixture only speaks standard RFB and never captures a desktop.
fn vnc(password: Option<&'static str>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let init = rfb::ServerInit {
            width: 2,
            height: 2,
            pixel_format: rfb::PixelFormat::BGRX,
            name: "Standard VNC fixture".into(),
        };
        if rfb::handshake::server_handshake(&mut stream, password, &init).is_ok() {
            let mut bytes = [0; 1024];
            while matches!(stream.read(&mut bytes), Ok(n) if n > 0) {}
        }
    });
    (address, server)
}

#[test]
fn connection_uses_fallback_and_preserves_the_checked_socket() {
    let (address, fixture) = vnc(None);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let bad = listener.local_addr().unwrap().to_string();
    let not_vnc = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"SSH-2.0-test").unwrap();
    });
    let server = Target {
        host: bad,
        access: Access {
            alternatives: vec![address.clone()],
            ..Default::default()
        },
    };
    let task = Task::start(
        server,
        Action::Connect,
        None,
        |mut stream| {
            let init = rfb::handshake::client_handshake(&mut stream, None)?;
            Ok((stream, init.name))
        },
        || {},
    );
    let (mut stream, name) = task
        .result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(name, "Standard VNC fixture");
    drop(task);
    // Completing the workflow must not cancel the socket now owned by VNC.
    stream.write_all(&[0]).unwrap();
    drop(stream);
    fixture.join().unwrap();
    not_vnc.join().unwrap();
}

#[test]
fn wrong_vnc_password_does_not_try_another_server() {
    let (address, fixture) = vnc(Some("correct"));
    let fallback = TcpListener::bind("127.0.0.1:0").unwrap();
    fallback.set_nonblocking(true).unwrap();
    let server = Target {
        host: address,
        access: Access {
            alternatives: vec![fallback.local_addr().unwrap().to_string()],
            ..Default::default()
        },
    };
    let task = Task::start(
        server,
        Action::Connect,
        None,
        |mut stream| rfb::handshake::client_handshake(&mut stream, Some("wrong")).map_err(vnc_error),
        || {},
    );
    let error = task
        .result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap_err();
    assert!(error.contains("VNC-Anmeldung"), "{error}");
    assert_eq!(fallback.accept().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    fixture.join().unwrap();
}

#[test]
fn ordinary_vnc_does_not_require_optional_ssh_or_wake_configuration() {
    let (address, fixture) = vnc(None);
    let server = Target {
        host: address,
        access: Access {
            ssh: Some(Ssh::default()),
            wake: Some(Wake::default()),
            ..Default::default()
        },
    };
    let (worker, _) = worker();
    let mut connection = worker.run(&server, Action::Connect).unwrap();
    rfb::handshake::client_handshake(&mut connection, None).unwrap();
    drop(connection);
    drop(worker);
    fixture.join().unwrap();
}

#[test]
fn cancelled_prompt_cannot_produce_a_password_or_trust_response() {
    let (worker, events) = worker();
    let cancel = worker.cancel.clone();
    let waiting = std::thread::spawn(move || {
        worker.ask(Prompt::Secret {
            title: "fixture".into(),
            label: "Password".into(),
            echo: false,
        })
    });
    let Event::Prompt(_, answer) = events.recv_timeout(Duration::from_secs(2)).unwrap() else {
        panic!("missing prompt");
    };
    cancel.stopped.store(true, Ordering::Release);
    let _ = answer.send(Answer::Secret(Zeroizing::new("not submitted".into())));
    assert!(waiting.join().unwrap().is_err());
}

#[test]
fn cancelling_interrupts_a_stalled_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let task = Task::start(
        Target {
            host: listener.local_addr().unwrap().to_string(),
            ..Default::default()
        },
        Action::Connect,
        None,
        Ok,
        || {},
    );
    let (mut stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    drop(task);
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn wake_sends_the_packet_from_the_selected_network() {
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let config = Wake {
        mac: "00:11:22:33:44:55".into(),
        destination: receiver.local_addr().unwrap().to_string(),
        relays: vec![],
    };
    let (worker, _) = worker();
    wake::send(&worker, &config).unwrap();
    let mut bytes = [0; 200];
    let count = receiver.recv(&mut bytes).unwrap();
    assert_eq!(&bytes[..count], wake::packet(&config.mac).unwrap());
}

#[test]
fn readiness_wait_is_bounded_even_when_a_port_never_sends_a_banner() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = Target {
        host: listener.local_addr().unwrap().to_string(),
        ..Default::default()
    };
    let fixture = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let _ = socket.read(&mut [0; 1]);
    });
    let (worker, _) = worker();
    let started = Instant::now();
    assert!(worker.wait_for_vnc(&server, Duration::from_millis(50)).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    worker.cancel.release_socket();
    fixture.join().unwrap();
}

#[test]
fn fragmented_vnc_banner_is_checked_without_consuming_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let fixture = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        for chunk in [b"RFB ".as_slice(), b"003", b".008\n"] {
            socket.write_all(chunk).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let (worker, _) = worker();
    let mut socket = worker.vnc_socket(&address, Duration::from_secs(2)).unwrap();
    let mut banner = [0; 12];
    socket.read_exact(&mut banner).unwrap();
    assert_eq!(&banner, b"RFB 003.008\n");
    fixture.join().unwrap();
}

#[test]
fn routes_are_ordered_and_deduplicated_without_assuming_an_os() {
    let server = Target {
        host: "device:1".into(),
        access: Access {
            alternatives: vec![
                "device::5901".into(),
                "  ".into(),
                "192.168.1.9::5900".into(),
                "[fd00::9]:5900".into(),
            ],
            ..Default::default()
        },
    };
    assert_eq!(
        addresses(&server),
        ["device:1", "192.168.1.9::5900", "[fd00::9]:5900"]
    );
    assert_eq!(endpoint("ssh-host:1", Some(22)).unwrap(), ("ssh-host".into(), 1));
    assert_eq!(endpoint("fd00::9", Some(22)).unwrap(), ("fd00::9".into(), 22));
    assert_eq!(
        endpoint("[fd00::9]:2222", Some(22)).unwrap(),
        ("fd00::9".into(), 2222)
    );
    for bad in [
        "",
        "ssh user@host",
        "user@host",
        "host:0",
        "host:65536",
        "host/command",
        "[invalid]:22",
    ] {
        assert!(endpoint(bad, Some(22)).is_err(), "{bad}");
    }
}

/// Run through `python3 tools/test-access-ssh.py`: it creates a private,
/// loopback-only OpenSSH server and ephemeral keys, never a desktop session.
#[test]
#[ignore = "requires the disposable OpenSSH fixture from tools/test-access-ssh.py"]
fn openssh_identity_authentication_and_relay() {
    let address = std::env::var("VNC_BEACH_TEST_SSH_ADDRESS").unwrap();
    let user = std::env::var("VNC_BEACH_TEST_SSH_USER").unwrap();
    let key = std::env::var("VNC_BEACH_TEST_SSH_KEY").unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let hosts = temporary.path().join("hosts");
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let (mut worker, events) = worker();
    worker.known_hosts = Some(hosts.clone());
    let ssh = Ssh {
        address: address.clone(),
        user,
        auth: Authentication::Key { path: key },
    };
    let offline = TcpListener::bind("127.0.0.1:0").unwrap();
    let offline_address = offline.local_addr().unwrap().to_string();
    drop(offline);
    let config = Wake {
        mac: "00:11:22:33:44:55".into(),
        destination: receiver.local_addr().unwrap().to_string(),
        relays: vec![
            Relay {
                ssh: Ssh {
                    address: offline_address,
                    ..ssh.clone()
                },
                shell: Shell::Posix,
            },
            Relay {
                ssh: ssh.clone(),
                shell: Shell::Posix,
            },
        ],
    };
    let remote = std::thread::spawn(move || {
        let session = ssh::login(&worker, &ssh, false).unwrap().unwrap();
        session
            .disconnect(None, "Fixture authentication complete", None)
            .unwrap();
        wake::send(&worker, &config).unwrap();
    });
    let mut trusted = 0;
    while !remote.is_finished() {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Prompt(Prompt::Trust { fingerprint, .. }, reply)) => {
                assert!(fingerprint.starts_with("SHA256:"));
                trusted += 1;
                reply.send(Answer::Trust).unwrap();
            }
            Ok(Event::Prompt(Prompt::Secret { .. }, _)) => {
                panic!("the fixture's unencrypted key must not prompt for a password")
            }
            _ => {}
        }
    }
    remote.join().unwrap();
    assert_eq!(trusted, 1, "the second connection must reuse the pinned identity");
    let mut bytes = [0; 102];
    assert_eq!(receiver.recv(&mut bytes).unwrap(), 102);
    assert_eq!(bytes, wake::packet("00:11:22:33:44:55").unwrap());

    let files: Vec<_> = std::fs::read_dir(&hosts)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    // Replace only the disposable fixture's pin. A changed identity must stop
    // before asking for a password or offering to trust a replacement.
    let (host, port) = endpoint(&address, Some(22)).unwrap();
    std::fs::write(&files[0], format!("[{host}]:{port} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n")).unwrap();
    let (mut worker, events) = super::tests::worker();
    worker.known_hosts = Some(hosts);
    let config = Ssh {
        address,
        user: "fixture".into(),
        auth: Authentication::Password,
    };
    let error = ssh::login(&worker, &config, false).err().unwrap().to_string();
    assert!(error.contains("geändert"), "{error}");
    assert!(!events.try_iter().any(|event| matches!(event, Event::Prompt(..))));
}

/// Password, multi-prompt keyboard-interactive, and a server disconnect during
/// authentication. Uses `tools/test-access-auth.py`, never OS account passwords.
#[test]
#[ignore = "requires the disposable SSH fixture from tools/test-access-auth.py"]
fn password_interactive_and_disconnect_authentication() {
    fn attempt(
        hosts: &std::path::Path,
        address: String,
        password: &'static str,
        allow_disconnect: bool,
    ) -> (Result<bool, String>, usize) {
        let (mut worker, events) = worker();
        worker.known_hosts = Some(hosts.to_owned());
        let remote = std::thread::spawn(move || {
            ssh::login(
                &worker,
                &Ssh {
                    address,
                    user: "fixture".into(),
                    auth: Authentication::Password,
                },
                allow_disconnect,
            )
            .map(|session| session.is_some())
            .map_err(|error| format!("{error:#}"))
        });
        let mut prompts = 0;
        while !remote.is_finished() {
            match events.recv_timeout(Duration::from_millis(100)) {
                Ok(Event::Prompt(Prompt::Trust { .. }, reply)) => {
                    reply.send(Answer::Trust).unwrap();
                }
                Ok(Event::Prompt(Prompt::Secret { label, .. }, reply)) => {
                    prompts += 1;
                    let value = if label.contains("Code") {
                        "123456"
                    } else {
                        password
                    };
                    reply.send(Answer::Secret(Zeroizing::new(value.into()))).unwrap();
                }
                _ => {}
            }
        }
        (remote.join().unwrap(), prompts)
    }
    let temporary = tempfile::tempdir().unwrap();
    let password_address = std::env::var("VNC_BEACH_TEST_PASSWORD_ADDRESS").unwrap();
    let interactive = std::env::var("VNC_BEACH_TEST_INTERACTIVE_ADDRESS").unwrap();
    let disconnect = std::env::var("VNC_BEACH_TEST_DISCONNECT_ADDRESS").unwrap();
    let (result, prompts) = attempt(
        temporary.path(),
        password_address.clone(),
        "fixture-password",
        false,
    );
    assert!(result.unwrap());
    assert_eq!(prompts, 1);
    let (result, prompts) = attempt(temporary.path(), password_address, "wrong-fixture-password", true);
    assert!(result.unwrap_err().contains("SSH-Anmeldung fehlgeschlagen"));
    assert_eq!(prompts, 1, "a rejected password is not submitted again");
    let (result, prompts) = attempt(temporary.path(), interactive, "fixture-password", false);
    assert!(result.unwrap());
    assert_eq!(
        prompts, 2,
        "each keyboard-interactive challenge receives a separate answer"
    );
    let (result, prompts) = attempt(temporary.path(), disconnect, "fixture-password", true);
    assert!(
        !result.unwrap(),
        "disconnecting during authentication does not confirm a sign-in"
    );
    assert_eq!(prompts, 1);
}
