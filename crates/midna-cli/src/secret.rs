//! `midna secret …`: use the secrets the human pasted (`[secret:NAME]`) without seeing them.
//! `exec` sets them in a command's environment and masks them in its output; `write` puts one
//! in a .env file. Nothing here prints a value.
use crate::args::Args;
use crate::print::{plain, table};
use crate::{Fail, OutFn, Res, call, connect};
use midna_proto::secrets::Scrubber;
use serde_json::{Value, json};
use std::io::{Read, Write};

fn cwd() -> Option<String> {
    std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned())
}

pub fn secret(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            a.check(&["all", "json"])?;
            out(&call("secret.list", json!({ "all": a.has("all"), "cwd": cwd() }))?, &print_list);
            Ok(())
        }
        "exec" | "run" => exec(a),
        "write" => {
            a.check(&["as", "json"])?;
            let name = a.need(2, "secret name")?;
            let file = a.need(3, "file to write (e.g. .env)")?;
            let path = std::env::current_dir().map_err(|e| Fail::Other(e.to_string()))?.join(file);
            let v = call("secret.write", json!({ "name": name, "path": path.to_string_lossy(), "key": a.get("as"), "cwd": cwd() }))?;
            out(&v, &|v| {
                let how = if v["replaced"].as_bool() == Some(true) { "replaced" } else { "added" };
                println!("{how} {} in {} (don't read the file back; that would show you the value)", plain(&v["key"]), plain(&v["path"]));
            });
            Ok(())
        }
        "rm" | "remove" => {
            let name = a.need(2, "secret name")?;
            call("secret.remove", json!({ "name": name, "cwd": cwd() }))?;
            out(&json!({ "ok": true }), &|_| println!("removed {name}"));
            Ok(())
        }
        "save" | "set" | "add" => save(a, out),
        other => Err(Fail::Usage(format!("unknown `secret {other}` (list | save | exec | write | rm)"))),
    }
}

/// `<command> | midna secret save NAME`: the value comes from stdin only, so it never shows
/// up in a command line, a transcript or `ps`.
fn save(a: &Args, out: OutFn) -> Res {
    a.check(&["label", "global", "json"])?;
    let name = a.need(2, "secret name (e.g. GH_TOKEN)")?;
    if a.pos.len() > 3 {
        return Err(Fail::Usage("the value goes on stdin, never as an argument: `<command> | midna secret save NAME`".into()));
    }
    let stdin = std::io::stdin();
    if std::io::IsTerminal::is_terminal(&stdin) {
        return Err(Fail::Usage("pipe the value in: `<command> | midna secret save NAME` (e.g. `gh auth token | midna secret save GH_TOKEN`)".into()));
    }
    let mut value = String::new();
    stdin.lock().take((64 << 10) + 1).read_to_string(&mut value).map_err(|e| Fail::Other(format!("reading stdin: {e}")))?;
    // One trailing newline is the command's, not the secret's.
    let value = value.strip_suffix('\n').map(|v| v.strip_suffix('\r').unwrap_or(v)).unwrap_or(&value);
    if value.is_empty() {
        return Err(Fail::Usage("nothing on stdin".into()));
    }
    let v = call("secret.set", json!({ "name": name, "value": value, "label": a.get("label"), "global": a.has("global"), "piped": true, "cwd": cwd() }))?;
    out(&v, &|v| {
        let scope = v["project_id"].as_str().map(|p| format!("project {p}")).unwrap_or_else(|| "every project".into());
        println!("saved {} for {scope}; use it as [secret:{}] with `midna secret exec {} -- <command>`", plain(&v["name"]), plain(&v["name"]), plain(&v["name"]));
    });
    Ok(())
}

fn print_list(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no secrets stored; the human stores one by pasting it into an agent terminal");
        return;
    }
    let mut rows = vec![vec!["NAME".into(), "PROJECT".into(), "WHAT".into(), "BY".into(), "USED".into()]];
    for s in list {
        let project = s["project_id"].as_str().unwrap_or("all").to_string();
        let mut by = s["added_by"]["kind"].as_str().unwrap_or("human").to_string();
        if s["exposed"].as_bool() == Some(true) {
            by.push_str(" (exposed)");
        }
        rows.push(vec![plain(&s["name"]), project, s["label"].as_str().unwrap_or("").to_string(), by, plain(&s["used"])]);
    }
    table(rows);
}

/// `exec NAME[,…] [VAR=NAME …] -- cmd…`: (env var, secret name) pairs from the positionals.
fn wanted(a: &Args) -> Result<Vec<(String, String)>, Fail> {
    let mut out = vec![];
    for spec in a.pos.iter().skip(2).flat_map(|p| p.split(',')).filter(|p| !p.is_empty()) {
        let (var, name) = spec.split_once('=').unwrap_or((spec, spec));
        if !midna_proto::secrets::valid_name(var) {
            return Err(Fail::Usage(format!("bad variable name {var:?}")));
        }
        out.push((var.to_string(), name.to_string()));
    }
    if out.is_empty() {
        return Err(Fail::Usage("which secret? `midna secret exec NAME -- <command>`".into()));
    }
    Ok(out)
}

fn exec(a: &Args) -> Res {
    let pairs = wanted(a)?;
    let Some((prog, args)) = a.rest.split_first() else {
        return Err(Fail::Usage("missing the command: `midna secret exec NAME -- <command…>`".into()));
    };
    let mut names: Vec<String> = pairs.iter().map(|(_, n)| n.clone()).collect();
    names.dedup();
    let v = connect()?.call_value("secret.exec_env", json!({ "names": names, "command": a.rest, "cwd": cwd() }))?;
    let values: std::collections::BTreeMap<String, String> = serde_json::from_value(v["values"].clone()).unwrap_or_default();
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    for (var, name) in &pairs {
        if let Some(v) = values.get(name) {
            cmd.env(var, v);
        }
    }
    let mut child = cmd.spawn().map_err(|e| Fail::Other(format!("{prog}: {e}")))?;
    let pump = |src: Box<dyn Read + Send>, to_err: bool| {
        let values = values.clone();
        std::thread::spawn(move || {
            let mut scrub = Scrubber::new(values.iter().map(|(n, v)| (n.as_str(), v.as_bytes())));
            let mut src = src;
            let mut buf = [0u8; 16 << 10];
            let write = |b: &[u8]| {
                if to_err {
                    let mut e = std::io::stderr().lock();
                    let _ = e.write_all(b).and_then(|_| e.flush());
                } else {
                    let mut o = std::io::stdout().lock();
                    let _ = o.write_all(b).and_then(|_| o.flush());
                }
            };
            loop {
                match src.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => write(&scrub.feed(&buf[..n])),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            write(&scrub.finish());
        })
    };
    let out_t = child.stdout.take().map(|s| pump(Box::new(s), false));
    let err_t = child.stderr.take().map(|s| pump(Box::new(s), true));
    let status = child.wait().map_err(|e| Fail::Other(e.to_string()))?;
    for t in [out_t, err_t].into_iter().flatten() {
        let _ = t.join();
    }
    use std::os::unix::process::ExitStatusExt;
    std::process::exit(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}
