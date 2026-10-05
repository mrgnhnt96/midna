//! The OS process table (macOS libproc + sysctl): every process under a terminal, so midna
//! sees what an agent spun up as processes (background shells, MCP servers, dev servers), not
//! only what its hooks report.
use midna_proto::{BackgroundTask, ProcessInfo, time};
use std::collections::HashMap;

pub fn all_pids() -> Vec<i32> {
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return vec![];
    }
    let mut buf = vec![0i32; n as usize + 64];
    let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut _, (buf.len() * 4) as i32) };
    buf.truncate(got.max(0) as usize);
    buf
}

struct Bsd {
    ppid: i32,
    pgid: i32,
    name: String,
    start: i64,
}

/// A process's working directory.
pub fn cwd(pid: i32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, &mut info as *mut _ as *mut libc::c_void, size) };
    if got != size {
        return None;
    }
    let bytes: Vec<u8> = info.pvi_cdir.vip_path.iter().flatten().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    Some(String::from_utf8_lossy(&bytes).into_owned()).filter(|p| p.starts_with('/'))
}

fn bsd_info(pid: i32) -> Option<Bsd> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut libc::c_void, size) };
    if got != size {
        return None;
    }
    let cstr = |b: &[libc::c_char]| {
        let bytes: Vec<u8> = b.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let name = cstr(&info.pbi_name);
    Some(Bsd {
        ppid: info.pbi_ppid as i32,
        pgid: info.pbi_pgid as i32,
        name: if name.is_empty() { cstr(&info.pbi_comm) } else { name },
        start: info.pbi_start_tvsec as i64,
    })
}

/// Parent pid, or None if `pid` is gone.
pub fn parent(pid: i32) -> Option<i32> {
    bsd_info(pid).map(|b| b.ppid)
}

/// Full argv joined by spaces (KERN_PROCARGS2), or None when the OS won't say.
pub fn command_line(pid: i32) -> Option<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) } != 0 || size < 4 {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut _, &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs2(&buf)
}

/// KERN_PROCARGS2 layout: argc (i32), the exec path, NUL padding, then argc NUL-terminated args.
fn parse_procargs2(buf: &[u8]) -> Option<String> {
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    let mut rest = &buf[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    rest = &rest[path_end..];
    let start = rest.iter().position(|&b| b != 0)?;
    rest = &rest[start..];
    let args: Vec<String> = rest.split(|&b| b == 0).take(argc).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    (!args.is_empty()).then(|| args.join(" "))
}

/// Every process under `root` (itself first, then children depth-first, oldest first).
pub fn tree(root: i32) -> Vec<ProcessInfo> {
    let mut table: HashMap<i32, Bsd> = HashMap::new();
    for pid in all_pids() {
        if pid > 0
            && let Some(b) = bsd_info(pid)
        {
            table.insert(pid, b);
        }
    }
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for (&pid, b) in &table {
        if pid != b.ppid {
            children.entry(b.ppid).or_default().push(pid);
        }
    }
    for kids in children.values_mut() {
        kids.sort_by_key(|p| (table.get(p).map(|b| b.start).unwrap_or(0), *p));
    }
    let mut out = vec![];
    let mut stack = vec![(root, 0u32)];
    while let Some((pid, depth)) = stack.pop() {
        let Some(b) = table.get(&pid) else { continue };
        out.push(ProcessInfo {
            pid,
            ppid: b.ppid,
            pgid: b.pgid,
            depth,
            name: b.name.clone(),
            command: command_line(pid).unwrap_or_default(),
            started_at: (b.start > 0).then(|| time::format_unix(b.start)),
            task_id: None,
        });
        if depth < 32
            && let Some(kids) = children.get(&pid)
        {
            stack.extend(kids.iter().rev().map(|&k| (k, depth + 1)));
        }
    }
    out
}

/// Tag each process with the agent's background shell task whose command it runs. Claude runs a
/// background command through a shell (`zsh -c '<snapshot>; eval "<command>"'`), so the shell
/// and everything under it belong to that task.
pub fn tag_tasks(procs: &mut [ProcessInfo], tasks: &[BackgroundTask]) {
    let shells: Vec<(&str, String)> = tasks
        .iter()
        .filter_map(|t| t.command.as_deref().map(|c| (t.id.as_str(), c.trim_end_matches('…').trim().to_string())))
        .filter(|(_, c)| c.len() >= 3)
        .collect();
    let mut owner: HashMap<i32, String> = HashMap::new();
    for i in 0..procs.len() {
        let p = &procs[i];
        let inherited = owner.get(&p.ppid).cloned();
        let own = shells.iter().find(|(_, c)| p.command.contains(c.as_str())).map(|(id, _)| id.to_string());
        if let Some(id) = own.or(inherited) {
            owner.insert(p.pid, id.clone());
            procs[i].task_id = Some(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn procargs2() {
        let mut b = 3i32.to_ne_bytes().to_vec();
        b.extend(b"/bin/sleep\0\0\0\0sleep\0400\0--x\0SECRET=env\0");
        assert_eq!(parse_procargs2(&b).as_deref(), Some("sleep 400 --x"));
        assert_eq!(parse_procargs2(&[1, 0]), None);
    }

    #[test]
    fn own_tree() {
        let me = std::process::id() as i32;
        let mut child = std::process::Command::new("/bin/sleep").arg("7.25").spawn().unwrap();
        let t = tree(me);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(t[0].pid, me);
        let c = t.iter().find(|p| p.pid == child.id() as i32).expect("child listed");
        assert_eq!((c.ppid, c.depth), (me, 1));
        assert!(c.command.contains("sleep 7.25"), "{c:?}");
    }

    #[test]
    fn tags_follow_the_shell() {
        let p = |pid, ppid, depth, cmd: &str| ProcessInfo { pid, ppid, pgid: 1, depth, name: String::new(), command: cmd.into(), started_at: None, task_id: None };
        let mut procs = vec![
            p(10, 1, 0, "claude"),
            p(11, 10, 1, "/bin/zsh -c source snap; eval 'npm run dev' < /dev/null"),
            p(12, 11, 2, "node vite"),
            p(13, 10, 1, "midna mcp"),
        ];
        let tasks = vec![BackgroundTask { id: "b1".into(), kind: "shell".into(), command: Some("npm run dev".into()), ..Default::default() }];
        tag_tasks(&mut procs, &tasks);
        let ids: Vec<Option<&str>> = procs.iter().map(|p| p.task_id.as_deref()).collect();
        assert_eq!(ids, vec![None, Some("b1"), Some("b1"), None]);
    }
}
