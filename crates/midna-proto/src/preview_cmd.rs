//! Running a link's preview command where the link lives: the command an app declares for its
//! links (`MidnaLinkPreview` in its Info.plist), or a link pattern's `preview:"<command> …"`.
//! The app runs them for this Mac's windows; midnad runs them for a window that drives this Mac
//! from another one (`links.preview`). Each prints one JSON object for the card,
//! `{"title": "…", "lines": […], …}`, which the app reads.
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// An app's preview command is a local tool, so it gets little time.
pub const APP_TIMEOUT: Duration = Duration::from_millis(1500);
/// Pattern previews usually ask a web API (Jira), so they get longer than an app's local tool.
pub const PATTERN_TIMEOUT: Duration = Duration::from_secs(5);
/// The most of a preview command's output that's read.
pub const OUT_LIMIT: usize = 64 << 10;

/// `MidnaLinkPreview` (as `plutil` prints it in JSON) -> (path inside the bundle, args).
pub fn parse_preview_command(v: &Value) -> Option<(String, Vec<String>)> {
    let mut parts = match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()?,
        _ => return None,
    }
    .into_iter();
    let path = parts.next().filter(|p| !p.is_empty() && !p.starts_with('/') && !Path::new(p).components().any(|c| matches!(c, std::path::Component::ParentDir)))?;
    Some((path, parts.collect()))
}

/// The command `bundle` declares for previews, resolved inside it (a symlink out of it is refused).
pub fn preview_command(bundle: &Path) -> Option<(PathBuf, Vec<String>)> {
    let mut cmd = Command::new("/usr/bin/plutil");
    cmd.args(["-extract", "MidnaLinkPreview", "json", "-o", "-"]).arg(bundle.join("Contents/Info.plist"));
    let (ok, out) = run_capped(cmd, 16 << 10, Duration::from_secs(2))?;
    let (rel, args) = parse_preview_command(&serde_json::from_slice(&out).ok().filter(|_| ok)?)?;
    let bundle = bundle.canonicalize().ok()?;
    let path = bundle.join(rel).canonicalize().ok()?;
    (path.starts_with(&bundle) && path.is_file()).then_some((path, args))
}

/// `<path> <args…> <url>`'s output, when it exits 0 in time.
pub fn run_preview(path: &Path, args: &[String], url: &str, timeout: Duration) -> Option<Vec<u8>> {
    let mut cmd = Command::new(path);
    cmd.args(args).arg(url);
    let (ok, out) = run_capped(cmd, OUT_LIMIT, timeout)?;
    ok.then_some(out)
}

/// `argv[0]` (a path, `~/…`, or a name looked up on `path`) run with `argv[1..]`.
pub fn pattern_command(argv: &[String], path: &str) -> Option<Command> {
    let prog = argv.first().filter(|p| !p.is_empty())?;
    let bin = match prog.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").ok()?).join(rest),
        None if prog.contains('/') => PathBuf::from(prog),
        None => std::env::split_paths(path).map(|d| d.join(prog)).find(|p| p.is_file())?,
    };
    let mut cmd = Command::new(bin);
    cmd.args(&argv[1..]).env("PATH", path);
    Some(cmd)
}

/// A link pattern's preview command's output, when it exits 0 in time.
pub fn run_pattern(argv: &[String], path: &str, timeout: Duration) -> Option<Vec<u8>> {
    let (ok, out) = run_capped(pattern_command(argv, path)?, OUT_LIMIT, timeout)?;
    ok.then_some(out)
}

/// The PATH preview commands run with: the login shell's (an app's or a daemon's own from
/// launchd is minimal) when `ask_shell`, else, or when it can't say, the usual places.
/// Callers keep it: asking the shell takes a moment.
pub fn user_path(ask_shell: bool) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let fallback = format!("{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
    if ask_shell { login_shell_path() } else { None }.filter(|p| !p.is_empty()).unwrap_or(fallback)
}

/// `$PATH` as the user's login shell sets it, None when it can't say within 5 seconds.
pub fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", "printf '\\n__MIDNA_PATH__%s\\n' \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut()?, &mut out).ok()?;
    out.lines().find_map(|l| l.strip_prefix("__MIDNA_PATH__")).map(str::to_string)
}

/// Run `cmd`, keeping at most `limit` bytes of its output; killed after `timeout`, with anything
/// it started (in its own process group, so a child holding the pipe can't outlive it).
pub fn run_capped(mut cmd: Command, limit: usize, timeout: Duration) -> Option<(bool, Vec<u8>)> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = vec![];
        let _ = (&mut out).take(limit as u64).read_to_end(&mut buf);
        buf // dropping the pipe here stops a writer that has more
    });
    let until = Instant::now() + timeout;
    let group = -(child.id() as libc::pid_t);
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(40)),
            _ => {
                unsafe { libc::kill(group, libc::SIGKILL) };
                let _ = child.wait();
                break false;
            }
        }
    };
    // a background child it left behind would hold the pipe open and the read with it
    unsafe { libc::kill(group, libc::SIGKILL) };
    Some((ok, reader.join().ok()?))
}
