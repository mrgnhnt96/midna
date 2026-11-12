//! Connections between Midnas: TCP, then `Noise_XX_25519_ChaChaPoly_SHA256` with prologue
//! [`PROLOGUE`]. Each process makes a fresh X25519 static key and signs it with the device's
//! Ed25519 key; the second and third handshake messages carry that key and signature
//! ([`Local::payload`]), so when the handshake is done each side knows the other's device key,
//! and so its fingerprint, for certain. Nothing in the connection is readable or changeable by
//! anyone else, and the handshake hash ([`Channel::hash`]) is the same on both ends only if no
//! one sat in the middle (pairing codes are made from it).
//!
//! On the wire every Noise message is a 2-byte big-endian length and the message. A [`Wire`]
//! message is JSON, sent as one or more Noise messages whose first plaintext byte says whether
//! more follow.
use super::identity::{self, Identity};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const NOISE: &str = "Noise_XX_25519_ChaChaPoly_SHA256";
pub const PROLOGUE: &[u8] = b"midna-nearby/1";
/// What the device key signs: this, then the X25519 static public key.
const STATIC_CONTEXT: &[u8] = b"midna-nearby static key:";
const MAX_NOISE: usize = 65535;
const AEAD_TAG: usize = 16;
/// The most plaintext one Noise message carries after its more-follows byte.
const CHUNK: usize = MAX_NOISE - AEAD_TAG - 1;
/// The largest [`Wire`] message, as JSON.
pub const MAX_MESSAGE: usize = 1 << 20;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a device that isn't paired was turned away.
pub const NOT_PAIRED: &str = "not_paired";

/// Everything Midnas say to each other. A device that isn't paired may only send `pair_request`
/// (and the rest of the pairing it starts); anything else gets `refused` with [`NOT_PAIRED`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Wire {
    /// Initiator: I'd like to pair; this is my name.
    PairRequest { name: String, protocol: u32 },
    /// Responder: my commitment to a nonce (hex SHA-256), and my name.
    PairCommit { commit: String, name: String },
    /// Initiator: my nonce (hex).
    PairNonce { nonce: String },
    /// Responder: the nonce I committed to (hex).
    PairReveal { nonce: String },
    /// My person confirmed the code matches.
    PairConfirm,
    /// My person said no, the codes differ, or time ran out.
    PairDecline { reason: String },
    /// Paired devices only: are you there?
    Ping,
    Pong,
    /// Paired devices only: a share for your inbox (`payload` is a `nearby::SharePayload`; kept
    /// as JSON so a kind this build doesn't know is refused rather than breaking the message).
    ShareOffer { id: String, payload: serde_json::Value },
    /// My person accepted or declined your share `id` (`state`: accepted | declined).
    ShareResult { id: String, state: String },
    /// Got share message `id` (an offer is in the inbox; a result is recorded).
    ShareAck { id: String },
    /// Not done; `reason` says why ([`NOT_PAIRED`], or words).
    Refused { reason: String },
    /// A message this build doesn't know (a newer peer's).
    #[serde(other)]
    Unknown,
}

/// This process's Noise static key and the device key's signature over it.
pub struct Local {
    noise_private: Vec<u8>,
    /// Ed25519 public key (32 bytes), then the signature (64).
    payload: Vec<u8>,
}

impl Local {
    pub fn new(id: &Identity) -> Local {
        let kp = builder().generate_keypair().expect("the Noise resolver makes X25519 keys");
        let mut msg = STATIC_CONTEXT.to_vec();
        msg.extend_from_slice(&kp.public);
        let mut payload = id.public_key().to_bytes().to_vec();
        payload.extend_from_slice(&id.sign(&msg).to_bytes());
        Local { noise_private: kp.private, payload }
    }
}

fn builder() -> snow::Builder<'static> {
    snow::Builder::new(NOISE.parse().expect("a valid Noise pattern"))
}

/// The device key behind a handshake payload, if it signed the Noise static key `remote_static`.
fn device_key(payload: &[u8], remote_static: Option<&[u8]>) -> Result<VerifyingKey, String> {
    let (Some(rs), Ok(key), Ok(sig)) = (remote_static, <[u8; 32]>::try_from(payload.get(..32).unwrap_or_default()), <[u8; 64]>::try_from(payload.get(32..).unwrap_or_default()))
    else {
        return Err("the other side didn't say who it is".into());
    };
    let key = VerifyingKey::from_bytes(&key).map_err(|_| "the other side sent a bad device key".to_string())?;
    let mut msg = STATIC_CONTEXT.to_vec();
    msg.extend_from_slice(rs);
    key.verify_strict(&msg, &Signature::from_bytes(&sig)).map_err(|_| "the other side's device key didn't sign its connection key".to_string())?;
    Ok(key)
}

/// A stream read in whole 2-byte-length frames, which tolerates read timeouts mid-frame.
struct Framed {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Framed {
    /// The next whole frame; None if a read timed out first.
    fn frame(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            if self.buf.len() >= 2 {
                let n = u16::from_be_bytes([self.buf[0], self.buf[1]]) as usize;
                if self.buf.len() >= 2 + n {
                    let f = self.buf[2..2 + n].to_vec();
                    self.buf.drain(..2 + n);
                    return Ok(Some(f));
                }
            }
            let mut tmp = [0u8; 16 * 1024];
            match self.stream.read(&mut tmp) {
                Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
                Ok(k) => self.buf.extend_from_slice(&tmp[..k]),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(None),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// The next frame, waiting until `deadline`.
    fn frame_by(&mut self, deadline: Instant) -> Result<Vec<u8>, String> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the other side took too long".into());
            }
            let _ = self.stream.set_read_timeout(Some(left));
            match self.frame() {
                Ok(Some(f)) => return Ok(f),
                Ok(None) => {}
                Err(e) => return Err(io_words(&e)),
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut out = Vec::with_capacity(2 + bytes.len());
        out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        out.extend_from_slice(bytes);
        self.stream.write_all(&out)
    }
}

fn io_words(e: &std::io::Error) -> String {
    match e.kind() {
        ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe => "the connection closed".into(),
        _ => e.to_string(),
    }
}

/// A finished handshake: an encrypted connection to a device whose key is known.
pub struct Channel {
    io: Framed,
    noise: snow::TransportState,
    /// The other device's key and fingerprint, proven by the handshake.
    pub peer_key: VerifyingKey,
    pub fingerprint: String,
    /// The handshake hash: equal on both ends unless someone is in the middle.
    pub hash: Vec<u8>,
    pub initiator: bool,
    partial: Vec<u8>,
}

/// Open a connection to `addr` and run the handshake as the initiator.
pub fn connect(addrs: &[SocketAddr], local: &Local) -> Result<Channel, String> {
    let mut last = "no address to connect to".to_string();
    for a in addrs {
        match TcpStream::connect_timeout(a, CONNECT_TIMEOUT) {
            Ok(s) => return handshake(s, local, true),
            Err(e) => last = format!("connecting to {a}: {e}"),
        }
    }
    Err(last)
}

/// Run the handshake as the responder on an accepted connection.
pub fn accept(stream: TcpStream, local: &Local) -> Result<Channel, String> {
    handshake(stream, local, false)
}

fn handshake(stream: TcpStream, local: &Local, initiator: bool) -> Result<Channel, String> {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT));
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut io = Framed { stream, buf: vec![] };
    let b = builder().local_private_key(&local.noise_private).and_then(|b| b.prologue(PROLOGUE)).map_err(|e| e.to_string())?;
    let mut hs = if initiator { b.build_initiator() } else { b.build_responder() }.map_err(|e| e.to_string())?;
    let mut msg = vec![0u8; MAX_NOISE];
    let mut payload = vec![0u8; MAX_NOISE];
    let noise = |e: snow::Error| format!("handshake: {e}");
    let peer_payload = if initiator {
        let n = hs.write_message(&[], &mut msg).map_err(noise)?;
        io.send(&msg[..n]).map_err(|e| io_words(&e))?;
        let f = io.frame_by(deadline)?;
        let n = hs.read_message(&f, &mut payload).map_err(noise)?;
        let peer = payload[..n].to_vec();
        let n = hs.write_message(&local.payload, &mut msg).map_err(noise)?;
        io.send(&msg[..n]).map_err(|e| io_words(&e))?;
        peer
    } else {
        let f = io.frame_by(deadline)?;
        hs.read_message(&f, &mut payload).map_err(noise)?;
        let n = hs.write_message(&local.payload, &mut msg).map_err(noise)?;
        io.send(&msg[..n]).map_err(|e| io_words(&e))?;
        let f = io.frame_by(deadline)?;
        let n = hs.read_message(&f, &mut payload).map_err(noise)?;
        payload[..n].to_vec()
    };
    let peer_key = device_key(&peer_payload, hs.get_remote_static())?;
    let hash = hs.get_handshake_hash().to_vec();
    let noise = hs.into_transport_mode().map_err(noise)?;
    Ok(Channel { io, noise, fingerprint: identity::fingerprint(&peer_key), peer_key, hash, initiator, partial: vec![] })
}

impl Channel {
    pub fn send(&mut self, msg: &Wire) -> Result<(), String> {
        let json = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        if json.len() > MAX_MESSAGE {
            return Err(format!("message too big ({} bytes)", json.len()));
        }
        let mut out = vec![0u8; MAX_NOISE];
        let mut chunks = json.chunks(CHUNK).peekable();
        if chunks.peek().is_none() {
            return Err("empty message".into());
        }
        while let Some(c) = chunks.next() {
            let mut plain = Vec::with_capacity(1 + c.len());
            plain.push(u8::from(chunks.peek().is_some()));
            plain.extend_from_slice(c);
            let n = self.noise.write_message(&plain, &mut out).map_err(|e| e.to_string())?;
            self.io.send(&out[..n]).map_err(|e| io_words(&e))?;
        }
        Ok(())
    }

    /// The next message, waiting at most `wait`: None if none came in time. An error means the
    /// connection is over (closed, broken, or the peer sent something that isn't ours).
    pub fn recv(&mut self, wait: Duration) -> Result<Option<Wire>, String> {
        let deadline = Instant::now() + wait;
        let mut plain = vec![0u8; MAX_NOISE];
        loop {
            let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            let _ = self.io.stream.set_read_timeout(Some(left));
            let frame = match self.io.frame() {
                Ok(Some(f)) => f,
                Ok(None) if Instant::now() >= deadline => return Ok(None),
                Ok(None) => continue,
                Err(e) => return Err(io_words(&e)),
            };
            let n = self.noise.read_message(&frame, &mut plain).map_err(|_| "a message failed to decrypt".to_string())?;
            let Some((&more, data)) = plain[..n].split_first() else { return Err("an empty message".into()) };
            if self.partial.len() + data.len() > MAX_MESSAGE {
                return Err("message too big".into());
            }
            self.partial.extend_from_slice(data);
            if more == 0 {
                let whole = std::mem::take(&mut self.partial);
                return serde_json::from_slice(&whole).map(Some).map_err(|e| format!("a message that isn't Nearby's: {e}"));
            }
        }
    }

    /// The next message within `wait`, which must come.
    pub fn expect(&mut self, wait: Duration) -> Result<Wire, String> {
        self.recv(wait)?.ok_or_else(|| "the other side took too long".into())
    }

    /// Close cleanly: stop sending, then let the other side read what was sent before the
    /// socket goes (closing with unread input would reset it and could lose the last message).
    pub fn finish(mut self) {
        let _ = self.io.stream.shutdown(Shutdown::Write);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let _ = self.io.stream.set_read_timeout(Some(Duration::from_millis(200)));
            match self.io.frame() {
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }
}

/// `n` random bytes.
pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    // SAFETY: arc4random_buf fills exactly `out.len()` bytes of a valid buffer.
    unsafe { libc::arc4random_buf(out.as_mut_ptr().cast(), out.len()) };
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn ident(tag: &str) -> Identity {
        let h = std::env::temp_dir().join(format!("midna-tr-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        let id = identity::load_or_create(&h);
        let _ = std::fs::remove_dir_all(&h);
        id
    }

    fn pair_up(a: &Identity, b: &Identity) -> (Result<Channel, String>, Result<Channel, String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let lb = Local::new(b);
        let t = std::thread::spawn(move || accept(l.accept().unwrap().0, &lb));
        let ca = connect(&[addr], &Local::new(a));
        (ca, t.join().unwrap())
    }

    #[test]
    fn both_sides_learn_each_others_key_and_talk() {
        let (a, b) = (ident("a"), ident("b"));
        let (ca, cb) = pair_up(&a, &b);
        let (mut ca, mut cb) = (ca.unwrap(), cb.unwrap());
        assert_eq!(ca.fingerprint, b.fingerprint);
        assert_eq!(cb.fingerprint, a.fingerprint);
        assert_eq!(ca.hash, cb.hash, "no one in the middle: the same handshake hash");
        assert!(ca.initiator && !cb.initiator);
        ca.send(&Wire::Ping).unwrap();
        assert_eq!(cb.expect(Duration::from_secs(2)).unwrap(), Wire::Ping);
        // A message bigger than one Noise message arrives whole.
        let big = "x".repeat(200_000);
        cb.send(&Wire::Refused { reason: big.clone() }).unwrap();
        assert_eq!(ca.expect(Duration::from_secs(5)).unwrap(), Wire::Refused { reason: big });
        assert_eq!(ca.recv(Duration::from_millis(50)).unwrap(), None, "nothing more");
        // Something from a newer build is Unknown, not an error.
        let raw = serde_json::from_str::<Wire>(r#"{"t":"offer","what":"file"}"#).unwrap();
        assert_eq!(raw, Wire::Unknown);
        cb.finish();
        assert!(ca.recv(Duration::from_secs(2)).is_err(), "closed");
    }

    #[test]
    fn a_payload_must_be_signed_by_the_key_it_names() {
        let a = ident("sig");
        let la = Local::new(&a);
        let kp = builder().generate_keypair().unwrap();
        // Right key, but it signed another static key.
        assert!(device_key(&la.payload, Some(&kp.public)).is_err());
        assert!(device_key(&la.payload[..50], Some(&kp.public)).is_err());
        assert!(device_key(&la.payload, None).is_err());
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(unhex(&hex(&[0, 1, 0xab, 0xff])).unwrap(), vec![0, 1, 0xab, 0xff]);
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }
}
