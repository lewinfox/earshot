//! A log file per run, so there's something to read after a crash or a freeze.
//!
//! Everything written to stderr (ours, and the analysis workers', which inherit it)
//! still reaches the terminal and is also copied, a line at a time with the time, to
//! `<data folder>/logs/earshot-<UTC time>.log`. The newest [`KEEP`] logs are kept.
//! A panic writes its message and a backtrace there; a memory watch notes when the
//! app grows by another [`STEP`] or the machine runs low (the kernel's out-of-memory
//! kill can end the whole desktop session, so it's worth seeing coming).
//!
//! Linux only for the copy (it moves fd 2 onto a pipe); elsewhere stderr is left as is.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The log file, once started, and when.
static LOG: OnceLock<(Mutex<File>, Instant)> = OnceLock::new();
/// The terminal stderr was, once it's copied to the log.
static TERMINAL: OnceLock<Mutex<File>> = OnceLock::new();

/// How many logs to keep.
const KEEP: usize = 20;
/// The app's memory growth worth a line.
const STEP: u64 = 512 << 20;
/// Free memory below this is worth a line (once a minute at most).
const LOW: u64 = 1 << 30;

/// Start logging into `data_dir/logs`. Returns the log's path, or why not.
pub fn start(data_dir: &Path) -> Result<PathBuf, String> {
    let dir = data_dir.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    prune(&dir);
    let path = dir.join(format!("earshot-{}.log", stamp(SystemTime::now(), true)));
    let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = LOG.set((Mutex::new(file), Instant::now()));
    tee_stderr()?;
    install_panic_hook();
    eprintln!(
        "earshot {} started: pid {}, log {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        path.display()
    );
    watch_memory();
    Ok(path)
}

/// A line in the log file only (not the terminal): detail worth having after a
/// crash, like which track each analysis was on.
pub fn note(line: &str) {
    write_line(line.as_bytes());
}

/// A line to the terminal, past the copy to the log.
fn to_terminal(line: &[u8]) {
    if let Some(t) = TERMINAL.get() {
        let mut t = t.lock().unwrap_or_else(|e| e.into_inner());
        let _ = t.write_all(line);
        let _ = t.write_all(b"\n");
    }
}

/// A line into the log file, stamped with the time.
fn write_line(line: &[u8]) {
    let Some((file, started)) = LOG.get() else {
        return;
    };
    // A panic while it was held leaves the file as good as it was.
    let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
    let _ = write!(
        f,
        "{} +{:.3} ",
        stamp(SystemTime::now(), false),
        started.elapsed().as_secs_f64()
    );
    let _ = f.write_all(line);
    let _ = f.write_all(b"\n");
    let _ = f.flush();
}

/// Keep the newest `KEEP - 1` logs, making room for this run's.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("earshot-") && n.ends_with(".log"))
        })
        .collect();
    // The names sort by time.
    logs.sort();
    let over = logs.len().saturating_sub(KEEP - 1);
    for p in &logs[..over] {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(target_os = "linux")]
fn tee_stderr() -> Result<(), String> {
    let mut fds = [0; 2];
    // SAFETY: plain fd calls; `fds` has room for the two ends.
    let terminal = unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err("can't make a pipe for the log".into());
        }
        let terminal = libc::dup(2);
        if terminal < 0 || libc::dup2(fds[1], 2) < 0 {
            return Err("can't redirect stderr to the log".into());
        }
        libc::close(fds[1]);
        terminal
    };
    use std::os::fd::FromRawFd;
    // SAFETY: both fds are ours alone from here on.
    let (pipe, terminal) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(terminal)) };
    let _ = TERMINAL.set(Mutex::new(terminal));
    std::thread::Builder::new()
        .name("earshot-log".into())
        .spawn(move || {
            for line in BufReader::new(pipe).split(b'\n').map_while(Result::ok) {
                to_terminal(&line);
                write_line(&line);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn tee_stderr() -> Result<(), String> {
    Err("logging to a file is only set up on Linux".into())
}

/// A panic's message, where it happened and a backtrace: straight into the log file
/// (the app may end before the copy from stderr catches up), and on stderr.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let backtrace = std::backtrace::Backtrace::force_capture();
        let msg = format!("PANIC in thread '{name}': {info}\n{backtrace}");
        if LOG.get().is_some() && TERMINAL.get().is_some() {
            note(&msg);
            to_terminal(msg.as_bytes());
        } else {
            eprintln!("{msg}");
        }
    }));
}

/// Every 10 s: a line when the app has grown by another `STEP`, or when the machine
/// is low on memory.
fn watch_memory() {
    let _ = std::thread::Builder::new()
        .name("earshot-memwatch".into())
        .spawn(|| {
            let mut logged = 0;
            let mut low_at: Option<Instant> = None;
            loop {
                let ours = status_kb("/proc/self/status", "VmRSS:").unwrap_or(0) * 1024;
                let free = status_kb("/proc/meminfo", "MemAvailable:").map(|kb| kb * 1024);
                if ours >= logged + STEP {
                    logged = ours - ours % STEP;
                    eprintln!("memory: earshot uses {} MB", ours >> 20);
                }
                if let Some(free) = free.filter(|f| *f < LOW)
                    && low_at.is_none_or(|t| t.elapsed() > Duration::from_secs(60))
                {
                    low_at = Some(Instant::now());
                    eprintln!(
                        "memory: the system has only {} MB free (earshot uses {} MB)",
                        free >> 20,
                        ours >> 20
                    );
                }
                std::thread::sleep(Duration::from_secs(10));
            }
        });
}

/// A `key: <n> kB` line's number from a /proc file.
fn status_kb(file: &str, key: &str) -> Option<u64> {
    std::fs::read_to_string(file)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix(key))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()
}

/// UTC as `2026-10-13 00:14:46` (or `2026-10-13T00-14-46Z` for a file name).
fn stamp(t: SystemTime, file_name: bool) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days since 1970 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, mi, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    if file_name {
        format!("{y:04}-{m:02}-{d:02}T{h:02}-{mi:02}-{s:02}Z")
    } else {
        format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_dates() {
        let t = UNIX_EPOCH + Duration::from_secs(1_791_846_886); // 2026-10-12 23:14:46 UTC
        assert_eq!(stamp(t, false), "2026-10-12 23:14:46");
        assert_eq!(stamp(t, true), "2026-10-12T23-14-46Z");
        assert_eq!(stamp(UNIX_EPOCH, false), "1970-01-01 00:00:00");
    }

    #[test]
    fn old_logs_are_pruned() {
        let dir = std::env::temp_dir().join(format!("earshot-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..KEEP + 5 {
            File::create(dir.join(format!("earshot-2026-01-01T00-00-{i:02}Z.log"))).unwrap();
        }
        File::create(dir.join("other.txt")).unwrap();
        prune(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP); // KEEP - 1 logs, and other.txt
        assert_eq!(left[0], "earshot-2026-01-01T00-00-06Z.log");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
