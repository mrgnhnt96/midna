//! window.* and session.focus: the daemon forwards commands to connected GUI clients.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;
use std::sync::Arc;

pub fn list(d: &Daemon) -> R {
    let keep_on_top = d.core().state.sessions.iter().filter(|s| s.keep_on_top).map(|s| s.id.clone()).collect();
    ok(WindowList { gui_connected: d.gui_connected(), keep_on_top })
}

pub fn command(d: &Arc<Daemon>, ctx: &Ctx, p: WindowCommandParams) -> R {
    // Split only adds a view beside what the human is looking at (front even replaces it),
    // so like front and open_screen it needs no setting; rules can still deny `split*`.
    let gentle = matches!(p.action, WindowAction::Front | WindowAction::OpenScreen | WindowAction::Split);
    let target_session = p.target.as_deref().and_then(|t| d.core().state.session(t).cloned());
    if p.action == WindowAction::Split {
        let value = p.value.as_ref().and_then(|v| v.as_str()).unwrap_or("side");
        if !matches!(value, "side" | "stacked" | "close") {
            return Err(RpcError::bad_params("split value must be side, stacked or close"));
        }
        if value != "close" && target_session.is_none() {
            return Err(RpcError::bad_params("split needs `target`: the session to show beside the selected one"));
        }
    }
    if !ctx.is_human() {
        if !gentle && !d.core().state.setting_bool("agents.may_move_windows") {
            return Err(RpcError::refused(format!(
                "window {} needs setting agents.may_move_windows (human only); front, open_screen and split are always allowed",
                p.action.as_str()
            )));
        }
        let value = format!("{} {}", p.action.as_str(), p.target.clone().unwrap_or_default()).trim().to_string();
        super::policy::gate(d, ctx, ActionKind::Window, &value, target_session.as_ref(), false)?;
    }
    if p.action == WindowAction::KeepOnTop
        && let Some(s) = &target_session {
            let on = p.value.as_ref().and_then(|v| v.as_bool()).unwrap_or(true);
            if let Some(sess) = d.core().state.session_mut(&s.id) {
                sess.keep_on_top = on;
            }
            d.mark_dirty();
        }
    let params = json!({ "action": p.action, "target": p.target, "value": p.value, "by": ctx.actor() });
    let delivered = d.gui_send("window.command", params.clone());
    d.emit(kinds::WINDOW_COMMAND, ctx.actor(), target_session.as_ref().map(|s| s.project_id.clone()), target_session.map(|s| s.id), params);
    ok(WindowCommandResult { ok: true, delivered })
}

pub fn focus(d: &Arc<Daemon>, ctx: &Ctx, p: IdParams) -> R {
    if d.core().state.session(&p.id).is_none() {
        return Err(RpcError::not_found(format!("no session {}", p.id)));
    }
    command(d, ctx, WindowCommandParams { action: WindowAction::Front, target: Some(p.id), value: None })
}
