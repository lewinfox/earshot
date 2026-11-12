//! Where background work runs. In the app, each piece gets a thread of its own
//! ([`spawn`]) or a thread that takes messages one at a time ([`serve`]). A test can
//! have none of it run by itself instead ([`step_this_thread`]): work started from
//! the test's thread is queued, and runs on that thread when the test says so
//! ([`run_pending`]), so the test never waits for another thread.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

type Task = Box<dyn FnOnce() + Send>;

thread_local! {
    /// The work waiting to run on this thread, if it steps its background work.
    static QUEUE: RefCell<Option<VecDeque<Task>>> = const { RefCell::new(None) };
}

/// Run `f` in the background: on a thread named `name`, or, on a thread that steps
/// its background work, the next time it runs what's pending.
pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let task: Task = Box::new(f);
    let task = QUEUE.with(|q| match q.borrow_mut().as_mut() {
        Some(q) => {
            q.push_back(task);
            None
        }
        None => Some(task),
    });
    if let Some(task) = task {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(task)
            .expect("spawn a background thread");
    }
}

/// Whether this thread steps its background work ([`step_this_thread`]).
pub fn stepped() -> bool {
    QUEUE.with(|q| q.borrow().is_some())
}

/// From now on, background work started from this thread waits for
/// [`run_pending`]. For tests: each runs on its own thread.
#[cfg(test)]
pub fn step_this_thread() {
    QUEUE.with(|q| {
        q.borrow_mut().get_or_insert_with(VecDeque::new);
    });
}

/// Run the work waiting on this thread, and whatever that starts, until none is
/// left. Returns how many pieces ran.
#[cfg(test)]
pub fn run_pending() -> usize {
    let mut ran = 0;
    // (The queue isn't borrowed while a task runs: it may start more.)
    while let Some(task) = QUEUE.with(|q| q.borrow_mut().as_mut().and_then(VecDeque::pop_front)) {
        task();
        ran += 1;
    }
    ran
}

/// How many pieces of work are waiting on this thread.
#[cfg(test)]
pub fn pending() -> usize {
    QUEUE.with(|q| q.borrow().as_ref().map_or(0, VecDeque::len))
}

/// Somewhere to send a background worker its messages ([`serve`]). Cheap to clone;
/// the worker's thread ends when the last clone goes.
pub struct Mailbox<T>(Kind<T>);

enum Kind<T> {
    Thread(Sender<T>),
    /// Each message is a piece of stepped work.
    Stepped(Arc<Mutex<dyn FnMut(T) + Send>>),
}

impl<T> Clone for Mailbox<T> {
    fn clone(&self) -> Self {
        Mailbox(match &self.0 {
            Kind::Thread(tx) => Kind::Thread(tx.clone()),
            Kind::Stepped(f) => Kind::Stepped(f.clone()),
        })
    }
}

impl<T: Send + 'static> Mailbox<T> {
    /// Never blocks.
    pub fn send(&self, msg: T) {
        match &self.0 {
            Kind::Thread(tx) => {
                let _ = tx.send(msg);
            }
            Kind::Stepped(handle) => {
                let handle = handle.clone();
                spawn("stepped", move || (handle.lock().unwrap())(msg));
            }
        }
    }
}

/// A background worker that takes messages one at a time, in order: a thread named
/// `name` (which runs `on_start` first, e.g. to lower its priority), or, started from
/// a thread that steps its background work, a piece of that work for each message.
pub fn serve<T: Send + 'static>(
    name: &str,
    on_start: impl FnOnce() + Send + 'static,
    mut handle: impl FnMut(T) + Send + 'static,
) -> Mailbox<T> {
    if stepped() {
        return Mailbox(Kind::Stepped(Arc::new(Mutex::new(handle))));
    }
    let (tx, rx) = channel::<T>();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            on_start();
            for msg in rx {
                handle(msg);
            }
        })
        .expect("spawn a background thread");
    Mailbox(Kind::Thread(tx))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    use super::*;

    #[test]
    fn stepped_work_waits_until_it_is_run() {
        step_this_thread();
        let n = Arc::new(AtomicUsize::new(0));
        let again = n.clone();
        let m = n.clone();
        spawn("a", move || {
            m.fetch_add(1, Relaxed);
            // Work started by stepped work is stepped too.
            spawn("b", move || {
                again.fetch_add(10, Relaxed);
            });
        });
        let seen = n.clone();
        let mail = serve("c", || {}, move |by: usize| {
            seen.fetch_add(by, Relaxed);
        });
        mail.send(100);
        assert_eq!((n.load(Relaxed), pending()), (0, 2));
        assert_eq!(run_pending(), 3);
        assert_eq!((n.load(Relaxed), pending()), (111, 0));
    }

    #[test]
    fn a_thread_that_does_not_step_gets_real_threads() {
        assert!(!stepped());
        let (tx, rx) = channel();
        let mail = serve("d", || {}, move |n: usize| tx.send(n).unwrap());
        mail.send(7);
        assert_eq!(rx.recv().unwrap(), 7);
        let (tx, rx) = channel();
        spawn("e", move || tx.send(8).unwrap());
        assert_eq!(rx.recv().unwrap(), 8);
    }
}
