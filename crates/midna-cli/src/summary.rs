//! `midna summary …`: the terminals that closed in a project lately, and apps' panels on them.
use crate::args::Args;
use crate::print::plain;
use crate::{Fail, OutFn, Res, call};
use serde_json::{Value, json};
use std::io::Read;

fn s(v: &Value, k: &str) -> String {
    plain(v.get(k).unwrap_or(&Value::Null))
}

/// Who closed it, in a few words.
fn closed_by(c: &Value) -> String {
    if c["how"] == "exited" {
        return "its process exited".into();
    }
    match (c["by"]["kind"].as_str(), c["by"]["session"].as_str()) {
        (Some("human"), _) => "you".into(),
        (Some("agent"), Some(sid)) if sid == c["id"] => "its agent".into(),
        (Some("agent"), Some(sid)) => format!("terminal {sid}"),
        (Some("agent"), None) => "an agent".into(),
        _ => "midna".into(),
    }
}

fn print_list(v: &Value) {
    let list = v["closed"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("nothing closed in the last {} days", s(v, "keep_days"));
    }
    for c in list {
        let agent = c["agent"].as_str().unwrap_or("shell");
        println!("{}  {}  ({agent}, {}, closed {} by {})", s(&c, "id"), s(&c, "name"), s(&c, "state"), crate::guide::ago(c["closed_at"].as_str().unwrap_or("")), closed_by(&c));
        let place = [c["worktree"].as_str().map(|w| format!("worktree {w}")), c["branch"].as_str().map(|b| format!("branch {b}"))];
        let place: Vec<String> = place.into_iter().flatten().collect();
        if !place.is_empty() {
            println!("    {}", place.join(", "));
        }
        if let Some(p) = c["prompts"].as_array().and_then(|p| p.first()) {
            println!("    first prompt: {}", s(p, "text").lines().next().unwrap_or(""));
        }
        for (owner, panel) in c["apps"].as_object().into_iter().flatten() {
            let badge = panel["badge"].as_str().map(|b| format!(" [{b}]")).unwrap_or_default();
            println!("    {owner}: {}{badge}", s(panel, "title"));
        }
    }
}

pub fn summary(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            a.check(&["project"])?;
            let v = call("summary.list", json!({ "project_id": a.get("project") }))?;
            out(&v, &print_list);
        }
        "remove" | "rm" => {
            a.check(&["project"])?;
            let id = a.pos.get(2);
            if id.is_none() && a.get("project").is_none() {
                return Err(Fail::Usage("summary remove needs a closed terminal's id, or --project <id|name> to clear that project's Summary".into()));
            }
            let v = call("summary.remove", json!({ "id": id, "project_id": a.get("project") }))?;
            out(&v, &|v| println!("removed {}", s(v, "removed")));
        }
        "resume" => {
            a.check(&[])?;
            let id = a.need(2, "closed terminal id")?;
            let v = call("summary.resume", json!({ "id": id }))?;
            out(&v, &|v| println!("opened {}  {}", s(&v["session"], "id"), s(&v["session"], "name")));
        }
        "annotate" => {
            a.check(&["owner", "file", "clear"])?;
            let id = a.need(2, "terminal id (running or closed)")?;
            let owner = a.get("owner").ok_or_else(|| Fail::Usage("summary annotate needs --owner <your app's name>".into()))?;
            let panel = if a.has("clear") {
                Value::Null
            } else {
                let file = a.get("file").ok_or_else(|| Fail::Usage("summary annotate needs --file panel.json (`-` reads stdin), or --clear".into()))?;
                let text = if file == "-" {
                    let mut t = String::new();
                    std::io::stdin().read_to_string(&mut t).map_err(|e| Fail::Usage(format!("reading stdin: {e}")))?;
                    t
                } else {
                    std::fs::read_to_string(file).map_err(|e| Fail::Usage(format!("reading {file}: {e}")))?
                };
                serde_json::from_str(&text).map_err(|e| Fail::Usage(format!("{file} isn't JSON: {e}")))?
            };
            let v = call("summary.annotate", json!({ "id": id, "owner": owner, "panel": panel }))?;
            out(&v, &|v| match (panel.is_null(), v["running"] == true) {
                (true, _) => println!("removed {owner}'s panel from {id}"),
                (false, true) => println!("{id} is still running; the panel shows once it closes"),
                (false, false) => println!("annotated {id}"),
            });
        }
        other => return Err(Fail::Usage(format!("unknown `summary {other}`; see midna summary --help"))),
    }
    Ok(())
}
