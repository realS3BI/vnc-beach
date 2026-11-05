//! Crash reports for development: panics (any thread) and native crashes
//! (signals on Unix, unhandled exceptions on Windows) leave a text file with
//! the version, the thread, a backtrace and the last log lines.
//!
//! `init_logging` replaces `env_logger::Builder::init` so the last records
//! (Info and up, whatever the console filter says) are kept in memory;
//! `install` sets the hooks. One file per process, created on the first crash:
//! `crash-<start time>-<pid>.txt` in the directory given to `install`.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Log lines kept for a report.
const RECENT_LINES: usize = 200;
/// Longer log lines are cut.
const LINE_CHARS: usize = 400;
/// Older reports are deleted at start.
const KEEP_REPORTS: usize = 20;

static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static REPORT: OnceLock<Report> = OnceLock::new();

/// The recent log without ever blocking: a crash while the lock is held must
/// still be reported.
fn recent() -> Option<std::sync::MutexGuard<'static, VecDeque<String>>> {
    match RECENT.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// Where and for what process reports are written.
struct Report {
    path: PathBuf,
    /// The first lines of every report (version, mode, OS, pid, start).
    header: String,
}

/// The default report directory for a user process: `~/Library/Logs/VNC
/// Beach/Crashes` on macOS (Console.app lists it), the local app data
/// `vnc-beach\crashes` elsewhere.
pub fn user_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(dirs::home_dir()?.join("Library/Logs/VNC Beach/Crashes"))
    } else {
        Some(dirs::data_local_dir()?.join("vnc-beach").join("crashes"))
    }
}

/// Installs `builder` as the logger and keeps the last Info+ records for
/// crash reports, also when the builder's filter hides them.
pub fn init_logging(mut builder: env_logger::Builder) {
    let inner = builder.build();
    let max = inner.filter().max(log::LevelFilter::Info);
    if log::set_boxed_logger(Box::new(Logger { inner })).is_ok() {
        log::set_max_level(max);
    }
}

struct Logger {
    inner: env_logger::Logger,
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info || self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if record.level() <= log::Level::Info {
            remember(record);
        }
        if self.inner.enabled(record.metadata()) {
            self.inner.log(record);
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

fn remember(record: &log::Record) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut line = format!("{} {:<5} {}: {}", utc_time(secs), record.level(), record.target(), record.args());
    if line.len() > LINE_CHARS {
        let mut cut = LINE_CHARS;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push('…');
    }
    if let Some(mut recent) = recent() {
        if recent.len() == RECENT_LINES {
            recent.pop_front();
        }
        recent.push_back(line);
    }
}

/// Sets the panic hook and the native crash handlers. `mode` names the
/// process (`viewer`, `background`, `agent`, ...). Reports beyond the newest
/// `KEEP_REPORTS` in `dir` are deleted.
pub fn install(dir: &Path, version: &str, mode: &str) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("crash reports disabled: {}: {e}", dir.display());
        return;
    }
    prune(dir, KEEP_REPORTS);
    let start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let pid = std::process::id();
    let header = header(version, mode, pid, start);
    let path = dir.join(format!("crash-{}-{pid}.txt", file_time(start)));
    if REPORT.set(Report { path, header }).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        on_panic(info);
        previous(info);
    }));
    native::install();
}

/// The crash reports in `dir`, newest first.
pub fn reports(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| is_report(&e.path()))
        .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH), e.path()))
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    files.into_iter().map(|(_, p)| p).collect()
}

fn is_report(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt"))
}

fn prune(dir: &Path, keep: usize) {
    for old in reports(dir).into_iter().skip(keep) {
        let _ = std::fs::remove_file(old);
    }
}

fn header(version: &str, mode: &str, pid: u32, start: u64) -> String {
    format!(
        "VNC Beach crash report\nversion: {version}\nmode: {mode}\nos: {} {}\npid: {pid}\nstarted: {} UTC\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        utc_time(start),
    )
}

thread_local! {
    /// Guards against a panic inside the hook (e.g. in the logger).
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn on_panic(info: &std::panic::PanicHookInfo) {
    if IN_HOOK.with(|f| f.replace(true)) {
        return;
    }
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".into());
    let location = info.location().map(|l| l.to_string()).unwrap_or_else(|| "unknown".into());
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    let backtrace = std::backtrace::Backtrace::force_capture();
    let body = format!(
        "kind: panic\nthread: {thread}\nmessage: {message}\nlocation: {location}\n\nbacktrace:\n{backtrace}\n"
    );
    if let Some(path) = write_report(&body) {
        log::error!("panic in thread {thread} at {location}: {message} (report: {})", path.display());
    }
    IN_HOOK.with(|f| f.set(false));
}

/// Appends a report (header, time, `body`, recent log) to this process's file.
fn write_report(body: &str) -> Option<PathBuf> {
    let report = REPORT.get()?;
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut text = String::new();
    let _ = write!(text, "{}time: {} UTC\n{body}\nrecent log:\n", report.header, utc_time(secs));
    match recent() {
        Some(recent) => {
            for line in recent.iter() {
                text.push_str(line);
                text.push('\n');
            }
        }
        None => text.push_str("(log busy)\n"),
    }
    text.push_str("\n\n");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&report.path).ok()?;
    file.write_all(text.as_bytes()).ok()?;
    Some(report.path.clone())
}

/// `2026-11-05 14:03:07` for Unix seconds (UTC). Pure arithmetic, so the
/// signal handler can use it.
fn utc_time(secs: u64) -> String {
    let mut buf = Buf::new();
    put_utc(&mut buf, secs);
    String::from_utf8_lossy(buf.bytes()).into_owned()
}

/// `20261105-140307` for file names.
fn file_time(secs: u64) -> String {
    utc_time(secs).replace('-', "").replace(' ', "-").replace(':', "")
}

fn put_utc(buf: &mut Buf, secs: u64) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let _ = write!(buf, "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem / 60 % 60, rem % 60);
}

/// Howard Hinnant's days-to-date conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// A fixed buffer for formatting without allocating (signal handlers).
struct Buf {
    data: [u8; 512],
    len: usize,
}

impl Buf {
    const fn new() -> Buf {
        Buf { data: [0; 512], len: 0 }
    }

    fn bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

impl std::fmt::Write for Buf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let n = s.len().min(self.data.len() - self.len);
        self.data[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

#[cfg(unix)]
mod native {
    //! Fatal signals. The handler only uses open/write/close, fixed buffers
    //! and a non-blocking try_lock, then restores the previous action and
    //! returns, so the signal hits again with the system's (or Rust's stack
    //! overflow) handler and the OS crash reporter still runs.

    use std::ffi::CString;
    use std::fmt::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::sync::OnceLock;

    use super::{put_utc, Buf, REPORT};

    const SIGNALS: [(libc::c_int, &str); 6] = [
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGTRAP, "SIGTRAP"),
    ];

    struct State {
        path: CString,
        previous: [libc::sigaction; SIGNALS.len()],
    }

    static STATE: OnceLock<State> = OnceLock::new();

    pub fn install() {
        let Some(report) = REPORT.get() else { return };
        let Ok(path) = CString::new(report.path.as_os_str().as_bytes()) else { return };
        // SAFETY: plain sigaction calls with zeroed, then filled structs.
        unsafe {
            let mut previous: [libc::sigaction; SIGNALS.len()] = std::mem::zeroed();
            for (i, (sig, _)) in SIGNALS.iter().enumerate() {
                libc::sigaction(*sig, std::ptr::null(), &mut previous[i]);
            }
            if STATE.set(State { path, previous }).is_err() {
                return;
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut action.sa_mask);
            for (sig, _) in SIGNALS {
                libc::sigaction(sig, &action, std::ptr::null_mut());
            }
        }
    }

    extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
        let Some(state) = STATE.get() else { return };
        let index = SIGNALS.iter().position(|(s, _)| *s == sig);
        // SAFETY: async-signal-safe calls only (open, write, close, time,
        // sigaction); `info` comes from the kernel.
        unsafe {
            let fd = libc::open(
                state.path.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            );
            if fd >= 0 {
                if let Some(report) = REPORT.get() {
                    write_all(fd, report.header.as_bytes());
                }
                let mut buf = Buf::new();
                let _ = buf.write_str("time: ");
                put_utc(&mut buf, libc::time(std::ptr::null_mut()).max(0) as u64);
                let name = index.map(|i| SIGNALS[i].1).unwrap_or("?");
                let _ = write!(buf, " UTC\nkind: signal {name} ({sig})");
                if !info.is_null() && sig != libc::SIGABRT {
                    let _ = write!(buf, ", address {:#x}", fault_address(&*info));
                }
                let _ = buf.write_str(
                    "\nnote: the system's crash report has the backtrace \
                     (macOS: ~/Library/Logs/DiagnosticReports, Console.app)\n\nrecent log:\n",
                );
                write_all(fd, buf.bytes());
                match super::recent() {
                    Some(recent) => {
                        for line in recent.iter() {
                            write_all(fd, line.as_bytes());
                            write_all(fd, b"\n");
                        }
                    }
                    None => write_all(fd, b"(log busy)\n"),
                }
                write_all(fd, b"\n\n");
                libc::close(fd);
            }
            match index {
                Some(i) => libc::sigaction(sig, &state.previous[i], std::ptr::null_mut()),
                None => {
                    let mut default: libc::sigaction = std::mem::zeroed();
                    default.sa_sigaction = libc::SIG_DFL;
                    libc::sigaction(sig, &default, std::ptr::null_mut())
                }
            };
            // Signals sent by kill()/raise() don't repeat on return.
            if !info.is_null() && (*info).si_code <= 0 && sig != libc::SIGABRT {
                libc::raise(sig);
            }
        }
    }

    #[cfg(target_vendor = "apple")]
    fn fault_address(info: &libc::siginfo_t) -> usize {
        info.si_addr as usize
    }

    #[cfg(not(target_vendor = "apple"))]
    fn fault_address(info: &libc::siginfo_t) -> usize {
        // SAFETY: only read for the fault signals, where the field is set.
        unsafe { info.si_addr() as usize }
    }

    unsafe fn write_all(fd: libc::c_int, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let n = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
            if n <= 0 {
                return;
            }
            bytes = &bytes[n as usize..];
        }
    }
}

#[cfg(windows)]
mod native {
    //! Unhandled SEH exceptions (access violations, stack overflows, ...).
    //! The filter runs on the faulting thread, writes the report and lets
    //! Windows Error Reporting carry on. Rust's own aborts use __fastfail and
    //! skip it; panics are covered by the panic hook.

    use std::sync::OnceLock;

    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetUnhandledExceptionFilter, EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };

    static PREVIOUS: OnceLock<LPTOP_LEVEL_EXCEPTION_FILTER> = OnceLock::new();

    const STACK_OVERFLOW: u32 = 0xC000_00FD;

    pub fn install() {
        // SAFETY: registers a plain function as the process-wide filter.
        let previous = unsafe { SetUnhandledExceptionFilter(Some(filter)) };
        let _ = PREVIOUS.set(previous);
    }

    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        // SAFETY: Windows passes valid pointers to the filter.
        let (code, address) = unsafe {
            match info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) {
                Some(r) => (r.ExceptionCode as u32, r.ExceptionAddress as usize),
                None => (0, 0),
            }
        };
        let thread = std::thread::current();
        let mut body = format!(
            "kind: exception {code:#010x} ({})\nthread: {}\naddress: {address:#x}{}\n",
            name(code),
            thread.name().unwrap_or("unnamed"),
            module_offset(address),
        );
        // No stack left to walk one after an overflow.
        if code != STACK_OVERFLOW {
            body.push_str(&format!("\nbacktrace:\n{}\n", std::backtrace::Backtrace::force_capture()));
        }
        if let Some(path) = super::write_report(&body) {
            log::error!("crash {code:#010x} at {address:#x} (report: {})", path.display());
        }
        match PREVIOUS.get().copied().flatten() {
            // SAFETY: the filter that was installed before ours.
            Some(previous) => unsafe { previous(info) },
            None => EXCEPTION_CONTINUE_SEARCH,
        }
    }

    fn name(code: u32) -> &'static str {
        match code {
            0xC000_0005 => "access violation",
            0xC000_001D => "illegal instruction",
            0xC000_0094 => "integer division by zero",
            0xC000_00FD => "stack overflow",
            0xC000_0374 => "heap corruption",
            0xC000_0409 => "stack buffer overrun",
            0x8000_0003 => "breakpoint",
            _ => "unknown",
        }
    }

    /// ` (vnc-beach.exe+0x1234)` for symbolizing with addr2line.
    fn module_offset(address: usize) -> String {
        let mut module = std::ptr::null_mut();
        // SAFETY: looks up the module containing `address` without taking a
        // reference; the name buffer is large enough for MAX_PATH.
        unsafe {
            if GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                address as *const u16,
                &mut module,
            ) == 0
            {
                return String::new();
            }
            let mut name = [0u16; 520];
            let len = GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) as usize;
            let path = String::from_utf16_lossy(&name[..len]);
            let file = path.rsplit('\\').next().unwrap_or(&path);
            format!(" ({file}+{:#x})", address - module as usize)
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod native {
    pub fn install() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc() {
        assert_eq!(utc_time(0), "1970-01-01 00:00:00");
        assert_eq!(utc_time(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(utc_time(1_793_973_787), "2026-11-06 14:03:07");
        assert_eq!(file_time(1_793_973_787), "20261106-140307");
    }

    #[test]
    fn prunes_old_reports_only() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            let path = dir.path().join(format!("crash-2026010{i}-000000-1.txt"));
            std::fs::write(&path, "x").unwrap();
            let time = UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + i);
            std::fs::File::options().write(true).open(&path).unwrap().set_modified(time).unwrap();
        }
        std::fs::write(dir.path().join("other.txt"), "keep").unwrap();
        prune(dir.path(), 2);
        let left: Vec<String> = reports(dir.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["crash-20260104-000000-1.txt", "crash-20260103-000000-1.txt"]);
        assert!(dir.path().join("other.txt").exists());
    }

    #[test]
    fn buffer_cuts_instead_of_failing() {
        let mut buf = Buf::new();
        for _ in 0..100 {
            let _ = buf.write_str("0123456789");
        }
        assert_eq!(buf.bytes().len(), 512);
    }
}
