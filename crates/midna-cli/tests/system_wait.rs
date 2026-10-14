//! `midna system wait` against an in-process daemon that sees a busy Mac. Its own binary:
//! MIDNA_DEBUG_LOAD is process-wide, and the other CLI tests want a calm Mac.
use serde_json::json;
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
    // Three times as much work as cores: 300% against the default system.busy_load of 200%.
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    unsafe { std::env::set_var("MIDNA_DEBUG_LOAD", (cpus * 3).to_string()) };
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
    assert!(String::from_utf8_lossy(&o.stdout).contains("300% per core, busy"), "{o:?}");

    // It returns as soon as the load is under system.busy_load, overloaded or not.
    let child = midna(&sock, &["system", "wait", "--json"]).spawn().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let mut h = midna_proto::Client::connect(&sock).unwrap();
    h.set_caller(None);
    h.call_value("settings.set", json!({ "key": "system.busy_load", "value": 400 })).unwrap();
    let start = Instant::now();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!((v["busy"].as_bool(), v["load_percent"].as_u64()), (Some(false), Some(300)), "{v}");

    handle.shutdown();
    let _ = std::fs::remove_dir_all(&home);
}
