//! The contents of file shares: files, images and folders between paired devices.
//!
//! Sending (`share.send` kind file) walks the path once: every regular file in it (symlinks and
//! special files are left out) goes in the offer with its size and SHA-256, so the offer shows
//! the size and the receiver can check what arrives. Nothing else moves until the receiving person
//! accepts. Then the receiver's `nearby-transfer` thread fetches the files one by one over a
//! connection of its own: [`Wire::FileGet`] names a file and a byte offset, and the sender streams
//! [`Wire::FileChunk`]s from there and a [`Wire::FileEnd`]. Files are staged under
//! `nearby/incoming/<share id>/` in this home, so a dropped connection or a restart picks up at
//! the length already staged. Each file is hashed as it arrives and must match its SHA-256 (one
//! mismatch starts the file over; a second gives up). When every file is in, the file or the
//! folder's tree moves into `nearby.downloads` (default `~/Downloads/Midna`) under a name not
//! taken there, marked as downloaded (`com.apple.quarantine`) and never executable, and the
//! sender is told ([`Wire::FileDone`]).
//!
//! What a peer names can't reach outside the target folder: the offer is refused unless every
//! path is relative plain names (`nearby::valid_path`), paths are joined part by part again here
//! ([`safe_join`]), the top name is made safe ([`nb::safe_name`]), files are opened without
//! following symlinks, and no file takes more bytes than the offer said. The sender only serves
//! files of its own outgoing shares, to the device it offered them to, from the path it read.
use super::share::{self, Entry};
use super::transport::{self, Channel, Wire};
use super::link;
use crate::daemon::Daemon;
use base64::Engine;
use midna_proto::nearby::{self as nb, ShareDirection, ShareFile, ShareItem, SharePayload, ShareState, Transfer, TransferState};
use midna_proto::{Actor, RpcError, kinds};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Raw bytes per chunk: as base64 in a [`Wire::FileChunk`] it stays well under
/// [`transport::MAX_MESSAGE`].
pub const CHUNK: usize = 192 * 1024;
/// How often `share.progress` fires while bytes move.
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
/// How often a waiting transfer is tried again.
const RETRY: Duration = Duration::from_secs(3);
/// The longest wait for the next chunk before the connection counts as dropped.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);
/// Checksum mismatches a file may have (each starts it over) before the transfer gives up.
const MAX_BAD: u32 = 1;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

// ---- sending ----

/// `share.send` kind file: the offer for the file or folder at `path` (absolute), and where to
/// serve it from.
pub fn prepare(path: &str) -> Result<(SharePayload, PathBuf), RpcError> {
    let given = Path::new(path);
    if !given.is_absolute() {
        return Err(RpcError::bad_params(format!("{path}: give the file's absolute path")));
    }
    let source = std::fs::canonicalize(given).map_err(|e| RpcError::bad_params(format!("{path}: {e}")))?;
    let name = source.file_name().and_then(|n| n.to_str()).map(str::to_string).unwrap_or_default();
    if !nb::valid_name(&name) {
        return Err(RpcError::bad_params(format!("{path}: its name can't be sent (it needs a plain UTF-8 name)")));
    }
    let meta = std::fs::metadata(&source).map_err(|e| RpcError::bad_params(format!("{path}: {e}")))?;
    let payload = if meta.is_dir() {
        let mut files = vec![];
        let mut dirs = vec![];
        walk(&source, "", 0, &mut files, &mut dirs)?;
        SharePayload::File { name, folder: true, files, dirs }
    } else if meta.is_file() {
        let (size, sha256) = hash_file(&source).map_err(|e| RpcError::bad_params(format!("{path}: {e}")))?;
        SharePayload::File { name: name.clone(), folder: false, files: vec![ShareFile { path: name, size, sha256 }], dirs: vec![] }
    } else {
        return Err(RpcError::bad_params(format!("{path}: only files and folders can be sent")));
    };
    payload.check().map_err(RpcError::bad_params)?;
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    if json.len() + 1024 > transport::MAX_MESSAGE {
        return Err(RpcError::bad_params(format!("{path}: the list of its files is too long to offer ({} KiB); zip the folder", json.len() / 1024)));
    }
    Ok((payload, source))
}

/// Every regular file and empty folder under `dir`, as paths below the shared folder.
fn walk(dir: &Path, rel: &str, depth: usize, files: &mut Vec<ShareFile>, dirs: &mut Vec<String>) -> Result<(), RpcError> {
    let io = |e: std::io::Error| RpcError::bad_params(format!("{}: {e}", dir.display()));
    if depth >= nb::MAX_DEPTH {
        return Err(RpcError::bad_params(format!("{}: the folder goes too deep (over {} levels)", dir.display(), nb::MAX_DEPTH)));
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir).map_err(io)?.collect::<Result<_, _>>().map_err(io)?;
    entries.sort_by_key(|e| e.file_name());
    let before = files.len() + dirs.len();
    for e in entries {
        let Some(name) = e.file_name().to_str().map(str::to_string).filter(|n| nb::valid_name(n)) else {
            return Err(RpcError::bad_params(format!("{}: its name can't be sent (it needs a plain UTF-8 name)", e.path().display())));
        };
        let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let kind = e.file_type().map_err(io)?;
        if kind.is_dir() {
            walk(&e.path(), &path, depth + 1, files, dirs)?;
        } else if kind.is_file() {
            let (size, sha256) = hash_file(&e.path()).map_err(io)?;
            files.push(ShareFile { path, size, sha256 });
        }
        if files.len() + dirs.len() > nb::MAX_FILES {
            return Err(RpcError::bad_params(format!("it holds more than {} files and folders; zip it", nb::MAX_FILES)));
        }
    }
    if files.len() + dirs.len() == before && !rel.is_empty() {
        dirs.push(rel.to_string());
    }
    Ok(())
}

/// A file's length and SHA-256 (hex), as read now.
fn hash_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            return Ok((n, transport::hex(&h.finalize())));
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
}

/// The paired device `from` asks for file `file` of share `id` from `offset`: stream it on `ch`
/// and return the message that ends it ([`Wire::FileEnd`], or `refused` with why).
pub fn serve(d: &Daemon, ch: &mut Channel, from: &str, id: &str, file: usize, offset: u64) -> Wire {
    let refuse = |reason: String| Wire::Refused { reason };
    let found = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        inbox.get(id).filter(|e| e.item.direction == ShareDirection::Outgoing && e.item.peer == from && e.item.state != ShareState::Declined).cloned()
    };
    let Some(Entry { item, source: Some(source), .. }) = found else {
        return refuse(format!("this Mac has no files of share {id} for you"));
    };
    let SharePayload::File { folder, files, .. } = &item.payload else { return refuse(format!("{id} isn't a file share")) };
    let Some(f) = files.get(file) else { return refuse(format!("{id} has no file {file}")) };
    let path = if *folder {
        match safe_join(&source, &f.path) {
            Some(p) => p,
            None => return refuse(format!("{id}: bad path")),
        }
    } else {
        source.clone()
    };
    let changed = || refuse(format!("changed: {} changed on this Mac since it was offered; send it again", f.path));
    let mut src = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path) {
        Ok(s) => s,
        Err(e) => return refuse(format!("gone: {} can't be read on this Mac any more ({e})", f.path)),
    };
    if src.metadata().map(|m| m.len()).ok() != Some(f.size) {
        return changed();
    }
    if offset > f.size || src.seek(SeekFrom::Start(offset)).is_err() {
        return refuse(format!("{}: no byte {offset}", f.path));
    }
    let before: u64 = files[..file].iter().map(|f| f.size).sum();
    let mut progress = Progress::new(d, id);
    progress.update(|t| {
        t.state = TransferState::Moving;
        t.error = None;
        t.files_done = file;
        t.done = before + offset;
    });
    let delay = d.cfg.nearby.serve_delay;
    let mut buf = vec![0u8; CHUNK];
    let mut at = offset;
    while at < f.size {
        if d.shutting_down.load(Ordering::Relaxed) {
            return refuse("this Mac's midna is stopping".into());
        }
        if !link::trusted(d, from) {
            return refuse(transport::NOT_PAIRED.into());
        }
        let want = CHUNK.min((f.size - at) as usize);
        let n = match src.read(&mut buf[..want]) {
            Ok(0) => return changed(),
            Ok(n) => n,
            Err(e) => return refuse(format!("{}: {e}", f.path)),
        };
        let msg = Wire::FileChunk { id: id.to_string(), file, offset: at, data: B64.encode(&buf[..n]) };
        if ch.send(&msg).is_err() {
            progress.flush();
            return refuse("the connection closed".into());
        }
        at += n as u64;
        progress.tick(|t| t.done = before + at);
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
    }
    progress.update(|t| t.done = before + at);
    progress.flush();
    Wire::FileEnd { id: id.to_string(), file }
}

/// The device `from` says it saved share `id`'s files, or gave up on them.
pub fn finished(d: &Daemon, from: &str, id: &str, saved: bool, error: Option<String>) -> Wire {
    let mine = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        inbox.get(id).is_some_and(|e| e.item.direction == ShareDirection::Outgoing && e.item.peer == from)
    };
    if mine {
        let mut progress = Progress::new(d, id);
        progress.update(|t| {
            if saved {
                t.state = TransferState::Done;
                t.done = t.total;
                t.files_done = t.files;
                t.error = None;
            } else {
                t.state = TransferState::Failed;
                t.error = error.map(|e| share::cut(&e, 500));
            }
        });
        progress.flush();
    }
    Wire::ShareAck { id: id.to_string() }
}

// ---- progress ----

/// Updates to a share's `transfer`: kept in memory as bytes move, saved and announced
/// (`share.progress`) at most once a [`PROGRESS_EVERY`] and when it is flushed.
struct Progress<'a> {
    d: &'a Daemon,
    id: String,
    last: Instant,
    dirty: bool,
}

impl<'a> Progress<'a> {
    fn new(d: &'a Daemon, id: &str) -> Progress<'a> {
        Progress { d, id: id.to_string(), last: Instant::now(), dirty: false }
    }

    /// Change the transfer and announce it now.
    fn update(&mut self, f: impl FnOnce(&mut Transfer)) {
        self.change(f);
        self.dirty = true;
        self.flush();
    }

    /// Change the transfer; announce it if a while has passed.
    fn tick(&mut self, f: impl FnOnce(&mut Transfer)) {
        self.change(f);
        self.dirty = true;
        if self.last.elapsed() >= PROGRESS_EVERY {
            self.flush();
        }
    }

    fn change(&self, f: impl FnOnce(&mut Transfer)) {
        let mut i = self.d.nearby.inner();
        let inbox = i.inbox(&self.d.cfg.home);
        if let Some(e) = inbox.get_mut(&self.id) {
            let t = e.item.transfer.get_or_insert_with(|| {
                let files = match &e.item.payload {
                    SharePayload::File { files, .. } => files.len(),
                    _ => 0,
                };
                Transfer::new(e.item.size, files)
            });
            f(t);
        }
    }

    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.last = Instant::now();
        let item = {
            let mut i = self.d.nearby.inner();
            let inbox = i.inbox(&self.d.cfg.home);
            let item = inbox.get(&self.id).map(|e| e.item.clone());
            let _ = inbox.save();
            item
        };
        if let Some(item) = item {
            self.d.emit(kinds::SHARE_PROGRESS, Actor::system(), None, None, serde_json::to_value(&item).unwrap_or_default());
        }
    }
}

// ---- receiving ----

/// Where a share's files are staged until they are all in.
pub fn stage_dir(home: &Path, id: &str) -> PathBuf {
    home.join("nearby").join("incoming").join(id)
}

/// `root` joined with `rel`'s `/`-separated parts, each a plain name; None if any isn't, so a
/// path can never point outside `root`.
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    if !nb::valid_path(rel) {
        return None;
    }
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    Some(p)
}

/// The folder accepted files land in: `nearby.downloads`, else `~/Downloads/Midna`.
pub fn downloads(d: &Daemon) -> Result<PathBuf, RpcError> {
    let set = d.setting("nearby.downloads");
    let set = set.as_str().map(str::trim).unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let path = match set {
        "" => PathBuf::from(&home).join("Downloads").join("Midna"),
        "~" => PathBuf::from(&home),
        s => match s.strip_prefix("~/") {
            Some(rest) => PathBuf::from(&home).join(rest),
            None => PathBuf::from(s),
        },
    };
    if !path.is_absolute() {
        return Err(RpcError::bad_params(format!("nearby.downloads ({set}) isn't an absolute path or ~/…")));
    }
    Ok(path)
}

/// Bytes free for this user on the volume `path` is (or would be) on.
fn free_space(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a statvfs to fill.
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

/// `share.accept` of a file share: what accepting does, once its place and room are checked.
pub fn accepting(d: &Daemon, it: &ShareItem) -> Result<String, RpcError> {
    if let Some(t) = it.transfer.as_ref().filter(|t| t.state != TransferState::Failed) {
        return Ok(match (&t.state, &t.saved_to) {
            (TransferState::Done, Some(to)) => format!("saved to {to}"),
            _ => it.result.clone().unwrap_or_else(|| "receiving".into()),
        });
    }
    let dest = downloads(d)?;
    if let Some(free) = free_space(&dest)
        && free < it.size
    {
        return Err(RpcError::refused(format!(
            "{} needs {} and {} has {} free",
            it.id,
            nb::size_label(it.size),
            dest.display(),
            nb::size_label(free)
        )));
    }
    Ok(format!("receiving into {}", dest.display()))
}

/// A file share was accepted here (or accepted again after it failed): start fetching it.
pub fn begin(d: &Daemon, id: &str) {
    let restart = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let Some(e) = inbox.get_mut(id) else { return };
        let SharePayload::File { files, .. } = &e.item.payload else { return };
        let fresh = e.item.transfer.as_ref().is_none_or(|t| t.state == TransferState::Failed);
        if fresh {
            e.item.transfer = Some(Transfer::new(e.item.size, files.len()));
            let _ = inbox.save();
        }
        fresh
    };
    if restart {
        let _ = std::fs::remove_dir_all(stage_dir(&d.cfg.home, id));
        Progress { d, id: id.to_string(), last: Instant::now(), dirty: true }.flush();
    }
    nudge(d);
}

/// Wake the transfer thread.
pub fn nudge(d: &Daemon) {
    let (lock, cv) = &*d.nearby.fetch;
    *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
    cv.notify_all();
}

/// Fetch accepted file shares, one at a time, oldest first, and try waiting ones again every
/// few seconds (a restart carries on with them).
pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let wake = d.nearby.fetch.clone();
    let _ = std::thread::Builder::new().name("nearby-transfer".into()).spawn(move || {
        loop {
            {
                let Some(d) = w.upgrade() else { return };
                for id in pending(&d) {
                    if d.shutting_down.load(Ordering::Relaxed) {
                        return;
                    }
                    run(&d, &id);
                }
            }
            let (lock, cv) = &*wake;
            let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
            if !*woken {
                woken = cv.wait_timeout(woken, RETRY).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
            }
            *woken = false;
        }
    });
}

/// Incoming file shares accepted and not finished, oldest first.
fn pending(d: &Daemon) -> Vec<String> {
    let mut i = d.nearby.inner();
    let inbox = i.inbox(&d.cfg.home);
    inbox
        .items
        .iter()
        .filter(|e| e.item.direction == ShareDirection::Incoming && e.item.state == ShareState::Accepted)
        .filter(|e| e.item.transfer.as_ref().is_some_and(|t| !t.is_over()))
        .map(|e| e.item.id.clone())
        .collect()
}

/// Why a fetch stopped.
enum Stop {
    /// The other device can't be reached or the connection dropped: wait and resume.
    Retry(String),
    /// Give up (the files changed or went, bad data, no room).
    Fail(String),
}

fn run(d: &Daemon, id: &str) {
    let item = {
        let mut i = d.nearby.inner();
        i.inbox(&d.cfg.home).get(id).map(|e| e.item.clone())
    };
    let Some(item) = item else { return };
    let mut progress = Progress::new(d, id);
    let mut ch = None;
    match fetch(d, &item, &mut progress, &mut ch) {
        Ok(saved) => {
            progress.update(|t| {
                t.state = TransferState::Done;
                t.done = t.total;
                t.files_done = t.files;
                t.saved_to = Some(saved.display().to_string());
                t.error = None;
            });
            set_result(d, id, format!("saved to {}", saved.display()));
            let _ = std::fs::remove_dir_all(stage_dir(&d.cfg.home, id));
            tell(d, &item, ch, true, None);
        }
        Err(Stop::Retry(why)) => progress.update(|t| {
            t.state = TransferState::Waiting;
            t.error = Some(why);
        }),
        Err(Stop::Fail(why)) => {
            progress.update(|t| {
                t.state = TransferState::Failed;
                t.error = Some(why.clone());
            });
            set_result(d, id, format!("failed: {why}"));
            let _ = std::fs::remove_dir_all(stage_dir(&d.cfg.home, id));
            tell(d, &item, ch, false, Some(why));
        }
    }
    progress.flush();
}

fn set_result(d: &Daemon, id: &str, result: String) {
    let mut i = d.nearby.inner();
    let inbox = i.inbox(&d.cfg.home);
    if let Some(e) = inbox.get_mut(id) {
        e.item.result = Some(result);
        let _ = inbox.save();
    }
}

/// Tell the sender how it ended (best effort: it only updates the sender's view), on the
/// connection the files came over or, if that has closed, a new one.
fn tell(d: &Daemon, item: &ShareItem, ch: Option<Channel>, saved: bool, error: Option<String>) {
    let msg = Wire::FileDone { id: item.id.clone(), ok: saved, error };
    let said = |mut ch: Channel| {
        let ok = ch.send(&msg).is_ok() && matches!(ch.expect(transport::HANDSHAKE_TIMEOUT), Ok(Wire::ShareAck { .. }));
        ch.finish();
        ok
    };
    if !ch.is_some_and(said)
        && let Ok(ch) = link::connect_peer(d, &item.peer)
    {
        said(ch);
    }
}

/// Fetch whatever of `item` isn't staged yet, check it, and land it; the saved path.
fn fetch(d: &Daemon, item: &ShareItem, progress: &mut Progress, ch: &mut Option<Channel>) -> Result<PathBuf, Stop> {
    let SharePayload::File { name, folder, files, dirs } = &item.payload else { return Err(Stop::Fail("not a file share".into())) };
    let root = stage_dir(&d.cfg.home, &item.id).join("root");
    let staged = |f: &ShareFile| if *folder { safe_join(&root, &f.path) } else { Some(root.join(nb::safe_name(name))) };
    let start = item.transfer.as_ref().map(|t| t.files_done).unwrap_or(0).min(files.len());
    let fail_io = |what: &str, e: std::io::Error| Stop::Fail(format!("{what}: {e}"));
    std::fs::create_dir_all(&root).map_err(|e| fail_io("staging", e))?;
    let _ = std::fs::set_permissions(stage_dir(&d.cfg.home, &item.id), std::fs::Permissions::from_mode(0o700));
    // Hash what is already staged of the file it stopped in, before connecting (a big one takes a
    // while, and an idle connection is closed).
    let mut resume = None;
    if let Some(f) = files.get(start) {
        let path = staged(f).ok_or_else(|| Stop::Fail(format!("{:?} isn't a path inside the folder", f.path)))?;
        let have = std::fs::symlink_metadata(&path).map(|m| if m.is_file() { m.len() } else { u64::MAX }).unwrap_or(0);
        if have > f.size {
            remove(&path);
        } else if have > 0 {
            resume = Some(hash_prefix(&path).map_err(|e| fail_io(&f.path, e))?);
        }
    }
    let before: u64 = files[..start].iter().map(|f| f.size).sum();
    let partial = resume.as_ref().map(|(n, _)| *n).unwrap_or(0);
    progress.update(|t| {
        t.files_done = start;
        t.done = before + partial;
    });
    if start < files.len() {
        let c = link::connect_peer(d, &item.peer).map_err(|e| Stop::Retry(format!("waiting for {}: {}", item.peer_name, e.message)))?;
        *ch = Some(c);
        progress.update(|t| {
            t.state = TransferState::Moving;
            t.error = None;
        });
    }
    let mut done = before;
    for (k, f) in files.iter().enumerate().skip(start) {
        let path = staged(f).ok_or_else(|| Stop::Fail(format!("{:?} isn't a path inside the folder", f.path)))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| fail_io(&f.path, e))?;
        }
        let mut bad = 0;
        loop {
            let (offset, hasher) = match resume.take() {
                Some((n, h)) => {
                    progress.update(|t| t.resumed += 1);
                    (n, h)
                }
                None => (0, Sha256::new()),
            };
            let ch = ch.as_mut().expect("connected");
            let sha = get(d, ch, &item.id, k, f, &path, offset, hasher, |at| progress.tick(|t| t.done = done + at))?;
            if sha == f.sha256 {
                break;
            }
            remove(&path);
            bad += 1;
            if bad > MAX_BAD {
                return Err(Stop::Fail(format!("{} arrived with the wrong checksum twice", f.path)));
            }
        }
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        done += f.size;
        progress.update(|t| {
            t.files_done = k + 1;
            t.done = done;
        });
    }
    for dir in dirs {
        let p = safe_join(&root, dir).ok_or_else(|| Stop::Fail(format!("{dir:?} isn't a path inside the folder")))?;
        std::fs::create_dir_all(&p).map_err(|e| fail_io(dir, e))?;
    }
    let src = if *folder { root.clone() } else { root.join(nb::safe_name(name)) };
    land(&src, &downloads(d).map_err(|e| Stop::Fail(e.message))?, name).map_err(Stop::Fail)
}

fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The length and running SHA-256 of a staged file.
fn hash_prefix(path: &Path) -> std::io::Result<(u64, Sha256)> {
    let mut f = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            return Ok((n, h));
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
}

/// Ask for file `k` from `offset` and write what comes to `path`; the whole file's SHA-256.
#[allow(clippy::too_many_arguments)]
fn get(d: &Daemon, ch: &mut Channel, id: &str, k: usize, f: &ShareFile, path: &Path, offset: u64, mut hasher: Sha256, mut moved: impl FnMut(u64)) -> Result<String, Stop> {
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(offset == 0)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| Stop::Fail(format!("{}: {e}", f.path)))?;
    out.seek(SeekFrom::Start(offset)).map_err(|e| Stop::Fail(format!("{}: {e}", f.path)))?;
    out.set_len(offset).map_err(|e| Stop::Fail(format!("{}: {e}", f.path)))?;
    if offset < f.size {
        ch.send(&Wire::FileGet { id: id.to_string(), file: k, offset }).map_err(Stop::Retry)?;
    } else {
        return Ok(transport::hex(&hasher.finalize()));
    }
    let bad = |what: &str| Stop::Fail(format!("the other Mac sent bad data ({what})"));
    let mut at = offset;
    loop {
        if d.shutting_down.load(Ordering::Relaxed) {
            return Err(Stop::Retry("midna stopped".into()));
        }
        let msg = match ch.recv(CHUNK_TIMEOUT) {
            Ok(Some(m)) => m,
            Ok(None) => return Err(Stop::Retry("the connection went quiet".into())),
            Err(e) => return Err(Stop::Retry(format!("the connection dropped ({e})"))),
        };
        match msg {
            Wire::FileChunk { id: got, file, offset: o, data } if got == id && file == k => {
                if o != at {
                    return Err(bad("out of order"));
                }
                let bytes = B64.decode(data.as_bytes()).map_err(|_| bad("not base64"))?;
                if bytes.is_empty() || at + bytes.len() as u64 > f.size {
                    return Err(bad("more than the file's size"));
                }
                out.write_all(&bytes).map_err(|e| Stop::Fail(format!("{}: {e}", f.path)))?;
                hasher.update(&bytes);
                at += bytes.len() as u64;
                moved(at);
            }
            Wire::FileEnd { id: got, file } if got == id && file == k => {
                if at != f.size {
                    return Err(Stop::Fail(format!("{} changed on the other Mac", f.path)));
                }
                out.sync_all().map_err(|e| Stop::Fail(format!("{}: {e}", f.path)))?;
                return Ok(transport::hex(&hasher.finalize()));
            }
            Wire::Refused { reason } if reason == transport::NOT_PAIRED => return Err(Stop::Fail("the other Mac isn't paired with this one any more".into())),
            Wire::Refused { reason } if reason.contains("stopping") || reason.contains("connection closed") => return Err(Stop::Retry(reason)),
            Wire::Refused { reason } => return Err(Stop::Fail(share::cut(&reason, 500))),
            _ => return Err(bad("an unexpected message")),
        }
    }
}

/// Move `src` (a staged file or folder) into `dest` under `name` made safe, or `name 2`, … if
/// that is taken; mark everything in it as downloaded first. The path it landed at.
pub fn land(src: &Path, dest: &Path, name: &str) -> Result<PathBuf, String> {
    midna_proto::paths::guard_write(dest).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    quarantine_tree(src);
    let name = nb::safe_name(name);
    let is_dir = src.is_dir();
    for n in 1..10_000 {
        let target = dest.join(numbered(&name, n, is_dir));
        match rename_excl(src, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.raw_os_error() == Some(libc::EEXIST) || e.raw_os_error() == Some(libc::ENOTEMPTY) => continue,
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => match copy_new(src, &target) {
                Ok(()) => {
                    let _ = if is_dir { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) };
                    return Ok(target);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("saving into {}: {e}", dest.display())),
            },
            Err(e) => return Err(format!("saving into {}: {e}", dest.display())),
        }
    }
    Err(format!("no free name for {name} in {}", dest.display()))
}

/// `name`, or with ` n` before its extension (a folder's whole name) from the second on.
fn numbered(name: &str, n: u32, is_dir: bool) -> String {
    if n == 1 {
        return name.to_string();
    }
    match name.rfind('.').filter(|&i| i > 0 && !is_dir) {
        Some(i) => format!("{} {n}{}", &name[..i], &name[i..]),
        None => format!("{name} {n}"),
    }
}

/// Rename that never replaces what is at `to`.
fn rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput));
    let (f, t) = (c(from)?, c(to)?);
    // SAFETY: two valid C strings.
    if unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// Copy a staged file or tree to `to`, which must not exist yet (another volume).
fn copy_new(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_new(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        let mut src = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(from)?;
        let mut out = OpenOptions::new().write(true).create_new(true).mode(0o644).custom_flags(libc::O_NOFOLLOW).open(to)?;
        std::io::copy(&mut src, &mut out)?;
        let _ = quarantine(to);
        Ok(())
    }
}

/// Mark every file under `p` as downloaded by Midna, so macOS checks it before it opens.
fn quarantine_tree(p: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(p) else { return };
    if meta.is_dir() {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            quarantine_tree(&e.path());
        }
    } else if meta.is_file() {
        let _ = quarantine(p);
    }
}

fn quarantine(p: &Path) -> std::io::Result<()> {
    let path = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let value = format!("0083;{:08x};Midna;", midna_proto::time::now_unix());
    // SAFETY: valid C strings and a buffer of the given length.
    let r = unsafe {
        libc::setxattr(path.as_ptr(), c"com.apple.quarantine".as_ptr(), value.as_ptr().cast(), value.len(), 0, libc::XATTR_NOFOLLOW)
    };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("midna-xfer-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_path_from_a_peer_never_leaves_the_root() {
        let root = Path::new("/r");
        assert_eq!(safe_join(root, "a/b.txt"), Some(PathBuf::from("/r/a/b.txt")));
        for bad in ["../x", "a/../../x", "/etc/passwd", "a//b", "a/", ".", "a/./b", "", "a\0b", "a/\nb"] {
            assert_eq!(safe_join(root, bad), None, "{bad:?}");
        }
        assert_eq!(nb::safe_name("../../.ssh"), "..-..-.ssh");
        assert_eq!(nb::safe_name(".."), "untitled");
        assert_eq!(nb::safe_name("a:b\x1b"), "a-b-");
    }

    #[test]
    fn numbered_names_keep_the_extension() {
        assert_eq!(numbered("a.txt", 1, false), "a.txt");
        assert_eq!(numbered("a.tar.gz", 2, false), "a.tar 2.gz");
        assert_eq!(numbered(".env", 2, false), ".env 2");
        assert_eq!(numbered("proj.v2", 3, true), "proj.v2 3");
    }

    #[test]
    fn prepare_lists_a_folder_with_checksums_and_skips_links() {
        let t = tmp("prep");
        let f = t.join("proj");
        std::fs::create_dir_all(f.join("src/deep")).unwrap();
        std::fs::create_dir_all(f.join("empty")).unwrap();
        std::fs::write(f.join("README"), b"hi").unwrap();
        std::fs::write(f.join("src/main.rs"), b"").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", f.join("src/link")).unwrap();
        let (p, source) = prepare(f.to_str().unwrap()).unwrap();
        assert_eq!(source, std::fs::canonicalize(&f).unwrap());
        let SharePayload::File { name, folder, files, dirs } = p else { panic!() };
        assert_eq!((name.as_str(), folder), ("proj", true));
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["README", "src/main.rs"]);
        assert_eq!(files[0].sha256, transport::hex(&Sha256::digest(b"hi")));
        assert_eq!(dirs, ["empty", "src/deep"]);
        assert!(prepare("relative/path").is_err());
        let (one, _) = prepare(f.join("README").to_str().unwrap()).unwrap();
        assert_eq!(one.size(), 2);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn landing_never_replaces_what_is_there() {
        let t = tmp("land");
        let dest = t.join("Downloads");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("a.txt"), b"mine").unwrap();
        let src = t.join("staged.txt");
        std::fs::write(&src, b"theirs").unwrap();
        let landed = land(&src, &dest, "a.txt").unwrap();
        assert_eq!(landed, dest.join("a 2.txt"));
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"mine");
        assert_eq!(std::fs::read(&landed).unwrap(), b"theirs");
        let dir = t.join("tree");
        std::fs::create_dir_all(dir.join("x")).unwrap();
        std::fs::write(dir.join("x/y"), b"1").unwrap();
        let landed = land(&dir, &dest, "../escape").unwrap();
        assert_eq!(landed, dest.join("..-escape"));
        assert_eq!(std::fs::read(landed.join("x/y")).unwrap(), b"1");
        let _ = std::fs::remove_dir_all(&t);
    }
}
