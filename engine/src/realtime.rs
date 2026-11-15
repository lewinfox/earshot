//! Surviving the real-time limit. The audio thread runs at real-time priority, and
//! Linux limits how long a real-time thread may run without a break (`RLIMIT_RTTIME`):
//! past the soft limit the process is sent SIGXCPU, and past the hard one (200 ms with
//! rtkit) it's killed. A machine too slow to make a block of sound in the time the
//! block lasts never gives the audio thread a break, so that is what happens.
//!
//! SIGXCPU's default action kills the process too. [`install`] handles it instead: the
//! handler only counts (the signal goes to the process, so it lands on whichever
//! thread). Each audio thread keeps an [`Overrun`] and asks it at the start of every
//! callback; when the count has moved, the thread drops itself to normal scheduling,
//! which the limit doesn't apply to. The sound may drop out, but the app lives.
//!
//! A stream opened later runs on a new thread, which the sound system makes real-time
//! as usual (setting the limit again): reopening the sound card is how to get real
//! time back.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

/// Times SIGXCPU has arrived.
static OVERRUNS: AtomicU32 = AtomicU32::new(0);

/// Handle SIGXCPU from now on (see the module). Safe to call again.
pub fn install() {
    #[cfg(target_os = "linux")]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: `sa` is zeroed and then filled in as sigaction(2) wants; the
            // handler is async-signal-safe (see `on_sigxcpu`).
            let failed = unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_sigxcpu as extern "C" fn(libc::c_int) as usize;
                // Whatever call the signal interrupts carries on.
                sa.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut sa.sa_mask);
                libc::sigaction(libc::SIGXCPU, &sa, std::ptr::null_mut()) != 0
            };
            if failed {
                eprintln!(
                    "audio: can't handle SIGXCPU ({}): running past the real-time limit \
                     will end earshot",
                    std::io::Error::last_os_error()
                );
            }
        });
    }
}

/// The SIGXCPU handler. It may run on any thread, in the middle of anything, so it
/// does the one thing that is safe there: add to an atomic (no allocation, no lock,
/// no I/O, `errno` untouched).
#[cfg(target_os = "linux")]
extern "C" fn on_sigxcpu(_signal: libc::c_int) {
    OVERRUNS.fetch_add(1, Relaxed);
}

/// One audio thread's view of the real-time limit: made when its stream is built,
/// asked at the start of each callback.
pub struct Overrun {
    /// The count when this thread last looked.
    seen: u32,
}

impl Default for Overrun {
    fn default() -> Self {
        Overrun::new()
    }
}

impl Overrun {
    /// Signals from before now are an earlier stream's business.
    pub fn new() -> Overrun {
        Overrun {
            seen: OVERRUNS.load(Relaxed),
        }
    }

    /// Call at the start of a callback, on the audio thread. If the process has run
    /// past the real-time limit since the last call, drops this thread to normal
    /// scheduling and returns true. Real-time safe: an atomic load, and one system
    /// call the time it happens.
    pub fn check(&mut self) -> bool {
        let now = OVERRUNS.load(Relaxed);
        if now == self.seen {
            return false;
        }
        self.seen = now;
        demote_current_thread();
        true
    }
}

/// Put the calling thread on the normal scheduler (which any thread may always do).
#[cfg(target_os = "linux")]
fn demote_current_thread() {
    // SAFETY: a zeroed sched_param is priority 0, the only one SCHED_OTHER takes; the
    // call changes this thread's scheduling and nothing else.
    unsafe {
        let param: libc::sched_param = std::mem::zeroed();
        libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_OTHER, &param);
    }
}

#[cfg(not(target_os = "linux"))]
fn demote_current_thread() {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn policy() -> libc::c_int {
        let mut policy = 0;
        // SAFETY: both out-pointers are valid for the call.
        unsafe {
            let mut param: libc::sched_param = std::mem::zeroed();
            libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut param);
        }
        policy
    }

    /// Make this thread real-time (SCHED_FIFO) with the limits rtkit sets: SIGXCPU
    /// after `soft_us` without a break, SIGKILL after `hard_us`. False if the system
    /// won't allow it.
    fn go_real_time(soft_us: u64, hard_us: u64) -> bool {
        // SAFETY: plain system calls on this thread and this process's limits.
        unsafe {
            let limit = libc::rlimit {
                rlim_cur: soft_us,
                rlim_max: hard_us,
            };
            if libc::setrlimit(libc::RLIMIT_RTTIME, &limit) != 0 {
                return false;
            }
            let param = libc::sched_param { sched_priority: 1 };
            libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) == 0
        }
    }

    /// One test, as the count is the whole process's: a signal raised here lands in the
    /// handler, the next check demotes the thread, and the one after has nothing new.
    /// Then, where the system allows real-time threads, the real thing: a real-time
    /// thread that never takes a break runs past the soft limit, is sent SIGXCPU by the
    /// kernel, and steps down before the hard limit would have killed the process.
    #[test]
    fn running_past_the_real_time_limit_demotes_the_thread_and_the_process_lives() {
        install();
        let mut overrun = Overrun::new();
        assert!(!overrun.check(), "nothing yet");
        // SAFETY: raises a signal this process handles.
        unsafe { libc::raise(libc::SIGXCPU) };
        assert!(overrun.check(), "seen by the next callback");
        assert_eq!(policy(), libc::SCHED_OTHER);
        assert!(!overrun.check(), "once per signal");
        // Another stream's thread, opened later, starts from here.
        assert!(!Overrun::new().check());

        let lived = std::thread::spawn(|| {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: the out-pointer is valid.
            unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut limit) };
            let hard = if limit.rlim_max == libc::RLIM_INFINITY {
                200_000
            } else {
                limit.rlim_max.min(200_000)
            };
            if !go_real_time(10_000, hard) {
                eprintln!("no real-time scheduling here: the kernel's SIGXCPU not tried");
                return None;
            }
            assert_eq!(policy(), libc::SCHED_FIFO);
            let mut overrun = Overrun::new();
            // Callbacks back to back, never waiting: each checks first, as the
            // stream's does. Without the handler the signal ends the process; without
            // the demotion the hard limit does, 200 ms in.
            let started = std::time::Instant::now();
            let mut demoted_after = None;
            while started.elapsed() < std::time::Duration::from_millis(500) {
                if overrun.check() && demoted_after.is_none() {
                    demoted_after = Some(started.elapsed());
                }
                std::hint::black_box((0..10_000u64).sum::<u64>());
            }
            // The kernel raised the soft limit by a second when it sent the signal;
            // put it back as found (it only ever counts for real-time threads).
            // SAFETY: restores this process's limit.
            unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) };
            Some((demoted_after, policy()))
        })
        .join()
        .expect("the thread ran to the end");
        match lived {
            Some((Some(after), policy)) => {
                assert!(after.as_millis() < 200, "demoted after {after:?}");
                assert_eq!(policy, libc::SCHED_OTHER);
            }
            // Had the kernel enforced the limit, the thread stepped down or the
            // process died: it doesn't enforce it here (some sandboxes).
            Some((None, _)) => eprintln!("no SIGXCPU from the kernel here: not tried"),
            None => {}
        }
    }
}
