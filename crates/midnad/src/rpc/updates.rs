//! `updates.*`: the app owns its updater (feed, signature check, staging and the bundle swap
//! all live in midna-app's lifecycle thread), so the daemon only proxies.
//!
//! - The GUI reports its state with `updates.report` (human only); the daemon keeps the last
//!   report in memory and emits `updates.status` when it changes.
//! - `updates.check` / `updates.install` are forwarded to every GUI connection as an
//!   `updates.command` notification (`{action: check|install, by}`), like `window.command`.
//!   `updates.install` is human only, so an agent's call becomes a needs-you approval and the
//!   human's approval runs it, unless the human turned on `agents.may_install_updates`.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;

pub fn status(d: &Daemon) -> UpdatesStatus {
    let mut s = d.updates.lock().unwrap_or_else(|e| e.into_inner()).clone();
    s.gui_connected = d.gui_connected();
    if s.state.is_empty() {
        s.state = "unknown".into();
    }
    if s.channel.is_none() {
        s.channel = Some(d.core().state.setting_str("updates.channel"));
    }
    s
}

pub fn get(d: &Daemon) -> R {
    ok(status(d))
}

pub fn report(d: &Daemon, ctx: &Ctx, p: UpdatesReportParams) -> R {
    let mut st = p.status;
    st.reported_at = Some(time::now_rfc3339());
    st.gui_connected = true;
    let changed = {
        let mut cur = d.updates.lock().unwrap_or_else(|e| e.into_inner());
        let mut a = cur.clone();
        let mut b = st.clone();
        // A new report time alone isn't news.
        a.reported_at = None;
        b.reported_at = None;
        a.gui_connected = true;
        let changed = a != b;
        *cur = st.clone();
        changed
    };
    if changed {
        d.emit(kinds::UPDATES_STATUS, ctx.actor(), None, None, serde_json::to_value(&st).unwrap_or_default());
    }
    ok(OkResult { ok: true })
}

/// `agents.may_install_updates`: an agent's `updates.install` goes straight to the GUI, which
/// still checks the update's signature before swapping the bundle.
pub fn agent_may_install(d: &Daemon, method: &str) -> bool {
    method == "updates.install" && d.core().state.setting_bool("agents.may_install_updates")
}

/// Forward `check` / `install` to the GUI.
pub fn command(d: &Daemon, ctx: &Ctx, action: &str) -> R {
    let delivered = d.gui_send("updates.command", json!({ "action": action, "by": ctx.actor() }));
    d.emit(kinds::UPDATES_REQUESTED, ctx.actor(), None, None, json!({ "action": action, "delivered": delivered }));
    ok(UpdatesCommandResult { ok: delivered > 0, delivered, status: status(d) })
}
