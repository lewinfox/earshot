//! Keep the screen on while earshot is open: no dimming, no screensaver, no lock or
//! sleep from sitting idle. Mixing is mostly a controller's knobs and faders, which
//! the system doesn't count as someone being there.
//!
//! Best effort: where the system has nobody to ask, it says so in the log and carries
//! on. The request lasts until earshot exits (or crashes), and the system drops it by
//! itself then.
//!
//! - **Linux:** the desktop's `org.freedesktop.ScreenSaver` on the session D-Bus
//!   (KDE, GNOME, Xfce, Cinnamon, MATE …), else the desktop portal's `Inhibit`.
//!   Both last as long as the D-Bus connection, which is kept until exit. A bare
//!   Wayland compositor with neither (sway, say) isn't covered.
//! - **macOS:** a power assertion, as a video player takes (`pmset -g assertions`
//!   lists it).
//! - **Windows:** `SetThreadExecutionState`.

/// Why, for the system's own lists of who is keeping the screen on.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const REASON: &str = "DJing";

/// Ask the system to keep the screen on from now until earshot exits. Call it on the
/// main thread: Windows drops the request when the calling thread ends.
pub fn hold() {
    os::hold();
}

#[cfg(target_os = "linux")]
mod os {
    use super::REASON;
    use std::collections::HashMap;
    use zbus::blocking::Connection;
    use zbus::zvariant::Value;

    /// The portal's flag for "the session going idle".
    const PORTAL_IDLE: u32 = 8;

    pub fn hold() {
        // Its own thread: the bus may take a moment to answer, and the connection
        // has to be kept somewhere.
        let spawned = std::thread::Builder::new()
            .name("awake".into())
            .spawn(|| match ask() {
                Ok((conn, how)) => {
                    eprintln!("[init] screen: kept on ({how})");
                    // Kept until exit: the request goes when the connection does.
                    std::mem::forget(conn);
                }
                Err(e) => eprintln!("[init] screen: can't keep it on: {e}"),
            });
        if let Err(e) = spawned {
            eprintln!("[init] screen: can't keep it on: {e}");
        }
    }

    fn ask() -> Result<(Connection, &'static str), String> {
        let conn = Connection::session().map_err(|e| e.to_string())?;
        let saver = conn.call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "Inhibit",
            &("earshot", REASON),
        );
        let saver = match saver {
            Ok(_) => return Ok((conn, "org.freedesktop.ScreenSaver")),
            Err(e) => e,
        };
        let options: HashMap<&str, Value> = HashMap::from([("reason", Value::from(REASON))]);
        let portal = conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Inhibit"),
            "Inhibit",
            &("", PORTAL_IDLE, options),
        );
        match portal {
            Ok(_) => Ok((conn, "the desktop portal")),
            Err(e) => Err(format!("screensaver: {saver}; portal: {e}")),
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::REASON;
    use std::ffi::{CString, c_char, c_void};

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;
    const ASSERTION_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            s: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
    }

    pub fn hold() {
        // The display staying on keeps the machine awake too.
        let kind = CString::new("PreventUserIdleDisplaySleep").unwrap();
        let name = CString::new(format!("earshot: {REASON}")).unwrap();
        let mut id = 0u32;
        // SAFETY: both strings are NUL-terminated and live through the calls; the
        // CFStrings made from them are released after. The assertion itself is never
        // released: it's wanted until exit, when the system drops it.
        let status = unsafe {
            let kind = CFStringCreateWithCString(std::ptr::null(), kind.as_ptr(), UTF8);
            let name = CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), UTF8);
            if kind.is_null() || name.is_null() {
                eprintln!("[init] screen: can't keep it on: no CFString");
                return;
            }
            let status = IOPMAssertionCreateWithName(kind, ASSERTION_ON, name, &mut id);
            CFRelease(kind);
            CFRelease(name);
            status
        };
        if status == 0 {
            eprintln!("[init] screen: kept on (power assertion {id})");
        } else {
            eprintln!("[init] screen: can't keep it on: IOKit error {status:#x}");
        }
    }
}

#[cfg(windows)]
mod os {
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;
    const ES_CONTINUOUS: u32 = 0x8000_0000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    pub fn hold() {
        // SAFETY: a plain call, no pointers. It holds while this thread lives.
        let before = unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED)
        };
        if before == 0 {
            eprintln!("[init] screen: can't keep it on: SetThreadExecutionState failed");
        } else {
            eprintln!("[init] screen: kept on");
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod os {
    pub fn hold() {}
}
