//! notify.*: which notifications midna posts (global settings + per-terminal overrides),
//! notifications agents send, and the sounds and images they use (`crate::notify_media`). The notifier itself is `crate::notify`.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::notify::{self, CATEGORIES, is_override_key, setting_key};
use midna_proto::*;
use serde_json::json;

/// The terminal a call is about: `session`, else the caller's own; None with `global`.
fn target(d: &Daemon, ctx: &Ctx, sid: Option<Id>, global: bool) -> Result<Option<Id>, RpcError> {
    if global {
        return Ok(None);
    }
    let Some(sid) = sid.or(ctx.session.clone()) else { return Ok(None) };
    if d.core().state.session(&sid).is_none() {
        return Err(RpcError::not_found(format!("no terminal {sid}")));
    }
    Ok(Some(sid))
}

fn listing(d: &Daemon, sid: Option<Id>) -> NotifyListResult {
    let core = d.core();
    let st = &core.state;
    let overrides = sid.as_deref().and_then(|s| st.session(s)).map(|s| s.notify.clone()).unwrap_or_default();
    let enabled = st.setting_bool("notify.enabled");
    let muted = overrides.get("enabled") == Some(&false);
    let categories = CATEGORIES
        .iter()
        .map(|c| {
            let global = st.setting_bool(&setting_key(c.key));
            let session = overrides.get(c.key).copied();
            NotifyCategoryInfo {
                key: c.key.into(),
                label: c.label.into(),
                description: c.description.into(),
                default: c.default,
                global,
                session,
                effective: enabled && !muted && session.unwrap_or(global),
                sound: st.setting_str(&notify::sound_key(c.key)),
                volume: st.setting_i64(&notify::volume_key(c.key)),
                image: match st.setting_str(&notify::image_key(c.key)).as_str() {
                    "" => st.setting_str("notify.image"),
                    "none" => String::new(),
                    v => v.to_string(),
                },
            }
        })
        .collect();
    NotifyListResult { enabled, session: sid, muted, categories }
}

pub fn list(d: &Daemon, ctx: &Ctx, p: NotifyListParams) -> R {
    let sid = target(d, ctx, p.session, p.global)?;
    ok(listing(d, sid))
}

pub fn set(d: &Daemon, ctx: &Ctx, p: NotifySetParams) -> R {
    if !is_override_key(&p.key) {
        let keys: Vec<&str> = std::iter::once("enabled").chain(CATEGORIES.iter().map(|c| c.key)).collect();
        return Err(RpcError::bad_params(format!("unknown notification key `{}`; one of: {}", p.key, keys.join(", "))));
    }
    let Some(sid) = target(d, ctx, p.session, p.global)? else {
        if !p.global {
            return Err(RpcError::bad_params("no terminal: pass `session`, run inside a midna terminal, or pass `global: true`"));
        }
        let key = setting_key(&p.key);
        match p.value {
            Some(v) => super::settings::set(d, ctx, SettingSetParams { key, value: json!(v) })?,
            None => super::settings::reset(d, ctx, SettingKeyParams { key })?,
        };
        return ok(listing(d, None));
    };
    let (project, map) = {
        let mut core = d.core();
        let s = core.state.session_mut(&sid).ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
        let before = s.notify.clone();
        match p.value {
            Some(v) => s.notify.insert(p.key.clone(), v),
            None => s.notify.remove(&p.key),
        };
        if s.notify == before {
            drop(core);
            return ok(listing(d, Some(sid)));
        }
        (s.project_id.clone(), s.notify.clone())
    };
    d.mark_dirty();
    d.emit(kinds::SESSION_NOTIFY, ctx.actor(), Some(project), Some(sid.clone()), json!({ "notify": map, "key": p.key, "value": p.value }));
    ok(listing(d, Some(sid)))
}

pub fn send(d: &Daemon, ctx: &Ctx, p: NotifySendParams) -> R {
    if p.title.trim().is_empty() {
        return Err(RpcError::bad_params("title is empty"));
    }
    let sid = target(d, ctx, p.session, false)?;
    ok(crate::notify::send(d, sid, &p.title, &p.body, p.sound, !ctx.is_human()))
}

pub fn media(d: &Daemon, p: NotifyMediaParams) -> R {
    ok(crate::notify_media::list(d, p.kind.as_deref()))
}

/// Copy a sound or image in, then point `use_for` settings at it.
pub fn import(d: &Daemon, ctx: &Ctx, p: NotifyImportParams) -> R {
    let m = crate::notify_media::import(d, ctx.actor(), &p.path)?;
    for key in &p.use_for {
        let fits = if m.kind == "sound" { key.starts_with("notify.sound.") } else { key == "notify.image" || key.starts_with("notify.image.") };
        if !fits {
            return Err(RpcError::bad_params(format!("imported {} as `{}`, but {key} doesn't take a {}", m.kind, m.name, m.kind)));
        }
        super::settings::set(d, ctx, SettingSetParams { key: key.clone(), value: json!(m.name) })?;
    }
    ok(NotifyMedia { used_by: crate::notify_media::used_by(d, &m.kind, &m.name), ..m })
}

/// Delete an imported file; the settings that used it go back to their defaults.
pub fn remove(d: &Daemon, ctx: &Ctx, p: NotifyRemoveParams) -> R {
    let (_, reset) = crate::notify_media::remove(d, ctx.actor(), &p.name)?;
    for key in &reset {
        super::settings::reset(d, ctx, SettingKeyParams { key: key.clone() })?;
    }
    ok(NotifyRemoveResult { removed: p.name, reset })
}

pub fn test(d: &Daemon, ctx: &Ctx, p: NotifyTestParams) -> R {
    let category = p.category.unwrap_or_else(|| "approval".into());
    if notify::category(&category).is_none() {
        let keys: Vec<&str> = CATEGORIES.iter().map(|c| c.key).collect();
        return Err(RpcError::bad_params(format!("unknown category `{category}`; one of: {}", keys.join(", "))));
    }
    let sid = target(d, ctx, p.session, false)?;
    ok(crate::notify::test(d, sid, &category))
}

/// Ask the app to remove notifications: one terminal's, or all of them.
pub fn clear(d: &Daemon, ctx: &Ctx, p: NotifyClearParams) -> R {
    let project = match p.session.as_deref() {
        Some(sid) => Some(d.core().state.session(sid).ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?.project_id.clone()),
        None => None,
    };
    let delivered = d.gui_connected();
    d.emit(kinds::NOTIFY_CLEARED, ctx.actor(), project, p.session.clone(), json!({}));
    ok(NotifyClearResult { delivered, session: p.session })
}

/// Notifications posted after `since`, newest first (tests left out).
fn posted_since(d: &Daemon, since: u64, limit: usize, session: Option<Id>) -> Vec<Event> {
    let filter = EventFilter { kinds: Some(vec![kinds::NOTIFY_POSTED.into()]), session_id: session, project_id: None };
    let mut v: Vec<Event> = d.log.list(since, limit, &filter).into_iter().filter(|e| e.data.get("test") != Some(&json!(true))).collect();
    v.reverse();
    v
}

/// The read marker, started at the log's end the first time it's needed.
fn read_seq(d: &Daemon) -> u64 {
    let mut core = d.core();
    if let Some(s) = core.state.notify_read_seq {
        return s;
    }
    let s = d.log.seq();
    core.state.notify_read_seq = Some(s);
    drop(core);
    d.mark_dirty();
    s
}

fn unread(d: &Daemon, read: u64) -> u32 {
    posted_since(d, read, 10_000, None).len() as u32
}

pub fn history(d: &Daemon, p: NotifyHistoryParams) -> R {
    let read = read_seq(d);
    let limit = p.limit.unwrap_or(200).clamp(1, 1000) as usize;
    let items = posted_since(d, 0, limit, p.session)
        .into_iter()
        .filter_map(|e| {
            let notification = serde_json::from_value(e.data).ok()?;
            Some(NotifyHistoryItem { seq: e.seq, at: e.at, session_id: e.session_id, project_id: e.project_id, unread: e.seq > read, notification })
        })
        .collect();
    ok(NotifyHistoryResult { items, unread: unread(d, read), read_seq: read })
}

pub fn read(d: &Daemon, ctx: &Ctx, p: NotifyReadParams) -> R {
    let before = read_seq(d);
    // Never moves back: an older seq (another window's stale view) changes nothing.
    let read = p.seq.unwrap_or_else(|| d.log.seq()).min(d.log.seq()).max(before);
    let unread = unread(d, read);
    if read != before {
        d.core().state.notify_read_seq = Some(read);
        d.mark_dirty();
        d.emit(kinds::NOTIFY_READ, ctx.actor(), None, None, json!({ "read_seq": read, "unread": unread }));
    }
    ok(NotifyReadResult { read_seq: read, unread })
}

pub fn play(d: &Daemon, ctx: &Ctx, p: NotifyPlayParams) -> R {
    let sid = target(d, ctx, p.session, false)?;
    let what = p.sound.trim();
    // `midna notify play glass` / `uh-oh`: built-in sounds by any case.
    let what = notify::builtin_sound_named(what).unwrap_or(what);
    ok(crate::notify::play(d, sid, what, p.volume, !ctx.is_human()).map_err(RpcError::bad_params)?)
}
