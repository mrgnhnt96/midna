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
        "rules" => rules_cmd(a, out),
        other => Err(Fail::Usage(format!("unknown monitor subcommand `{other}` (grant|revoke|list|rules)"))),
    }
}

/// `midna monitor rules [list|add <text>|remove <n|text>|path]`: the human's rules for monitors,
/// a markdown file in midna's home (`monitor-rules.md`) that midnad puts in every monitor's
/// briefing and notes. Anyone may edit it: the human in an editor, an agent here.
fn rules_cmd(a: &Args, out: OutFn) -> Res {
    let home = call("daemon.info", json!({}))?["home"].as_str().map(str::to_string).ok_or_else(|| Fail::Usage("midnad didn't say where its home is".into()))?;
    let path = std::path::Path::new(&home).join("monitor-rules.md");
    let read = || std::fs::read_to_string(&path).unwrap_or_default();
    let write = |s: &str| std::fs::write(&path, s).map_err(|e| Fail::Usage(format!("{}: {e}", path.display())));
    let shown = path.display().to_string();
    let words = || a.pos.iter().skip(3).cloned().collect::<Vec<_>>().join(" ");
    match a.pos.get(2).map(String::as_str).unwrap_or("list") {
        "list" | "ls" | "show" => {
            a.check(&[])?;
            let text = read();
            let v = json!({ "path": shown, "text": text, "rules": rule_items(&text).into_iter().map(|(_, r)| r).collect::<Vec<_>>() });
            out(&v, &|v| match v["text"].as_str().unwrap_or("").trim() {
                "" => println!("no monitor rules yet (add one: midna monitor rules add \"<rule>\"; file {})", v["path"].as_str().unwrap_or("")),
                t => {
                    println!("{t}");
                    eprintln!("({})", v["path"].as_str().unwrap_or(""));
                }
            });
            Ok(())
        }
        "add" => {
            a.check(&[])?;
            let rule = words();
            if rule.trim().is_empty() {
                return Err(Fail::Usage("usage: midna monitor rules add \"<rule>\"".into()));
            }
            let text = add_rule(&read(), &rule);
            write(&text)?;
            out(&json!({ "path": shown, "added": rule.trim(), "text": text }), &|v| println!("added: {}", v["added"].as_str().unwrap_or("")));
            Ok(())
        }
        "remove" | "rm" => {
            a.check(&[])?;
            let which = words();
            let (text, removed) = remove_rule(&read(), &which).map_err(Fail::Usage)?;
            write(&text)?;
            out(&json!({ "path": shown, "removed": removed, "text": text }), &|v| println!("removed: {}", v["removed"].as_str().unwrap_or("")));
            Ok(())
        }
        "path" => {
            a.check(&[])?;
            out(&json!({ "path": shown }), &|v| println!("{}", v["path"].as_str().unwrap_or("")));
            Ok(())
        }
        other => Err(Fail::Usage(format!("unknown monitor rules subcommand `{other}` (list|add|remove|path)"))),
    }
}

/// The file's top-level list items, as (line index, text without the bullet).
pub fn rule_items(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, l)| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).map(|r| (i, r.trim().to_string())))
        .collect()
}

/// `text` with `rule` appended as a list item.
pub fn add_rule(text: &str, rule: &str) -> String {
    let rule = rule.trim();
    let rule = rule.strip_prefix("- ").unwrap_or(rule);
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("- {rule}\n"));
    out
}

/// `text` without the list item `which` names (its number from 1, its text, or a piece of its
/// text that only it has), and the item's text.
pub fn remove_rule(text: &str, which: &str) -> Result<(String, String), String> {
    let which = which.trim();
    let items = rule_items(text);
    if items.is_empty() {
        return Err("there are no monitor rules to remove".into());
    }
    let found: Vec<&(usize, String)> = match which.parse::<usize>() {
        Ok(n) => items.get(n.wrapping_sub(1)).into_iter().collect(),
        Err(_) if which.is_empty() => vec![],
        Err(_) => {
            let exact: Vec<_> = items.iter().filter(|(_, r)| r.eq_ignore_ascii_case(which)).collect();
            if exact.is_empty() { items.iter().filter(|(_, r)| r.to_lowercase().contains(&which.to_lowercase())).collect() } else { exact }
        }
    };
    let (line, rule) = match found.as_slice() {
        [one] => (*one).clone(),
        [] => return Err(format!("no monitor rule matches `{which}` (1-{} or part of its text; see midna monitor rules)", items.len())),
        _ => return Err(format!("`{which}` matches {} rules; give its number (midna monitor rules lists them)", found.len())),
    };
    let mut out: String = text.lines().enumerate().filter(|(i, _)| *i != line).map(|(_, l)| format!("{l}\n")).collect();
    if !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    Ok((out, rule))
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

#[cfg(test)]
mod rules_tests {
    use super::*;

    #[test]
    fn rules_add_and_remove_like_a_markdown_list() {
        let t = add_rule("", "Everything except Slack DMs");
        assert_eq!(t, "- Everything except Slack DMs\n");
        let t = add_rule("# Work Mac\n- No force pushes", "- Never merge a PR");
        assert_eq!(t, "# Work Mac\n- No force pushes\n- Never merge a PR\n");
        assert_eq!(rule_items(&t), vec![(1, "No force pushes".to_string()), (2, "Never merge a PR".to_string())]);
        // by number, by text, by a piece only one rule has
        assert_eq!(remove_rule(&t, "1").unwrap(), ("# Work Mac\n- Never merge a PR\n".to_string(), "No force pushes".to_string()));
        assert_eq!(remove_rule(&t, "never merge a pr").unwrap().0, "# Work Mac\n- No force pushes\n");
        assert_eq!(remove_rule(&t, "force").unwrap().1, "No force pushes");
        // ambiguous, missing, out of range
        assert!(remove_rule(&t, "e").unwrap_err().contains("matches 2 rules"));
        assert!(remove_rule(&t, "slack").unwrap_err().contains("no monitor rule matches"));
        assert!(remove_rule(&t, "3").is_err() && remove_rule(&t, "0").is_err());
        assert!(remove_rule("", "1").unwrap_err().contains("no monitor rules"));
    }
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
