//! links.*: an agent terminal's session links (collected in `crate::links`).
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;

/// The terminal a call is about: `session`, else the caller's own.
fn session(d: &Daemon, ctx: &Ctx, sid: Option<Id>) -> Result<Id, RpcError> {
    let sid = sid.or(ctx.session.clone()).ok_or_else(|| RpcError::bad_params("no session: pass `session` or run inside a midna terminal"))?;
    if d.core().state.session(&sid).is_none() {
        return Err(RpcError::not_found(format!("no terminal {sid}")));
    }
    Ok(sid)
}

pub fn list(d: &Daemon, ctx: &Ctx, p: LinksListParams) -> R {
    let sid = session(d, ctx, p.session)?;
    let turn = match &p.turn {
        None => None,
        Some(TurnRef::N(n)) => Some(*n),
        Some(TurnRef::Name(s)) => match s.trim() {
            "last" | "latest" => Some(crate::links::prompt_times(d, &sid).len() as u32).filter(|n| *n > 0),
            n => Some(n.parse::<u32>().map_err(|_| RpcError::bad_params(format!("turn must be a prompt number or \"last\", not `{n}`")))?),
        },
    };
    let links = d
        .links
        .list(&d.cfg.home, &sid)
        .into_iter()
        .filter(|l| p.kind.is_none_or(|k| l.kind == k) && (!p.pinned || l.pinned) && (p.turn.is_none() || turn.is_some_and(|t| l.turns.contains(&t))))
        .collect();
    ok(LinksListResult { session: sid, turn, links })
}

pub fn pin(d: &Daemon, ctx: &Ctx, p: LinksPinParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(crate::links::pin(d, &sid, &p.link, p.pinned, ctx.actor())?)
}

pub fn remove(d: &Daemon, ctx: &Ctx, p: LinksRemoveParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(if p.restore { crate::links::restore(d, &sid, &p.link)? } else { crate::links::remove(d, &sid, &p.link)? })
}

pub fn add(d: &Daemon, ctx: &Ctx, p: LinksAddParams) -> R {
    let sid = session(d, ctx, p.session.clone())?;
    ok(crate::links::add(d, &sid, &p, ctx.actor())?)
}

/// At most this many apps, and this many rules per app.
const MAX_OWNERS: usize = 32;
const MAX_APP_PATTERNS: usize = 64;

pub fn patterns_list(d: &Daemon) -> R {
    let core = d.core();
    let apps = core.state.app_link_patterns.iter().map(|(owner, patterns)| AppLinkPatterns { owner: owner.clone(), patterns: patterns.clone() }).collect();
    ok(LinkPatternsList { apps })
}

/// Replace an app's link patterns wholesale; an empty list removes them.
pub fn patterns_register(d: &Daemon, ctx: &Ctx, p: LinkPatternsRegisterParams) -> R {
    let owner = owner_name(&p.owner)?;
    if p.patterns.len() > MAX_APP_PATTERNS {
        return Err(RpcError::bad_params(format!("at most {MAX_APP_PATTERNS} patterns per app")));
    }
    // Check every rule before storing any, so a bad one leaves the old set in place.
    let patterns: Vec<String> = p.patterns.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
    for r in &patterns {
        midna_proto::link_patterns::LinkPattern::parse(r).map_err(|e| RpcError::bad_params(format!("rule `{r}`: {e}")))?;
    }
    {
        let mut core = d.core();
        let apps = &mut core.state.app_link_patterns;
        if patterns.is_empty() {
            apps.remove(&owner);
        } else {
            if !apps.contains_key(&owner) && apps.len() >= MAX_OWNERS {
                return Err(RpcError::bad_params(format!("at most {MAX_OWNERS} apps can register link patterns")));
            }
            apps.insert(owner.clone(), patterns.clone());
        }
    }
    d.mark_dirty();
    d.emit(kinds::LINK_PATTERNS_CHANGED, ctx.actor(), None, None, json!({ "owner": owner, "count": patterns.len() }));
    ok(AppLinkPatterns { owner, patterns })
}

/// How long a port's answer is reused: a menu opens it on every click of the same link.
const SERVED_BY_TTL: std::time::Duration = std::time::Duration::from_secs(3);

pub fn served_by(d: &Daemon, p: LinksServedByParams) -> R {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    use std::time::Instant;
    static CACHE: LazyLock<Mutex<HashMap<u16, (Instant, Option<ServedBy>)>>> = LazyLock::new(Default::default);
    let port = local_port(&p.url).ok_or_else(|| RpcError::bad_params(format!("not an http(s) link to this Mac: {}", p.url)))?;
    if let Some((at, hit)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&port) {
        if at.elapsed() < SERVED_BY_TTL {
            return ok(hit.clone());
        }
    }
    let found = find_server(d, port);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|_, (at, _)| at.elapsed() < SERVED_BY_TTL);
    cache.insert(port, (Instant::now(), found.clone()));
    ok(found)
}

/// The terminal whose process tree holds a listener on `port`.
fn find_server(d: &Daemon, port: u16) -> Option<ServedBy> {
    let roots: std::collections::HashMap<i32, (Id, String)> = {
        let core = d.core();
        core.rt
            .iter()
            .filter(|(_, rt)| rt.pid > 1)
            .filter_map(|(id, rt)| {
                let s = core.state.session(id)?;
                Some((rt.pid, (id.clone(), if s.name.is_empty() { s.title.clone() } else { s.name.clone() })))
            })
            .collect()
    };
    if roots.is_empty() {
        return None;
    }
    crate::procs::listening_pids(port).into_iter().find_map(|pid| {
        let mut at = pid;
        for _ in 0..64 {
            if let Some((session, name)) = roots.get(&at) {
                let pname = crate::procs::name(pid).unwrap_or_default();
                let command = crate::procs::server_name(&pname, &crate::procs::command_line(pid).unwrap_or_default());
                return Some(ServedBy { session: session.clone(), name: name.clone(), command, pid, port });
            }
            at = crate::procs::parent(at).filter(|&p| p > 1)?;
        }
        None
    })
}

/// The port an http(s) link to this Mac points at, or None for any other link.
fn local_port(url: &str) -> Option<u16> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        (host, after.strip_prefix(':'))
    } else {
        match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    let host = host.to_ascii_lowercase();
    let local = host == "localhost" || host.ends_with(".localhost") || host == "::1" || host == "0.0.0.0" || host.split('.').next() == Some("127") && host.parse::<std::net::Ipv4Addr>().is_ok();
    if !local {
        return None;
    }
    match port {
        None | Some("") => Some(default),
        Some(p) => p.parse().ok().filter(|&p| p > 0),
    }
}

#[cfg(test)]
mod tests {
    use super::local_port;

    #[test]
    fn local_ports() {
        assert_eq!(local_port("http://localhost:5173/decks"), Some(5173));
        assert_eq!(local_port("http://127.0.0.1:8000?x=1"), Some(8000));
        assert_eq!(local_port("https://[::1]:3000/#top"), Some(3000));
        assert_eq!(local_port("http://LOCALHOST"), Some(80));
        assert_eq!(local_port("https://app.localhost/"), Some(443));
        assert_eq!(local_port("http://user:pw@0.0.0.0:4000/"), Some(4000));
        assert_eq!(local_port("http://127.1.2.3:9"), Some(9));
        assert_eq!(local_port("http://example.com:3000"), None);
        assert_eq!(local_port("http://127.example.com:3000"), None);
        assert_eq!(local_port("ftp://localhost:21"), None);
        assert_eq!(local_port("http://localhost:0"), None);
        assert_eq!(local_port("http://localhost:99999"), None);
        assert_eq!(local_port("localhost:3000"), None);
    }
}

fn owner_name(owner: &str) -> Result<String, RpcError> {
    let owner = owner.trim().to_string();
    if owner.is_empty() || owner.chars().count() > 64 || owner.chars().any(char::is_control) {
        return Err(RpcError::bad_params("owner must be 1–64 characters, the app's name (e.g. \"Taskboard\")"));
    }
    Ok(owner)
}

/// What the human approves of an app's link menus: its name and the commands they run. None
/// when they run none, so there's nothing to approve.
pub fn fingerprint(owner: &str, menus: &[AppLinkMenu]) -> Option<String> {
    use sha2::{Digest, Sha256};
    let commands = midna_proto::link_actions::commands(menus);
    if commands.is_empty() {
        return None;
    }
    let text = format!("{owner}\n{}", commands.join("\n"));
    Some(crate::webhooks::sig::hex_encode(&Sha256::digest(text.as_bytes())))
}

fn app_actions(state: &crate::state::State, owner: &str, a: &crate::state::AppActions) -> AppLinkActions {
    let fp = fingerprint(owner, &a.menus);
    let approved = fp.is_none() || a.approved == fp;
    let open = |id: &Id| state.needs_you.iter().any(|n| n.id == *id);
    let approval = a.asked.as_ref().filter(|(f, id)| !approved && Some(f) == fp.as_ref() && open(id)).map(|(_, id)| id.clone());
    AppLinkActions { owner: owner.to_string(), menus: a.menus.clone(), approved, approval }
}

pub fn actions_list(d: &Daemon) -> R {
    let core = d.core();
    let apps = core.state.app_link_actions.iter().map(|(owner, a)| app_actions(&core.state, owner, a)).collect();
    ok(LinkActionsList { apps })
}

/// Replace an app's link menus wholesale; an empty list removes them and forgets the approval.
/// Commands not yet approved ask the human once per set: the same commands again keep the
/// approval (or the open ask), changed ones take back the old ask and raise a new one.
pub fn actions_register(d: &Daemon, ctx: &Ctx, p: LinkActionsRegisterParams) -> R {
    let owner = owner_name(&p.owner)?;
    midna_proto::link_actions::validate(&p.menus).map_err(RpcError::bad_params)?;
    let fp = fingerprint(&owner, &p.menus);
    let (stale, ask) = {
        let mut core = d.core();
        let apps = &mut core.state.app_link_actions;
        if p.menus.is_empty() {
            (apps.remove(&owner).and_then(|a| a.asked).map(|(_, id)| id), false)
        } else {
            if !apps.contains_key(&owner) && apps.len() >= MAX_OWNERS {
                return Err(RpcError::bad_params(format!("at most {MAX_OWNERS} apps can register link menus")));
            }
            let a = apps.entry(owner.clone()).or_default();
            a.menus = p.menus.clone();
            let asked_now = a.asked.as_ref().is_some_and(|(f, _)| Some(f) == fp.as_ref());
            let stale = if asked_now { None } else { a.asked.take().map(|(_, id)| id) };
            (stale, fp.is_some() && a.approved != fp && !asked_now)
        }
    };
    if let Some(id) = stale {
        let why = if p.menus.is_empty() { "the app removed its link menus" } else { "the app changed its link menus' commands" };
        d.withdraw_needs_you(&id, why);
    }
    if let (true, Some(fp)) = (ask, &fp) {
        let id = ask_approval(d, ctx, &owner, &p.menus, fp);
        if let Some(a) = d.core().state.app_link_actions.get_mut(&owner) {
            a.asked = Some((fp.clone(), id));
        }
    }
    d.mark_dirty();
    let view = {
        let core = d.core();
        core.state.app_link_actions.get(&owner).map(|a| app_actions(&core.state, &owner, a))
    };
    let view = view.unwrap_or(AppLinkActions { owner: owner.clone(), menus: vec![], approved: true, approval: None });
    d.emit(kinds::LINK_ACTIONS_CHANGED, ctx.actor(), None, None, json!({ "owner": owner, "count": view.menus.len(), "approved": view.approved }));
    ok(view)
}

/// The needs-you that asks the human to approve an app's link menu commands; approving it runs
/// `links.actions.approve` for exactly these.
fn ask_approval(d: &Daemon, ctx: &Ctx, owner: &str, menus: &[AppLinkMenu], fp: &str) -> Id {
    let mut item = d.new_needs_you(NeedsYouKind::Approval, format!("{owner} wants to add commands to the link menu"), ctx.actor(), ctx.session.clone());
    let commands: Vec<String> = midna_proto::link_actions::commands(menus).iter().map(|c| format!("  {c}")).collect();
    item.detail = format!(
        "Approving lets {owner}'s link menu items run these, with the clicked link's text in place of $1 and the like:\n{}\n\
         Until then they stay hidden; its open and copy items work already. It asks again whenever these change.",
        commands.join("\n")
    );
    let cli = format!("links.actions.approve {owner}");
    item.approval = Some(ApprovalRequest {
        action: PolicyAction { kind: ActionKind::Cli, value: cli, session: ctx.session.clone(), project: item.project_id.clone() },
        matched_rule: None,
        target_session: None,
    });
    let params = json!({ "owner": owner, "fingerprint": fp });
    d.core().state.deferred.insert(item.id.clone(), crate::state::Deferred { method: "links.actions.approve".into(), params, guard: Some(fp.to_string()) });
    d.raise_needs_you(item).id
}

pub fn actions_approve(d: &Daemon, ctx: &Ctx, p: LinkActionsApproveParams) -> R {
    let view = {
        let mut core = d.core();
        let a = core.state.app_link_actions.get_mut(&p.owner).ok_or_else(|| RpcError::not_found(format!("{} has no link menus", p.owner)))?;
        if fingerprint(&p.owner, &a.menus).as_deref() != Some(p.fingerprint.as_str()) {
            return Err(RpcError::conflict(format!("{}'s link menu commands changed since this was asked; it asks again", p.owner)));
        }
        a.approved = Some(p.fingerprint.clone());
        let a = a.clone();
        app_actions(&core.state, &p.owner, &a)
    };
    d.mark_dirty();
    d.emit(kinds::LINK_ACTIONS_CHANGED, ctx.actor(), None, None, json!({ "owner": p.owner, "count": view.menus.len(), "approved": true }));
    ok(view)
}
