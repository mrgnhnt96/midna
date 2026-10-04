//! midna-release: update-signing helper (docs/RELEASING.md).
//!
//!   midna-release keygen <secret.key>            write a PKCS#8 ed25519 key (0600), print the public key (base64)
//!   midna-release pubkey <secret.key>            print the public key (base64, raw 32 bytes)
//!   midna-release feed <secret.key> <archive> --version V --url URL [--notes TEXT] [--min-macos 12.0] [--arch universal]
//!                                                print a signed feed entry (JSON)
//!   midna-release verify <feed.json> <pubkey-b64> [archive]   check a feed entry (and the archive's sha256)
//!
//! The signed message must match `signed_message` in crates/midna-app/src/updater.rs.
use base64::Engine;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde_json::{Value, json};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn die(msg: &str) -> ! {
    eprintln!("midna-release: {msg}");
    std::process::exit(1);
}

fn signed_message(version: &str, sha256: &str, min_macos: &str, arch: &str) -> String {
    format!("midna-update-v1\nversion={version}\nsha256={}\nminimum_macos={min_macos}\narch={arch}\n", sha256.to_ascii_lowercase())
}

fn load_key(path: &str) -> Ed25519KeyPair {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let der = b64().decode(raw.trim()).unwrap_or_else(|_| die("secret key isn't base64 PKCS#8"));
    Ed25519KeyPair::from_pkcs8(&der).unwrap_or_else(|_| die("secret key isn't an ed25519 PKCS#8 key"))
}

fn sha256_file(path: &str) -> (String, u64) {
    let mut f = std::fs::File::open(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    let mut n_total = 0u64;
    loop {
        let n = f.read(&mut buf).unwrap_or_else(|e| die(&e.to_string()));
        if n == 0 {
            break;
        }
        n_total += n as u64;
        ctx.update(&buf[..n]);
    }
    (ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect(), n_total)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let path = args.get(1).unwrap_or_else(|| die("keygen <secret.key>"));
            if std::path::Path::new(path).exists() {
                die(&format!("{path} exists; refusing to overwrite a release key"));
            }
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap_or_else(|_| die("keygen failed"));
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap_or_else(|e| die(&e.to_string()));
            std::io::Write::write_all(&mut f, format!("{}\n", b64().encode(pkcs8.as_ref())).as_bytes()).unwrap();
            let kp = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
            println!("{}", b64().encode(kp.public_key().as_ref()));
        }
        Some("pubkey") => {
            let kp = load_key(args.get(1).unwrap_or_else(|| die("pubkey <secret.key>")));
            println!("{}", b64().encode(kp.public_key().as_ref()));
        }
        Some("feed") => {
            let (key, archive) = match (args.get(1), args.get(2)) {
                (Some(k), Some(a)) => (k, a),
                _ => die("feed <secret.key> <archive> --version V --url URL"),
            };
            let version = flag(&args, "--version").unwrap_or_else(|| die("--version is required"));
            let url = flag(&args, "--url").unwrap_or_else(|| die("--url is required"));
            let notes = flag(&args, "--notes").unwrap_or_default();
            let min = flag(&args, "--min-macos").unwrap_or_else(|| "12.0".into());
            let arch = flag(&args, "--arch").unwrap_or_else(|| "universal".into());
            let kp = load_key(key);
            let (sha, size) = sha256_file(archive);
            let sig = kp.sign(signed_message(&version, &sha, &min, &arch).as_bytes());
            let pub_date = std::process::Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            let entry = json!({
                "version": version, "pub_date": pub_date, "notes": notes, "url": url,
                "sha256": sha, "size": size, "minimum_macos": min, "arch": arch,
                "signature": b64().encode(sig.as_ref()),
            });
            println!("{}", serde_json::to_string_pretty(&entry).unwrap());
        }
        Some("verify") => {
            let (feed, key) = match (args.get(1), args.get(2)) {
                (Some(f), Some(k)) => (f, k),
                _ => die("verify <feed.json> <pubkey-b64> [archive]"),
            };
            let v: Value = serde_json::from_str(&std::fs::read_to_string(feed).unwrap_or_else(|e| die(&e.to_string()))).unwrap_or_else(|e| die(&e.to_string()));
            let s = |k: &str, d: &str| v.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
            let msg = signed_message(&s("version", ""), &s("sha256", ""), &s("minimum_macos", "12.0"), &s("arch", "universal"));
            let sig = b64().decode(s("signature", "")).unwrap_or_else(|_| die("signature isn't base64"));
            let pk = b64().decode(key.trim()).unwrap_or_else(|_| die("public key isn't base64"));
            UnparsedPublicKey::new(&ED25519, pk).verify(msg.as_bytes(), &sig).unwrap_or_else(|_| die("signature does NOT verify"));
            if let Some(a) = args.get(3) {
                let (sha, _) = sha256_file(a);
                if sha != s("sha256", "").to_ascii_lowercase() {
                    die("archive sha256 does NOT match the feed");
                }
            }
            println!("ok: {} signed by {}", s("version", ""), key.trim());
        }
        _ => die("usage: keygen | pubkey | feed | verify (see src/main.rs)"),
    }
}
