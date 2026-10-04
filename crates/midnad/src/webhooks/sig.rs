//! HMAC-SHA256 signature checks for GitHub (`X-Hub-Signature-256: sha256=<hex>`) and
//! Bitbucket Cloud (`X-Hub-Signature: sha256=<hex>`). Comparison is constant time.
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

pub fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `sha256=<hex>` for `body` under `secret` (what GitHub/Bitbucket send).
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut m = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    m.update(body);
    format!("sha256={}", hex_encode(&m.finalize().into_bytes()))
}

/// Does `header` (`sha256=<hex>`) sign `body` under `secret`? Constant-time compare.
pub fn verify(secret: &[u8], body: &[u8], header: &str) -> bool {
    let Some(hex) = header.trim().strip_prefix("sha256=") else { return false };
    let Some(expected) = hex_decode(hex) else { return false };
    let Ok(mut m) = HmacSha256::new_from_slice(secret) else { return false };
    m.update(body);
    m.verify_slice(&expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn github_doc_vector() {
        // From GitHub's "Validating webhook deliveries" docs.
        let h = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(verify(b"It's a Secret to Everybody", b"Hello, World!", h));
        assert_eq!(sign(b"It's a Secret to Everybody", b"Hello, World!"), h);
        assert!(!verify(b"wrong", b"Hello, World!", h));
        assert!(!verify(b"It's a Secret to Everybody", b"Hello, World?", h));
        assert!(!verify(b"It's a Secret to Everybody", b"Hello, World!", "sha1=abc"));
        assert!(!verify(b"It's a Secret to Everybody", b"Hello, World!", "sha256=zz"));
    }
}
