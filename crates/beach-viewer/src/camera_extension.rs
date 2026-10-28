//! Installs the VNC Beach camera extension (`apps/macos-camera`) through the
//! SystemExtensions framework. macOS asks the user to allow it in System
//! Settings once; afterwards video apps list a camera called "VNC Beach" that
//! shows the webcam of a connected viewer (`beach_server::camera`).

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{define_class, msg_send, AllocAnyThread};

const EXTENSION_ID: &str = "at.schlossers.vnc-beach.camera";

#[link(name = "SystemExtensions", kind = "framework")]
extern "C" {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// macOS waits for the user in System Settings.
    NeedsApproval,
    Done,
    Failed(String),
}

/// Where the delegate reports; one request at a time.
static REPORT: Mutex<Option<Sender<Progress>>> = Mutex::new(None);

fn report(progress: Progress) {
    log::info!("camera extension: {progress:?}");
    if let Some(tx) = REPORT.lock().unwrap().as_ref() {
        let _ = tx.send(progress);
    }
}

/// `-[NSString UTF8String]` of any NSString, or of `description`.
unsafe fn string(obj: *mut AnyObject) -> String {
    if obj.is_null() {
        return String::new();
    }
    let utf8: *const std::ffi::c_char = msg_send![obj, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VNCBeachCameraExtensionDelegate"]
    struct Delegate;

    impl Delegate {
        #[unsafe(method(request:actionForReplacingExtension:withExtension:))]
        fn replace(&self, _request: *mut AnyObject, _old: *mut AnyObject, _new: *mut AnyObject) -> isize {
            1 // OSSystemExtensionReplacementActionReplace: always take the bundled version
        }

        #[unsafe(method(requestNeedsUserApproval:))]
        fn needs_approval(&self, _request: *mut AnyObject) {
            report(Progress::NeedsApproval);
        }

        #[unsafe(method(request:didFinishWithResult:))]
        fn finished(&self, _request: *mut AnyObject, _result: isize) {
            report(Progress::Done);
        }

        #[unsafe(method(request:didFailWithError:))]
        fn failed(&self, _request: *mut AnyObject, error: *mut AnyObject) {
            let message = unsafe {
                let text: *mut AnyObject = msg_send![error, localizedDescription];
                string(text)
            };
            report(Progress::Failed(message));
        }
    }
);

/// Whether this app bundle carries the extension. Builds without it
/// (no provisioning profile at build time) don't offer the camera.
pub fn bundled() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            let contents = exe.parent()?.parent()?.to_path_buf();
            Some(
                contents
                    .join("Library/SystemExtensions")
                    .join(format!("{EXTENSION_ID}.systemextension"))
                    .is_dir(),
            )
        })
        .unwrap_or(false)
}

/// Whether the virtual camera is installed and running.
pub fn installed() -> bool {
    beach_server::camera::installed()
}

/// Asks macOS to install (or update) the extension. Progress arrives on the
/// receiver; `NeedsApproval` may come before `Done`.
pub fn activate() -> Receiver<Progress> {
    let (tx, rx) = channel();
    *REPORT.lock().unwrap() = Some(tx.clone());
    let (Some(request_class), Some(manager_class), Some(string_class)) = (
        AnyClass::get(c"OSSystemExtensionRequest"),
        AnyClass::get(c"OSSystemExtensionManager"),
        AnyClass::get(c"NSString"),
    ) else {
        let _ = tx.send(Progress::Failed("SystemExtensions nicht verfügbar".into()));
        return rx;
    };
    let queue = dispatch2::DispatchQueue::global_queue(dispatch2::GlobalQueueIdentifier::QualityOfService(
        dispatch2::DispatchQoS::Default,
    ));
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
    unsafe {
        let id = std::ffi::CString::new(EXTENSION_ID).unwrap();
        let identifier: *mut AnyObject = msg_send![string_class, stringWithUTF8String: id.as_ptr()];
        let request: *mut AnyObject = msg_send![
            request_class,
            activationRequestForExtension: identifier,
            queue: &*queue as *const dispatch2::DispatchQueue as *mut AnyObject
        ];
        let _: () = msg_send![request, setDelegate: &*delegate];
        let manager: *mut AnyObject = msg_send![manager_class, sharedManager];
        let _: () = msg_send![manager, submitRequest: request];
    }
    // The request keeps only a weak reference to its delegate.
    std::mem::forget(delegate);
    rx
}

/// `--install-camera`: activates the extension and waits for the outcome.
pub fn install_and_wait() -> anyhow::Result<()> {
    anyhow::ensure!(bundled(), "this build has no camera extension");
    let rx = activate();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(300)) {
            Ok(Progress::NeedsApproval) => {
                println!("Allow \"VNC Beach\" in System Settings > General > Login Items & Extensions > Camera Extensions");
            }
            Ok(Progress::Done) => return Ok(()),
            Ok(Progress::Failed(message)) => anyhow::bail!("activation failed: {message}"),
            Err(_) => anyhow::bail!("no answer from the system extension manager"),
        }
    }
}
