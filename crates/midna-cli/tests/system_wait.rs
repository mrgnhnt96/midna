//! `midna system wait` against an in-process daemon that sees a busy Mac. Its own binary:
//! MIDNA_DEBUG_CPU is process-wide, and the other CLI tests want a calm Mac.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn midna(sock: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_midna"));
    cmd.args(args).env("MIDNA_SOCKET", sock).env_remove("MIDNA_SESSION").env_remove("MIDNA_HOME");
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

#[test]
fn wait_returns_once_the_busy_gate_would_open() {
    // Every core busy, nearly all of it midna's terminals: the busy gate holds agents back.
    unsafe { std::env::set_var("MIDNA_DEBUG_CPU", "100:95") };
    let home = PathBuf::from(format!("/tmp/midna-w-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut cfg = midnad::Config::for_home(home.clone());
    cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
    cfg.cli_path = env!("CARGO_BIN_EXE_midna").into();
    let handle = midnad::start(cfg).unwrap();
    let sock = home.join("midnad.sock").to_string_lossy().into_owned();

    // Still busy at the timeout: exit 4, with the load.
    let o = midna(&sock, &["system", "wait", "--timeout", "1"]).output().unwrap();
    assert_eq!(o.status.code(), Some(4), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stdout).contains("CPU 100% of the Mac: midna's terminals 95%"), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stdout).contains("because of midna's terminals"), "{o:?}");

    // It returns as soon as the gate would open: here other apps become the cause, though the
    // CPU is still busy.
    let child = midna(&sock, &["system", "wait", "--json"]).spawn().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    unsafe { std::env::set_var("MIDNA_DEBUG_CPU", "100:10") };
    let start = Instant::now();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!((v["held"].as_bool(), v["cpu_busy"].as_bool(), v["cpu_percent"].as_u64()), (Some(false), Some(true), Some(100)), "{v}");

    handle.shutdown();
    let _ = std::fs::remove_dir_all(&home);
}
