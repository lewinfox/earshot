//! What earshot is doing in the background, for the status line and the top bar's
//! jobs pill: each job (an export, an import, analysis…) registers here when it
//! starts ([`Jobs::start`]), or while it waits its turn ([`Jobs::queue`]),
//! says what it's doing and how far it's got as it goes ([`Job::say`],
//! [`Job::progress`]), and is gone when its [`Job`] is dropped, finished or not.
//! The list is shared between the UI and the jobs' threads; each change asks for
//! a repaint, so the status line keeps up.
//!
//! A job's own panel still gets its messages and result through its events: the
//! list is only what the status line shows.

use std::sync::{Arc, Mutex};

/// Asks the UI to draw again.
type Wake = Arc<dyn Fn() + Send + Sync>;

/// The jobs running now.
#[derive(Clone)]
pub struct Jobs {
    inner: Arc<Mutex<Inner>>,
    wake: Wake,
}

#[derive(Default)]
struct Inner {
    next: u64,
    /// In the order they started.
    list: Vec<Entry>,
}

struct Entry {
    id: u64,
    now: Running,
}

/// A job, as the status line shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Running {
    /// What it is: "Exporting to STICK".
    pub title: String,
    /// What it's doing now: "Writing Serato tags" (may be empty).
    pub message: String,
    /// How far through: (done, of).
    pub progress: Option<(u64, u64)>,
    /// Waiting its turn, not running yet.
    pub queued: bool,
}

impl Running {
    /// How far through, 0 to 1, when it's counted.
    pub fn fraction(&self) -> Option<f32> {
        self.progress
            .filter(|&(_, of)| of > 0)
            .map(|(n, of)| (n.min(of) as f64 / of as f64) as f32)
    }

    /// "Exporting to STICK: Writing Serato tags".
    pub fn text(&self) -> String {
        if self.message.is_empty() {
            format!("{}…", self.title)
        } else {
            format!("{}: {}", self.title, self.message)
        }
    }
}

impl Default for Jobs {
    /// Nothing to wake (tests, and before there's a window).
    fn default() -> Self {
        Jobs::new(|| {})
    }
}

impl Jobs {
    /// `wake` is called after every change.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Jobs {
        Jobs {
            inner: Arc::default(),
            wake: Arc::new(wake),
        }
    }

    /// A job called `title` is running until the returned [`Job`] is dropped.
    pub fn start(&self, title: impl Into<String>) -> Job {
        self.add(title.into(), false)
    }

    /// A job called `title` waiting its turn (listed after those running), until
    /// it's dropped.
    pub fn queue(&self, title: impl Into<String>) -> Job {
        self.add(title.into(), true)
    }

    fn add(&self, title: String, queued: bool) -> Job {
        let id = {
            let mut i = self.inner.lock().unwrap();
            i.next += 1;
            let id = i.next;
            i.list.push(Entry {
                id,
                now: Running {
                    title,
                    message: String::new(),
                    progress: None,
                    queued,
                },
            });
            id
        };
        (self.wake)();
        Job {
            jobs: self.clone(),
            id,
        }
    }

    /// The jobs: those running, oldest first, then those queued.
    pub fn list(&self) -> Vec<Running> {
        let i = self.inner.lock().unwrap();
        let mut out: Vec<Running> = i.list.iter().map(|e| e.now.clone()).collect();
        out.sort_by_key(|r| r.queued);
        out
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Running)) {
        {
            let mut i = self.inner.lock().unwrap();
            let Some(e) = i.list.iter_mut().find(|e| e.id == id) else {
                return;
            };
            let before = e.now.clone();
            f(&mut e.now);
            if e.now == before {
                return;
            }
        }
        (self.wake)();
    }
}

/// A job running: see [`Jobs::start`]. Dropping it ends it.
pub struct Job {
    jobs: Jobs,
    id: u64,
}

impl Job {
    /// What it's doing now. The count stays until [`Job::progress`] or
    /// [`Job::uncounted`] changes it.
    pub fn say(&self, message: impl Into<String>) {
        let message = message.into();
        self.jobs.update(self.id, |r| r.message = message);
    }

    /// How far it's got: `done` of `of`.
    pub fn progress(&self, done: u64, of: u64) {
        self.jobs.update(self.id, |r| r.progress = Some((done, of)));
    }

    /// Not counted (any more): a step with no count.
    pub fn uncounted(&self) {
        self.jobs.update(self.id, |r| r.progress = None);
    }

    /// A message from cratelib, which counts in its text ("Reading tracks 12/300"):
    /// the message, and its count when it has one (else none).
    pub fn report(&self, message: &str) {
        let progress = counted(message);
        let message = message.to_owned();
        self.jobs.update(self.id, |r| {
            r.message = message;
            r.progress = progress;
        });
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.jobs
            .inner
            .lock()
            .unwrap()
            .list
            .retain(|e| e.id != self.id);
        (self.jobs.wake)();
    }
}

/// The last "n/m" in `text`.
fn counted(text: &str) -> Option<(u64, u64)> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ':'))
        .rev()
        .find_map(|w| {
            let (n, m) = w.split_once('/')?;
            let (n, m): (u64, u64) = (n.parse().ok()?, m.parse().ok()?);
            (m > 0 && n <= m).then_some((n, m))
        })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn a_job_is_listed_until_dropped_and_each_change_wakes_the_ui() {
        let woken = Arc::new(AtomicUsize::new(0));
        let w = woken.clone();
        let jobs = Jobs::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
        });
        let a = jobs.start("Exporting");
        let b = jobs.start("Importing");
        a.say("Writing tags");
        a.progress(3, 12);
        a.progress(3, 12); // no change: no wake
        let list = jobs.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].text(), "Exporting: Writing tags");
        assert_eq!(list[0].fraction(), Some(0.25));
        assert_eq!(list[1].text(), "Importing…");
        assert_eq!(list[1].fraction(), None);
        let q = jobs.queue("Waiting");
        assert_eq!(jobs.list()[2].title, "Waiting");
        let c = jobs.start("Later");
        assert_eq!(jobs.list()[2].title, "Later", "queued ones last");
        drop((q, c));
        drop(a);
        assert_eq!(jobs.list()[0].title, "Importing");
        drop(b);
        assert!(jobs.list().is_empty());
        assert_eq!(woken.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn cratelibs_messages_count_by_their_last_n_of_m() {
        let jobs = Jobs::default();
        let j = jobs.start("Reading");
        j.report("Reading Mixxx tracks 1/2 (of 4/8)");
        assert_eq!(jobs.list()[0].progress, Some((4, 8)));
        j.report("Writing analysis files");
        assert_eq!(jobs.list()[0].progress, None);
        j.report("Looking in AC/DC");
        assert_eq!(jobs.list()[0].progress, None);
        j.report("odd 5/0");
        assert_eq!(jobs.list()[0].progress, None);
    }
}
