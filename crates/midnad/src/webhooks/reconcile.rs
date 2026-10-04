//! Missed GitHub delivery recovery (best effort). For each active GitHub trigger with a
//! `github_hook_id` and an exact `filter.repo`, ask GitHub (through an authenticated `gh`) for
//! the hook's recent deliveries and run the ones midnad never received, as `recovered`.
//!
//! Only deliveries from the last 3 days, and never from before the trigger was enabled, are
//! considered, so enabling a trigger doesn't replay old history.
use crate::daemon::Daemon;
use crate::git::run_cmd;
use midna_proto::*;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub const WINDOW_SECS: i64 = 3 * 86_400;

/// One row of `GET /repos/{owner}/{repo}/hooks/{id}/deliveries`.
#[derive(Clone, Debug, PartialEq)]
pub struct GhDelivery {
    pub id: u64,
    pub guid: String,
    pub delivered_at: i64,
    pub redelivery: bool,
    pub status_code: u16,
    pub event: String,
    pub action: Option<String>,
}

pub fn parse_list(json: &str) -> Vec<GhDelivery> {
    let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(json) else { return vec![] };
    rows.iter()
        .filter_map(|r| {
            Some(GhDelivery {
                id: r.get("id")?.as_u64()?,
                guid: r.get("guid")?.as_str()?.to_string(),
                delivered_at: time::parse_rfc3339(r.get("delivered_at")?.as_str()?)?,
                redelivery: r.get("redelivery").and_then(Value::as_bool).unwrap_or(false),
                status_code: r.get("status_code").and_then(Value::as_u64).unwrap_or(0) as u16,
                event: r.get("event")?.as_str()?.to_string(),
                action: r.get("action").and_then(Value::as_str).map(str::to_string),
            })
        })
        .collect()
}

/// Deliveries (one per GUID, oldest first) newer than `since` that midnad hasn't seen.
pub fn missing(list: &[GhDelivery], known: &HashSet<String>, since: i64) -> Vec<GhDelivery> {
    let mut by_guid: BTreeMap<&str, &GhDelivery> = BTreeMap::new();
    for d in list.iter().filter(|d| d.delivered_at >= since && !known.contains(&d.guid) && d.event != "ping") {
        // Keep the original attempt (redeliveries share the GUID).
        let e = by_guid.entry(&d.guid).or_insert(d);
        if e.redelivery && !d.redelivery {
            *e = d;
        }
    }
    let mut v: Vec<GhDelivery> = by_guid.into_values().cloned().collect();
    v.sort_by_key(|d| d.delivered_at);
    v
}

/// `GET …/deliveries/{id}`: (event, guid, payload).
pub fn parse_detail(json: &str) -> Option<(String, String, Value)> {
    let v: Value = serde_json::from_str(json).ok()?;
    let req = v.get("request")?;
    let headers = req.get("headers")?.as_object()?;
    let h = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).and_then(|(_, v)| v.as_str()).map(str::to_string);
    let event = h("X-GitHub-Event").or_else(|| v.get("event").and_then(Value::as_str).map(str::to_string))?;
    let guid = h("X-GitHub-Delivery").or_else(|| v.get("guid").and_then(Value::as_str).map(str::to_string))?;
    let payload = match req.get("payload")? {
        Value::String(s) => serde_json::from_str(s).ok()?,
        p => p.clone(),
    };
    Some((event, guid, payload))
}

fn gh(args: &[&str]) -> Option<String> {
    let mut argv = vec!["gh"];
    argv.extend_from_slice(args);
    run_cmd(&argv, "/", Duration::from_secs(20), None, &[])
}

fn gh_disabled() -> bool {
    std::env::var("MIDNA_NO_GH").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Run in the background unless one is already running.
pub fn spawn(d: &Arc<Daemon>, reason: &'static str) {
    let wanted = d.core().state.triggers.iter().any(|t| t.github_hook_id.is_some() && t.state == TriggerState::Active);
    if !wanted {
        return;
    }
    let d = d.clone();
    let _ = std::thread::Builder::new().name("webhooks-reconcile".into()).spawn(move || {
        run(&d, reason);
    });
}

pub fn run(d: &Arc<Daemon>, reason: &str) -> ReconcileStatus {
    if d.webhooks.reconciling.swap(true, Ordering::SeqCst) {
        return ReconcileStatus { error: Some("a reconcile is already running".into()), ..d.webhooks.st().reconcile.clone() };
    }
    let st = run_inner(d, reason);
    d.webhooks.reconciling.store(false, Ordering::SeqCst);
    d.webhooks.st().reconcile = st.clone();
    st
}

fn run_inner(d: &Arc<Daemon>, reason: &str) -> ReconcileStatus {
    let mut st = ReconcileStatus { last_run_at: Some(time::now_rfc3339()), reason: Some(reason.into()), ..Default::default() };
    // (repo, hook id) -> earliest enabled_at among its active triggers.
    let mut hooks: BTreeMap<(String, u64), i64> = BTreeMap::new();
    let now = time::now_unix();
    for t in d.core().state.triggers.iter() {
        let (Some(h), Some(repo)) = (t.github_hook_id, t.filter.repo.as_ref()) else { continue };
        if t.source != TriggerSource::Github || t.state != TriggerState::Active || repo.contains(['*', '?']) || !repo.contains('/') {
            continue;
        }
        let since = t.enabled_at.as_deref().and_then(time::parse_rfc3339).unwrap_or(now).max(now - WINDOW_SECS);
        let e = hooks.entry((repo.clone(), h)).or_insert(since);
        *e = (*e).min(since);
    }
    if hooks.is_empty() {
        return st;
    }
    if gh_disabled() {
        st.error = Some("gh lookups are disabled (MIDNA_NO_GH)".into());
        return st;
    }
    if gh(&["auth", "status"]).is_none() {
        st.error = Some("gh is not installed or not authenticated (run `gh auth login`)".into());
        return st;
    }
    let mut errors = vec![];
    for ((repo, hook), since) in hooks {
        st.hooks_checked += 1;
        let Some(list) = gh(&["api", &format!("repos/{repo}/hooks/{hook}/deliveries?per_page=100")]) else {
            errors.push(format!("{repo} hook {hook}: could not list deliveries"));
            continue;
        };
        let known: HashSet<String> = d.core().state.deliveries.iter().filter(|x| x.verdict != Verdict::BadSignature).map(|x| x.delivery_guid.clone()).collect();
        for m in missing(&parse_list(&list), &known, since) {
            let Some(detail) = gh(&["api", &format!("repos/{repo}/hooks/{hook}/deliveries/{}", m.id)]) else {
                errors.push(format!("{repo}: could not fetch delivery {}", m.id));
                continue;
            };
            let Some((event, guid, payload)) = parse_detail(&detail) else { continue };
            if crate::webhooks::process::recover(d, &event, &guid, payload).is_some() {
                st.recovered += 1;
            }
        }
    }
    if !errors.is_empty() {
        st.error = Some(errors.join("; "));
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"[
      {"id": 3, "guid": "c61b0f8e-2a95", "delivered_at": "2026-10-03T03:47:55Z", "redelivery": false, "duration": 10.0,
       "status": "Couldn't connect to server", "status_code": 0, "event": "pull_request", "action": "opened",
       "installation_id": null, "repository_id": 1},
      {"id": 4, "guid": "c61b0f8e-2a95", "delivered_at": "2026-10-03T07:00:00Z", "redelivery": true, "duration": 0.2,
       "status": "OK", "status_code": 202, "event": "pull_request", "action": "opened", "installation_id": null, "repository_id": 1},
      {"id": 2, "guid": "7c1e04b2-9a1f", "delivered_at": "2026-10-03T09:42:07Z", "redelivery": false, "duration": 0.1,
       "status": "OK", "status_code": 202, "event": "pull_request", "action": "opened", "installation_id": null, "repository_id": 1},
      {"id": 1, "guid": "old-one", "delivered_at": "2026-09-20T00:00:00Z", "redelivery": false, "duration": 0.1,
       "status": "OK", "status_code": 202, "event": "push", "action": null, "installation_id": null, "repository_id": 1},
      {"id": 0, "guid": "ping-1", "delivered_at": "2026-10-03T01:00:00Z", "redelivery": false, "duration": 0.1,
       "status": "OK", "status_code": 200, "event": "ping", "action": null, "installation_id": null, "repository_id": 1}
    ]"#;

    #[test]
    fn list_and_missing() {
        let l = parse_list(LIST);
        assert_eq!(l.len(), 5);
        assert_eq!(l[0].status_code, 0);
        assert_eq!(l[0].action.as_deref(), Some("opened"));
        let since = time::parse_rfc3339("2026-09-30T09:42:07Z").unwrap();
        let known: HashSet<String> = ["7c1e04b2-9a1f".to_string()].into();
        let m = missing(&l, &known, since);
        // old-one is outside the window, ping is skipped, the known one is skipped, and the
        // redelivered GUID appears once (its original attempt).
        assert_eq!(m.iter().map(|d| (d.id, d.guid.as_str())).collect::<Vec<_>>(), vec![(3, "c61b0f8e-2a95")]);
        assert!(parse_list("{\"message\":\"Not Found\"}").is_empty());
    }

    #[test]
    fn detail() {
        let j = r#"{"id": 3, "guid": "c61b0f8e-2a95", "event": "pull_request", "action": "opened",
          "request": {"headers": {"Accept": "*/*", "X-GitHub-Delivery": "c61b0f8e-2a95", "X-GitHub-Event": "pull_request",
             "X-Hub-Signature-256": "sha256=…"}, "payload": {"action": "opened", "pull_request": {"number": 230}}},
          "response": {"headers": {}, "payload": null}}"#;
        let (event, guid, payload) = parse_detail(j).unwrap();
        assert_eq!((event.as_str(), guid.as_str()), ("pull_request", "c61b0f8e-2a95"));
        assert_eq!(payload["pull_request"]["number"], 230);
        // payload as a JSON string also works
        let j2 = r#"{"request": {"headers": {"x-github-event": "push", "x-github-delivery": "g"}, "payload": "{\"ref\":\"refs/heads/main\"}"}}"#;
        assert_eq!(parse_detail(j2).unwrap().2["ref"], "refs/heads/main");
        assert!(parse_detail("{}").is_none());
    }
}
