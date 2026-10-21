use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn start(
    address: &str,
    config: &str,
    action: i32,
    callback: Option<EventCallback>,
    context: *mut c_void,
) -> *mut BeachConnection {
    let mut error = [0; 1024];
    unsafe {
        let result = beach_connection_start(
            CString::new(address).unwrap().as_ptr(),
            std::ptr::null(),
            CString::new(config).unwrap().as_ptr(),
            action,
            std::ptr::null(),
            24,
            callback,
            context,
            error.as_mut_ptr(),
            error.len(),
        );
        assert!(
            !result.is_null(),
            "{}",
            CStr::from_ptr(error.as_ptr()).to_str().unwrap()
        );
        result
    }
}

fn poll(connection: *mut BeachConnection) -> serde_json::Value {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let text = unsafe { beach_connection_poll(connection) };
        if !text.is_null() {
            let event = serde_json::from_str(unsafe { CStr::from_ptr(text) }.to_str().unwrap()).unwrap();
            unsafe { beach_string_free(text) };
            return event;
        }
        assert!(Instant::now() < until, "preparation timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

unsafe extern "C" fn callback(context: *mut c_void, kind: i32, _: *const c_char) {
    if kind == EVENT_CLIPBOARD {
        unsafe { &*(context as *const AtomicUsize) }.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn wake_to_vnc_handoff_preserves_callbacks_until_session_free() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let config = serde_json::json!({"wake":{"mac":"00:11:22:33:44:55", "destination":udp.local_addr().unwrap().to_string()}}).to_string();
    let (send, receive) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        rfb::handshake::server_handshake(
            &mut stream,
            None,
            &rfb::ServerInit {
                width: 2,
                height: 2,
                pixel_format: PixelFormat::BGRX,
                name: "Mobile fixture".into(),
            },
        )
        .unwrap();
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
        stream.write_all(&[3, 0, 0, 0, 0, 0, 0, 2, b'o', b'k']).unwrap();
        while stream.read(&mut [0; 4096]).is_ok_and(|n| n > 0) {}
    });
    let count = AtomicUsize::new(0);
    let connection = start(
        &address,
        &config,
        2,
        Some(callback),
        (&count as *const AtomicUsize).cast_mut().cast(),
    );
    let mut packet = [0; 102];
    assert_eq!(udp.recv(&mut packet).unwrap(), 102);
    assert_eq!(&packet[..6], &[255; 6]);
    for chunk in packet[6..].chunks_exact(6) {
        assert_eq!(chunk, [0, 17, 34, 51, 68, 85]);
    }
    loop {
        let event = poll(connection);
        match event["kind"].as_str().unwrap() {
            "connected" => break,
            "progress" => {}
            _ => panic!("unexpected event: {event}"),
        }
    }
    let session = unsafe { beach_connection_take_session(connection) };
    assert!(!session.is_null());
    assert!(unsafe { beach_connection_take_session(connection) }.is_null());
    unsafe { beach_connection_free(connection) };
    send.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    while count.load(Ordering::SeqCst) == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "transferring a session must preserve its callback sink"
    );
    unsafe { beach_session_free(session) };
    server.join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn cancelling_preparation_closes_the_checked_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let connection = start(
        &listener.local_addr().unwrap().to_string(),
        "{}",
        0,
        None,
        std::ptr::null_mut(),
    );
    let (mut stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    unsafe { beach_connection_free(connection) };
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
}

#[test]
fn stale_or_wrong_prompt_answers_cannot_consume_the_current_question() {
    let connection = start("127.0.0.1::1", "{}", 0, None, std::ptr::null_mut());
    let (reply, answers) = std::sync::mpsc::channel();
    unsafe { &*connection }.state.lock().unwrap().pending = Some(Pending {
        id: 42,
        secret: true,
        reply,
    });
    unsafe {
        assert!(!beach_connection_answer(connection, 41, 1, c"secret".as_ptr()));
        assert!(!beach_connection_answer(connection, 42, 0, std::ptr::null()));
        assert!(beach_connection_answer(connection, 42, 1, c"secret".as_ptr()));
        assert!(!beach_connection_answer(connection, 42, 1, c"secret".as_ptr()));
        beach_connection_free(connection);
    }
    let Answer::Secret(secret) = answers.recv().unwrap() else {
        panic!("wrong answer kind")
    };
    assert_eq!(&*secret, "secret");
}
