//! Midna's own things as shares (`share.send` kind `object`): a Summary record, policy rules,
//! triggers, a theme, settings or queued prompts.
//!
//! The sender reads them from its stores ([`build`]) and leaves out what is only this Mac's:
//! project ids become project names, secrets, a trigger's state and hook id, a summary's
//! conversation id and agent arguments never travel, and things tied to one terminal (rules on
//! a session, triggers on a terminal) can't be sent. Settings that name this Mac's files or
//! decide what reaches it (`nearby.*`, `webhooks.*`, `updates.*`, trusted folders and clients,
//! paths, custom scripts and images) never travel either way.
//!
//! The receiver works out what accepting would change against its own state ([`plan`]): every
//! line of the review says what is added or changed and what is skipped and why. The incoming
//! share's approval shows that review; accepting works it out again and applies only what it
//! still says. Accepting is the human's alone (`share.accept` is human only), and [`apply`]
//! refuses anyone else for what needs a human: rules, triggers and human-only settings.
//! Rules are added as the human's; triggers go through `trigger.add` switched off (a local one
//! as a draft, a webhook one waiting for its secret); settings through `settings.set`.
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::nearby::{MidnaObject, ObjectType, SharedPrompt, SharedRule, SharedTheme, SharedTrigger};
use midna_proto::settings::{SettingKind, SettingSpec};
use midna_proto::{
    Actor, ClosedTerminal, Effect, Id, ROOT_PROJECT_ID, RpcError, Rule, RuleScope, SendWhen, SettingSetParams, SummaryPanel, Trigger,
    TriggerAction, TriggerAddParams, TriggerFilter, TriggerSource, TriggerState, themes, time,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;

/// What Root (terminals in no project) is called on the wire.
const ROOT_NAME: &str = "Root";
/// The Summary panel an imported record gets.
const PANEL_OWNER: &str = "nearby";

/// Settings that never travel: they name this Mac's files or folders, or decide what reaches it.
fn stays_here(key: &str, spec: &SettingSpec, value: &Value) -> bool {
    const PREFIXES: [&str; 4] = ["nearby.", "webhooks.", "updates.", "notify.image"];
    const KEYS: [&str; 3] = ["agents.trust_folders", "agents.trusted_clients", "finder.quick_action"];
    PREFIXES.iter().any(|p| key.starts_with(p))
        || KEYS.contains(&key)
        || matches!(spec.ty, SettingKind::PathList)
        || crate::rpc::settings::custom_script(spec, value, &Value::Null)
}

fn bad(why: impl Into<String>) -> RpcError {
    RpcError::bad_params(why)
}

fn project_name(d: &Daemon, id: &str) -> Option<String> {
    if id == ROOT_PROJECT_ID {
        return Some(ROOT_NAME.into());
    }
    d.core().state.project(id).map(|p| p.name.clone())
}

/// This Mac's project of that name (exactly, else ignoring case).
pub(super) fn project_named(d: &Daemon, name: &str) -> Option<Id> {
    let core = d.core();
    let ps = &core.state.projects;
    ps.iter().find(|p| p.name == name).or_else(|| ps.iter().find(|p| p.name.eq_ignore_ascii_case(name))).map(|p| p.id.clone())
}

fn no_project(name: &str) -> String {
    format!("no project named “{name}” here")
}

/// `share.send` kind object: read what `ids` names from this Mac's stores.
pub fn build(d: &Daemon, kind: ObjectType, ids: &[String]) -> Result<MidnaObject, RpcError> {
    let named = |what: &str| if ids.is_empty() { Err(bad(format!("name the {what} to send in `ids`"))) } else { Ok(()) };
    let obj = match kind {
        ObjectType::Summary => {
            let [id] = ids else { return Err(bad("name one Summary record (`summary.list`) in `ids`")) };
            let mut record = crate::summary::get(d, id).ok_or_else(|| RpcError::not_found(format!("no closed terminal {id} in a Summary")))?;
            let project = project_name(d, &record.project_id).ok_or_else(|| RpcError::not_found(format!("{id}'s project is gone")))?;
            record.conversation_id = None;
            record.agent_args.clear();
            record.prompts.iter_mut().for_each(|p| p.conversation = None);
            MidnaObject::Summary { project, record }
        }
        ObjectType::Rules => {
            named("rules (rule.list)")?;
            let now = time::now_unix();
            let rules: Vec<Rule> = d.core().state.rules.clone();
            let mut out = vec![];
            for id in ids {
                let r = rules.iter().find(|r| &r.id == id).ok_or_else(|| RpcError::not_found(format!("no rule {id}")))?;
                let left = r.expires_at.as_deref().and_then(time::parse_rfc3339).map(|t| t - now);
                if left.is_some_and(|s| s <= 0) {
                    return Err(bad(format!("rule {id} has run out")));
                }
                let project = match &r.scope {
                    RuleScope::Global => None,
                    RuleScope::Project(p) => Some(project_name(d, p).ok_or_else(|| RpcError::not_found(format!("rule {id}'s project is gone")))?),
                    RuleScope::Session(_) => return Err(bad(format!("rule {id} is for one terminal, which only this Mac has"))),
                };
                out.push(SharedRule { effect: r.effect, matcher: r.matcher.clone(), project, expires_in_secs: left.map(|s| s as u64) });
            }
            MidnaObject::Rules { rules: out }
        }
        ObjectType::Triggers => {
            named("triggers (trigger.list)")?;
            let all: Vec<Trigger> = d.core().state.triggers.clone();
            let mut out = vec![];
            for id in ids {
                let t = all.iter().find(|t| &t.id == id).ok_or_else(|| RpcError::not_found(format!("no trigger {id}")))?;
                out.push(shared_trigger(d, t)?);
            }
            MidnaObject::Triggers { triggers: out }
        }
        ObjectType::Theme => {
            let [id] = ids else { return Err(bad("name one theme (`themes.list`) in `ids`")) };
            if themes::BUILTIN_IDS.contains(&id.as_str()) {
                return Err(bad(format!("{id} is built in; every Midna has it")));
            }
            let (all, _) = themes::load_all(&themes::themes_dir(&d.cfg.home));
            let t = all.into_iter().find(|t| &t.id == id).ok_or_else(|| RpcError::not_found(format!("no theme {id} (themes.list)")))?;
            let colors = themes::UI_TOKENS.iter().zip(t.ui).map(|(k, c)| (k.to_string(), themes::to_hex(c))).collect();
            MidnaObject::Theme { theme: SharedTheme { id: t.id, name: t.name, dark: t.dark, colors, ansi: t.ansi.iter().map(|c| themes::to_hex(*c)).collect() } }
        }
        ObjectType::Settings => {
            let core = d.core();
            let keys: Vec<String> = if ids.is_empty() { core.state.settings.keys().cloned().collect() } else { ids.to_vec() };
            let mut values = BTreeMap::new();
            for key in keys {
                let spec = core.state.setting_spec(&key).ok_or_else(|| RpcError::not_found(format!("no setting {key}")))?;
                let value = core.state.setting(&key);
                if stays_here(&key, spec, &value) {
                    if ids.is_empty() {
                        continue;
                    }
                    return Err(bad(format!("{key} is this Mac's own and isn't sent")));
                }
                values.insert(key, value);
            }
            if values.is_empty() {
                return Err(bad("no changed settings that may be sent"));
            }
            MidnaObject::Settings { values }
        }
        ObjectType::Queue => {
            named("queued prompts (queue.list)")?;
            let core = d.core();
            let mut prompts = vec![];
            for id in ids {
                let m = core.state.sessions.iter().flat_map(|s| s.queue.iter()).find(|m| &m.id == id);
                let m = m.ok_or_else(|| RpcError::not_found(format!("no queued prompt {id}")))?;
                if !m.images.is_empty() {
                    return Err(bad(format!("{id} has images, which aren't sent; send them as files")));
                }
                if m.text.is_empty() {
                    return Err(bad(format!("{id} has no text")));
                }
                prompts.push(SharedPrompt { text: m.text.clone(), enter: m.enter });
            }
            MidnaObject::Queue { prompts }
        }
    };
    let payload = midna_proto::nearby::SharePayload::Object { object: obj };
    payload.check().map_err(bad)?;
    let midna_proto::nearby::SharePayload::Object { object } = payload else { unreachable!() };
    Ok(object)
}

fn shared_trigger(d: &Daemon, t: &Trigger) -> Result<SharedTrigger, RpcError> {
    if t.builtin.is_some() {
        return Err(bad(format!("{} is built in; every Midna has it", t.id)));
    }
    let one_terminal = t.filter.session.is_some() || matches!(&t.action, TriggerAction::StartAgent { session: Some(_), .. });
    if one_terminal {
        return Err(bad(format!("{} is for one terminal, which only this Mac has", t.id)));
    }
    let gone = || RpcError::not_found(format!("{}'s project is gone", t.id));
    let mut filter = t.filter.clone();
    if let Some(p) = &filter.project {
        filter.project = Some(project_name(d, p).ok_or_else(gone)?);
    }
    let mut action = t.action.clone();
    if let TriggerAction::StartAgent { project_id, .. } | TriggerAction::RunCommand { project_id, .. } = &mut action {
        *project_id = project_name(d, project_id).ok_or_else(gone)?;
    }
    Ok(SharedTrigger {
        name: t.name.clone(),
        source: t.source,
        event: t.event.clone(),
        filter,
        action,
        session_name_template: t.session_name_template.clone(),
        cooldown_secs: t.cooldown_secs,
    })
}

/// One thing accepting would do on this Mac.
#[derive(Clone, Debug)]
pub enum Todo {
    Rule { effect: Effect, matcher: midna_proto::Matcher, scope: RuleScope, expires_in: Option<u64> },
    Trigger(Box<TriggerAddParams>),
    Setting { key: String, value: Value, human_only: bool },
    Theme { id: String, json: String },
    Summary(Box<ClosedTerminal>),
    Prompt(SharedPrompt),
}

impl Todo {
    fn needs_human(&self) -> bool {
        match self {
            Todo::Rule { .. } | Todo::Trigger(_) => true,
            Todo::Setting { human_only, .. } => *human_only,
            _ => false,
        }
    }
}

/// A line of the review, with what it applies (none for a skipped one).
#[derive(Clone, Debug)]
pub struct Change {
    pub line: String,
    pub todo: Option<Todo>,
}

fn skip(line: String) -> Change {
    Change { line: format!("· skipped: {line}"), todo: None }
}

fn take(line: String, todo: Todo) -> Change {
    Change { line: format!("+ {line}"), todo: Some(todo) }
}

/// What accepting `obj` would change on this Mac, line by line.
pub fn plan(d: &Daemon, obj: &MidnaObject) -> Vec<Change> {
    match obj {
        MidnaObject::Rules { rules } => rules.iter().map(|r| plan_rule(d, r)).collect(),
        MidnaObject::Triggers { triggers } => triggers.iter().map(|t| plan_trigger(d, t)).collect(),
        MidnaObject::Settings { values } => values.iter().map(|(k, v)| plan_setting(d, k, v)).collect(),
        MidnaObject::Theme { theme } => vec![plan_theme(d, theme)],
        MidnaObject::Summary { project, record } => vec![plan_summary(d, project, record)],
        MidnaObject::Queue { prompts } => prompts.iter().map(|p| take(format!("queue “{}”{}", crate::nearby::share::cut(&p.text, 80), if p.enter { "" } else { " (typed, not sent)" }), Todo::Prompt(p.clone()))).collect(),
    }
}

fn kind_word(v: &impl serde::Serialize) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn plan_rule(d: &Daemon, r: &SharedRule) -> Change {
    let what = format!("{} {} `{}`", r.effect.as_str(), kind_word(&r.matcher.kind), r.matcher.pattern);
    let scope = match &r.project {
        None => RuleScope::Global,
        Some(name) => match project_named(d, name) {
            Some(id) => RuleScope::Project(id),
            None => return skip(format!("rule {what}: {}", no_project(name))),
        },
    };
    let mut line = format!("rule: {what}, {}", r.project.as_deref().map(|p| format!("in {p}")).unwrap_or_else(|| "everywhere".into()));
    if let Some(s) = r.expires_in_secs {
        line.push_str(&format!(", for {}", for_how_long(s)));
    }
    let have = d.core().state.rules.iter().any(|x| x.effect == r.effect && x.matcher == r.matcher && x.scope == scope);
    if have {
        return skip(format!("rule {what}: you have it"));
    }
    if r.matcher.pattern.trim().is_empty() {
        return skip(format!("rule {what}: its pattern is empty"));
    }
    take(line, Todo::Rule { effect: r.effect, matcher: r.matcher.clone(), scope, expires_in: r.expires_in_secs })
}

fn for_how_long(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s} seconds"),
        s if s < 7200 => format!("{} minutes", s / 60),
        s if s < 172_800 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

fn plan_trigger(d: &Daemon, t: &SharedTrigger) -> Change {
    let name = crate::nearby::share::cut(&t.name, 80);
    let mut filter: TriggerFilter = t.filter.clone();
    filter.session = None;
    if let Some(p) = &filter.project {
        match project_named(d, p) {
            Some(id) => filter.project = Some(id),
            None => return skip(format!("trigger “{name}”: {}", no_project(p))),
        }
    }
    let mut action = t.action.clone();
    if let TriggerAction::StartAgent { project_id, .. } | TriggerAction::RunCommand { project_id, .. } = &mut action {
        match project_named(d, project_id) {
            Some(id) => *project_id = id,
            None => return skip(format!("trigger “{name}”: {}", no_project(project_id))),
        }
    }
    if let TriggerAction::StartAgent { session, .. } = &mut action {
        *session = None;
    }
    let have = d.core().state.triggers.iter().any(|x| x.name == t.name && x.event == t.event && x.source == t.source && x.action == action);
    if have {
        return skip(format!("trigger “{name}”: you have it"));
    }
    let p = TriggerAddParams {
        name: t.name.clone(),
        source: t.source,
        event: t.event.clone(),
        filter,
        action,
        github_hook_id: None,
        session_name_template: t.session_name_template.clone(),
        cooldown_secs: t.cooldown_secs,
        enabled: false,
    };
    let probe = Trigger {
        id: "t_000000".into(),
        name: p.name.clone(),
        source: p.source,
        event: p.event.clone(),
        filter: p.filter.clone(),
        action: p.action.clone(),
        enabled: false,
        state: TriggerState::Draft,
        secret_set: false,
        created_by: Actor::human(),
        created_at: time::now_rfc3339(),
        last_fired_at: None,
        fired: 0,
        last_fired_summary: None,
        enabled_at: None,
        secret_set_at: None,
        secret_store: None,
        github_hook_id: None,
        session_name_template: p.session_name_template.clone(),
        cooldown_secs: p.cooldown_secs,
        builtin: None,
    };
    if let Err(e) = crate::rpc::trigger::validate(d, &probe) {
        return skip(format!("trigger “{name}”: {}", e.message));
    }
    let off = if t.source == TriggerSource::Local { "off, as a draft" } else { "off, until you give it its secret" };
    let mut line = format!("trigger “{name}” on {} ({off})", t.event);
    if let Some((mode, tools)) = p.action.agent_access() {
        let mode = mode.map(|m| format!(" in {m} mode")).unwrap_or_default();
        let tools = if tools.is_empty() { String::new() } else { format!(", allowed to use {} without asking", tools.join(", ")) };
        line.push_str(&format!("; the agents it starts run{mode}{tools}"));
    }
    take(line, Todo::Trigger(Box::new(p)))
}

fn shown(v: &Value) -> String {
    crate::nearby::share::cut(&match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }, 80)
}

fn plan_setting(d: &Daemon, key: &str, value: &Value) -> Change {
    let (spec, current) = {
        let core = d.core();
        (core.state.setting_spec(key), core.state.setting(key))
    };
    let Some(spec) = spec else { return skip(format!("{key}: no such setting here")) };
    if stays_here(key, spec, value) {
        return skip(format!("{key}: this Mac's own"));
    }
    let value = match spec.coerce(value).and_then(|v| crate::notify_media::check_setting(&d.cfg.home, key, &v).map(|_| v)) {
        Ok(v) => v,
        Err(e) => return skip(format!("{key}: {e}")),
    };
    if value == current {
        return skip(format!("{key}: already {}", shown(&value)));
    }
    let human = if spec.human_only { " (human only)" } else { "" };
    take(format!("{key}: {} → {}{human}", shown(&current), shown(&value)), Todo::Setting { key: key.into(), value, human_only: spec.human_only })
}

fn theme_json(t: &SharedTheme) -> String {
    let file = json!({
        "name": t.name,
        "kind": if t.dark { "dark" } else { "light" },
        "extends": if t.dark { themes::DEFAULT_DARK } else { themes::DEFAULT_LIGHT },
        "colors": t.colors,
        "terminal": { "ansi": t.ansi },
    });
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

fn plan_theme(d: &Daemon, t: &SharedTheme) -> Change {
    let name = crate::nearby::share::cut(&t.name, 80);
    if !themes::valid_id(&t.id) || themes::BUILTIN_IDS.contains(&t.id.as_str()) {
        return skip(format!("the theme “{name}”: `{}` isn't a theme id it may take", t.id));
    }
    let dir = themes::themes_dir(&d.cfg.home);
    let (mine, _) = themes::load_all(&dir);
    let same = |x: &themes::ThemeDef| {
        x.dark == t.dark
            && themes::UI_TOKENS.iter().zip(x.ui).all(|(k, c)| t.colors.get(*k).is_some_and(|h| themes::parse_hex(h) == Some(c)))
            && x.ansi.iter().zip(&t.ansi).all(|(c, h)| themes::parse_hex(h) == Some(*c))
    };
    if mine.iter().any(|x| x.name == t.name && same(x)) {
        return skip(format!("the theme “{name}”: you have it"));
    }
    let taken = |id: &str| themes::BUILTIN_IDS.contains(&id) || dir.join(format!("{id}.json")).exists() || mine.iter().any(|x| x.id == id);
    let id = (1..100).map(|n| if n == 1 { t.id.clone() } else { format!("{}-{n}", t.id) }).find(|id| themes::valid_id(id) && !taken(id));
    let Some(id) = id else { return skip(format!("the theme “{name}”: no free id for it")) };
    let json = theme_json(t);
    let (resolved, errors) = themes::resolve_files(vec![(id.clone(), dir.join(format!("{id}.json")), json.clone())]);
    if !errors.is_empty() || !resolved.iter().any(|x| x.id == id) {
        return skip(format!("the theme “{name}”: {}", errors.join("; ")));
    }
    let kind = if t.dark { "dark" } else { "light" };
    let note = if id == t.id { String::new() } else { format!(" as {id} (you have a different {})", t.id) };
    take(format!("the {kind} theme “{name}”{note}; pick it in Settings → Appearance"), Todo::Theme { id, json })
}

fn plan_summary(d: &Daemon, project: &str, record: &ClosedTerminal) -> Change {
    let name = crate::nearby::share::cut(&record.name, 80);
    let (pid, under) = if project == ROOT_NAME {
        (ROOT_PROJECT_ID.to_string(), ROOT_NAME.to_string())
    } else {
        match project_named(d, project) {
            Some(id) => (id, project.to_string()),
            None => (ROOT_PROJECT_ID.to_string(), format!("{ROOT_NAME} ({})", no_project(project))),
        }
    };
    if crate::summary::get(d, &record.id).is_some() {
        return skip(format!("the Summary of “{name}”: you have it"));
    }
    let keep = d.core().state.setting_i64("summary.keep_days").max(1);
    let closed = time::parse_rfc3339(&record.closed_at);
    if closed.is_none_or(|t| t < time::now_unix() - keep * 86_400) {
        return skip(format!("the Summary of “{name}”: it closed more than {keep} days ago (summary.keep_days)"));
    }
    let mut rec = record.clone();
    rec.project_id = pid.clone();
    rec.conversation_id = None;
    rec.agent_args.clear();
    rec.prompts.iter_mut().for_each(|p| p.conversation = None);
    if let Some(p) = d.core().state.project(&pid) {
        rec.cwd = p.path.clone();
    }
    take(format!("the Summary of “{name}” ({} prompts) under {under}", rec.turns), Todo::Summary(Box::new(rec)))
}

/// What `obj` holds, line by line, as the sender sees it (the guard's approval shows it).
pub fn listing(obj: &MidnaObject) -> String {
    let lines: Vec<String> = match obj {
        MidnaObject::Summary { project, record } => {
            let prompts = record.prompts.iter().take(10).map(|p| format!("  › {}", crate::nearby::share::cut(&p.text, 80)));
            std::iter::once(format!("“{}” in {project}, {} prompts, closed {}", record.name, record.turns, record.closed_at)).chain(prompts).collect()
        }
        MidnaObject::Rules { rules } => rules
            .iter()
            .map(|r| format!("{} {} `{}`{}", r.effect.as_str(), kind_word(&r.matcher.kind), r.matcher.pattern, r.project.as_deref().map(|p| format!(" in {p}")).unwrap_or_default()))
            .collect(),
        MidnaObject::Triggers { triggers } => triggers.iter().map(|t| format!("“{}” on {}", t.name, t.event)).collect(),
        MidnaObject::Theme { theme } => vec![format!("“{}” ({}, {})", theme.name, theme.id, if theme.dark { "dark" } else { "light" })],
        MidnaObject::Settings { values } => values.iter().map(|(k, v)| format!("{k} = {}", shown(v))).collect(),
        MidnaObject::Queue { prompts } => prompts.iter().map(|p| format!("› {}", crate::nearby::share::cut(&p.text, 200))).collect(),
    };
    lines.join("\n")
}

/// The approval's review: what accepting `obj` would change here.
pub fn review(d: &Daemon, obj: &MidnaObject) -> String {
    let changes = plan(d, obj);
    let mut out: Vec<String> = changes.iter().map(|c| c.line.clone()).collect();
    if changes.iter().all(|c| c.todo.is_none()) {
        out.push("Nothing here would change.".into());
    } else if changes.iter().any(|c| c.todo.as_ref().is_some_and(Todo::needs_human)) {
        out.push("Only you can accept this: it changes policy, triggers or human-only settings.".into());
    }
    out.join("\n")
}

/// Whether accepting `obj` would do something only the human may.
pub fn needs_human(d: &Daemon, obj: &MidnaObject) -> bool {
    plan(d, obj).iter().any(|c| c.todo.as_ref().is_some_and(Todo::needs_human))
}

/// Accept `obj` from `from` (a device's name): apply what the review says now. Prompts go into
/// `session`'s queue; without one they wait in the inbox. Returns what happened, for people.
pub fn apply(d: &Daemon, ctx: &Ctx, obj: &MidnaObject, session: Option<&str>, from: &str) -> Result<String, RpcError> {
    let changes = plan(d, obj);
    if !ctx.is_human() && changes.iter().any(|c| c.todo.as_ref().is_some_and(Todo::needs_human)) {
        return Err(RpcError::human_only("only the human may accept rules, triggers or human-only settings from another device"));
    }
    if let MidnaObject::Queue { .. } = obj {
        let Some(sid) = session else {
            return Ok("kept in the inbox; queue it with midna inbox accept <id> --into <terminal>".into());
        };
        if d.core().state.session(sid).is_none() {
            return Err(RpcError::not_found(format!("no terminal {sid}")));
        }
    } else if session.is_some() {
        return Err(bad("only queued prompts go into a terminal"));
    }
    let (mut done, mut skipped, mut failed) = (0usize, changes.iter().filter(|c| c.todo.is_none()).count(), vec![]);
    for c in changes {
        let Some(todo) = c.todo else { continue };
        match apply_one(d, ctx, todo, session, from) {
            Ok(true) => done += 1,
            Ok(false) => skipped += 1,
            Err(e) => failed.push(e.message),
        }
    }
    let what = obj.object_type().word();
    let mut out = match (obj, session) {
        (MidnaObject::Queue { .. }, Some(sid)) => format!("queued {done} in {sid}"),
        _ => format!("added {done} ({what})"),
    };
    if skipped > 0 {
        out.push_str(&format!(", skipped {skipped}"));
    }
    if !failed.is_empty() {
        out.push_str(&format!(", {} failed: {}", failed.len(), failed.join("; ")));
    }
    Ok(out)
}

fn apply_one(d: &Daemon, ctx: &Ctx, todo: Todo, session: Option<&str>, from: &str) -> Result<bool, RpcError> {
    match todo {
        Todo::Rule { effect, matcher, scope, expires_in } => {
            crate::rpc::policy::insert_rule(d, crate::rpc::policy::new_rule(effect, matcher, scope, expires_in, ctx.actor()));
        }
        Todo::Trigger(p) => {
            crate::rpc::trigger::add(d, ctx, *p)?;
        }
        Todo::Setting { key, value, .. } => {
            crate::rpc::settings::set(d, ctx, SettingSetParams { key, value })?;
        }
        Todo::Theme { id, json } => write_theme(d, &id, &json).map_err(|e| RpcError::internal(format!("saving the theme: {e}")))?,
        Todo::Summary(mut rec) => {
            let panel = SummaryPanel { app: Some("Nearby".into()), title: format!("Sent from {from}"), ..SummaryPanel::default() };
            rec.apps.insert(PANEL_OWNER.into(), panel);
            return Ok(crate::summary::import(d, *rec, ctx.actor()));
        }
        Todo::Prompt(p) => {
            let sid = session.ok_or_else(|| bad("no terminal"))?;
            crate::queue::add(d, sid, p.text, p.enter, vec![], SendWhen::Idle, None, ctx.actor(), None)?;
        }
    }
    Ok(true)
}

fn write_theme(d: &Daemon, id: &str, json: &str) -> std::io::Result<()> {
    let dir = themes::themes_dir(&d.cfg.home);
    let path = dir.join(format!("{id}.json"));
    midna_proto::paths::guard_write(&path)?;
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{id}.json.tmp-{}", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(json.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, &path)
}
