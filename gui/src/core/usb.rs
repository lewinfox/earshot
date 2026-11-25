//! Hearing USB devices being plugged in and pulled out on macOS: IOKit tells a run
//! loop when a USB device appears or has gone. The MIDI watcher waits on it
//! (`midi::watch`), to look for controllers then rather than every so often.
//!
//! [`News`] belongs to the thread that makes it: the notifications arrive on that
//! thread's run loop, which [`News::wait`] runs.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

type Port = *mut c_void;
type Source = *mut c_void;
type RunLoop = *mut c_void;
type Dictionary = *mut c_void;
type CfString = *const c_void;
/// A Mach port's name: an IOKit object, or an iterator of them.
type Object = u32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IONotificationPortCreate(main_port: u32) -> Port;
    fn IONotificationPortDestroy(port: Port);
    fn IONotificationPortGetRunLoopSource(port: Port) -> Source;
    fn IOServiceMatching(class: *const c_char) -> Dictionary;
    fn IOServiceAddMatchingNotification(
        port: Port,
        kind: *const c_char,
        matching: Dictionary,
        callback: unsafe extern "C" fn(*mut c_void, Object),
        context: *mut c_void,
        iterator: *mut Object,
    ) -> i32;
    fn IOIteratorNext(iterator: Object) -> Object;
    fn IOObjectRelease(object: Object) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: CfString;
    fn CFRunLoopGetCurrent() -> RunLoop;
    fn CFRunLoopAddSource(run_loop: RunLoop, source: Source, mode: CfString);
    fn CFRunLoopRemoveSource(run_loop: RunLoop, source: Source, mode: CfString);
    fn CFRunLoopRunInMode(mode: CfString, seconds: f64, return_after_source: u8) -> i32;
}

/// A USB device, as IOKit classes them (since macOS 10.11).
const USB_DEVICE: &std::ffi::CStr = c"IOUSBHostDevice";
/// IOKit's notifications for a matching device appearing, and for one having gone
/// (`kIOFirstMatchNotification`, `kIOTerminatedNotification`).
const APPEARED: &std::ffi::CStr = c"IOServiceFirstMatch";
const GONE: &std::ffi::CStr = c"IOServiceTerminate";

/// Take every device out of an iterator IOKit handed over: that's what arms it to
/// tell of the next one.
///
/// # Safety
/// `iterator` must be an iterator IOKit gave (or 0, which has none).
unsafe fn drain(iterator: Object) {
    loop {
        // SAFETY: the caller's iterator; each object taken is released once.
        let device = unsafe { IOIteratorNext(iterator) };
        if device == 0 {
            return;
        }
        // SAFETY: as above.
        unsafe { IOObjectRelease(device) };
    }
}

/// IOKit's callback: a USB device appeared or went. `context` is the [`News`]'s flag.
unsafe extern "C" fn on_device(context: *mut c_void, iterator: Object) {
    // SAFETY: `context` is the flag `News::new` gave, which lives as long as the
    // notification port that calls this; the iterator is IOKit's own.
    unsafe {
        (*context.cast::<AtomicBool>()).store(true, Ordering::Relaxed);
        drain(iterator);
    }
}

/// Wakes when a USB device is plugged in or pulled out ([`News::wait`]).
pub struct News {
    port: Port,
    source: Source,
    run_loop: RunLoop,
    /// The two notifications' iterators, released with it.
    iterators: [Object; 2],
    /// Set by [`on_device`]. Boxed: IOKit holds its address.
    changed: Box<AtomicBool>,
}

impl News {
    /// `None` if IOKit won't give the notifications: look every so often instead.
    pub fn new() -> Option<News> {
        let changed = Box::new(AtomicBool::new(false));
        let context = std::ptr::from_ref::<AtomicBool>(&changed).cast_mut().cast();
        // SAFETY: 0 is the default main port. Each call takes valid, zero-ended
        // strings; `IOServiceAddMatchingNotification` takes over the dictionary it's
        // given (so one is made for each); `context` outlives the port (both are
        // the `News`'s, and the port is destroyed first).
        unsafe {
            let port = IONotificationPortCreate(0);
            if port.is_null() {
                return None;
            }
            let mut iterators = [0; 2];
            for (kind, iterator) in [APPEARED, GONE].into_iter().zip(&mut iterators) {
                let matching = IOServiceMatching(USB_DEVICE.as_ptr());
                let added = !matching.is_null()
                    && IOServiceAddMatchingNotification(
                        port,
                        kind.as_ptr(),
                        matching,
                        on_device,
                        context,
                        iterator,
                    ) == 0;
                if !added {
                    IONotificationPortDestroy(port);
                    return None;
                }
                // What's plugged in already isn't news; taking it arms the iterator.
                drain(*iterator);
            }
            let source = IONotificationPortGetRunLoopSource(port);
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
            Some(News {
                port,
                source,
                run_loop,
                iterators,
                changed,
            })
        }
    }

    /// Wait up to `up_to` for a USB device to come or go. True if one did. On the
    /// thread that made it.
    pub fn wait(&self, up_to: Duration) -> bool {
        // SAFETY: runs this thread's run loop, which has the port's source, for at
        // most that long, returning once a notification has been handled.
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, up_to.as_secs_f64(), 1) };
        self.changed.swap(false, Ordering::Relaxed)
    }
}

impl Drop for News {
    fn drop(&mut self) {
        // SAFETY: what `new` made, undone once, on its own thread: the source off
        // the run loop, the port destroyed (no more callbacks, so the flag can go
        // after), the iterators released.
        unsafe {
            CFRunLoopRemoveSource(self.run_loop, self.source, kCFRunLoopDefaultMode);
            IONotificationPortDestroy(self.port);
            for i in self.iterators {
                IOObjectRelease(i);
            }
        }
    }
}
