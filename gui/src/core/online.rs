//! Whether this computer is online: bucket tracks that aren't downloaded can't be
//! loaded when it isn't, so the lists leave them out until it is (`LibraryIndex`).
//!
//! "Online" is the operating system having a route to the internet: asked by
//! connecting a UDP socket to a public address, which sends nothing (UDP has no
//! handshake) and only has the OS pick the way out, or say there's none. It's the
//! same on every platform, takes no time, and needs no service to ask. It doesn't
//! say the internet actually answers (a Wi-Fi that wants a login looks online): a
//! bucket that can't be reached then says so itself when it's read or loaded from.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Public addresses to find a way to (never sent to): an IPv4 and an IPv6 one, as
/// a network may have only either. (Cloudflare's resolver's, as stable as any.)
const PROBES: [&str; 2] = ["1.1.1.1:53", "[2606:4700:4700::1111]:53"];

/// How often [`watch`] looks.
const EVERY: Duration = Duration::from_secs(3);

/// Whether the computer has a way out to the internet now. Instant: nothing is sent.
pub fn online() -> bool {
    PROBES.iter().any(|to| {
        let to: SocketAddr = to.parse().expect("a socket address");
        let from = match to {
            SocketAddr::V4(_) => "0.0.0.0:0",
            SocketAddr::V6(_) => "[::]:0",
        };
        UdpSocket::bind(from).is_ok_and(|s| s.connect(to).is_ok())
    })
}

/// Watch it on a thread of its own: `changed` hears whenever it changes (not at the
/// start: ask [`online`] then). The thread ends within a second of `stop` being set.
pub fn watch(stop: Arc<AtomicBool>, changed: impl Fn(bool) + Send + 'static) {
    let mut last = online();
    std::thread::Builder::new()
        .name("earshot-online".into())
        .spawn(move || {
            loop {
                // A second at a time, to notice `stop`.
                for _ in 0..EVERY.as_secs() {
                    std::thread::sleep(Duration::from_secs(1));
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                }
                let now = online();
                if now != last {
                    last = now;
                    changed(now);
                }
            }
        })
        .expect("spawn the online watcher");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asking_sends_nothing_and_answers_at_once() {
        // Whatever the answer on this machine, it comes without waiting on a network.
        let t = std::time::Instant::now();
        let _ = online();
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
