//! Messages for the user, shown in the bottom bar until dismissed or for
//! [`SHOW_FOR`]: what happened that they should know about (a USB stick that's gone,
//! say). Raised by the app (`EarshotApp::notify`).

use std::time::{Duration, Instant};

/// How long a message shows unless dismissed.
pub const SHOW_FOR: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Amber: worth knowing.
    Info,
    /// Red: something didn't work.
    Warning,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub id: u64,
    pub level: Level,
    pub text: String,
    /// When it goes away by itself.
    pub until: Instant,
}

/// The messages showing, oldest first.
#[derive(Default)]
pub struct Notices {
    list: Vec<Notice>,
    next: u64,
}

impl Notices {
    pub fn push(&mut self, level: Level, text: impl Into<String>, now: Instant) {
        self.next += 1;
        self.list.push(Notice {
            id: self.next,
            level,
            text: text.into(),
            until: now + SHOW_FOR,
        });
    }

    pub fn dismiss(&mut self, id: u64) {
        self.list.retain(|n| n.id != id);
    }

    /// Drop the ones whose time is up; returns when the next one is.
    pub fn expire(&mut self, now: Instant) -> Option<Instant> {
        self.list.retain(|n| n.until > now);
        self.list.iter().map(|n| n.until).min()
    }

    pub fn shown(&self) -> &[Notice] {
        &self.list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissed_or_timed_out() {
        let mut n = Notices::default();
        let t = Instant::now();
        n.push(Level::Info, "one", t);
        n.push(Level::Warning, "two", t + Duration::from_secs(5));
        n.dismiss(n.shown()[1].id);
        assert_eq!(n.shown().len(), 1);
        assert_eq!(n.expire(t + Duration::from_secs(9)), Some(t + SHOW_FOR));
        assert_eq!(n.expire(t + SHOW_FOR), None);
        assert!(n.shown().is_empty());
    }
}
