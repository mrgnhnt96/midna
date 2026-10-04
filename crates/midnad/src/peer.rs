//! Caller identity from the Unix socket peer: LOCAL_PEERPID / LOCAL_PEERTOKEN, `proc_pidpath`,
//! the peer's code signature, and its process ancestry.
//!
//! Limitation (see docs/SECURITY.md): every process runs as the same user, so none of this is
//! authentication against a determined local attacker. It stops an agent from *casually* or
//! *accidentally* getting the human's role through midna's own interfaces:
//! - Signed builds: the GUI must carry this daemon's Team ID and the app's bundle identifier
//!   (checked by the kernel-backed code-signing API, keyed by the peer's audit token).
//! - Unsigned builds: the GUI is matched by name (`midna-app`, or inside `Midna.app`), and in
//!   debug builds only, by the `MIDNA_APP_PATH` override.
//! - Any process running inside one of this daemon's terminals is an agent, whatever its
//!   executable, and it can only speak for the terminal it runs in.
use std::os::fd::RawFd;

const SOL_LOCAL: libc::c_int = 0;
const LOCAL_PEERPID: libc::c_int = 0x002;
const LOCAL_PEERTOKEN: libc::c_int = 0x006;

pub fn peer_pid(fd: RawFd) -> Option<i32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let r = unsafe { libc::getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, &mut pid as *mut _ as *mut _, &mut len) };
    (r == 0 && pid > 0).then_some(pid)
}

/// The peer's audit token (32 bytes), which names the exact process image (no pid reuse race).
pub fn peer_token(fd: RawFd) -> Option<[u8; 32]> {
    let mut tok = [0u8; 32];
    let mut len = tok.len() as libc::socklen_t;
    let r = unsafe { libc::getsockopt(fd, SOL_LOCAL, LOCAL_PEERTOKEN, tok.as_mut_ptr() as *mut _, &mut len) };
    (r == 0 && len as usize == tok.len()).then_some(tok)
}

pub fn pid_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

/// Parent pid, or None for a gone process / pid 0.
pub fn parent_pid(pid: i32) -> Option<i32> {
    if pid <= 1 {
        return None;
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut _, size) };
    (n == size).then_some(info.pbi_ppid as i32)
}

/// `pid` and its ancestors, nearest first (bounded, stops at launchd).
pub fn ancestry(pid: i32) -> Vec<i32> {
    let mut out = vec![pid];
    let mut cur = pid;
    while out.len() < 64 {
        match parent_pid(cur) {
            Some(p) if p > 1 && !out.contains(&p) => {
                out.push(p);
                cur = p;
            }
            _ => break,
        }
    }
    out
}

/// Which of `terminals` (session id, shell pid) does `pid` run inside? Matches the nearest
/// ancestor that is a terminal's shell, or the terminal's POSIX session (processes that
/// `setsid` keep their ancestry; processes that double-fork away keep their session id).
pub fn terminal_of(pid: i32, terminals: &[(String, i32)]) -> Option<String> {
    if terminals.is_empty() {
        return None;
    }
    for a in ancestry(pid) {
        if let Some((id, _)) = terminals.iter().find(|(_, p)| *p == a) {
            return Some(id.clone());
        }
    }
    let sid = unsafe { libc::getsid(pid) };
    terminals.iter().find(|(_, p)| sid > 1 && *p == sid).map(|(id, _)| id.clone())
}

fn canon(p: &str) -> String {
    std::fs::canonicalize(p).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string())
}

/// How the daemon recognizes the human GUI.
#[derive(Clone, Debug, Default)]
pub struct GuiIdentity {
    /// Exact executable (MIDNA_APP_PATH; debug builds and tests only).
    pub app_path: Option<String>,
    /// Code-signing requirement the GUI must satisfy (signed builds).
    pub requirement: Option<String>,
}

impl GuiIdentity {
    /// Is the peer (pid / audit token / executable path) the midna GUI?
    pub fn matches(&self, exe: &str, pid: i32, token: Option<&[u8; 32]>) -> bool {
        if let Some(app) = &self.app_path {
            return canon(exe) == canon(app);
        }
        if let Some(req) = &self.requirement {
            return codesign::satisfies(pid, token, req);
        }
        is_gui_name(exe)
    }
}

/// The unsigned-build convention: an executable named `midna-app`, or one inside `Midna.app`.
pub fn is_gui_name(exe: &str) -> bool {
    let name = exe.rsplit('/').next().unwrap_or("");
    name == "midna-app" || exe.contains("/Midna.app/Contents/MacOS/")
}

/// Back-compat helper used by tests: name rule, or exact path when given.
pub fn is_gui(exe: &str, app_path: Option<&str>) -> bool {
    match app_path {
        Some(app) => canon(exe) == canon(app),
        None => is_gui_name(exe),
    }
}

pub mod codesign {
    //! Thin FFI over Security.framework's code-signing API.
    use core_foundation::base::TCFType;
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFTypeRef, OSStatus};
    use core_foundation_sys::dictionary::CFDictionaryRef;
    use core_foundation_sys::string::CFStringRef;
    use security_framework::os::macos::code_signing::{Flags, GuestAttributes, SecCode, SecRequirement};
    use std::str::FromStr;

    const K_SEC_CS_SIGNING_INFORMATION: u32 = 1 << 1;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecCodeCopySigningInformation(code: CFTypeRef, flags: u32, information: *mut CFDictionaryRef) -> OSStatus;
        static kSecCodeInfoTeamIdentifier: CFStringRef;
        static kSecCodeInfoIdentifier: CFStringRef;
    }

    /// (identifier, team id) of this process's own signature; team is None when ad-hoc/unsigned.
    pub fn own_identity() -> Option<(String, Option<String>)> {
        let me = SecCode::for_self(Flags::NONE).ok()?;
        let mut info: CFDictionaryRef = std::ptr::null();
        let st = unsafe { SecCodeCopySigningInformation(me.as_CFTypeRef(), K_SEC_CS_SIGNING_INFORMATION, &mut info) };
        if st != 0 || info.is_null() {
            return None;
        }
        let dict: CFDictionary<CFString, core_foundation::base::CFType> = unsafe { CFDictionary::wrap_under_create_rule(info) };
        let get = |k: CFStringRef| -> Option<String> {
            let key = unsafe { CFString::wrap_under_get_rule(k) };
            dict.find(&key).and_then(|v| v.downcast::<CFString>()).map(|s| s.to_string())
        };
        let id = get(unsafe { kSecCodeInfoIdentifier })?;
        Some((id, get(unsafe { kSecCodeInfoTeamIdentifier })))
    }

    /// The requirement the GUI must meet, derived from this daemon's own signature: same Team
    /// ID, and the app's identifier (ours minus `.daemon`). None for unsigned / ad-hoc builds.
    pub fn gui_requirement() -> Option<String> {
        let (id, team) = own_identity()?;
        let team = team.filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()))?;
        let app_id = id.strip_suffix(".daemon").unwrap_or(&id);
        if !app_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
            return None;
        }
        Some(format!("identifier \"{app_id}\" and anchor apple generic and certificate leaf[subject.OU] = \"{team}\""))
    }

    /// Does the running process (by audit token, else pid) satisfy `requirement`?
    pub fn satisfies(pid: i32, token: Option<&[u8; 32]>, requirement: &str) -> bool {
        let Ok(req) = SecRequirement::from_str(requirement) else { return false };
        let mut attrs = GuestAttributes::new();
        let data;
        match token {
            Some(t) => {
                data = CFData::from_buffer(t);
                attrs.set_audit_token(data.as_concrete_TypeRef());
            }
            None => attrs.set_pid(pid),
        }
        let Ok(code) = SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE) else { return false };
        code.check_validity(Flags::NONE, &req).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_rule() {
        assert!(is_gui_name("/Applications/Midna.app/Contents/MacOS/midna-app"));
        assert!(is_gui_name("/x/target/debug/midna-app"));
        assert!(!is_gui_name("/x/target/debug/midna"));
        assert!(!is_gui_name("/x/midna-app-evil"));
    }

    #[test]
    fn ancestry_and_terminal_of() {
        let me = std::process::id() as i32;
        let chain = ancestry(me);
        assert_eq!(chain[0], me);
        assert!(chain.len() >= 2, "a test process has a parent: {chain:?}");
        let parent = chain[1];
        // Running "inside" a terminal whose shell is our parent.
        assert_eq!(terminal_of(me, &[("aaaa".into(), 999_999), ("bbbb".into(), parent)]).as_deref(), Some("bbbb"));
        assert_eq!(terminal_of(me, &[("aaaa".into(), 999_999)]), None);
    }

    #[test]
    fn unsigned_test_binary_has_no_requirement() {
        // `cargo test` binaries are ad-hoc (linker) signed at most: no Team ID, so the GUI is
        // matched by name. A requirement that nothing satisfies is refused.
        assert!(codesign::gui_requirement().is_none());
        assert!(!codesign::satisfies(std::process::id() as i32, None, "identifier \"com.example.none\" and anchor apple generic"));
    }
}
