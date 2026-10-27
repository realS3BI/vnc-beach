//! Desktop adapter for the shared connection preparation.

use crate::client::Connection;
use crate::servers::Server;
pub use beach_access::{Access, Action, Answer, Authentication, Event, Prompt, Relay, Shell, Ssh, Wake};

pub type Task = beach_access::Task<Connection>;

/// Connects to `server`; with `account`, through an account ticket (the
/// password, if any, stays the fallback for servers without one).
pub fn start(
    server: Server,
    account: Option<crate::account::Target>,
    action: Action,
    pf: rfb::PixelFormat,
    repaint: impl Fn() + Send + Sync + 'static,
) -> Task {
    let repaint = std::sync::Arc::new(repaint);
    let notify = repaint.clone();
    Task::start(
        beach_access::Target {
            host: server.host,
            access: server.access,
        },
        action,
        crate::servers::config_dir().map(|dir| dir.join("ssh_hosts")),
        move |stream| {
            Connection::from_stream(
                stream,
                (!server.password.is_empty()).then_some(server.password.as_str()),
                account.as_ref(),
                pf,
                move || repaint(),
            )
            .map_err(beach_access::vnc_error)
        },
        move || notify(),
    )
}
