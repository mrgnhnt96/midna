//! settings.* handlers. Values live in state; the catalog lives in midna_proto::settings.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::settings::{SETTINGS, SettingSpec, setting};
use midna_proto::*;
use serde_json::{Value, json};

fn cli_value(v: &Value) -> String {
    match v {
        Value::String(s) if s.is_empty() => "''".into(),
        Value::String(s) if s.contains(char::is_whitespace) => format!("'{s}'"),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.is_empty() => "''".into(),
        Value::Array(a) if a.iter().all(Value::is_string) => cli_value(&Value::String(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","))),
        v => v.to_string(),
    }
}

fn entry(spec: &SettingSpec, value: Value) -> SettingEntry {
    let default = spec.default.to_json();
    let example = if value == default { default.clone() } else { value.clone() };
    SettingEntry {
        key: spec.key.into(),
        ty: spec.setting_type(),
        cli: format!("midna settings set {} {}", spec.key, cli_value(&example)),
        value,
        default,
        description: spec.description.into(),
        human_only: spec.human_only,
        section: spec.section.into(),
    }
}

fn spec(key: &str) -> Result<&'static SettingSpec, RpcError> {
    setting(key).ok_or_else(|| RpcError::not_found(format!("unknown setting `{key}`; see settings.list")))
}

pub fn list(d: &Daemon) -> R {
    let core = d.core();
    ok(SETTINGS.iter().map(|s| entry(s, core.state.setting(s.key))).collect::<Vec<_>>())
}

pub fn get(d: &Daemon, p: SettingKeyParams) -> R {
    let s = spec(&p.key)?;
    ok(entry(s, d.core().state.setting(s.key)))
}

/// Header/row/status scripts take built-in parts or an executable path. midnad runs that path
/// for every terminal, outside any agent's policy rules, so pointing it at a custom executable
/// is human only (an agent may still pick built-ins).
/// `ui.status.items` and `ui.header.buttons` the same way: an agent may hide, show and reorder items, and keep script
/// paths already there, but adding a new path asks the human.
fn custom_script(s: &SettingSpec, value: &Value, current: &Value) -> bool {
    match s.key {
        "ui.header.script" | "ui.row.script" | "ui.status.script" => value.as_str().is_some_and(|v| !midna_proto::settings::is_builtin_script(v)),
        "ui.status.items" | "ui.header.buttons" => {
            let had = |p: &str| current.as_array().is_some_and(|a| a.iter().any(|i| i.as_str() == Some(p)));
            value.as_array().is_some_and(|a| a.iter().filter_map(Value::as_str).any(|i| i.starts_with('/') && !had(i)))
        }
        _ => false,
    }
}

pub fn set(d: &Daemon, ctx: &Ctx, p: SettingSetParams) -> R {
    let s = spec(&p.key)?;
    let value = s.coerce(&p.value).map_err(RpcError::bad_params)?;
    crate::notify_media::check_setting(&d.cfg.home, s.key, &value).map_err(RpcError::bad_params)?;
    let current = d.core().state.setting(s.key);
    if (s.human_only || custom_script(s, &value, &current)) && !ctx.is_human() {
        let cli = format!("settings set {} {}", s.key, cli_value(&value));
        let params = json!({ "key": s.key, "value": value });
        return Err(super::defer_to_human(d, ctx, &format!("Agent asks to change {}", s.key), &cli, "settings.set", &params));
    }
    apply(d, ctx.actor(), s, value)
}

pub fn reset(d: &Daemon, ctx: &Ctx, p: SettingKeyParams) -> R {
    let s = spec(&p.key)?;
    if s.human_only && !ctx.is_human() {
        let params = json!({ "key": s.key });
        return Err(super::defer_to_human(d, ctx, &format!("Agent asks to reset {}", s.key), &format!("settings reset {}", s.key), "settings.reset", &params));
    }
    apply(d, ctx.actor(), s, s.default.to_json())
}

/// Set a setting from inside midnad (no caller to authorize), e.g. a human's answer that
/// saves a choice. Logs and ignores a value the catalog rejects.
pub fn set_as(d: &Daemon, by: Actor, key: &str, value: Value) {
    let Some(s) = setting(key) else { return };
    match s.coerce(&value) {
        Ok(v) => drop(apply(d, by, s, v)),
        Err(e) => eprintln!("midnad: set {key}: {e}"),
    }
}

fn apply(d: &Daemon, by: Actor, s: &SettingSpec, value: Value) -> R {
    let old = {
        let mut core = d.core();
        let old = core.state.setting(s.key);
        if value == s.default.to_json() {
            core.state.settings.remove(s.key);
        } else {
            core.state.settings.insert(s.key.into(), value.clone());
        }
        old
    };
    if old != value {
        d.mark_dirty();
        d.emit(kinds::SETTINGS_CHANGED, by, None, None, json!({ "key": s.key, "value": value, "old": old }));
        if s.key == "agents.claude.statusline" {
            crate::hooks::write_claude_settings(d);
        }
        if s.key.starts_with("terminal.auto_name") {
            crate::auto_name::refresh_all(d);
        }
    }
    ok(entry(s, value))
}
