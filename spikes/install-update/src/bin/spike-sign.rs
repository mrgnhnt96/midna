//! Updater signing helper, equivalent to `tauri signer generate/sign`:
//! minisign keys, and the signature is base64(minisign .sig text).
use base64::{Engine, engine::general_purpose::STANDARD};
use minisign::{KeyPair, SecretKeyBox};
use std::fs;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a[1].as_str() {
        "keygen" => {
            let kp = KeyPair::generate_encrypted_keypair(Some("spike".into())).unwrap();
            fs::write(&a[2], kp.sk.to_box(None).unwrap().to_string()).unwrap();
            // Tauri/cargo-packager pubkey = base64 of the minisign .pub file text
            println!("{}", STANDARD.encode(kp.pk.to_box().unwrap().to_string()));
        }
        "sign" => {
            let sk = SecretKeyBox::from_string(&fs::read_to_string(&a[2]).unwrap())
                .unwrap()
                .into_secret_key(Some("spike".into()))
                .unwrap();
            let data = fs::File::open(&a[3]).unwrap();
            let sig = minisign::sign(None, &sk, data, Some(&a[3]), Some("midna spike")).unwrap();
            println!("{}", STANDARD.encode(sig.to_string()));
        }
        _ => panic!("keygen <sk> | sign <sk> <file>"),
    }
}
