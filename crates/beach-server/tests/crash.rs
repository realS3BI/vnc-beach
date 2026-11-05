//! Real crashes in a child process (this test binary started again), so the
//! hooks are checked end to end.

use std::path::Path;
use std::process::Command;

const CHILD: &str = "BEACH_CRASH_TEST_CHILD";

/// The child side: installs the hooks in the directory from the environment
/// and crashes the requested way.
#[test]
fn child() {
    let Ok(kind) = std::env::var(CHILD) else { return };
    let dir = std::env::var("BEACH_CRASH_TEST_DIR").unwrap();
    beach_server::crash::init_logging(env_logger::Builder::new());
    beach_server::crash::install(Path::new(&dir), "9.9.9", "test");
    log::info!("about to crash: {kind}");
    match kind.as_str() {
        "panic" => {
            let _ = std::thread::Builder::new()
                .name("worker".into())
                .spawn(|| panic!("boom from the worker"))
                .unwrap()
                .join();
        }
        "segv" => unsafe {
            std::ptr::null_mut::<u8>().write_volatile(1);
        },
        _ => unreachable!(),
    }
}

fn crash(kind: &str) -> (std::process::ExitStatus, String) {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CHILD, kind)
        .env("BEACH_CRASH_TEST_DIR", dir.path())
        .status()
        .unwrap();
    let reports = beach_server::crash::reports(dir.path());
    assert_eq!(reports.len(), 1, "one report for {kind}");
    (status, std::fs::read_to_string(&reports[0]).unwrap())
}

#[test]
fn panic_in_a_thread_is_reported() {
    let (_, report) = crash("panic");
    assert!(report.contains("version: 9.9.9\nmode: test\n"), "{report}");
    assert!(report.contains("kind: panic\nthread: worker\nmessage: boom from the worker\n"), "{report}");
    assert!(report.contains("tests/crash.rs:") || report.contains("tests\\crash.rs:"), "{report}");
    assert!(report.contains("backtrace:\n"), "{report}");
    assert!(report.contains("INFO  crash: about to crash: panic"), "{report}");
}

#[cfg(unix)]
#[test]
fn segfault_is_reported_and_still_kills() {
    use std::os::unix::process::ExitStatusExt as _;
    let (status, report) = crash("segv");
    assert!(
        report.contains("kind: signal SIGSEGV") || report.contains("kind: signal SIGBUS"),
        "{report}"
    );
    assert!(report.contains("address 0x0"), "{report}");
    assert!(report.contains("about to crash: segv"), "{report}");
    assert!(matches!(status.signal(), Some(libc::SIGSEGV | libc::SIGBUS)), "{status:?}");
}
