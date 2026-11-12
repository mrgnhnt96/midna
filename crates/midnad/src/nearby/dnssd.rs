//! A thin layer over macOS's DNS-SD client (`dns_sd.h`, in libSystem). Every ref is used on the
//! one thread that made it: callbacks run inside `Ref::process` and only queue an [`Ev`] for that
//! thread to read with [`take_events`].
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};

type ServiceRef = *mut c_void;
type RegisterReply = extern "C" fn(ServiceRef, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);
type BrowseReply = extern "C" fn(ServiceRef, u32, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);
type ResolveReply = extern "C" fn(ServiceRef, u32, u32, i32, *const c_char, *const c_char, u16, u16, *const u8, *mut c_void);

unsafe extern "C" {
    fn DNSServiceRegister(
        sd: *mut ServiceRef, flags: u32, iface: u32, name: *const c_char, regtype: *const c_char, domain: *const c_char,
        host: *const c_char, port: u16, txt_len: u16, txt: *const c_void, cb: RegisterReply, ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceBrowse(sd: *mut ServiceRef, flags: u32, iface: u32, regtype: *const c_char, domain: *const c_char, cb: BrowseReply, ctx: *mut c_void) -> i32;
    fn DNSServiceResolve(
        sd: *mut ServiceRef, flags: u32, iface: u32, name: *const c_char, regtype: *const c_char, domain: *const c_char, cb: ResolveReply,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceRefSockFD(sd: ServiceRef) -> c_int;
    fn DNSServiceProcessResult(sd: ServiceRef) -> i32;
    fn DNSServiceRefDeallocate(sd: ServiceRef);
}

/// `kDNSServiceFlagsAdd`: a browse result is an add (else a remove).
const FLAG_ADD: u32 = 0x2;
/// `kDNSServiceInterfaceIndexAny`.
pub const ANY: u32 = 0;
/// `kDNSServiceInterfaceIndexLocalOnly`: this Mac only (tests).
pub const LOCAL_ONLY: u32 = u32::MAX;

/// What a callback reported. `tag` is the number the ref was made with.
#[derive(Debug)]
pub enum Ev {
    Registered { tag: usize, err: i32, name: String },
    Browsed { tag: usize, err: i32, add: bool, iface: u32, name: String, regtype: String, domain: String },
    Resolved { tag: usize, err: i32, host: String, port: u16, txt: Vec<u8> },
}

thread_local! {
    static EVENTS: RefCell<Vec<Ev>> = const { RefCell::new(Vec::new()) };
}

/// The events callbacks on this thread queued since the last call.
pub fn take_events() -> Vec<Ev> {
    EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

fn push(ev: Ev) {
    EVENTS.with(|e| e.borrow_mut().push(ev));
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: DNS-SD passes NUL-terminated strings valid for the callback's duration.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

extern "C" fn on_register(_: ServiceRef, _flags: u32, err: i32, name: *const c_char, _: *const c_char, _: *const c_char, ctx: *mut c_void) {
    push(Ev::Registered { tag: ctx as usize, err, name: text(name) });
}

extern "C" fn on_browse(
    _: ServiceRef, flags: u32, iface: u32, err: i32, name: *const c_char, regtype: *const c_char, domain: *const c_char, ctx: *mut c_void,
) {
    push(Ev::Browsed { tag: ctx as usize, err, add: flags & FLAG_ADD != 0, iface, name: text(name), regtype: text(regtype), domain: text(domain) });
}

extern "C" fn on_resolve(
    _: ServiceRef, _flags: u32, _iface: u32, err: i32, _full: *const c_char, host: *const c_char, port: u16, txt_len: u16, txt: *const u8,
    ctx: *mut c_void,
) {
    let txt = if txt.is_null() {
        vec![]
    } else {
        // SAFETY: DNS-SD passes `txt_len` readable bytes at `txt`.
        unsafe { std::slice::from_raw_parts(txt, txt_len as usize) }.to_vec()
    };
    push(Ev::Resolved { tag: ctx as usize, err, host: text(host), port: u16::from_be(port), txt });
}

/// A live DNS-SD operation; dropping it stops it (and withdraws a registration).
pub struct Ref(ServiceRef);

impl Ref {
    fn made(sd: ServiceRef, err: i32) -> Result<Ref, i32> {
        if err != 0 || sd.is_null() {
            return Err(if err != 0 { err } else { -65537 });
        }
        let r = Ref(sd);
        // An upgrade execs a new midnad; its socket to mDNSResponder mustn't leak into it.
        // SAFETY: fcntl on a descriptor DNS-SD owns.
        unsafe { libc::fcntl(r.fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        Ok(r)
    }

    pub fn fd(&self) -> c_int {
        // SAFETY: self.0 is a live ref.
        unsafe { DNSServiceRefSockFD(self.0) }
    }

    /// Read what mDNSResponder sent (call when `fd` is readable); callbacks run in here.
    pub fn process(&self) -> Result<(), i32> {
        // SAFETY: self.0 is a live ref, used on the thread that made it.
        match unsafe { DNSServiceProcessResult(self.0) } {
            0 => Ok(()),
            e => Err(e),
        }
    }
}

impl Drop for Ref {
    fn drop(&mut self) {
        // SAFETY: self.0 is live and deallocated once.
        unsafe { DNSServiceRefDeallocate(self.0) }
    }
}

fn cstr(s: &str) -> Result<CString, i32> {
    CString::new(s).map_err(|_| -65540)
}

/// Advertise `name` (mDNS renames it on a clash) of type `regtype` on `port`.
pub fn register(tag: usize, iface: u32, name: &str, regtype: &str, port: u16, txt: &[u8]) -> Result<Ref, i32> {
    let (name, regtype) = (cstr(name)?, cstr(regtype)?);
    let mut sd: ServiceRef = std::ptr::null_mut();
    // SAFETY: valid out-pointer and NUL-terminated strings; DNS-SD copies the TXT bytes.
    let err = unsafe {
        DNSServiceRegister(
            &mut sd, 0, iface, name.as_ptr(), regtype.as_ptr(), std::ptr::null(), std::ptr::null(), port.to_be(), txt.len() as u16,
            txt.as_ptr().cast(), on_register, tag as *mut c_void,
        )
    };
    Ref::made(sd, err)
}

pub fn browse(tag: usize, iface: u32, regtype: &str) -> Result<Ref, i32> {
    let regtype = cstr(regtype)?;
    let mut sd: ServiceRef = std::ptr::null_mut();
    // SAFETY: valid out-pointer and NUL-terminated strings.
    let err = unsafe { DNSServiceBrowse(&mut sd, 0, iface, regtype.as_ptr(), std::ptr::null(), on_browse, tag as *mut c_void) };
    Ref::made(sd, err)
}

pub fn resolve(tag: usize, iface: u32, name: &str, regtype: &str, domain: &str) -> Result<Ref, i32> {
    let (name, regtype, domain) = (cstr(name)?, cstr(regtype)?, cstr(domain)?);
    let mut sd: ServiceRef = std::ptr::null_mut();
    // SAFETY: valid out-pointer and NUL-terminated strings.
    let err = unsafe { DNSServiceResolve(&mut sd, 0, iface, name.as_ptr(), regtype.as_ptr(), domain.as_ptr(), on_resolve, tag as *mut c_void) };
    Ref::made(sd, err)
}

/// A DNS-SD error code in words.
pub fn describe(err: i32) -> String {
    let what = match err {
        -65537 => "unknown error",
        -65539 => "out of memory",
        -65540 => "bad parameter",
        -65548 => "name conflict",
        -65555 => "no auth",
        -65563 => "mDNSResponder isn't running",
        -65569 => "lost the connection to mDNSResponder",
        -65570 | -65571 => "denied (is Local Network access off for Midna in System Settings › Privacy & Security?)",
        _ => "DNS-SD error",
    };
    format!("{what} ({err})")
}
