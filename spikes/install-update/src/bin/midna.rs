//! Spike "app": the bundle's main executable (LSUIElement, no UI). Commands:
//!   register | unregister | status          SMAppService.agent(plistName:)
//!   legacy-install | legacy-remove           ~/Library/LaunchAgents + launchctl (macOS < 13)
//!   ping | ax | update <latest.json url> | version
use objc2_foundation::NSString;
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use std::{env, fs};

const VERSION: &str = match option_env!("MIDNA_VERSION") { Some(v) => v, None => "0.0.0" };
const LABEL: &str = "com.mrgnhnt.midna.daemon";
const PUBKEY: &str = env!("MIDNA_UPDATER_PUBKEY");

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const std::ffi::c_void;
}

fn support() -> PathBuf { PathBuf::from(env::var("HOME").unwrap()).join("Library/Application Support/com.mrgnhnt.midna") }
fn bundle() -> PathBuf { env::current_exe().unwrap().parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf() }
fn agent() -> objc2::rc::Retained<SMAppService> {
    unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(&format!("{LABEL}.plist"))) }
}
fn status_str(s: SMAppServiceStatus) -> &'static str {
    match s { SMAppServiceStatus::NotRegistered => "notRegistered", SMAppServiceStatus::Enabled => "enabled",
        SMAppServiceStatus::RequiresApproval => "requiresApproval", SMAppServiceStatus::NotFound => "notFound", _ => "?" }
}
fn say(msg: &str) {
    println!("{msg}");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(support().join("app.log")) {
        let _ = writeln!(f, "app v{VERSION} pid={} {msg}", std::process::id());
    }
}
fn daemon(cmd: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(support().join("midnad.sock"))?;
    writeln!(s, "{cmd}")?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn main() {
    let _ = fs::create_dir_all(support());
    let args: Vec<String> = env::args().collect();
    let uid = unsafe { libc::getuid() };
    match args.get(1).map(String::as_str).unwrap_or("launched") {
        "version" => say(&format!("midna v{VERSION} bundle={}", bundle().display())),
        "status" => say(&format!("SMAppService status={}", status_str(unsafe { agent().status() }))),
        "register" => {
            let a = agent();
            let r = unsafe { a.registerAndReturnError() };
            say(&format!("register -> {:?}; status={}", r.as_ref().map_err(|e| e.to_string()), status_str(unsafe { a.status() })));
        }
        "unregister" => {
            let a = agent();
            let r = unsafe { a.unregisterAndReturnError() };
            say(&format!("unregister -> {:?}; status={}", r.as_ref().map_err(|e| e.to_string()), status_str(unsafe { a.status() })));
        }
        "legacy-install" => {
            let plist = PathBuf::from(env::var("HOME").unwrap()).join(format!("Library/LaunchAgents/{LABEL}.plist"));
            let exe = bundle().join("Contents/MacOS/midnad");
            fs::write(&plist, format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>serve</string></array>
  <key>KeepAlive</key><true/>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
"#, exe.display())).unwrap();
            let o = Command::new("launchctl").args(["bootstrap", &format!("gui/{uid}"), plist.to_str().unwrap()]).output().unwrap();
            say(&format!("legacy bootstrap rc={:?} {}", o.status.code(), String::from_utf8_lossy(&o.stderr).trim()));
        }
        "legacy-remove" => {
            let o = Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{LABEL}")]).output().unwrap();
            let plist = PathBuf::from(env::var("HOME").unwrap()).join(format!("Library/LaunchAgents/{LABEL}.plist"));
            let _ = fs::remove_file(plist);
            say(&format!("legacy bootout rc={:?} {}", o.status.code(), String::from_utf8_lossy(&o.stderr).trim()));
        }
        "ping" => say(&format!("{:?}", daemon("PING"))),
        "ax" => say(&format!("AXIsProcessTrusted={}", unsafe { AXIsProcessTrusted() })),
        "update" => update(&args[2]),
        // Launch via `open --args ax-wait <endpoint>`: prompt, poll, then update.
        "ax-wait" => {
            use objc2_foundation::{NSDictionary, NSNumber};
            let key: &NSString = unsafe { &*(kAXTrustedCheckOptionPrompt as *const NSString) };
            let yes = NSNumber::new_bool(true);
            let opts = NSDictionary::from_slices(&[key], &[&*yes]);
            let t = unsafe { AXIsProcessTrustedWithOptions(objc2::rc::Retained::as_ptr(&opts) as *const _) };
            say(&format!("ax-wait: AXIsProcessTrustedWithOptions(prompt=true) -> {t}; daemon: {:?}", daemon("PING")));
            let start = Instant::now();
            while !unsafe { AXIsProcessTrusted() } {
                if start.elapsed().as_secs() > 1800 { return say("ax-wait: timed out after 30 min"); }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            say(&format!("ax-wait: TRUSTED after {:?}; running update", start.elapsed()));
            update(&args[2]);
        }
        other => {
            // Launched by LaunchServices (e.g. relaunch after update).
            say(&format!("launched ({other}) v{VERSION} from {}; daemon: {:?}; AX={}", bundle().display(), daemon("PING"), unsafe { AXIsProcessTrusted() }));
        }
    }
}

fn update(endpoint: &str) {
    let t0 = Instant::now();
    let cfg = cargo_packager_updater::Config {
        endpoints: vec![endpoint.parse().unwrap()],
        pubkey: PUBKEY.into(),
        ..Default::default()
    };
    let up = match cargo_packager_updater::check_update(VERSION.parse().unwrap(), cfg) {
        Ok(Some(u)) => u,
        Ok(None) => return say("no update"),
        Err(e) => return say(&format!("check failed: {e}")),
    };
    say(&format!("[{:?}] update available v{} -> v{} at {} (extract_path={})", t0.elapsed(), VERSION, up.version, up.download_url, up.extract_path.display()));
    say(&format!("daemon before install: {:?}", daemon("PING")));
    let bytes = match up.download() { Ok(b) => b, Err(e) => return say(&format!("download/verify failed: {e}")) };
    say(&format!("[{:?}] downloaded {} bytes, minisign signature verified", t0.elapsed(), bytes.len()));
    if let Err(e) = up.install(bytes) { return say(&format!("install failed: {e}")); }
    say(&format!("[{:?}] bundle replaced", t0.elapsed()));
    say(&format!("daemon after bundle replace (still old image): {:?}", daemon("PING")));
    let new_d = bundle().join("Contents/MacOS/midnad");
    say(&format!("[{:?}] UPGRADE -> {:?}", t0.elapsed(), daemon(&format!("UPGRADE {}", new_d.display()))));
    std::thread::sleep(std::time::Duration::from_millis(300));
    say(&format!("[{:?}] daemon after re-exec: {:?}", t0.elapsed(), daemon("PING")));
    // Relaunch the (new) app via LaunchServices, then exit.
    let o = Command::new("open").args(["-n", bundle().to_str().unwrap(), "--args", "relaunched"]).status();
    say(&format!("[{:?}] relaunch via open -n: {o:?}; old app exiting", t0.elapsed()));
}
