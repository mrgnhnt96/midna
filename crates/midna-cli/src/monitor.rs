//! `midna monitor …`: monitor grants, which let one terminal's agent answer other terminals'
//! prompts and approvals. Only the human gives or revokes one; an agent's call asks the human.
use crate::args::Args;
use crate::print::{monitor_grant_text, plain, table};
use crate::{Fail, OutFn, Res, call};
use midna_proto::time;
use serde_json::{Value, json};

pub fn monitor(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            a.check(&[])?;
            let v = call("session.list", json!({}))?;
            let granted: Vec<Value> = v.as_array().into_iter().flatten().filter(|s| s["monitor_grant"].is_object()).cloned().collect();
            out(&Value::Array(granted), &list);
            Ok(())
        }
        "grant" => {
            a.check(&["scope", "until", "for"])?;
            let id = a.need(2, "terminal id")?;
            let scope = a.get("scope").unwrap_or("all");
            if !matches!(scope, "prompts" | "approvals" | "all") {
                return Err(Fail::Usage(format!("--scope `{scope}`: use prompts, approvals or all")));
            }
            let until = until_from(a.get("until"), a.get("for"), time::now_unix(), time::local_offset_secs()).map_err(Fail::Usage)?;
            let mut p = json!({ "id": id, "scope": scope });
            if let Some(u) = until {
                p["until"] = json!(u);
            }
            let v = call("session.monitor_grant", p)?;
            out(&v, &|v| println!("{id} may answer other terminals' {}", monitor_grant_text(&v["monitor_grant"])));
            Ok(())
        }
        "revoke" => {
            a.check(&[])?;
            let id = a.need(2, "terminal id")?;
            let v = call("session.monitor_revoke", json!({ "id": id }))?;
            out(&v, &|_| println!("{id} has no monitor grant"));
            Ok(())
        }
        other => Err(Fail::Usage(format!("unknown monitor subcommand `{other}` (grant|revoke|list)"))),
    }
}

fn list(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no terminal has a monitor grant");
        return;
    }
    let mut rows = vec![vec!["ID".into(), "NAME".into(), "GRANT".into()]];
    for x in list {
        rows.push(vec![plain(&x["id"]), plain(&x["name"]), monitor_grant_text(&x["monitor_grant"])]);
    }
    table(rows);
}

/// `--until` (a time of day: the next time the local clock reads it, or RFC 3339) or `--for`
/// (a duration) as the RFC 3339 time the grant ends; None when neither was given.
/// `offset` is seconds east of UTC.
pub fn until_from(until: Option<&str>, dur: Option<&str>, now: i64, offset: i64) -> Result<Option<String>, String> {
    match (until, dur) {
        (Some(_), Some(_)) => Err("pick one of --until and --for".into()),
        (None, None) => Ok(None),
        (None, Some(d)) => {
            let mins = crate::queue::parse_minutes(d).map_err(|e| format!("--for {e}"))?;
            Ok(Some(time::format_unix(now + i64::from(mins) * 60)))
        }
        (Some(u), None) => {
            if let Some(t) = time::parse_rfc3339(u) {
                return if t > now { Ok(Some(time::format_unix(t))) } else { Err(format!("--until {u} has already passed")) };
            }
            let mins = midna_proto::keep_awake::parse_time(u).map_err(|e| format!("--until {e}"))?;
            let day = (now + offset).div_euclid(86_400) * 86_400 - offset;
            let t = day + i64::from(mins) * 60;
            Ok(Some(time::format_unix(if t <= now { t + 86_400 } else { t })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-23T22:30:00Z
    const NOW: i64 = 1_795_473_000;

    #[test]
    fn until_a_time_of_day_is_the_next_time_the_clock_reads_it() {
        assert_eq!(time::format_unix(NOW), "2026-11-23T22:30:00Z");
        // UTC: 2am is tomorrow, 11pm is later today
        assert_eq!(until_from(Some("2am"), None, NOW, 0), Ok(Some("2026-11-24T02:00:00Z".into())));
        assert_eq!(until_from(Some("11pm"), None, NOW, 0), Ok(Some("2026-11-23T23:00:00Z".into())));
        // 7 hours west of UTC it is 15:30 local; 2am local is 09:00Z tomorrow, 5pm local is 00:00Z
        assert_eq!(until_from(Some("2am"), None, NOW, -7 * 3600), Ok(Some("2026-11-24T09:00:00Z".into())));
        assert_eq!(until_from(Some("17:00"), None, NOW, -7 * 3600), Ok(Some("2026-11-24T00:00:00Z".into())));
    }

    #[test]
    fn until_takes_rfc3339_and_for_takes_a_duration() {
        assert_eq!(until_from(Some("2026-11-24T06:00:00-07:00"), None, NOW, 0), Ok(Some("2026-11-24T13:00:00Z".into())));
        assert!(until_from(Some("2026-11-23T06:00:00Z"), None, NOW, 0).unwrap_err().contains("already passed"));
        assert_eq!(until_from(None, Some("8h"), NOW, 0), Ok(Some("2026-11-24T06:30:00Z".into())));
        assert_eq!(until_from(None, Some("90"), NOW, 0), Ok(Some("2026-11-24T00:00:00Z".into())));
        assert_eq!(until_from(None, None, NOW, 0), Ok(None));
        assert!(until_from(Some("2am"), Some("8h"), NOW, 0).is_err());
        assert!(until_from(Some("soon"), None, NOW, 0).unwrap_err().starts_with("--until"));
    }
}
