//! Keep a deck's audio in RAM: the track and its stems, which the audio thread reads
//! every block. Swapped out, a read there waits on the kernel; when memory is short
//! that wait can run past the real-time limit and the system kills the app (SIGXCPU).
//!
//! A [`MemLock`] locks (`mlock`) the memory of what it holds while it lives, and
//! unlocks it when dropped. Locking is best effort: refused (over the limit, `ulimit
//! -l`), it says so once in the log and the audio plays as before.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use earshot_engine::decode::TrackAudio;
use earshot_engine::stems::StemAudio;

/// Logged a refusal already.
static REFUSED: AtomicBool = AtomicBool::new(false);

pub struct MemLock {
    /// Holds the memory, so it's there until it's unlocked.
    _owner: Arc<dyn Send + Sync>,
    ptr: usize,
    len: usize,
    locked: bool,
}

impl MemLock {
    pub fn track(audio: &Arc<TrackAudio>) -> Arc<MemLock> {
        let s = audio.samples.as_slice();
        MemLock::new(audio.clone(), s.as_ptr() as usize, std::mem::size_of_val(s))
    }

    pub fn stems(stems: &Arc<StemAudio>) -> Arc<MemLock> {
        let s = stems.raw();
        MemLock::new(stems.clone(), s.as_ptr() as usize, std::mem::size_of_val(s))
    }

    fn new(owner: Arc<dyn Send + Sync>, ptr: usize, len: usize) -> Arc<MemLock> {
        let locked = len > 0 && lock(ptr, len);
        Arc::new(MemLock {
            _owner: owner,
            ptr,
            len,
            locked,
        })
    }
}

impl Drop for MemLock {
    fn drop(&mut self) {
        if self.locked {
            unlock(self.ptr, self.len);
        }
    }
}

#[cfg(unix)]
fn lock(ptr: usize, len: usize) -> bool {
    // SAFETY: the range is a live allocation, kept alive by the MemLock's owner.
    let ok = unsafe { libc::mlock(ptr as *const libc::c_void, len) } == 0;
    if !ok && !REFUSED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "memory: couldn't keep {} MB of deck audio in RAM ({}); it may be swapped out",
            len >> 20,
            std::io::Error::last_os_error()
        );
    }
    ok
}

#[cfg(unix)]
fn unlock(ptr: usize, len: usize) {
    // SAFETY: as for `lock`; unlocking never touches the memory.
    unsafe { libc::munlock(ptr as *const libc::c_void, len) };
}

#[cfg(not(unix))]
fn lock(_: usize, _: usize) -> bool {
    let _ = &REFUSED;
    false
}

#[cfg(not(unix))]
fn unlock(_: usize, _: usize) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_is_locked_while_held() {
        let audio = Arc::new(TrackAudio {
            samples: vec![0.5; 1 << 16],
            sample_rate: 44_100,
        });
        let lock = MemLock::track(&audio);
        // The test machine's limit allows this much (64 KB is the smallest default).
        assert!(lock.locked);
        drop(lock);
    }
}
