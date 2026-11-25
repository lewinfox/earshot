//! Hearing when drives come and go on macOS, rather than looking for them over and
//! over: a kqueue the kernel wakes when a filesystem is mounted or unmounted, or
//! `/Volumes` (where macOS mounts sticks) gains or loses a folder. Either is news;
//! both are asked for, so one the system won't give doesn't matter.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

// What the filesystem filter reports (`sys/mount.h`): a filesystem was mounted,
// unmounted, or changed.
const VQ_MOUNT: u32 = 0x0008;
const VQ_UNMOUNT: u32 = 0x0010;
const VQ_UPDATE: u32 = 0x0100;

/// Where macOS mounts sticks.
const VOLUMES: &std::ffi::CStr = c"/Volumes";

/// Wakes when a drive comes or goes ([`News::wait`]).
pub struct News {
    queue: OwnedFd,
    /// `/Volumes`, open only to be told of changes to it (kept open while watched).
    _volumes: Option<OwnedFd>,
}

impl News {
    /// `None` if the kernel gives neither kind of news: look every so often instead.
    pub fn new() -> Option<News> {
        // SAFETY: no arguments; a valid descriptor, or -1.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return None;
        }
        // SAFETY: a descriptor just made, owned from here on.
        let queue = unsafe { OwnedFd::from_raw_fd(kq) };
        // SAFETY: a valid C string; a valid descriptor, or -1.
        let dir = unsafe { libc::open(VOLUMES.as_ptr(), libc::O_EVTONLY) };
        // SAFETY: as above.
        let volumes = (dir >= 0).then(|| unsafe { OwnedFd::from_raw_fd(dir) });

        let ask = |ident: usize, filter: i16, fflags: u32| {
            let change = libc::kevent {
                ident,
                filter,
                // (Cleared once reported: each change is told once.)
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: one valid change, no events wanted back, no timeout.
            let r = unsafe {
                libc::kevent(
                    queue.as_raw_fd(),
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            r == 0
        };
        let mounts = ask(0, libc::EVFILT_FS, VQ_MOUNT | VQ_UNMOUNT | VQ_UPDATE);
        let folder = volumes.as_ref().is_some_and(|d| {
            ask(
                d.as_raw_fd() as usize,
                libc::EVFILT_VNODE,
                libc::NOTE_WRITE | libc::NOTE_LINK | libc::NOTE_RENAME | libc::NOTE_DELETE,
            )
        });
        (mounts || folder).then_some(News {
            queue,
            _volumes: volumes,
        })
    }

    /// Wait up to `up_to` for a drive to come or go. True if one did.
    pub fn wait(&self, up_to: Duration) -> bool {
        let timeout = libc::timespec {
            tv_sec: up_to.as_secs() as libc::time_t,
            tv_nsec: up_to.subsec_nanos() as libc::c_long,
        };
        // SAFETY: plain data the kernel fills in.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: no changes, room for one event, a valid timeout.
        let n = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &mut event,
                1,
                &timeout,
            )
        };
        n > 0
    }
}
