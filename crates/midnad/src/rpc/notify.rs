//! notify.*: which notifications midna posts (global settings + per-terminal overrides),
//! notifications agents send, and the sounds and images they use (`crate::notify_media`). The notifier itself is `crate::notify`.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::notify::{self, CATEGORIES, is_override_key, setting_key};
use midna_proto::*;
use serde_json::json;
use std::collections::{HashMap, HashSet};

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
    let builtin = CATEGORIES.iter().map(|c| (c.key, c.label, c.description, c.default));
    let custom = st.notify_kinds.iter().map(|k| (k.key.as_str(), k.label.as_str(), k.description.as_str(), true));
    let categories = builtin
        .chain(custom)
        .map(|(key, label, description, default)| {
            let global = st.setting_bool(&setting_key(key));
            let session = overrides.get(key).copied();
            NotifyCategoryInfo {
                key: key.into(),
                label: label.into(),
                description: description.into(),
                default,
                global,
                session,
                effective: enabled && !muted && session.unwrap_or(global),
                sound: st.setting_str(&notify::sound_key(key)),
                volume: st.setting_i64(&notify::volume_key(key)),
                image: match st.setting_str(&notify::image_key(key)).as_str() {
                    "" => st.setting_str("notify.image"),
                    "none" => String::new(),
                    v => v.to_string(),
                },
                stay: st.setting_i64(&notify::stay_key(key)),
                color: st.setting_str(&notify::color_key(key)),
                custom: notify::category(key).is_none(),
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
    if !is_override_key(&p.key) && d.core().state.notify_kind(&p.key).is_none() {
        let core = d.core();
        let custom = core.state.notify_kinds.iter().map(|k| k.key.as_str());
        let keys: Vec<&str> = std::iter::once("enabled").chain(CATEGORIES.iter().map(|c| c.key)).chain(custom).collect();
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
    let category = match p.category.as_deref() {
        None | Some("agent") => "agent".to_string(),
        Some(k) if d.core().state.notify_kind(k).is_some() => k.to_string(),
        Some(k) => {
            let core = d.core();
            let keys: Vec<&str> = core.state.notify_kinds.iter().map(|k| k.key.as_str()).collect();
            let known = if keys.is_empty() { "none added yet (notify.kinds.add)".into() } else { keys.join(", ") };
            return Err(RpcError::bad_params(format!("no kind `{k}` to send as; kinds you can send to: agent, {known}")));
        }
    };
    let extras = extras(p.id, p.open, p.actions)?;
    let sid = target(d, ctx, p.session, false)?;
    let mut r = crate::notify::send_as(d, &category, sid, &p.title, &p.body, p.sound, !ctx.is_human(), extras);
    if let (true, Some(secs), Some(id)) = (r.posted, p.wait_secs, r.id.as_deref()) {
        r.response = crate::notify::wait_response(d, id, secs);
    }
    ok(r)
}

/// Check `notify.send`'s id, URL and buttons.
pub fn extras(id: Option<String>, open: Option<String>, actions: Vec<String>) -> Result<crate::notify::Extras, RpcError> {
    let id = id.map(|i| i.trim().to_string()).filter(|i| !i.is_empty());
    if let Some(i) = id.as_deref().filter(|i| !notify::valid_id(i)) {
        return Err(RpcError::bad_params(format!("`{i}` isn't a notification id: 1–64 of A-Z a-z 0-9 . _ : -")));
    }
    let open = open.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    if let Some(u) = open.as_deref().filter(|u| !notify::valid_open_url(u)) {
        return Err(RpcError::bad_params(format!("`{u}` isn't a URL to open: give one with a scheme (https://…, file://…)")));
    }
    let actions: Vec<String> = actions.iter().map(|a| a.trim().to_string()).collect();
    if actions.len() > notify::MAX_ACTIONS {
        return Err(RpcError::bad_params(format!("at most {} actions", notify::MAX_ACTIONS)));
    }
    if let Some(a) = actions.iter().find(|a| a.is_empty() || a.chars().count() > notify::ACTION_MAX) {
        return Err(RpcError::bad_params(format!("action {a:?} must be 1–{} characters", notify::ACTION_MAX)));
    }
    if let Some(a) = actions.iter().enumerate().find(|(i, a)| actions[..*i].contains(a)).map(|(_, a)| a) {
        return Err(RpcError::bad_params(format!("action {a:?} is listed twice")));
    }
    Ok(crate::notify::Extras { id, open, actions })
}

/// Ask the app to take a `notify.send` notification away, wherever it shows.
pub fn withdraw(d: &Daemon, ctx: &Ctx, p: NotifyWithdrawParams) -> R {
    let id = p.id.trim();
    if !notify::valid_id(id) {
        return Err(RpcError::bad_params(format!("`{id}` isn't a notification id")));
    }
    let sent = crate::notify::sent(d, id);
    let delivered = d.gui_connected();
    let (project, session) = sent.map(|s| (s.project, s.session)).unwrap_or_default();
    d.emit(kinds::NOTIFY_WITHDRAWN, ctx.actor(), project, session, json!({ "id": id }));
    ok(NotifyWithdrawResult { id: id.into(), delivered })
}

/// What the human did with a `notify.send` notification, waiting up to `wait_secs` for it.
pub fn response(d: &Daemon, p: NotifyResponseParams) -> R {
    if crate::notify::sent(d, &p.id).is_none() {
        return Err(RpcError::not_found(format!("no notification `{}` (unknown, or sent before midnad last restarted)", p.id)));
    }
    let response = crate::notify::wait_response(d, &p.id, p.wait_secs.unwrap_or(0));
    ok(NotifyResponseResult { id: p.id, response })
}

/// The app reports a click, button or dismissal (human only: agents can't answer for the human).
pub fn respond(d: &Daemon, p: NotifyRespondParams) -> R {
    let s = crate::notify::respond(d, &p.id, p.response).map_err(RpcError::not_found)?;
    ok(NotifyResponseResult { id: s.id, response: s.response })
}

/// At most this many kinds you added.
const MAX_KINDS: usize = 32;

fn kind_info(st: &crate::state::State, k: &notify::CustomKind) -> NotifyKindInfo {
    let field = |f: &str| st.setting(&notify::kind_key(f, &k.key));
    NotifyKindInfo {
        key: k.key.clone(),
        label: k.label.clone(),
        description: k.description.clone(),
        enabled: field("").as_bool().unwrap_or(true),
        stay: field("stay").as_i64().unwrap_or(0),
        color: field("color").as_str().unwrap_or("").into(),
        sound: field("sound").as_str().unwrap_or("").into(),
        push: field("push").as_bool().unwrap_or(false),
    }
}

pub fn kinds_list(d: &Daemon) -> R {
    let core = d.core();
    ok(NotifyKindsList { kinds: core.state.notify_kinds.iter().map(|k| kind_info(&core.state, k)).collect() })
}

/// Add a kind (or with `replace` update one), then set the settings it came with.
pub fn kinds_add(d: &Daemon, ctx: &Ctx, p: NotifyKindsAddParams) -> R {
    let key = p.key.trim().to_ascii_lowercase();
    if !notify::valid_kind_key(&key) {
        return Err(RpcError::bad_params(format!("`{key}` isn't a kind key: 1–32 lowercase letters, digits and _, starting with a letter")));
    }
    if notify::reserved_kind_key(&key) {
        return Err(RpcError::bad_params(format!("`{key}` is a built-in kind or setting; pick another key")));
    }
    // `enabled` names the kind's own switch (`notify.<key>`, field "").
    let field = |f: &str| if f == "enabled" { "" } else { f }.to_string();
    for f in p.settings.keys() {
        if f.is_empty() || !notify::KIND_FIELDS.contains(&field(f).as_str()) {
            let names: Vec<&str> = notify::KIND_FIELDS.iter().map(|f| if f.is_empty() { "enabled" } else { f }).collect();
            return Err(RpcError::bad_params(format!("unknown kind setting `{f}`; one of: {}", names.join(", "))));
        }
    }
    // Check every value before adding anything, so a bad one leaves no half-made kind.
    for (f, v) in &p.settings {
        let k = notify::kind_key(&field(f), &key);
        let s = midna_proto::settings::custom_kind_spec(&field(f)).expect("checked above");
        let v = s.coerce(v).map_err(|e| RpcError::bad_params(e.replace(s.key, &k)))?;
        crate::notify_media::check_setting(&d.cfg.home, &k, &v).map_err(RpcError::bad_params)?;
    }
    let label = p.label.as_deref().map(str::trim).filter(|l| !l.is_empty()).unwrap_or(&key).chars().take(40).collect::<String>();
    let description = p.description.as_deref().unwrap_or("").trim().chars().take(200).collect::<String>();
    let action = {
        let mut core = d.core();
        let kinds = &mut core.state.notify_kinds;
        let full = kinds.len() >= MAX_KINDS;
        match kinds.iter_mut().find(|k| k.key == key) {
            Some(_) if !p.replace => return Err(RpcError::bad_params(format!("kind `{key}` exists; pass replace=true to update it"))),
            Some(k) => {
                k.label = label;
                if p.description.is_some() {
                    k.description = description;
                }
                "updated"
            }
            None if full => return Err(RpcError::bad_params(format!("at most {MAX_KINDS} kinds; remove one first"))),
            None => {
                kinds.push(notify::CustomKind { key: key.clone(), label, description });
                "added"
            }
        }
    };
    d.mark_dirty();
    for (f, v) in &p.settings {
        super::settings::set(d, ctx, SettingSetParams { key: notify::kind_key(&field(f), &key), value: v.clone() })?;
    }
    d.emit(kinds::NOTIFY_KINDS_CHANGED, ctx.actor(), None, None, json!({ "action": action, "key": key }));
    let core = d.core();
    ok(kind_info(&core.state, core.state.notify_kind(&key).expect("just added")))
}

/// Remove a kind you added, its settings, and every terminal's override for it.
pub fn kinds_remove(d: &Daemon, ctx: &Ctx, p: NotifyKindsRemoveParams) -> R {
    {
        let mut core = d.core();
        let st = &mut core.state;
        let Some(i) = st.notify_kinds.iter().position(|k| k.key == p.key) else {
            let why = if notify::category(&p.key).is_some() { format!("`{}` is built in: turn it off with notify.{} false", p.key, p.key) } else { format!("no kind `{}`", p.key) };
            return Err(RpcError::not_found(why));
        };
        st.notify_kinds.remove(i);
        for k in notify::kind_keys(&p.key) {
            st.settings.remove(&k);
        }
        for s in &mut st.sessions {
            s.notify.remove(&p.key);
        }
    }
    d.mark_dirty();
    d.emit(kinds::NOTIFY_KINDS_CHANGED, ctx.actor(), None, None, json!({ "action": "removed", "key": p.key }));
    ok(OkResult { ok: true })
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
    if !d.core().state.is_notify_kind(&category) {
        let core = d.core();
        let keys: Vec<&str> = CATEGORIES.iter().map(|c| c.key).chain(core.state.notify_kinds.iter().map(|k| k.key.as_str())).collect();
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

/// Notifications posted after `since`, newest first (tests left out). A notification a later
/// one with the same `id` replaced is left out too.
fn posted_since(d: &Daemon, since: u64, limit: usize, session: Option<Id>) -> Vec<Event> {
    let filter = EventFilter { kinds: Some(vec![kinds::NOTIFY_POSTED.into()]), session_id: session, project_id: None };
    let mut v: Vec<Event> = d.log.list(since, limit, &filter).into_iter().filter(|e| e.data.get("test") != Some(&json!(true))).collect();
    v.reverse();
    let mut ids = HashSet::new();
    v.retain(|e| e.data["id"].as_str().is_none_or(|id| ids.insert(id.to_string())));
    v
}

/// When each notification id was last withdrawn (`notify.withdraw`): a post with that id
/// before then is withdrawn.
fn withdrawn(d: &Daemon) -> HashMap<String, u64> {
    let filter = EventFilter { kinds: Some(vec![kinds::NOTIFY_WITHDRAWN.into()]), session_id: None, project_id: None };
    d.log.list(0, 10_000, &filter).into_iter().filter_map(|e| Some((e.data["id"].as_str()?.to_string(), e.seq))).collect()
}

fn is_withdrawn(gone: &HashMap<String, u64>, e: &Event) -> bool {
    e.data["id"].as_str().and_then(|id| gone.get(id)).is_some_and(|w| *w > e.seq)
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

/// Unread notifications of the kinds that count on the bell (`notify.bell.<kind>`; a kind
/// since removed doesn't).
fn unread(d: &Daemon, read: u64) -> u32 {
    let posted = posted_since(d, read, 10_000, None);
    let gone = withdrawn(d);
    let core = d.core();
    let rings = |e: &&Event| core.state.setting_bool(&notify::bell_key(e.data["category"].as_str().unwrap_or("")));
    posted.iter().filter(rings).filter(|e| !is_withdrawn(&gone, e)).count() as u32
}

pub fn history(d: &Daemon, p: NotifyHistoryParams) -> R {
    let read = read_seq(d);
    let limit = p.limit.unwrap_or(200).clamp(1, 1000) as usize;
    let gone = withdrawn(d);
    let items = posted_since(d, 0, limit, p.session)
        .into_iter()
        .filter_map(|e| {
            let withdrawn = is_withdrawn(&gone, &e);
            let notification = serde_json::from_value(e.data).ok()?;
            Some(NotifyHistoryItem { seq: e.seq, at: e.at, session_id: e.session_id, project_id: e.project_id, unread: e.seq > read && !withdrawn, withdrawn, notification })
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
