//! Formatting a stick for DJ players: everything on it erased, then one FAT32
//! partition in an MBR partition table, the layout Rekordbox, Serato and every CDJ
//! read. On Linux through udisks (over D-Bus, with `gdbus`): a desktop user may
//! format a removable drive without a password, and udisks itself refuses the
//! computer's own disks. On macOS, `diskutil eraseDisk`.
//!
//! Before anything is erased, the drive must be a USB drive or memory card and
//! everything on it mounted where sticks are (`super::ROOTS`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::Drive;

/// How long the new partition gets to be mounted.
const MOUNT_WAIT: Duration = Duration::from_secs(10);

/// A FAT volume label: at most 11 characters, capitals, digits, spaces, `_` and `-`.
pub fn fat_label(s: &str) -> String {
    let l: String = s
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-'))
        .take(11)
        .collect();
    let l = l.trim();
    if l.is_empty() {
        "EARSHOT".into()
    } else {
        l.into()
    }
}

/// Erase `d` and make it one FAT32 partition called `label` (made a FAT label),
/// mounted again. Returns where. Blocking: a few seconds.
pub fn format(d: &Drive, label: &str) -> Result<PathBuf, String> {
    let label = fat_label(label);
    if cfg!(target_os = "macos") {
        mac(d, &label)
    } else {
        linux(d, &label)
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{cmd}: {}", err.trim()))
    }
}

// ---- Linux --------------------------------------------------------------------------

fn linux(d: &Drive, label: &str) -> Result<PathBuf, String> {
    let part = d
        .device
        .strip_prefix("/dev/")
        .ok_or("earshot can't tell which device the stick is")?;
    let disk = whole_disk(part)?;
    if !removable(&disk) {
        return Err(format!(
            "/dev/{disk} isn't a USB drive or memory card: earshot won't format it"
        ));
    }
    let mounts = fs::read_to_string("/proc/self/mounts").map_err(|e| e.to_string())?;
    let mounted = mounts_on(&mounts, &disk);
    if let Some((dev, at)) = mounted
        .iter()
        .find(|(_, at)| !super::ROOTS.iter().any(|r| at.starts_with(r)))
    {
        return Err(format!(
            "{dev} is mounted at {}: earshot only formats sticks",
            at.display()
        ));
    }
    for (dev, _) in &mounted {
        run(
            "udisksctl",
            &["unmount", "--no-user-interaction", "-b", dev],
        )?;
    }
    let no_ask = "'auth.no_user_interaction': <true>";
    let object = format!("/org/freedesktop/UDisks2/block_devices/{disk}");
    // A new, empty MBR partition table (the old one, and what it held, gone).
    gdbus(
        &object,
        "org.freedesktop.UDisks2.Block.Format",
        &["'dos'", &format!("{{{no_ask}}}")],
    )?;
    // One FAT32 partition (type 0x0c, FAT32 LBA) over all of it, from 1 MiB.
    let sectors: u64 = fs::read_to_string(format!("/sys/class/block/{disk}/size"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or("earshot can't tell how big the stick is")?;
    const MIB: u64 = 1 << 20;
    let size = (sectors * 512).saturating_sub(2 * MIB);
    let made = gdbus(
        &object,
        "org.freedesktop.UDisks2.PartitionTable.CreatePartitionAndFormat",
        &[
            &MIB.to_string(),
            &size.to_string(),
            "'0x0c'",
            "''",
            &format!("{{{no_ask}}}"),
            "'vfat'",
            &format!("{{'label': <'{label}'>, {no_ask}}}"),
        ],
    )?;
    // "(objectpath '/org/freedesktop/UDisks2/block_devices/sdb1',)"
    let new = made
        .split("block_devices/")
        .nth(1)
        .and_then(|s| s.split('\'').next())
        .ok_or_else(|| format!("udisks said: {}", made.trim()))?;
    let dev = format!("/dev/{new}");
    // The desktop may mount it first; if so, that's where it is.
    let tried = run("udisksctl", &["mount", "--no-user-interaction", "-b", &dev]);
    let start = Instant::now();
    loop {
        let mounts = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
        if let Some((_, at)) = mounts_on(&mounts, new).into_iter().next() {
            return Ok(at);
        }
        if start.elapsed() > MOUNT_WAIT {
            return Err(match tried {
                Err(e) => format!("Formatted, but it couldn't be mounted: {e}"),
                Ok(_) => "Formatted, but it didn't appear mounted".into(),
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn gdbus(object: &str, method: &str, args: &[&str]) -> Result<String, String> {
    let mut all = vec![
        "call",
        "--system",
        "--dest",
        "org.freedesktop.UDisks2",
        "--object-path",
        object,
        "--method",
        method,
        "--timeout",
        "600",
    ];
    all.extend_from_slice(args);
    run("gdbus", &all)
}

/// The disk a partition (`sdb1`) is on (`sdb`); a disk with no partition table is
/// its own.
fn whole_disk(part: &str) -> Result<String, String> {
    let sys = fs::canonicalize(format!("/sys/class/block/{part}"))
        .map_err(|e| format!("/dev/{part}: {e}"))?;
    let disk = if sys.join("partition").exists() {
        sys.parent().and_then(Path::file_name)
    } else {
        sys.file_name()
    };
    disk.map(|n| n.to_string_lossy().into_owned())
        .filter(|n| n.chars().all(|c| c.is_ascii_alphanumeric()))
        .ok_or_else(|| format!("/dev/{part}: which disk is it on?"))
}

/// A USB drive or memory card: on the USB or MMC bus, or marked removable.
fn removable(disk: &str) -> bool {
    let sys = fs::canonicalize(format!("/sys/class/block/{disk}")).unwrap_or_default();
    let on_bus = sys
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with("usb"))
        || sys.to_string_lossy().contains("/mmc");
    let flagged = fs::read_to_string(format!("/sys/class/block/{disk}/removable"))
        .is_ok_and(|s| s.trim() == "1");
    on_bus || flagged
}

/// What's mounted from `disk` or its partitions: (device, where).
fn mounts_on(mounts: &str, disk: &str) -> Vec<(String, PathBuf)> {
    let dev = format!("/dev/{disk}");
    super::parse_all_mounts(mounts)
        .into_iter()
        .filter(|(d, _, _)| {
            d.strip_prefix(&dev).is_some_and(|rest| {
                // sdb → sdb, sdb1; mmcblk0 → mmcblk0p1; not sdba.
                rest.is_empty()
                    || rest.chars().all(|c| c.is_ascii_digit())
                    || rest
                        .strip_prefix('p')
                        .is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))
            })
        })
        .map(|(d, at, _)| (d, at))
        .collect()
}

// ---- macOS --------------------------------------------------------------------------

fn mac(d: &Drive, label: &str) -> Result<PathBuf, String> {
    let info = run("diskutil", &["info", &d.path.to_string_lossy()])?;
    let field = |k: &str| {
        info.lines().find_map(|l| {
            let (key, value) = l.split_once(':')?;
            (key.trim() == k).then(|| value.trim().to_string())
        })
    };
    let whole = field("Part of Whole").ok_or("diskutil didn't say which disk it's on")?;
    if field("Device Location").as_deref() == Some("Internal") || whole == "disk0" {
        return Err(format!(
            "{whole} is inside this computer: earshot won't format it"
        ));
    }
    run(
        "diskutil",
        &[
            "eraseDisk",
            "FAT32",
            label,
            "MBRFormat",
            &format!("/dev/{whole}"),
        ],
    )?;
    let at = PathBuf::from("/Volumes").join(label);
    let start = Instant::now();
    while !at.is_dir() {
        if start.elapsed() > MOUNT_WAIT {
            return Err("Formatted, but it didn't appear in /Volumes".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_fat_labels() {
        assert_eq!(fat_label("my stick"), "MY STICK");
        assert_eq!(fat_label("Lewin's DJ USB!"), "LEWINS DJ U");
        assert_eq!(fat_label("  "), "EARSHOT");
        assert_eq!(fat_label("ünï"), "N");
    }

    #[test]
    fn a_disks_mounts_are_its_partitions_only() {
        let text = "\
/dev/sdb1 /media/me/A vfat rw 0 0
/dev/sdb2 /media/me/B vfat rw 0 0
/dev/sdba1 /media/me/C vfat rw 0 0
/dev/mmcblk0p1 /media/me/SD vfat rw 0 0
/dev/nvme0n1p2 / ext4 rw 0 0
";
        let at = |v: Vec<(String, PathBuf)>| v.into_iter().map(|(d, _)| d).collect::<Vec<_>>();
        assert_eq!(at(mounts_on(text, "sdb")), ["/dev/sdb1", "/dev/sdb2"]);
        assert_eq!(at(mounts_on(text, "mmcblk0")), ["/dev/mmcblk0p1"]);
        assert!(mounts_on(text, "sdc").is_empty());
    }
}
