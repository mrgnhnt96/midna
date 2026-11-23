//! Whether this Mac's network changed: a new Wi-Fi network, interface or address. A drive link
//! parked because Bonjour couldn't see its Mac is tried again then (`relay::network_changed`):
//! a Mac that wasn't on the old network may be on the new one.
//!
//! The nearby thread polls it ([`Watch::poll`]); it reads the interfaces' IPv4 addresses with
//! `getifaddrs` every [`LOOK`], no more. Only an address that wasn't there before counts: losing
//! one (Wi-Fi off) brings no Mac nearer.
use std::collections::BTreeSet;
use std::ffi::CStr;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

/// How often the addresses are read.
pub const LOOK: Duration = Duration::from_secs(5);

/// The addresses as last read.
#[derive(Default)]
pub struct Watch {
    last: Option<BTreeSet<String>>,
    next: Option<Instant>,
}

impl Watch {
    /// Read the addresses when they're due ([`LOOK`]); whether one appeared since the last read.
    pub fn poll(&mut self) -> bool {
        let now = Instant::now();
        if self.next.is_some_and(|t| now < t) {
            return false;
        }
        self.next = Some(now + LOOK);
        self.see(addresses())
    }

    /// The addresses now: whether one of them is new. The first read only sets where it starts.
    fn see(&mut self, now: BTreeSet<String>) -> bool {
        let new = self.last.as_ref().is_some_and(|last| now.difference(last).next().is_some());
        self.last = Some(now);
        new
    }
}

/// Whether interface `name` is a network other Macs can be on: not loopback, a tunnel (VPNs
/// come and go on their own), AirDrop's peer-to-peer links or a bridge for virtual machines.
fn counts(name: &str) -> bool {
    !["lo", "utun", "awdl", "llw", "bridge", "gif", "stf", "anpi", "ap"].iter().any(|p| name.starts_with(p))
}

/// "<interface> <address>" for each IPv4 address on an interface that's up and [`counts`].
fn addresses() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list this frees below with freeifaddrs.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned, alive until freeifaddrs.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        let up = ifa.ifa_flags & libc::IFF_UP as u32 != 0;
        if !up || ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
            continue;
        }
        // SAFETY: a non-null ifa_addr points at a sockaddr whose family says what it is.
        if i32::from(unsafe { (*ifa.ifa_addr).sa_family }) != libc::AF_INET {
            continue;
        }
        // SAFETY: ifa_name is a C string, and an AF_INET address is a sockaddr_in.
        let (name, sin) = unsafe { (CStr::from_ptr(ifa.ifa_name).to_string_lossy(), &*(ifa.ifa_addr as *const libc::sockaddr_in)) };
        if counts(&name) {
            out.insert(format!("{name} {}", Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))));
        }
    }
    // SAFETY: `head` came from getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(head) };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(a: &[&str]) -> BTreeSet<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_an_address_that_wasnt_there_is_a_change() {
        let mut w = Watch::default();
        assert!(!w.see(set(&["en0 192.168.1.20"])), "the first read only sets where it starts");
        assert!(!w.see(set(&["en0 192.168.1.20"])), "the same");
        assert!(w.see(set(&["en0 10.0.0.5"])), "another Wi-Fi network");
        assert!(!w.see(set(&[])), "Wi-Fi off: no Mac comes nearer");
        assert!(w.see(set(&["en0 10.0.0.5"])), "back on");
        assert!(w.see(set(&["en0 10.0.0.5", "en7 192.168.2.3"])), "a new interface (Ethernet)");
        assert!(!w.see(set(&["en7 192.168.2.3"])), "one of them gone");
    }

    #[test]
    fn tunnels_airdrop_and_loopback_are_not_the_network() {
        for name in ["lo0", "utun3", "awdl0", "llw0", "bridge100", "anpi0", "ap1"] {
            assert!(!counts(name), "{name}");
        }
        for name in ["en0", "en7"] {
            assert!(counts(name), "{name}");
        }
    }

    #[test]
    fn it_reads_this_macs_addresses_and_looks_again_only_when_due() {
        let all = addresses();
        assert!(all.iter().all(|a| !a.starts_with("lo")), "{all:?}");
        let mut w = Watch::default();
        assert!(!w.poll(), "the first look");
        assert!(!w.poll(), "not due yet: nothing read");
    }
}
