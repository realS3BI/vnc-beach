//! A virtual display while no monitor is attached (Windows).
//!
//! Switched-off monitors can drop out of Windows entirely; the desktop then
//! shrinks to a 1024×768 stand-in that VNC Beach would share. With the Amyuni
//! virtual display driver (`usbmmidd_v2`, see `docs/headless-windows.md`)
//! installed, the capture thread adds one 2560×1440 display while no other
//! display is active and removes it again once one is.
//!
//! The driver only knows "add a display" and "remove a display", not which
//! one, so VNC Beach only removes a display it added itself (remembered in a
//! marker file) and only while it is the driver's single display. Displays
//! of other applications are never removed or resized.
//!
//! The decisions live in [`Fallback`] over the [`Driver`] trait so they can be
//! tested without the hardware; [`Amyuni`] is the Windows implementation.

use std::time::{Duration, Instant};

use anyhow::bail;

/// Resolution of the virtual display.
pub const WIDTH: u32 = 2560;
pub const HEIGHT: u32 = 1440;
/// How long a freshly added display may take to show up.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(2);
/// Pause after a failed attempt.
const RETRY: Duration = Duration::from_secs(10);

/// Active displays as far as the fallback cares.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Displays {
    /// Monitors and virtual displays of other drivers on the desktop.
    pub others: usize,
    /// Active displays of the virtual display driver.
    pub virtuals: Vec<VirtualDisplay>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDisplay {
    /// GDI device name, e.g. `\\.\DISPLAY3`.
    pub device: String,
    pub width: u32,
    pub height: u32,
}

/// What [`Fallback`] needs from the system.
pub trait Driver {
    /// The active displays, split into the driver's and everyone else's.
    fn displays(&mut self) -> anyhow::Result<Displays>;
    /// Adds one virtual display.
    fn add(&mut self) -> anyhow::Result<()>;
    /// Removes one virtual display (the driver picks which).
    fn remove(&mut self) -> anyhow::Result<()>;
    /// Sets the resolution of the display `device` (a GDI device name).
    fn set_mode(&mut self, device: &str, width: u32, height: u32) -> anyhow::Result<()>;
    /// Whether VNC Beach added the driver's current display; survives restarts.
    fn owned(&self) -> bool;
    /// Records whether VNC Beach owns the driver's current display.
    fn set_owned(&mut self, owned: bool);
    /// The current time; tests substitute a fake clock.
    fn now(&self) -> Instant {
        Instant::now()
    }
    /// Waits for `duration`; tests advance the fake clock instead.
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Keeps one virtual display around while nothing else is. Owned by the
/// capture thread, which calls [`Fallback::tick`] about once a second.
pub struct Fallback<D> {
    driver: D,
    /// No new attempt before this, after a failure.
    paused_until: Option<Instant>,
    /// The last failure or note, so a lasting one is logged once.
    last_message: String,
}

impl<D: Driver> Fallback<D> {
    /// A fallback over `driver` that hasn't failed yet.
    pub fn new(driver: D) -> Self {
        Fallback {
            driver,
            paused_until: None,
            last_message: String::new(),
        }
    }

    /// Adds, resizes or removes the virtual display as needed. Returns true
    /// if it changed the displays, so the caller re-reads the arrangement.
    pub fn tick(&mut self) -> bool {
        let now = self.driver.now();
        if self.paused_until.is_some_and(|until| now < until) {
            return false;
        }
        self.paused_until = None;
        match self.step() {
            Ok(changed) => changed,
            Err(e) => {
                self.note(log::Level::Warn, format!("virtual display: {e:#}"));
                self.paused_until = Some(self.driver.now() + RETRY);
                false
            }
        }
    }

    /// One decision without the error pause of [`Fallback::tick`]: removes
    /// an owned display once a monitor is back, adds one while nothing is
    /// active and keeps the resolution of the owned one. True if it changed
    /// the displays.
    fn step(&mut self) -> anyhow::Result<bool> {
        let displays = self.driver.displays()?;
        let owned = self.driver.owned();
        if displays.others > 0 {
            if !owned {
                self.note(log::Level::Debug, String::new());
                return Ok(false);
            }
            return match displays.virtuals.len() {
                0 => {
                    // Removed by someone else (or the driver is gone).
                    self.driver.set_owned(false);
                    Ok(false)
                }
                1 => {
                    self.driver.remove()?;
                    self.driver.set_owned(false);
                    self.note(
                        log::Level::Info,
                        "a monitor is back: removed the virtual display".into(),
                    );
                    Ok(true)
                }
                n => bail!(
                    "{n} displays of the virtual display driver are active; leaving them alone because the \
                     driver cannot tell which one VNC Beach added"
                ),
            };
        }
        match (displays.virtuals.as_slice(), owned) {
            ([], _) => self.add(),
            ([display], true) => self.ensure_mode(display),
            (_, false) => {
                self.note(
                    log::Level::Info,
                    "no monitor attached, but another application's virtual display is active; using it as it is"
                        .into(),
                );
                Ok(false)
            }
            (all, true) => bail!(
                "{} displays of the virtual display driver are active; leaving them alone because the \
                 driver cannot tell which one VNC Beach added",
                all.len()
            ),
        }
    }

    /// Adds a virtual display, waits up to [`APPEAR_TIMEOUT`] for it to
    /// become active and sets its resolution.
    fn add(&mut self) -> anyhow::Result<bool> {
        // Mark first: if the process dies right after adding, the next one
        // still knows the display is ours.
        self.driver.set_owned(true);
        if let Err(e) = self.driver.add() {
            self.driver.set_owned(false);
            return Err(e.context("no monitor attached and adding a virtual display failed"));
        }
        let deadline = self.driver.now() + APPEAR_TIMEOUT;
        loop {
            let displays = self.driver.displays()?;
            if let [display] = displays.virtuals.as_slice() {
                self.note(
                    log::Level::Info,
                    "no monitor attached: added a virtual display".into(),
                );
                self.ensure_mode(display)?;
                return Ok(true);
            }
            if !displays.virtuals.is_empty() {
                bail!(
                    "{} virtual displays appeared instead of one",
                    displays.virtuals.len()
                );
            }
            if self.driver.now() >= deadline {
                // It may still turn up; it's marked as ours, so it's then
                // reused instead of adding another one.
                bail!("the added virtual display did not show up within {APPEAR_TIMEOUT:?}");
            }
            self.driver.sleep(Duration::from_millis(100));
        }
    }

    /// Sets our display to [`WIDTH`]×[`HEIGHT`] if it isn't, and checks the
    /// result.
    fn ensure_mode(&mut self, display: &VirtualDisplay) -> anyhow::Result<bool> {
        if (display.width, display.height) == (WIDTH, HEIGHT) {
            self.note(log::Level::Debug, String::new());
            return Ok(false);
        }
        self.driver.set_mode(&display.device, WIDTH, HEIGHT)?;
        let now = self.driver.displays()?;
        match now.virtuals.iter().find(|v| v.device == display.device) {
            Some(v) if (v.width, v.height) == (WIDTH, HEIGHT) => {
                self.note(
                    log::Level::Info,
                    format!("virtual display {} set to {WIDTH}x{HEIGHT}", display.device),
                );
                Ok(true)
            }
            Some(v) => bail!(
                "virtual display {} runs at {}x{} instead of {WIDTH}x{HEIGHT}",
                v.device,
                v.width,
                v.height
            ),
            None => bail!(
                "virtual display {} vanished while setting its resolution",
                display.device
            ),
        }
    }

    /// Logs `message` unless it repeats the previous one. An empty message
    /// only resets, so the next problem is logged again.
    fn note(&mut self, level: log::Level, message: String) {
        if message != self.last_message {
            if !message.is_empty() {
                log::log!(level, "{message}");
            }
            self.last_message = message;
        }
    }

    /// The driver, for assertions in tests.
    #[cfg(test)]
    fn driver(&self) -> &D {
        &self.driver
    }
}

#[cfg(windows)]
pub use windows::{driver_installed, Amyuni};

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::path::PathBuf;

    use anyhow::{anyhow, bail};
    use windows_sys::core::GUID;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
        SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    };
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_NO_MORE_ITEMS, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsW, CDS_UPDATEREGISTRY, DEVMODEW,
        DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP,
        DISPLAY_DEVICE_MIRRORING_DRIVER, DISP_CHANGE_SUCCESSFUL, DM_PELSHEIGHT, DM_PELSWIDTH,
        ENUM_CURRENT_SETTINGS,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    use super::{Displays, Driver, VirtualDisplay};

    /// How the driver's displays show up in `EnumDisplayDevicesW`.
    const ADAPTER_NAME: &str = "USB Mobile Monitor Virtual Display";
    /// The driver's control interface.
    const INTERFACE: GUID = GUID::from_u128(0xb5ffd75f_da40_4353_8ff8_b6daf6f1d8ca);
    const IOCTL_MONITOR: u32 = 2_307_084;
    const ADD: [u8; 4] = [0x10, 0, 0, 0];
    const REMOVE: [u8; 4] = [0, 0, 0, 0];

    /// The Amyuni `usbmmidd_v2` driver.
    pub struct Amyuni {
        /// Exists while VNC Beach's display is active.
        marker: Option<PathBuf>,
        owned: bool,
    }

    impl Amyuni {
        /// The driver, with ownership read from the marker file in
        /// `%ProgramData%\VNC Beach`.
        pub fn new() -> Amyuni {
            let marker = std::env::var_os("ProgramData")
                .map(|dir| PathBuf::from(dir).join("VNC Beach").join("virtual-display"));
            let owned = marker.as_ref().is_some_and(|m| m.exists());
            Amyuni { marker, owned }
        }
    }

    impl Default for Amyuni {
        /// Same as [`Amyuni::new`].
        fn default() -> Self {
            Self::new()
        }
    }

    impl Driver for Amyuni {
        /// Counts active monitors per adapter through `EnumDisplayDevicesW`;
        /// the driver's own adapters become [`VirtualDisplay`]s.
        fn displays(&mut self) -> anyhow::Result<Displays> {
            let mut displays = Displays::default();
            let adapters = devices(None);
            // None at all is a transient state (session switch, driver
            // restart), not "no monitor": don't add a display for it.
            anyhow::ensure!(!adapters.is_empty(), "cannot enumerate display adapters");
            for adapter in adapters {
                if adapter.StateFlags & DISPLAY_DEVICE_MIRRORING_DRIVER != 0 {
                    continue;
                }
                let name = wstr(&adapter.DeviceName);
                if wstr(&adapter.DeviceString) == ADAPTER_NAME {
                    if adapter.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0 {
                        let (width, height) = mode(&adapter.DeviceName).unwrap_or((0, 0));
                        displays.virtuals.push(VirtualDisplay {
                            device: name,
                            width,
                            height,
                        });
                    }
                    continue;
                }
                // An active adapter alone proves nothing: without monitors
                // NVIDIA keeps a stand-in desktop with no (or the "Default
                // Monitor") device under it.
                displays.others += devices(Some(&adapter.DeviceName))
                    .iter()
                    .filter(|m| m.StateFlags & DISPLAY_DEVICE_ACTIVE != 0)
                    .filter(|m| {
                        let id = wstr(&m.DeviceID).to_ascii_uppercase();
                        id.starts_with("MONITOR\\") && !id.contains("DEFAULT_MONITOR")
                    })
                    .count();
            }
            Ok(displays)
        }

        /// Asks the driver for one more display.
        fn add(&mut self) -> anyhow::Result<()> {
            control(ADD)
        }

        /// Asks the driver to drop one of its displays.
        fn remove(&mut self) -> anyhow::Result<()> {
            control(REMOVE)
        }

        /// Changes the resolution through `ChangeDisplaySettingsExW` and
        /// stores it in the registry.
        fn set_mode(&mut self, device: &str, width: u32, height: u32) -> anyhow::Result<()> {
            let name: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
            // SAFETY: `name` is NUL-terminated and `dm` sized; both outlive the calls.
            unsafe {
                let mut dm: DEVMODEW = std::mem::zeroed();
                dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
                if EnumDisplaySettingsW(name.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) == 0 {
                    bail!("cannot read the mode of {device}");
                }
                dm.dmPelsWidth = width;
                dm.dmPelsHeight = height;
                dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
                let result = ChangeDisplaySettingsExW(
                    name.as_ptr(),
                    &dm,
                    std::ptr::null_mut(),
                    CDS_UPDATEREGISTRY,
                    std::ptr::null(),
                );
                if result != DISP_CHANGE_SUCCESSFUL {
                    bail!("setting {device} to {width}x{height} failed (ChangeDisplaySettingsExW {result})");
                }
            }
            Ok(())
        }

        /// Whether the marker file existed at start or was written since.
        fn owned(&self) -> bool {
            self.owned
        }

        /// Creates or deletes the marker file; failures are only logged.
        fn set_owned(&mut self, owned: bool) {
            if owned == self.owned {
                return;
            }
            self.owned = owned;
            let Some(marker) = &self.marker else { return };
            let result = if owned {
                std::fs::create_dir_all(marker.parent().unwrap())
                    .and_then(|()| std::fs::write(marker, b"added by VNC Beach\r\n"))
            } else {
                std::fs::remove_file(marker).or_else(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => Ok(()),
                    _ => Err(e),
                })
            };
            if let Err(e) = result {
                log::warn!("virtual display: cannot update {}: {e}", marker.display());
            }
        }
    }

    /// Whether the driver is installed and its control interface present.
    pub fn driver_installed() -> bool {
        with_interface(|_| Ok(())).is_ok()
    }

    /// Display adapters (`parent` = `None`) or the monitors of one adapter.
    fn devices(parent: Option<&[u16; 32]>) -> Vec<DISPLAY_DEVICEW> {
        let mut all = Vec::new();
        for i in 0.. {
            // SAFETY: `device` is sized and `parent` NUL-terminated by the API.
            unsafe {
                let mut device: DISPLAY_DEVICEW = std::mem::zeroed();
                device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
                let parent = parent.map_or(std::ptr::null(), |p| p.as_ptr());
                if EnumDisplayDevicesW(parent, i, &mut device, 0) == 0 {
                    break;
                }
                all.push(device);
            }
        }
        all
    }

    /// Current resolution of a display adapter.
    fn mode(device: &[u16; 32]) -> Option<(u32, u32)> {
        // SAFETY: `dm` is sized; the device name is NUL-terminated.
        unsafe {
            let mut dm: DEVMODEW = std::mem::zeroed();
            dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            (EnumDisplaySettingsW(device.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) != 0)
                .then_some((dm.dmPelsWidth, dm.dmPelsHeight))
        }
    }

    /// A NUL-terminated UTF-16 buffer as a string.
    fn wstr(text: &[u16]) -> String {
        let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        String::from_utf16_lossy(&text[..len])
    }

    /// Sends an add or remove request to the driver.
    fn control(input: [u8; 4]) -> anyhow::Result<()> {
        with_interface(|path| {
            // SAFETY: `path` is NUL-terminated; the handle is closed on every path.
            unsafe {
                let handle = CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                );
                if handle == INVALID_HANDLE_VALUE {
                    bail!(
                        "cannot open the virtual display driver: {}",
                        std::io::Error::last_os_error()
                    );
                }
                let mut returned = 0;
                let ok = DeviceIoControl(
                    handle,
                    IOCTL_MONITOR,
                    input.as_ptr().cast::<c_void>(),
                    input.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                );
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                if ok == 0 {
                    bail!("the virtual display driver refused the request: {error}");
                }
            }
            Ok(())
        })
    }

    /// Calls `f` with the NUL-terminated path of the driver's interface.
    fn with_interface<T>(f: impl FnOnce(&[u16]) -> anyhow::Result<T>) -> anyhow::Result<T> {
        // SAFETY: the device list is destroyed on every path; the detail
        // buffer is u64-aligned and as large as SetupAPI asked for.
        unsafe {
            let set = SetupDiGetClassDevsW(
                &INTERFACE,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            );
            if set as isize == INVALID_HANDLE_VALUE as isize {
                bail!("SetupDiGetClassDevsW failed: {}", std::io::Error::last_os_error());
            }
            let result = (|| {
                let mut data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
                data.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
                if SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &INTERFACE, 0, &mut data) == 0 {
                    return Err(if GetLastError() == ERROR_NO_MORE_ITEMS {
                        anyhow!(
                            "the virtual display driver is not installed (see docs/headless-windows.md, \
                             or run vnc-beach.exe --install-display-driver as administrator)"
                        )
                    } else {
                        anyhow!(
                            "SetupDiEnumDeviceInterfaces failed: {}",
                            std::io::Error::last_os_error()
                        )
                    });
                }
                let mut needed = 0;
                SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &data,
                    std::ptr::null_mut(),
                    0,
                    &mut needed,
                    std::ptr::null_mut(),
                );
                if (needed as usize) < std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
                    bail!(
                        "SetupDiGetDeviceInterfaceDetailW failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
                let detail = buffer.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
                (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                if SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &data,
                    detail,
                    needed,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) == 0
                {
                    bail!(
                        "SetupDiGetDeviceInterfaceDetailW failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                let start = std::ptr::addr_of!((*detail).DevicePath).cast::<u16>();
                let max = (needed as usize - (start as usize - detail as usize)) / 2;
                let path = std::slice::from_raw_parts(start, max);
                let len = path.iter().position(|&c| c == 0).unwrap_or(max);
                let path: Vec<u16> = path[..len].iter().copied().chain(Some(0)).collect();
                f(&path)
            })();
            SetupDiDestroyDeviceInfoList(set);
            result
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{HEIGHT, WIDTH};
        use super::*;

        /// Prints what the fallback sees. Changes nothing.
        #[test]
        #[ignore = "reads the real display configuration"]
        fn inspect_real_displays() {
            for adapter in devices(None) {
                println!(
                    "adapter {} {:?} flags {:#x}",
                    wstr(&adapter.DeviceName),
                    wstr(&adapter.DeviceString),
                    adapter.StateFlags
                );
                for monitor in devices(Some(&adapter.DeviceName)) {
                    println!(
                        "  monitor {:?} {:?} flags {:#x}",
                        wstr(&monitor.DeviceString),
                        wstr(&monitor.DeviceID),
                        monitor.StateFlags
                    );
                }
            }
            println!("driver installed: {}", driver_installed());
            println!("{:?}", Amyuni::new().displays().unwrap());
        }

        /// Adds a real virtual display, sets 2560×1440, checks it and removes
        /// it again. Leaves the ownership marker alone, so a running VNC
        /// Beach doesn't treat the test display as its own.
        #[test]
        #[ignore = "adds and removes a real virtual display"]
        fn real_virtual_monitor_cycle() {
            let mut driver = Amyuni::new();
            let before = driver.displays().unwrap();
            println!("before: {before:?}");
            control(ADD).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let added = loop {
                let now = driver.displays().unwrap();
                if let Some(v) = now.virtuals.iter().find(|v| !before.virtuals.contains(v)) {
                    break v.clone();
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "no new virtual display: {now:?}"
                );
                std::thread::sleep(std::time::Duration::from_millis(100));
            };
            println!("added: {added:?}");
            let result = driver.set_mode(&added.device, WIDTH, HEIGHT).and_then(|()| {
                let now = driver.displays()?;
                let v = now.virtuals.iter().find(|v| v.device == added.device);
                println!("after set_mode: {v:?}");
                anyhow::ensure!(v.is_some_and(|v| (v.width, v.height) == (WIDTH, HEIGHT)));
                Ok(())
            });
            control(REMOVE).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(2));
            let after = driver.displays().unwrap();
            println!("after: {after:?}");
            result.unwrap();
            assert_eq!(after.virtuals.len(), before.virtuals.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake system: `appear_after` polls until an added display shows up.
    struct Fake {
        others: usize,
        virtuals: Vec<VirtualDisplay>,
        owned: bool,
        /// Resolution new displays come up with.
        initial: (u32, u32),
        fail_add: bool,
        /// Added displays not visible yet: (device, visible at).
        pending: Vec<(VirtualDisplay, Instant)>,
        appear_after: Duration,
        /// Resolution `set_mode` really ends up with.
        mode_result: Option<(u32, u32)>,
        clock: Instant,
        added: usize,
        removed: usize,
        mode_sets: usize,
        next_id: usize,
    }

    impl Fake {
        /// A system with `others` monitors and no virtual display.
        fn new(others: usize) -> Fake {
            Fake {
                others,
                virtuals: Vec::new(),
                owned: false,
                initial: (1920, 1080),
                fail_add: false,
                pending: Vec::new(),
                appear_after: Duration::from_millis(300),
                mode_result: None,
                clock: Instant::now(),
                added: 0,
                removed: 0,
                mode_sets: 0,
                next_id: 3,
            }
        }

        /// A driver display with a fresh device name.
        fn virtual_display(&mut self, (width, height): (u32, u32)) -> VirtualDisplay {
            self.next_id += 1;
            VirtualDisplay {
                device: format!(r"\\.\DISPLAY{}", self.next_id),
                width,
                height,
            }
        }

        /// Moves the fake clock forward.
        fn advance(&mut self, d: Duration) {
            self.clock += d;
        }
    }

    impl Driver for Fake {
        /// Activates the added displays whose delay has passed.
        fn displays(&mut self) -> anyhow::Result<Displays> {
            let now = self.clock;
            let (ready, waiting) = self.pending.drain(..).partition(|(_, at)| *at <= now);
            self.pending = waiting;
            self.virtuals.extend(ready.into_iter().map(|(v, _)| v));
            Ok(Displays {
                others: self.others,
                virtuals: self.virtuals.clone(),
            })
        }

        /// Queues a display that appears after `appear_after`, or fails if
        /// `fail_add` is set.
        fn add(&mut self) -> anyhow::Result<()> {
            if self.fail_add {
                bail!("the virtual display driver is not installed");
            }
            self.added += 1;
            let v = self.virtual_display(self.initial);
            self.pending.push((v, self.clock + self.appear_after));
            Ok(())
        }

        /// Drops the newest display, like the real driver.
        fn remove(&mut self) -> anyhow::Result<()> {
            self.removed += 1;
            self.virtuals.pop();
            Ok(())
        }

        /// Applies `mode_result` if set, otherwise the requested size.
        fn set_mode(&mut self, device: &str, width: u32, height: u32) -> anyhow::Result<()> {
            self.mode_sets += 1;
            let (w, h) = self.mode_result.unwrap_or((width, height));
            for v in self.virtuals.iter_mut().filter(|v| v.device == device) {
                (v.width, v.height) = (w, h);
            }
            Ok(())
        }

        /// The in-memory ownership flag.
        fn owned(&self) -> bool {
            self.owned
        }

        /// Sets the in-memory ownership flag.
        fn set_owned(&mut self, owned: bool) {
            self.owned = owned;
        }

        /// The fake clock.
        fn now(&self) -> Instant {
            self.clock
        }

        /// Advances the fake clock instead of waiting.
        fn sleep(&mut self, d: Duration) {
            self.advance(d);
        }
    }

    /// A fallback over `fake`.
    fn fallback(fake: Fake) -> Fallback<Fake> {
        Fallback::new(fake)
    }

    /// Moves the fake clock of `f`'s driver forward.
    fn advance(f: &mut Fallback<Fake>, d: Duration) {
        f.driver.advance(d);
    }

    #[test]
    fn leaves_attached_monitors_alone() {
        let mut f = fallback(Fake::new(2));
        assert!(!f.tick());
        assert_eq!(
            (f.driver().added, f.driver().removed, f.driver().mode_sets),
            (0, 0, 0)
        );
    }

    #[test]
    fn adds_one_display_at_2560x1440_and_keeps_it() {
        let mut f = fallback(Fake::new(0));
        assert!(f.tick());
        let d = f.driver();
        assert_eq!((d.added, d.mode_sets), (1, 1));
        assert!(d.owned);
        assert_eq!(d.virtuals.len(), 1);
        assert_eq!((d.virtuals[0].width, d.virtuals[0].height), (WIDTH, HEIGHT));
        // Later checks (new viewers, reconnects) find it and do nothing.
        for _ in 0..5 {
            advance(&mut f, Duration::from_secs(1));
            assert!(!f.tick());
        }
        assert_eq!((f.driver().added, f.driver().mode_sets), (1, 1));
    }

    #[test]
    fn waits_for_a_slow_display_without_adding_twice() {
        let mut fake = Fake::new(0);
        fake.appear_after = Duration::from_secs(5);
        let mut f = fallback(fake);
        assert!(!f.tick(), "the display isn't there after two seconds");
        assert_eq!(f.driver().added, 1);
        assert!(f.driver().owned);
        // It shows up meanwhile; the next attempt (after the pause) reuses it.
        advance(&mut f, Duration::from_secs(5));
        assert!(!f.tick(), "still paused");
        advance(&mut f, Duration::from_secs(10));
        assert!(f.tick());
        let d = f.driver();
        assert_eq!(d.added, 1);
        assert_eq!(d.virtuals.len(), 1);
        assert_eq!((d.virtuals[0].width, d.virtuals[0].height), (WIDTH, HEIGHT));
    }

    #[test]
    fn removes_its_display_when_a_monitor_returns() {
        let mut f = fallback(Fake::new(0));
        assert!(f.tick());
        f.driver.others = 1;
        advance(&mut f, Duration::from_secs(1));
        assert!(f.tick());
        let d = f.driver();
        assert_eq!(d.removed, 1);
        assert!(d.virtuals.is_empty());
        assert!(!d.owned);
        // Monitors off again: a new one, but still only one.
        f.driver.others = 0;
        advance(&mut f, Duration::from_secs(1));
        assert!(f.tick());
        assert_eq!(f.driver().added, 2);
        assert_eq!(f.driver().virtuals.len(), 1);
    }

    #[test]
    fn keeps_its_display_across_restarts() {
        // A previous process added it (marker file) and set it up.
        let mut fake = Fake::new(0);
        fake.owned = true;
        let v = fake.virtual_display((WIDTH, HEIGHT));
        fake.virtuals.push(v);
        let mut f = fallback(fake);
        assert!(!f.tick());
        assert_eq!((f.driver().added, f.driver().mode_sets), (0, 0));
        // …and it still goes once a monitor is back.
        f.driver.others = 1;
        assert!(f.tick());
        assert_eq!(f.driver().removed, 1);
    }

    #[test]
    fn fixes_the_resolution_of_its_display() {
        let mut fake = Fake::new(0);
        fake.owned = true;
        let v = fake.virtual_display((1920, 1080));
        fake.virtuals.push(v);
        let mut f = fallback(fake);
        assert!(f.tick());
        assert_eq!(f.driver().mode_sets, 1);
        assert_eq!(
            (f.driver().virtuals[0].width, f.driver().virtuals[0].height),
            (WIDTH, HEIGHT)
        );
    }

    #[test]
    fn never_touches_other_applications_displays() {
        // Another application's virtual display is the only display.
        let mut fake = Fake::new(0);
        let v = fake.virtual_display((1920, 1080));
        fake.virtuals.push(v);
        let mut f = fallback(fake);
        assert!(!f.tick());
        // Monitors come back: still not ours to remove.
        f.driver.others = 1;
        assert!(!f.tick());
        let d = f.driver();
        assert_eq!((d.added, d.removed, d.mode_sets), (0, 0, 0));
        assert_eq!(d.virtuals.len(), 1);
    }

    #[test]
    fn refuses_to_guess_between_several_driver_displays() {
        let mut fake = Fake::new(1);
        fake.owned = true;
        for _ in 0..2 {
            let v = fake.virtual_display((WIDTH, HEIGHT));
            fake.virtuals.push(v);
        }
        let mut f = fallback(fake);
        assert!(!f.tick());
        assert_eq!(f.driver().removed, 0);
        assert_eq!(f.driver().virtuals.len(), 2);
    }

    #[test]
    fn pauses_after_driver_errors() {
        let mut fake = Fake::new(0);
        fake.fail_add = true;
        let mut f = fallback(fake);
        assert!(!f.tick());
        assert!(!f.driver().owned, "a failed add isn't ours");
        // Ticks within the pause don't try again.
        for _ in 0..9 {
            advance(&mut f, Duration::from_secs(1));
            assert!(!f.tick());
        }
        // The driver got installed meanwhile.
        f.driver.fail_add = false;
        advance(&mut f, Duration::from_secs(1));
        assert!(f.tick());
        assert_eq!(f.driver().added, 1);
    }

    #[test]
    fn reports_a_resolution_the_driver_does_not_take() {
        let mut fake = Fake::new(0);
        fake.mode_result = Some((1920, 1080));
        let mut f = fallback(fake);
        assert!(!f.tick());
        // The display stays (and stays ours); after the pause it's tried again
        // without adding another one.
        advance(&mut f, Duration::from_secs(10));
        f.driver.mode_result = None;
        assert!(f.tick());
        let d = f.driver();
        assert_eq!((d.added, d.mode_sets), (1, 2));
        assert_eq!((d.virtuals[0].width, d.virtuals[0].height), (WIDTH, HEIGHT));
    }

    #[test]
    fn forgets_a_display_someone_else_removed() {
        let mut fake = Fake::new(1);
        fake.owned = true;
        let mut f = fallback(fake);
        assert!(!f.tick());
        assert!(!f.driver().owned);
    }
}
