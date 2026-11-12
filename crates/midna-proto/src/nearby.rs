//! Nearby: Midnas on the same network find each other over Bonjour (`peers.list`).
//!
//! Each midna home has a persistent Ed25519 device key. Its fingerprint (SHA-256 of the public
//! key, lowercase hex) is the device's real id; the name is for display only and anyone can
//! claim any name. midnad advertises and browses [`SERVICE`] (Midna Dev and every home other
//! than the installed Midna's use [`DEV_SERVICE`], so they never see production) with a TXT
//! record carrying [`TXT_PROTOCOL`], [`TXT_FINGERPRINT`] and [`TXT_NAME`].
use crate::types::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The installed Midna's Bonjour service type.
pub const SERVICE: &str = "_midna._tcp";
/// Midna Dev's, and any home other than the installed Midna's.
pub const DEV_SERVICE: &str = "_midna-dev._tcp";
/// The Nearby protocol this build speaks. A peer advertising another one is listed but can't
/// be sent to until one side updates.
pub const PROTOCOL: u32 = 1;
/// How long `nearby.visibility` stays `everyone` before going back to `paired`.
pub const EVERYONE_SECS: i64 = 600;

/// TXT record keys.
pub const TXT_PROTOCOL: &str = "v";
pub const TXT_FINGERPRINT: &str = "fp";
pub const TXT_NAME: &str = "name";

/// Who can find this Mac (setting `nearby.visibility`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Nearby is off: midnad neither advertises nor looks for other Midnas.
    Off,
    /// Only paired devices can find this Mac. midnad still looks for others.
    #[default]
    Paired,
    /// Every Midna on the network can find this Mac, for 10 minutes, then back to `paired`.
    Everyone,
}

impl Visibility {
    pub fn parse(s: &str) -> Option<Visibility> {
        match s {
            "off" => Some(Visibility::Off),
            "paired" => Some(Visibility::Paired),
            "everyone" => Some(Visibility::Everyone),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Off => "off",
            Visibility::Paired => "paired",
            Visibility::Everyone => "everyone",
        }
    }
}

/// Is `s` a Bonjour service type midnad may use (`_name._tcp`, name 1–15 of a-z, 0-9, `-`)?
pub fn valid_service(s: &str) -> bool {
    let Some(name) = s.strip_prefix('_').and_then(|r| r.strip_suffix("._tcp")) else { return false };
    (1..=15).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// A device id shortened for people: the first 16 hex digits in groups of four.
pub fn short_fingerprint(fp: &str) -> String {
    let head: Vec<String> = fp.as_bytes().chunks(4).take(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    head.join(" ")
}

/// Build a DNS-SD TXT record from key=value pairs. Values longer than a TXT string allows are cut
/// at a character boundary.
pub fn txt_record(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in pairs {
        let room = 255 - k.len() - 1;
        let mut end = v.len().min(room);
        while !v.is_char_boundary(end) {
            end -= 1;
        }
        let entry = format!("{k}={}", &v[..end]);
        out.push(entry.len() as u8);
        out.extend_from_slice(entry.as_bytes());
    }
    out
}

/// Read a DNS-SD TXT record into key=value pairs (keys lowercased; malformed entries skipped).
pub fn parse_txt(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let len = bytes[i] as usize;
        let Some(entry) = bytes.get(i + 1..i + 1 + len) else { break };
        i += 1 + len;
        let entry = String::from_utf8_lossy(entry);
        if let Some((k, v)) = entry.split_once('=')
            && !k.is_empty()
        {
            out.push((k.to_ascii_lowercase(), v.to_string()));
        }
    }
    out
}

/// Another Midna seen on the network.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Peer {
    /// SHA-256 of its device public key, lowercase hex: the device's real id. Display names can
    /// repeat or lie; this can't.
    pub fingerprint: String,
    /// The name it advertises (its `nearby.name`, else its computer name). Display only.
    pub name: String,
    /// The Nearby protocol it speaks (see `me.protocol`).
    pub protocol: u32,
    /// Its host name on the network (`Name.local.`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The port it advertises.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// When this daemon first saw it (since it last appeared).
    pub seen_since: Timestamp,
}

/// This daemon's side of Nearby.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NearbyMe {
    /// This device's id (SHA-256 of its public key, lowercase hex).
    pub fingerprint: String,
    /// The name others see (setting `nearby.name`, else the Mac's computer name).
    pub name: String,
    /// Setting `nearby.visibility`.
    pub visibility: Visibility,
    /// While visibility is `everyone`: when it goes back to `paired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub everyone_until: Option<Timestamp>,
    /// The Bonjour service type this daemon uses; none = Nearby is off for this daemon (an
    /// in-process test daemon, or `MIDNA_NEARBY_SERVICE=off`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Advertising itself right now (only while visibility is `everyone`, for now: paired-only
    /// advertising comes with pairing).
    pub advertising: bool,
    /// Looking for other Midnas right now (visibility isn't `off`).
    pub browsing: bool,
    pub protocol: u32,
    /// Why Bonjour isn't working, when it isn't (midnad retries every few seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersListResult {
    pub me: NearbyMe,
    /// Midnas this daemon can see now, by name. A peer that only shows itself to paired devices
    /// isn't listed until it is paired with this one.
    pub peers: Vec<Peer>,
}

/// Data of the `peers.left` event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeerLeft {
    pub fingerprint: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_types() {
        for ok in [SERVICE, DEV_SERVICE, "_midna-t0a1b2c._tcp", "_a._tcp"] {
            assert!(valid_service(ok), "{ok}");
        }
        for bad in ["midna._tcp", "_midna._udp", "_._tcp", "_Midna._tcp", "_-midna._tcp", "_midna-._tcp", "_a-very-long-name1._tcp", "_mid na._tcp"] {
            assert!(!valid_service(bad), "{bad}");
        }
    }

    #[test]
    fn txt_round_trips_and_cuts_long_values() {
        let long = "é".repeat(200);
        let rec = txt_record(&[(TXT_PROTOCOL, "1"), (TXT_FINGERPRINT, "abcd"), (TXT_NAME, &long)]);
        let back = parse_txt(&rec);
        assert_eq!(back[0], ("v".into(), "1".into()));
        assert_eq!(back[1], ("fp".into(), "abcd".into()));
        assert!(back[2].1.len() <= 250 && long.starts_with(&back[2].1), "cut at a char boundary");
        assert_eq!(parse_txt(&[5, b'x', b'=', b'1']), vec![], "a truncated entry is dropped");
        assert_eq!(parse_txt(&[3, b'=', b'1', b'2', 3, b'K', b'=', b'v']), vec![("k".into(), "v".into())]);
    }

    #[test]
    fn short_fingerprints() {
        assert_eq!(short_fingerprint("0123456789abcdef0123"), "0123 4567 89ab cdef");
        assert_eq!(short_fingerprint("01234"), "0123 4");
    }

    #[test]
    fn visibility_names() {
        for v in [Visibility::Off, Visibility::Paired, Visibility::Everyone] {
            assert_eq!(Visibility::parse(v.as_str()), Some(v));
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
        }
        assert_eq!(Visibility::default(), Visibility::Paired);
    }
}
