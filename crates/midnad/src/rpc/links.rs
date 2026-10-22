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
    let owner = p.owner.trim().to_string();
    if owner.is_empty() || owner.chars().count() > 64 || owner.chars().any(char::is_control) {
        return Err(RpcError::bad_params("owner must be 1–64 characters, the app's name (e.g. \"Taskboard\")"));
    }
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
