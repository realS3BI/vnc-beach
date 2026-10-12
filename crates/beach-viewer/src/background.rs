//! `vnc-beach --background`: the tray icon process that runs the server.
//! It answers the viewer over [`crate::control`] and starts the viewer
//! window on a click.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use beach_server::{Options, Server};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};

use crate::config::{self, Config};
use crate::control::{self, Command, Status};
use crate::{autostart, icon};

pub fn run() -> anyhow::Result<()> {
    if control::call(Command::Status).is_some() {
        log::info!("background process already running");
        return Ok(());
    }
    let state = Arc::new(Mutex::new(State::load()));
    state.lock().unwrap().apply();
    let quit = Arc::new(AtomicBool::new(false));
    control::serve({
        let (state, quit) = (state.clone(), quit.clone());
        move |cmd| {
            let mut st = state.lock().unwrap();
            match cmd {
                Command::Status => {}
                Command::Start => {
                    st.config.server.enabled = true;
                    st.save();
                    st.apply();
                }
                Command::Stop => {
                    st.config.server.enabled = false;
                    st.save();
                    st.apply();
                }
                Command::Reload => {
                    st.config = config::load();
                    st.apply();
                }
                Command::Quit => quit.store(true, Ordering::Release),
            }
            st.status()
        }
    })?;

    let mut builder = EventLoop::builder();
    #[cfg(target_os = "macos")]
    {
        // No Dock icon: this process only has the menu bar item.
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder.build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut tray = Tray {
        state,
        quit,
        icon: None,
        items: None,
        refreshed: Instant::now(),
    };
    event_loop.run_app(&mut tray)?;
    control::forget();
    Ok(())
}

/// The server and its configuration.
struct State {
    config: Config,
    server: Option<Server>,
    /// Options the running server was started with.
    running: Option<Options>,
    error: Option<String>,
}

impl State {
    fn load() -> State {
        let mut config = config::load();
        if config.server.password.is_empty() {
            config.server.password = beach_server::random_password();
            if let Err(e) = config::save(&config) {
                log::warn!("cannot save generated password: {e}");
            }
        }
        // The config decides; the registry entry follows it (and the exe path
        // in it stays current after the program was moved).
        if autostart::supported() && config.server.autostart {
            if let Err(e) = autostart::set(true) {
                log::warn!("autostart: {e}");
            }
        }
        State {
            config,
            server: None,
            running: None,
            error: None,
        }
    }

    fn options(&self) -> Options {
        let s = &self.config.server;
        Options {
            bind: "0.0.0.0".into(),
            port: s.port,
            password: Some(s.password.clone()),
            display: s.display,
            fps: s.fps.max(1),
            view_only: s.view_only,
            name: None,
        }
    }

    /// Starts, stops or restarts the server so it matches the config.
    fn apply(&mut self) {
        let wanted = self.config.server.enabled.then(|| self.options());
        if wanted == self.running {
            return;
        }
        if let Some(server) = self.server.take() {
            server.stop();
        }
        self.running = None;
        self.error = None;
        if let Some(opts) = wanted {
            match Server::start(opts.clone()) {
                Ok(server) => {
                    self.server = Some(server);
                    self.running = Some(opts);
                }
                Err(e) => {
                    log::error!("server failed to start: {e:#}");
                    self.error = Some(format!("{e:#}"));
                }
            }
        }
    }

    fn save(&mut self) {
        if let Err(e) = config::save(&self.config) {
            log::warn!("cannot save config: {e}");
        }
    }

    /// Permissions the running server lacks.
    fn missing(&self) -> Vec<String> {
        let Some(server) = &self.server else {
            return Vec::new();
        };
        let mut missing = Vec::new();
        if server.input_missing() {
            missing.push("accessibility".to_string());
        }
        if !beach_server::permissions::screen_recording() {
            missing.push("screen_recording".to_string());
        }
        missing
    }

    /// Restarts the server once Accessibility has been granted, so viewers
    /// get keyboard and mouse without a manual restart.
    fn retry_input(&mut self) {
        let waiting = self.server.as_ref().is_some_and(|s| s.input_missing());
        if waiting && beach_server::permissions::accessibility() {
            log::info!("Accessibility granted, restarting the server with input");
            self.running = None;
            self.apply();
        }
    }

    fn status(&self) -> Status {
        Status {
            missing: self.missing(),
            pid: std::process::id(),
            running: self.server.is_some(),
            enabled: self.config.server.enabled,
            port: self.config.server.port,
            password: self.config.server.password.clone(),
            viewers: self.server.as_ref().map(|s| s.viewers()).unwrap_or_default(),
            error: self.error.clone(),
            autostart: autostart::enabled(),
        }
    }
}

struct Items {
    status: MenuItem,
    open: MenuItem,
    toggle: MenuItem,
    autostart: CheckMenuItem,
    quit: MenuItem,
}

struct Tray {
    state: Arc<Mutex<State>>,
    quit: Arc<AtomicBool>,
    icon: Option<TrayIcon>,
    items: Option<Items>,
    refreshed: Instant,
}

impl Tray {
    fn build(&mut self) -> anyhow::Result<()> {
        let items = Items {
            status: MenuItem::new("Server", false, None),
            open: MenuItem::new("VNC Beach öffnen", true, None),
            toggle: MenuItem::new("Server anhalten", true, None),
            autostart: CheckMenuItem::new(
                "Beim Anmelden starten",
                autostart::supported(),
                autostart::enabled(),
                None,
            ),
            quit: MenuItem::new("Beenden", true, None),
        };
        let menu = Menu::new();
        menu.append_items(&[
            &items.status,
            &items.open,
            &PredefinedMenuItem::separator(),
            &items.toggle,
            &items.autostart,
            &PredefinedMenuItem::separator(),
            &items.quit,
        ])?;
        let image = icon::tray();
        let image = tray_icon::Icon::from_rgba(image.rgba, image.width, image.height)?;
        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("VNC Beach")
            .with_menu_on_left_click(true);
        // On macOS the icon is a template image: the menu bar tints it for
        // the light, dark and clicked states.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(image);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(image);
        let icon = builder.build()?;
        self.icon = Some(icon);
        self.items = Some(items);
        self.refresh();
        Ok(())
    }

    /// Updates the menu texts from the server state.
    fn refresh(&mut self) {
        let Some(items) = &self.items else { return };
        let status = {
            let mut st = self.state.lock().unwrap();
            st.retry_input();
            st.status()
        };
        let text = if status.running {
            let viewers = match status.viewers.len() {
                0 => format!("Server online, Port {}", status.port),
                1 => format!("Server online, Port {}, 1 Zuschauer", status.port),
                n => format!("Server online, Port {}, {n} Zuschauer", status.port),
            };
            if status.missing.is_empty() {
                viewers
            } else {
                format!("{viewers} (Freigaben fehlen)")
            }
        } else if let Some(e) = &status.error {
            format!("Server-Fehler: {e}")
        } else {
            "Server aus".to_string()
        };
        items.status.set_text(&text);
        items.toggle.set_text(if status.enabled {
            "Server anhalten"
        } else {
            "Server starten"
        });
        items.autostart.set_checked(status.autostart);
        if let Some(icon) = &self.icon {
            let _ = icon.set_tooltip(Some(format!("VNC Beach – {text}")));
        }
        self.refreshed = Instant::now();
    }

    fn handle_events(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let Some(items) = &self.items else { break };
            let id = event.id();
            if *id == items.open.id() {
                open_viewer();
            } else if *id == items.toggle.id() {
                let mut st = self.state.lock().unwrap();
                st.config.server.enabled = !st.config.server.enabled;
                st.save();
                st.apply();
            } else if *id == items.autostart.id() {
                let on = items.autostart.is_checked();
                if let Err(e) = autostart::set(on) {
                    log::warn!("autostart: {e}");
                }
                let mut st = self.state.lock().unwrap();
                st.config.server.autostart = on;
                st.save();
            } else if *id == items.quit.id() {
                self.quit.store(true, Ordering::Release);
            }
            self.refresh();
        }
        // Both mouse buttons open the menu; drain the click events anyway so
        // the channel doesn't fill up.
        while TrayIconEvent::receiver().try_recv().is_ok() {}
        if self.quit.load(Ordering::Acquire) {
            if let Some(server) = self.state.lock().unwrap().server.take() {
                server.stop();
            }
            event_loop.exit();
        }
    }
}

impl ApplicationHandler for Tray {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        if self.icon.is_none() {
            if let Err(e) = self.build() {
                log::error!("tray icon: {e:#}");
                self.quit.store(true, Ordering::Release);
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.handle_events(event_loop);
        if self.refreshed.elapsed() > Duration::from_secs(2) {
            self.refresh();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(500),
        ));
    }
}

/// Shows the viewer window: brings the running one to the front or starts
/// a new process.
fn open_viewer() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log::warn!("cannot start the viewer: {e}");
            return;
        }
    };
    // `open` on the app bundle brings a running viewer to the front instead
    // of starting a second one. Without a viewer it would only "activate"
    // this background process, so that case starts the executable directly.
    #[cfg(target_os = "macos")]
    if let Some(bundle) = exe
        .ancestors()
        .nth(3)
        .filter(|p| p.extension().is_some_and(|ext| ext == "app"))
    {
        if viewer_running(&exe) {
            match std::process::Command::new("open").arg(bundle).spawn() {
                Ok(_) => return,
                Err(e) => log::warn!("open {}: {e}", bundle.display()),
            }
        }
    }
    if let Err(e) = std::process::Command::new(exe).spawn() {
        log::warn!("cannot start the viewer: {e}");
    }
}

/// Whether a viewer process of this executable (one without `--background`)
/// is running, according to `ps`.
#[cfg(target_os = "macos")]
fn viewer_running(exe: &std::path::Path) -> bool {
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "args="]).output() else {
        return false;
    };
    let exe = exe.to_string_lossy();
    String::from_utf8_lossy(&out.stdout).lines().any(|line| {
        let line = line.trim_end();
        (line == exe || line.starts_with(&format!("{exe} "))) && !line.contains("--background")
    })
}
