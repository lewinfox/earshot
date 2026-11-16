//! Whether the machine is holding its CPU back: a low-power profile, or CPUs capped
//! well below their top speed. Held to a fraction of its speed, a laptop can take
//! longer to make a block of sound than the block lasts (the audio drops out, and the
//! system can end the app: `earshot_engine::realtime`), and analysis takes ten times
//! as long. Nothing in earshot is wrong then, so the top bar says what is.
//!
//! Linux only (it reads `/sys`, or asks `powerprofilesctl`); elsewhere there's never a
//! warning.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// CPUs held below this share of their top speed count as capped.
pub const CAPPED: f64 = 0.6;
/// How often the watch looks.
const TICK: Duration = Duration::from_secs(5);
/// Every this many looks it asks `powerprofilesctl` (which costs a process), on
/// machines with no profile in `/sys`.
const ASK_EVERY: u32 = 6;

/// The watch's latest finding (`None`: it isn't running).
static LATEST: Mutex<Option<Option<Warning>>> = Mutex::new(None);

/// With less memory than this left (kB: 1.5 GB), there's no room for the analysis
/// worker and its model.
pub const MEMORY_SHORT_KB: u64 = 1_500_000;

/// Whether the watch last found memory short (never, when it isn't running).
static MEMORY_SHORT: AtomicBool = AtomicBool::new(false);

/// `MemAvailable` from `/proc/meminfo`'s text, kB.
pub fn mem_available_kb(meminfo: &str) -> Option<u64> {
    let rest = meminfo.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Whether there's too little memory left to start the analysis worker, as the watch
/// last read it.
pub fn memory_short() -> bool {
    MEMORY_SHORT.load(Ordering::Relaxed)
}

/// What the system says about its power, as read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// The power profile's name (`low-power`, `balanced`, `performance`, …).
    pub profile: Option<String>,
    /// Each CPU's (or group of CPUs') speed limit now and its top speed, kHz.
    pub cpus: Vec<(u64, u64)>,
    /// Intel's limit on every CPU, percent of its top speed.
    pub max_perf_pct: Option<u32>,
}

/// The CPU is held back: for the top bar and the log.
#[derive(Clone, Debug, PartialEq)]
pub struct Warning {
    /// A few words, for a pill.
    pub text: String,
    /// What it means and what to do about it.
    pub detail: String,
}

/// Whether a power profile's name means low power: `low-power` is the kernel's name
/// (`/sys/firmware/acpi/platform_profile`), `power-saver` is `powerprofilesctl`'s.
pub fn is_low_power(profile: &str) -> bool {
    matches!(
        profile.trim().to_ascii_lowercase().as_str(),
        "low-power" | "power-saver"
    )
}

impl Reading {
    /// Read the files under `sys` (`/sys`). What isn't there stays empty.
    pub fn read(sys: &Path) -> Reading {
        let text = |p: &Path| {
            let s = std::fs::read_to_string(p).ok()?;
            Some(s.trim().to_string()).filter(|s| !s.is_empty())
        };
        let cpu = sys.join("devices/system/cpu");
        let mut cpus = vec![];
        if let Ok(entries) = std::fs::read_dir(cpu.join("cpufreq")) {
            for dir in entries.flatten().map(|e| e.path()) {
                let khz = |name: &str| text(&dir.join(name))?.parse::<u64>().ok();
                if let (Some(now), Some(top)) = (khz("scaling_max_freq"), khz("cpuinfo_max_freq")) {
                    cpus.push((now, top));
                }
            }
        }
        cpus.sort_unstable();
        Reading {
            profile: text(&sys.join("firmware/acpi/platform_profile")),
            cpus,
            max_perf_pct: text(&cpu.join("intel_pstate/max_perf_pct")).and_then(|s| s.parse().ok()),
        }
    }

    /// The fastest any CPU may go now and the fastest any can, kHz, if it's held
    /// below [`CAPPED`] of that. The fastest CPU is the one that counts: a machine
    /// with slower cores too (efficiency cores) isn't capped by having them.
    pub fn capped(&self) -> Option<(u64, u64)> {
        let top = self.cpus.iter().map(|c| c.1).max().filter(|t| *t > 0)?;
        let mut now = self.cpus.iter().map(|c| c.0).max()?;
        if let Some(pct) = self.max_perf_pct {
            now = now.min(top * u64::from(pct.min(100)) / 100);
        }
        ((now as f64) < top as f64 * CAPPED).then_some((now, top))
    }

    /// What to tell the user, if the CPU is held back.
    pub fn warning(&self) -> Option<Warning> {
        let low = self.profile.as_deref().filter(|p| is_low_power(p));
        let capped = self.capped();
        let ghz = |khz: u64| format!("{:.1} GHz", khz as f64 / 1e6);
        let speeds = capped.map(|(now, top)| format!("{} of {}", ghz(now), ghz(top)));
        let text = match (low, capped) {
            (Some(_), _) => "low-power mode".to_string(),
            (None, Some((now, _))) => format!("CPU capped at {}", ghz(now)),
            (None, None) => return None,
        };
        let what = match (low, &speeds) {
            (Some(p), Some(s)) => format!("The power profile is {p}: the CPUs are held to {s}."),
            (Some(p), None) => format!("The power profile is {p}: the CPU is held back."),
            (None, Some(s)) => format!("The CPUs are held to {s}."),
            (None, None) => String::new(),
        };
        Some(Warning {
            text,
            detail: format!(
                "{what} The sound may drop out and analysis is slow. Plug in, or pick \
                 the balanced or performance power profile."
            ),
        })
    }
}

/// The watch's latest finding: `None` when it isn't running (tests), `Some(None)`
/// when the CPU isn't held back.
pub fn latest() -> Option<Option<Warning>> {
    LATEST.lock().unwrap().clone()
}

/// What `powerprofilesctl get` says, for machines whose profile isn't in `/sys`.
/// `None` if it isn't installed or its daemon isn't running.
fn ask_powerprofilesctl() -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let out = std::process::Command::new("powerprofilesctl")
        .arg("get")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Start watching (every [`TICK`]): a reading for [`latest`], and a line in the log
/// when it changes. Nothing to watch off Linux.
pub fn watch() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("earshot-power".into())
        .spawn(|| {
            let sys = Path::new("/sys");
            // The profile `powerprofilesctl` last gave, and whether it's worth asking
            // again (not once it has failed: it isn't there).
            let mut asked: Option<String> = None;
            let mut ask = true;
            let mut logged: Option<Warning> = None;
            for tick in 0u32.. {
                let mut reading = Reading::read(sys);
                if reading.profile.is_none() && ask {
                    if tick % ASK_EVERY == 0 {
                        asked = ask_powerprofilesctl();
                        ask = asked.is_some();
                    }
                    reading.profile = asked.clone();
                }
                let warning = reading.warning();
                if warning != logged {
                    match &warning {
                        Some(w) => eprintln!("power: {}. {}", w.text, w.detail),
                        None => eprintln!("power: the CPU is no longer held back"),
                    }
                    logged = warning.clone();
                }
                *LATEST.lock().unwrap() = Some(warning);
                let short = std::fs::read_to_string("/proc/meminfo")
                    .ok()
                    .and_then(|t| mem_available_kb(&t))
                    .is_some_and(|kb| kb < MEMORY_SHORT_KB);
                if short != MEMORY_SHORT.swap(short, Ordering::Relaxed) {
                    if short {
                        eprintln!("power: less than 1.5 GB of memory is left");
                    } else {
                        eprintln!("power: there's memory to spare again");
                    }
                }
                std::thread::sleep(TICK);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(profile: Option<&str>, cpus: &[(u64, u64)], pct: Option<u32>) -> Reading {
        Reading {
            profile: profile.map(str::to_string),
            cpus: cpus.to_vec(),
            max_perf_pct: pct,
        }
    }

    #[test]
    fn reads_the_memory_left_from_meminfo() {
        let text = "MemTotal:       16000000 kB\nMemFree:          200000 kB\nMemAvailable:    1234567 kB\n";
        assert_eq!(mem_available_kb(text), Some(1_234_567));
        assert_eq!(mem_available_kb("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn low_power_profiles_by_either_name() {
        for p in ["low-power", "power-saver", "low-power\n", " Power-Saver "] {
            assert!(is_low_power(p), "{p:?}");
        }
        for p in [
            "balanced",
            "performance",
            "balanced-performance",
            "quiet",
            "cool",
            "",
        ] {
            assert!(!is_low_power(p), "{p:?}");
        }
    }

    #[test]
    fn the_low_power_profile_is_warned_about_with_the_speeds_it_holds_the_cpus_to() {
        // The laptop that was killed: every core held at 1.1 GHz of up to 4.9.
        let r = reading(Some("low-power"), &[(1_100_000, 4_900_000); 8], None);
        let w = r.warning().expect("a warning");
        assert_eq!(w.text, "low-power mode");
        assert!(
            w.detail.starts_with(
                "The power profile is low-power: the CPUs are held to 1.1 GHz of 4.9 GHz."
            ),
            "{}",
            w.detail
        );
        // The profile alone is enough (the cap may not show in the limits).
        let r = reading(Some("power-saver"), &[(4_900_000, 4_900_000)], None);
        assert_eq!(r.warning().expect("a warning").text, "low-power mode");
        assert_eq!(reading(Some("low-power"), &[], None).capped(), None);
    }

    #[test]
    fn capped_cpus_are_warned_about_whatever_the_profile() {
        let r = reading(Some("balanced"), &[(1_100_000, 4_900_000); 4], None);
        let w = r.warning().expect("a warning");
        assert_eq!(w.text, "CPU capped at 1.1 GHz");
        assert!(
            w.detail
                .starts_with("The CPUs are held to 1.1 GHz of 4.9 GHz.")
        );
        // Intel's percentage limit, with the per-CPU limits untouched.
        let r = reading(None, &[(4_000_000, 4_000_000)], Some(30));
        assert_eq!(r.capped(), Some((1_200_000, 4_000_000)));
        assert_eq!(
            r.warning().expect("a warning").text,
            "CPU capped at 1.2 GHz"
        );
    }

    #[test]
    fn a_machine_at_speed_has_no_warning() {
        assert_eq!(reading(None, &[], None).warning(), None, "nothing known");
        let full = [(4_900_000, 4_900_000); 4];
        assert_eq!(reading(Some("balanced"), &full, Some(100)).warning(), None);
        assert_eq!(reading(Some("performance"), &full, None).warning(), None);
        // A little below the top (turbo off, say) isn't "well below".
        let r = reading(None, &[(3_600_000, 4_900_000)], None);
        assert_eq!(r.warning(), None);
        // Fast cores at full speed, slow cores slower still: the fast ones count.
        let r = reading(
            None,
            &[(4_900_000, 4_900_000), (1_000_000, 3_600_000)],
            None,
        );
        assert_eq!(r.warning(), None);
        // Fast and slow cores all held down.
        let r = reading(
            None,
            &[(2_000_000, 4_900_000), (2_000_000, 3_600_000)],
            None,
        );
        assert_eq!(r.capped(), Some((2_000_000, 4_900_000)));
    }

    #[test]
    fn reads_the_profile_and_the_limits_from_sys() {
        let sys = std::env::temp_dir().join(format!("earshot-power-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sys);
        let write = |path: &str, text: &str| {
            let p = sys.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        assert_eq!(Reading::read(&sys), Reading::default(), "nothing there");
        write("firmware/acpi/platform_profile", "low-power\n");
        for (policy, now) in [("policy0", "1100000\n"), ("policy4", "900000\n")] {
            let dir = format!("devices/system/cpu/cpufreq/{policy}");
            write(&format!("{dir}/scaling_max_freq"), now);
            write(&format!("{dir}/cpuinfo_max_freq"), "4900000\n");
        }
        // One without a top speed, and one that isn't a number: left out.
        write("devices/system/cpu/cpufreq/policy8/scaling_max_freq", "1\n");
        write("devices/system/cpu/cpufreq/policy9/scaling_max_freq", "?\n");
        write(
            "devices/system/cpu/cpufreq/policy9/cpuinfo_max_freq",
            "4900000\n",
        );
        write("devices/system/cpu/intel_pstate/max_perf_pct", "22\n");
        let r = Reading::read(&sys);
        assert_eq!(
            r,
            reading(
                Some("low-power"),
                &[(900_000, 4_900_000), (1_100_000, 4_900_000)],
                Some(22)
            )
        );
        assert_eq!(r.capped(), Some((1_078_000, 4_900_000)));
        assert_eq!(r.warning().expect("a warning").text, "low-power mode");
        std::fs::remove_dir_all(&sys).unwrap();
    }
}
