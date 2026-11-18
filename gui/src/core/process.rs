//! Starting the programs earshot runs in the background (the worker's Python, ffmpeg).

use std::ffi::OsStr;
use std::process::Command;

/// A [`Command`] for a program that runs out of sight. On Windows a console program
/// started from a windowed one opens a console window of its own, unless told not to;
/// the programs it starts in turn (the worker's ffmpeg and Rubber Band) share its
/// hidden console, so they don't either.
pub fn background(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
