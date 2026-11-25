//! USB sticks on Windows: which drive letters are sticks, how big and how full they
//! are, flushing what was written to one, and ejecting it so Windows shows it as
//! safe to remove. (`super` has Linux's and macOS's ways; formatting isn't done here
//! yet: `super::format`.)
//!
//! A stick is a volume with a drive letter that Windows calls removable (flash
//! drives, card readers), or a fixed disk on the USB, SD or MMC bus (USB hard disks
//! and the larger sticks that say they're fixed), other than the system's own.
//! Its path is its root (`E:\`) and its device the volume itself (`\\.\E:`).
//!
//! Sticks coming and going are heard, not looked for: [`News`] is a hidden window
//! that gets Windows' device-change message.
//!
//! The parts that only work out names are built for tests on every platform; the
//! calls into Windows only there.

use std::path::Path;

/// A filesystem's name as Windows gives it, as the rest of earshot knows it (the
/// names Linux's mount table uses: `super::Drive::needs_format` and `notes`).
pub fn fs_kind(windows_name: &str) -> String {
    match windows_name.to_ascii_uppercase().as_str() {
        "FAT32" | "FAT" | "FAT16" | "FAT12" => "vfat".into(),
        "EXFAT" => "exfat".into(),
        "NTFS" => "ntfs".into(),
        _ => windows_name.to_ascii_lowercase(),
    }
}

/// The drive letter a path is on (`E:\Music\a.mp3`: `E`), if it starts with one.
pub fn letter(at: &Path) -> Option<char> {
    let s = at.to_string_lossy();
    // (Also the long form, `\\?\E:\…`.)
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    let mut chars = s.chars();
    let (l, colon) = (chars.next()?, chars.next()?);
    (l.is_ascii_alphabetic() && colon == ':').then(|| l.to_ascii_uppercase())
}

/// A drive letter's root folder (`E:\`).
pub fn root(letter: char) -> String {
    format!("{letter}:\\")
}

/// A drive letter's volume, as a device to open (`\\.\E:`).
pub fn device(letter: char) -> String {
    format!(r"\\.\{letter}:")
}

/// A stick, as [`volumes`] finds it.
#[cfg_attr(not(windows), allow(dead_code))] // only made on Windows
#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    /// `\\.\E:`
    pub device: String,
    /// `E:\`
    pub path: std::path::PathBuf,
    /// As [`fs_kind`] names it.
    pub fstype: String,
    /// Its volume label, or `E:` if it has none.
    pub label: String,
}

#[cfg(windows)]
pub use sys::{News, eject, flush, space, volumes};

#[cfg(windows)]
mod sys {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FlushFileBuffers, GetDiskFreeSpaceExW,
        GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetErrorMode};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_STORAGE_EJECT_MEDIA,
        IOCTL_STORAGE_MEDIA_REMOVAL, IOCTL_STORAGE_QUERY_PROPERTY, PREVENT_MEDIA_REMOVAL,
        PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        StorageDeviceProperty,
    };

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, DBT_DEVNODES_CHANGED,
        DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE,
        PeekMessageW, QS_ALLINPUT, RegisterClassW, TranslateMessage, WM_DEVICECHANGE, WNDCLASSW,
    };

    use super::{Volume, device, fs_kind, letter, root};

    /// A drive came or went since [`News::wait`] last said so (set by the window's
    /// procedure, which Windows calls on the watcher's thread).
    static CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// The hidden window's procedure: notes Windows' device-change message, which it
    /// sends every top-level window when a volume arrives or has gone (a stick
    /// plugged in or pulled out, a card put in a reader), or the devices changed.
    unsafe extern "system" fn on_message(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_DEVICECHANGE
            && matches!(
                wparam as u32,
                DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE | DBT_DEVNODES_CHANGED
            )
        {
            CHANGED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        // SAFETY: passing on exactly what Windows gave this procedure.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    /// Hears when drives come and go, rather than looking for them over and over: a
    /// window that's never shown, on the thread that makes it, which must also be
    /// the one that [`News::wait`]s (Windows delivers a window's messages to its
    /// own thread).
    pub struct News(HWND);

    impl News {
        /// `None` if Windows won't make the window: look every so often instead.
        pub fn new() -> Option<News> {
            let class = wide("earshot-drive-news");
            // SAFETY: null asks for this program's own module.
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let def = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(on_message),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
            };
            // SAFETY: `def` is whole, and its strings outlive the calls. (Registering
            // the class a second time fails, harmlessly: it's there.) The window is a
            // top-level one (no parent), as the broadcast needs, with no style, size
            // or place: it's never shown.
            let window = unsafe {
                RegisterClassW(&def);
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                )
            };
            (!window.is_null()).then_some(News(window))
        }

        /// Wait up to `up_to` for a drive to come or go. True if one did.
        pub fn wait(&self, up_to: Duration) -> bool {
            let until = std::time::Instant::now() + up_to;
            loop {
                // Whatever has arrived for the window goes to its procedure.
                // SAFETY: `msg` is plain data Windows fills in; the window is ours.
                unsafe {
                    let mut msg: MSG = std::mem::zeroed();
                    while PeekMessageW(&mut msg, self.0, 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                if CHANGED.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    return true;
                }
                let left = until.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return false;
                }
                // Sleep until a message arrives, or the time is up.
                // SAFETY: no handles to wait on, only this thread's messages.
                unsafe {
                    MsgWaitForMultipleObjects(
                        0,
                        std::ptr::null(),
                        0,
                        left.as_millis().min(u32::MAX as u128) as u32,
                        QS_ALLINPUT,
                    );
                }
            }
        }
    }

    impl Drop for News {
        fn drop(&mut self) {
            // SAFETY: the window this made, destroyed once, on its own thread.
            unsafe { DestroyWindow(self.0) };
        }
    }

    // `GetDriveTypeW`'s answers (winbase.h).
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    /// `GetVolumeInformationW`'s flag for a volume that can't be written to.
    const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;
    // `STORAGE_BUS_TYPE`s a stick or card can be on (winioctl.h).
    const BUS_USB: i32 = 7;
    const BUS_SD: i32 = 12;
    const BUS_MMC: i32 = 13;

    /// A string as Windows takes it: UTF-16, ending in a zero.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// An open volume, closed when dropped.
    struct Handle(HANDLE);

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: a handle `CreateFileW` gave, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Open a volume (`\\.\E:`) with this access (0: only to ask about it).
    fn open(device: &str, access: u32) -> std::io::Result<Handle> {
        let name = wide(device);
        // SAFETY: `name` is a zero-ended UTF-16 string that outlives the call; the
        // other arguments are plain values or null where null is allowed.
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Handle(h))
        }
    }

    /// Send a volume a control code with `input` (a plain struct, or `()`), wanting
    /// nothing back.
    fn control<T>(h: &Handle, code: u32, input: Option<&T>) -> std::io::Result<()> {
        let mut returned = 0u32;
        let (ptr, len) = match input {
            Some(i) => (std::ptr::from_ref(i).cast(), size_of::<T>() as u32),
            None => (std::ptr::null(), 0),
        };
        // SAFETY: `h` is an open handle; the input buffer is `len` readable bytes (or
        // null with 0); there's no output buffer; `returned` is a valid u32.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                code,
                ptr,
                len,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// The bus a volume's disk is on (`STORAGE_BUS_TYPE`), if Windows says.
    fn bus(letter: char) -> Option<i32> {
        let h = open(&device(letter), 0).ok()?;
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        // The descriptor's fixed part is all that's read; Windows fills what fits.
        let mut out = [0u8; 1024];
        let mut returned = 0u32;
        // SAFETY: `h` is an open handle; `query` and `out` are valid for the sizes
        // given; `returned` is a valid u32.
        let ok = unsafe {
            DeviceIoControl(
                h.0,
                IOCTL_STORAGE_QUERY_PROPERTY,
                std::ptr::from_ref(&query).cast(),
                size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                out.as_mut_ptr().cast(),
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || (returned as usize) < size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
            return None;
        }
        // SAFETY: Windows wrote at least a whole descriptor into `out`; read without
        // assuming the byte buffer is aligned for it.
        let d: STORAGE_DEVICE_DESCRIPTOR = unsafe { std::ptr::read_unaligned(out.as_ptr().cast()) };
        Some(d.BusType)
    }

    /// A volume's label, filesystem name and flags; `None` with nothing in the drive
    /// (an empty card reader) or nothing Windows can read on it.
    fn info(letter: char) -> Option<(String, String, u32)> {
        let at = wide(&root(letter));
        let (mut label, mut fs) = ([0u16; 261], [0u16; 261]);
        let mut flags = 0u32;
        // SAFETY: `at` is zero-ended; both buffers are as long as said; the pointers
        // not wanted are null, which the call allows.
        let ok = unsafe {
            GetVolumeInformationW(
                at.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut flags,
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        let text = |b: &[u16]| {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            String::from_utf16_lossy(&b[..end])
        };
        (ok != 0).then(|| (text(&label), text(&fs), flags))
    }

    /// The sticks plugged in now.
    pub fn volumes() -> Vec<Volume> {
        // An empty card reader mustn't put up a "there is no disk in the drive" box.
        // SAFETY: sets this process's error mode; no pointers.
        unsafe { SetErrorMode(SEM_FAILCRITICALERRORS) };
        // The system's own drive is never a stick, whatever bus it's on.
        let system = std::env::var("SystemDrive")
            .ok()
            .and_then(|d| super::letter(Path::new(&d)))
            .unwrap_or('C');
        // SAFETY: no arguments.
        let present = unsafe { GetLogicalDrives() };
        let mut out = vec![];
        for (i, l) in ('A'..='Z').enumerate() {
            if present & (1 << i) == 0 || l == system {
                continue;
            }
            let at = wide(&root(l));
            // SAFETY: `at` is a zero-ended UTF-16 string.
            let kind = unsafe { GetDriveTypeW(at.as_ptr()) };
            let stick = match kind {
                DRIVE_REMOVABLE => true,
                DRIVE_FIXED => matches!(bus(l), Some(BUS_USB | BUS_SD | BUS_MMC)),
                _ => false,
            };
            if !stick {
                continue;
            }
            let Some((label, fs, _)) = info(l) else {
                continue;
            };
            out.push(Volume {
                device: device(l),
                path: PathBuf::from(root(l)),
                fstype: fs_kind(&fs),
                label: match label.trim() {
                    "" => format!("{l}:"),
                    name => name.to_string(),
                },
            });
        }
        out
    }

    /// Total and free bytes of the drive `at` is on, and whether it can be written to.
    pub fn space(at: &Path) -> (u64, u64, bool) {
        let Some(l) = letter(at) else {
            return (0, 0, false);
        };
        let writable = info(l).is_some_and(|(_, _, flags)| flags & FILE_READ_ONLY_VOLUME == 0);
        let dir = wide(&root(l));
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: `dir` is zero-ended; `free` and `total` are valid u64s; the third
        // (all free bytes, whoever may use them) isn't wanted.
        let ok = unsafe {
            GetDiskFreeSpaceExW(dir.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
        };
        if ok == 0 {
            return (0, 0, writable);
        }
        (total, free, writable)
    }

    /// Write everything Windows has cached for the drive `at` is on to it. Best
    /// effort: a volume earshot isn't let open for writing is left to Windows.
    pub fn flush(at: &Path) {
        let Some(l) = letter(at) else { return };
        if let Ok(h) = open(&device(l), GENERIC_READ | GENERIC_WRITE) {
            // SAFETY: an open handle.
            unsafe { FlushFileBuffers(h.0) };
        }
    }

    /// Flush a stick, take it out of use and eject it, so Windows shows it as safe
    /// to remove. `device`: its volume (`\\.\E:`). Fails while anything has a file
    /// on it open.
    pub fn eject(device: &str) -> Result<(), String> {
        let h = open(device, GENERIC_READ | GENERIC_WRITE)
            .map_err(|e| format!("Windows wouldn't open the drive to eject it: {e}"))?;
        // SAFETY: an open handle.
        unsafe { FlushFileBuffers(h.0) };
        // Locked only if nothing else has it open; whatever just closed a file on it
        // may take a moment to let go.
        let mut locked = Err(std::io::Error::other("not tried"));
        for _ in 0..10 {
            locked = control::<()>(&h, FSCTL_LOCK_VOLUME, None);
            if locked.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        locked.map_err(|e| {
            format!("Something is still using the drive (close its files and windows): {e}")
        })?;
        control::<()>(&h, FSCTL_DISMOUNT_VOLUME, None)
            .map_err(|e| format!("Windows couldn't take the drive out of use: {e}"))?;
        // Dismounted is safe to pull out; ejecting the media (and letting it be) is a
        // courtesy some drives refuse.
        let allow = PREVENT_MEDIA_REMOVAL {
            PreventMediaRemoval: false,
        };
        let _ = control(&h, IOCTL_STORAGE_MEDIA_REMOVAL, Some(&allow));
        let _ = control::<()>(&h, IOCTL_STORAGE_EJECT_MEDIA, None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_filesystem_names_become_the_ones_earshot_knows() {
        assert_eq!(fs_kind("FAT32"), "vfat");
        assert_eq!(fs_kind("FAT"), "vfat");
        assert_eq!(fs_kind("exFAT"), "exfat");
        assert_eq!(fs_kind("NTFS"), "ntfs");
        assert_eq!(fs_kind("ReFS"), "refs");
    }

    #[test]
    fn a_paths_drive_letter_gives_its_root_and_its_volume() {
        assert_eq!(letter(Path::new(r"e:\Music\a.mp3")), Some('E'));
        assert_eq!(letter(Path::new(r"E:")), Some('E'));
        assert_eq!(letter(Path::new(r"\\?\F:\PIONEER")), Some('F'));
        assert_eq!(letter(Path::new(r"\\server\share\a.mp3")), None);
        assert_eq!(letter(Path::new("/media/stick")), None);
        assert_eq!(root('E'), r"E:\");
        assert_eq!(device('E'), r"\\.\E:");
    }
}
