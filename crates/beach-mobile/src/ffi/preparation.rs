//! Nonblocking preparation API. Each handle serializes polling and answers;
//! freeing a handle must not race another call using the same raw pointer.

use super::*;
use beach_access::{Access, Action, Answer, Prompt, Target, Task};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, TryRecvError};
use zeroize::Zeroizing;

/// Sends `body` (JSON) by POST to `url` with `token` as bearer token,
/// blocking. Writes the answer's body (NUL-terminated) into `answer` and
/// returns the HTTP status, or 0 with a reason in `answer` if no answer came.
pub type HttpPost = unsafe extern "C" fn(
    url: *const c_char,
    token: *const c_char,
    body: *const c_char,
    answer: *mut c_char,
    answer_len: usize,
) -> i32;

fn post_with(post: HttpPost, url: &str, token: &str, body: &str) -> crate::account::Answer {
    let (url, token, body) = (cstring(url), cstring(token), cstring(body));
    let mut answer = vec![0 as c_char; 16 * 1024];
    let status = unsafe { post(url.as_ptr(), token.as_ptr(), body.as_ptr(), answer.as_mut_ptr(), answer.len()) };
    *answer.last_mut().unwrap() = 0;
    let text = unsafe { CStr::from_ptr(answer.as_ptr()) }.to_string_lossy().into_owned();
    match u16::try_from(status) {
        Ok(status) if status > 0 => Ok((status, text)),
        _ => Err(text),
    }
}

fn cstring(text: &str) -> CString {
    CString::new(text.replace('\0', "")).unwrap()
}

pub struct BeachConnection {
    state: Mutex<Preparation>,
    sink: Sink,
}

struct Preparation {
    task: Task<BeachSession>,
    pending: Option<Pending>,
    sequence: u64,
    ready: Option<BeachSession>,
    terminal: bool,
    transferred: bool,
}

struct Pending {
    id: u64,
    secret: bool,
    reply: Sender<Answer>,
}

/// Validates serialized optional capabilities without contacting a device.
/// # Safety
/// JSON and error buffer must be valid for the supplied lengths.
#[no_mangle]
pub unsafe extern "C" fn beach_access_validate(
    json: *const c_char,
    error: *mut c_char,
    error_len: usize,
) -> bool {
    let result = unsafe { config(json) }.and_then(|config| config.validate());
    if let Err(reason) = result {
        unsafe { fill(error, error_len, &format!("{reason:#}")) };
        return false;
    }
    true
}

unsafe fn config(json: *const c_char) -> anyhow::Result<Access> {
    serde_json::from_str(unsafe { c_str(json) }.unwrap_or("{}"))
        .map_err(|_| anyhow::anyhow!("Die Einstellungen für Erreichbarkeit & Start sind ungültig."))
}

/// Starts preparation followed by the standard RFB handshake on a worker.
/// action: 0 connect, 1 SSH sign-in, 2 wake. NULL on invalid configuration.
/// With `account_json` (`{"url","token","device"}`, see
/// [`crate::account::Target`]) and `http_post`, an account ticket is
/// preferred over the VNC password.
/// # Safety
/// Strings/buffers must be valid. Context remains alive until free, or until
/// the transferred session is freed. Callbacks must not free their own handle.
#[no_mangle]
pub unsafe extern "C" fn beach_connection_start(
    address: *const c_char,
    password: *const c_char,
    access_json: *const c_char,
    account_json: *const c_char,
    http_post: Option<HttpPost>,
    action: i32,
    known_hosts_dir: *const c_char,
    depth: u8,
    callback: Option<EventCallback>,
    context: *mut c_void,
    error: *mut c_char,
    error_len: usize,
) -> *mut BeachConnection {
    let result = (|| {
        let host = unsafe { c_str(address) }
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("Bitte eine VNC-Adresse eingeben."))?
            .to_owned();
        let access = unsafe { config(access_json) }?;
        let action = match action {
            0 => Action::Connect,
            1 => Action::SignIn,
            2 => Action::Wake,
            _ => anyhow::bail!("Unbekannte Verbindungsaktion."),
        };
        // iOS broadcast networking needs a restricted Apple entitlement.
        // The app sends via an SSH relay instead, including on the home LAN.
        #[cfg(target_os = "ios")]
        if matches!(action, Action::Wake) && access.wake.as_ref().is_some_and(|wake| wake.relays.is_empty()) {
            anyhow::bail!(
                "Zum Aufwecken auf iPhone und iPad ein Zugangsgerät im LAN des Computers einrichten."
            );
        }
        let account = match (unsafe { c_str(account_json) }.filter(|s| !s.is_empty()), http_post) {
            (Some(json), Some(post)) => {
                let target: crate::account::Target = serde_json::from_str(json)
                    .map_err(|_| anyhow::anyhow!("Die Kontodaten für die Verbindung sind ungültig."))?;
                Some((target, post))
            }
            (None, _) => None,
            (Some(_), None) => anyhow::bail!("Für Kontogeräte fehlt der HTTP-Zugang."),
        };
        let known_hosts = unsafe { c_str(known_hosts_dir) }
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let password = Zeroizing::new(unsafe { c_str(password) }.unwrap_or("").to_owned());
        let pf = match depth {
            8 => PixelFormat::BGR233,
            16 => PixelFormat::RGB565,
            _ => PixelFormat::BGRX,
        };
        let sink: Sink = Arc::new(Mutex::new(callback.map(|cb| (cb, Context(context)))));
        let events = sink.clone();
        let task = Task::start(
            Target { host, access },
            action,
            known_hosts,
            move |stream| {
                let notifications = events.clone();
                let mut tickets = |device: &str, nonce: &[u8; rfb::handshake::TICKET_NONCE_LEN]| match &account {
                    Some((target, post)) => {
                        let post = *post;
                        crate::account::ticket(target, &|url, token, body| post_with(post, url, token, body), device, nonce)
                    }
                    None => Err("not signed in".to_string()),
                };
                let source: Option<rfb::handshake::TicketSource> = match account {
                    Some(_) => Some(&mut tickets),
                    None => None,
                };
                let conn = Connection::from_stream(
                    stream,
                    (!password.is_empty()).then_some(password.as_str()),
                    source,
                    pf,
                    move |event| emit(&notifications, event),
                )
                .map_err(beach_access::vnc_error)?;
                Ok(BeachSession { conn, sink: events })
            },
            || {},
        );
        Ok::<_, anyhow::Error>(BeachConnection {
            state: Mutex::new(Preparation {
                task,
                pending: None,
                sequence: 0,
                ready: None,
                terminal: false,
                transferred: false,
            }),
            sink,
        })
    })();
    match result {
        Ok(connection) => Box::into_raw(Box::new(connection)),
        Err(reason) => {
            unsafe { fill(error, error_len, &format!("{reason:#}")) };
            std::ptr::null_mut()
        }
    }
}

/// Returns one owned UTF-8 JSON event or NULL when nothing is waiting.
/// Events: progress{text}, trust{id,address,fingerprint},
/// secret{id,title,label,echo}, connected, failed{message}.
/// # Safety
/// Handle must be live. Release returned text with beach_string_free.
#[no_mangle]
pub unsafe extern "C" fn beach_connection_poll(connection: *mut BeachConnection) -> *mut c_char {
    let Some(connection) = (unsafe { connection.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let mut state = connection.state.lock().unwrap();
    if state.terminal {
        return std::ptr::null_mut();
    }
    let event = match state.task.events.try_recv() {
        Ok(beach_access::Event::Progress(text)) => serde_json::json!({"kind":"progress", "text":text}),
        Ok(beach_access::Event::Prompt(prompt, reply)) => {
            state.sequence += 1;
            let id = state.sequence;
            let secret = matches!(prompt, Prompt::Secret { .. });
            state.pending = Some(Pending { id, secret, reply });
            match prompt {
                Prompt::Trust { address, fingerprint } => {
                    serde_json::json!({"kind":"trust", "id":id, "address":address, "fingerprint":fingerprint})
                }
                Prompt::Secret { title, label, echo } => {
                    serde_json::json!({"kind":"secret", "id":id, "title":title, "label":label, "echo":echo})
                }
            }
        }
        Err(_) => match state.task.result.try_recv() {
            Ok(Ok(session)) => {
                state.ready = Some(session);
                state.terminal = true;
                serde_json::json!({"kind":"connected"})
            }
            result => {
                let message = match result {
                    Ok(Err(message)) => message,
                    Err(TryRecvError::Disconnected) => "Der Verbindungsversuch wurde beendet.".into(),
                    Err(TryRecvError::Empty) => return std::ptr::null_mut(),
                    Ok(Ok(_)) => unreachable!(),
                };
                state.terminal = true;
                serde_json::json!({"kind":"failed", "message":message})
            }
        },
    };
    CString::new(event.to_string()).unwrap().into_raw()
}

/// Answers only the matching outstanding prompt. kind: 0 trust, 1 secret.
/// # Safety
/// Handle and optional UTF-8 value must be valid; value is copied immediately.
#[no_mangle]
pub unsafe extern "C" fn beach_connection_answer(
    connection: *mut BeachConnection,
    id: u64,
    kind: i32,
    value: *const c_char,
) -> bool {
    let Some(connection) = (unsafe { connection.as_ref() }) else {
        return false;
    };
    let mut state = connection.state.lock().unwrap();
    if state.terminal
        || !state
            .pending
            .as_ref()
            .is_some_and(|p| p.id == id && kind == i32::from(p.secret))
    {
        return false;
    }
    let answer = if kind == 0 {
        Answer::Trust
    } else {
        let Some(value) = (unsafe { c_str(value) }) else {
            return false;
        };
        Answer::Secret(Zeroizing::new(value.to_owned()))
    };
    state.pending.take().unwrap().reply.send(answer).is_ok()
}

/// Transfers the authenticated session exactly once after a connected event.
/// # Safety
/// Handle must be live. The caller owns the returned session.
#[no_mangle]
pub unsafe extern "C" fn beach_connection_take_session(
    connection: *mut BeachConnection,
) -> *mut BeachSession {
    let Some(connection) = (unsafe { connection.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let mut state = connection.state.lock().unwrap();
    let Some(session) = state.ready.take() else {
        return std::ptr::null_mut();
    };
    state.transferred = true;
    Box::into_raw(Box::new(session))
}

/// Cancels promptly and frees the handle. No callbacks run afterwards unless
/// ownership was transferred to a session, whose lifetime governs callbacks.
/// # Safety
/// Handle must come from start, have no concurrent callers and never be reused.
#[no_mangle]
pub unsafe extern "C" fn beach_connection_free(connection: *mut BeachConnection) {
    if connection.is_null() {
        return;
    }
    let connection = unsafe { Box::from_raw(connection) };
    if !connection.state.lock().unwrap().transferred {
        *connection.sink.lock().unwrap() = None;
    }
    drop(connection);
}

/// Releases an owned event string.
/// # Safety
/// Text must come from poll and must not have been freed before.
#[no_mangle]
pub unsafe extern "C" fn beach_string_free(text: *mut c_char) {
    if !text.is_null() {
        drop(unsafe { CString::from_raw(text) });
    }
}

#[cfg(test)]
mod tests;
