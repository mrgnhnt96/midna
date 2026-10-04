//! Append-only event log with monotonically increasing `seq`, fan-out to subscribed
//! connections, and a bounded in-memory window.
//!
//! **Files.** `events.jsonl` holds the current (UTC) month. When the month changes it is moved
//! to `events/YYYY-MM.jsonl` and a fresh file starts. A legacy single file that spans several
//! months is split into monthly archives when the log opens. Nothing is ever deleted: the
//! archives are the audit trail.
//!
//! **Memory.** Only the last [`RETAIN_DAYS`] days (at most [`MAX_EVENTS`] events) stay in
//! memory, for insights (month range + the previous month for deltas) and live replay. Reads
//! that reach further back (`events.list` / `events.subscribe` with an old `since_seq`) are
//! served from the files.
use crate::conn::{Out, OutTx};
use midna_proto::*;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Days kept in memory: insights' month range (30 days) plus the period before it.
pub const RETAIN_DAYS: i64 = 62;
/// Hard cap on in-memory events, whatever their age.
pub const MAX_EVENTS: usize = 200_000;
/// Trim the window every this many appends.
const TRIM_EVERY: u64 = 1024;

struct Sub {
    tx: OutTx,
    filter: EventFilter,
}

struct Inner {
    path: PathBuf,
    dir: PathBuf,
    file: std::fs::File,
    /// `YYYY-MM` of the events in `events.jsonl`.
    file_month: String,
    seq: u64,
    /// The in-memory window, oldest first.
    events: Vec<Event>,
    subs: Vec<Sub>,
    appended: u64,
    retain_secs: i64,
    max_events: usize,
}

pub struct EventLog {
    inner: Mutex<Inner>,
}

fn notification(e: &Event) -> String {
    json!({ "jsonrpc": "2.0", "method": "event", "params": e }).to_string()
}

fn month_of(at: &str) -> String {
    at.get(..7).unwrap_or("0000-00").to_string()
}

fn read_file(p: &Path) -> Vec<Event> {
    let Ok(f) = std::fs::File::open(p) else { return vec![] };
    // A torn last line (crash mid-write) is skipped, not fatal.
    std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str::<Event>(&l).ok()).collect()
}

fn append_lines(p: &Path, events: &[Event]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    let mut buf = String::new();
    for e in events {
        buf.push_str(&serde_json::to_string(e).unwrap_or_default());
        buf.push('\n');
    }
    f.write_all(buf.as_bytes())
}

/// Monthly archives, oldest first.
fn archives(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect())
        .unwrap_or_default();
    v.sort();
    v
}

impl EventLog {
    pub fn open(path: &Path) -> std::io::Result<EventLog> {
        EventLog::open_with(path, RETAIN_DAYS * 86_400, MAX_EVENTS)
    }

    /// `open` with an explicit window (tests).
    pub fn open_with(path: &Path, retain_secs: i64, max_events: usize) -> std::io::Result<EventLog> {
        let dir = path.parent().unwrap_or(Path::new(".")).join("events");
        let this_month = month_of(&time::now_rfc3339());
        let mut current = read_file(path);
        // Compaction: events of earlier months move to their monthly archive.
        if current.iter().any(|e| month_of(&e.at) < this_month) {
            std::fs::create_dir_all(&dir)?;
            let (old, keep): (Vec<Event>, Vec<Event>) = current.into_iter().partition(|e| month_of(&e.at) < this_month);
            let mut by_month: std::collections::BTreeMap<String, Vec<Event>> = Default::default();
            for e in old {
                by_month.entry(month_of(&e.at)).or_default().push(e);
            }
            for (m, evs) in &by_month {
                append_lines(&dir.join(format!("{m}.jsonl")), evs)?;
            }
            let tmp = path.with_extension("jsonl.tmp");
            let _ = std::fs::remove_file(&tmp);
            append_lines(&tmp, &keep)?;
            std::fs::rename(&tmp, path)?;
            current = keep;
        }
        let cutoff = time::now_unix() - retain_secs;
        let cutoff_month = month_of(&time::format_unix(cutoff));
        let mut events = vec![];
        let mut seq = 0;
        let arch = archives(&dir);
        for (i, p) in arch.iter().enumerate() {
            let m = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            // Older archives are only read for the last seq when nothing newer exists.
            if m >= cutoff_month || (i + 1 == arch.len() && current.is_empty()) {
                let evs = read_file(p);
                seq = seq.max(evs.last().map(|e| e.seq).unwrap_or(0));
                events.extend(evs.into_iter().filter(|e| m >= cutoff_month && time::parse_rfc3339(&e.at).is_none_or(|t| t >= cutoff)));
            }
        }
        seq = seq.max(current.last().map(|e| e.seq).unwrap_or(0));
        events.extend(current.into_iter().filter(|e| time::parse_rfc3339(&e.at).is_none_or(|t| t >= cutoff)));
        if events.len() > max_events {
            events.drain(..events.len() - max_events);
        }
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        Ok(EventLog {
            inner: Mutex::new(Inner {
                path: path.to_path_buf(),
                dir,
                file,
                file_month: this_month,
                seq,
                events,
                subs: vec![],
                appended: 0,
                retain_secs,
                max_events,
            }),
        })
    }

    pub fn append(&self, kind: &str, actor: Actor, project_id: Option<Id>, session_id: Option<Id>, data: Value) -> Event {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.seq += 1;
        let e = Event { seq: g.seq, at: time::now_rfc3339(), kind: kind.to_string(), actor, project_id, session_id, data };
        let month = month_of(&e.at);
        if month != g.file_month {
            g.rotate(&month);
        }
        let mut line = serde_json::to_string(&e).unwrap_or_default();
        line.push('\n');
        if let Err(err) = g.file.write_all(line.as_bytes()) {
            eprintln!("midnad: events.jsonl write failed: {err}");
        }
        let note = notification(&e);
        g.subs.retain(|s| !s.filter.matches(&e) || s.tx.send(Out::Line(note.clone())).is_ok());
        g.events.push(e.clone());
        g.appended += 1;
        if g.appended.is_multiple_of(TRIM_EVERY) || g.events.len() > g.max_events {
            g.trim();
        }
        e
    }

    pub fn seq(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).seq
    }

    /// Register a subscriber; replays events after `since` first (atomically, so nothing is
    /// lost). A `since` older than the in-memory window is replayed from the files.
    pub fn subscribe(&self, since: Option<u64>, filter: EventFilter, tx: OutTx) -> u64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(since) = since {
            for e in g.older_than_window(since, usize::MAX, &filter) {
                let _ = tx.send(Out::Line(notification(&e)));
            }
            let start = g.events.partition_point(|e| e.seq <= since);
            for e in &g.events[start..] {
                if filter.matches(e) {
                    let _ = tx.send(Out::Line(notification(e)));
                }
            }
        }
        g.subs.push(Sub { tx, filter });
        g.seq
    }

    /// Events with seq > since matching the filter; the newest `limit` of them, oldest first.
    /// When the window holds fewer than `limit` matches and `since` predates it, the rest
    /// comes from the files.
    pub fn list(&self, since: u64, limit: usize, filter: &EventFilter) -> Vec<Event> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let start = g.events.partition_point(|e| e.seq <= since);
        let mut out: Vec<Event> = g.events[start..].iter().filter(|e| filter.matches(e)).cloned().collect();
        if out.len() > limit {
            out.drain(..out.len() - limit);
        } else if out.len() < limit {
            let mut older = g.older_than_window(since, limit - out.len(), filter);
            older.append(&mut out);
            out = older;
        }
        out
    }

    /// Run `f` over the in-memory window (insights).
    pub fn with_events<R>(&self, f: impl FnOnce(&[Event]) -> R) -> R {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&g.events)
    }

    /// Oldest seq still in memory (tests, diagnostics).
    pub fn window_start(&self) -> Option<u64> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).events.first().map(|e| e.seq)
    }
}

impl Inner {
    /// Move `events.jsonl` (last month) to `events/<month>.jsonl` and start a new file.
    fn rotate(&mut self, month: &str) {
        let _ = self.file.flush();
        let res = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let dest = self.dir.join(format!("{}.jsonl", self.file_month));
            if dest.exists() {
                append_lines(&dest, &read_file(&self.path))?;
                std::fs::remove_file(&self.path)?;
            } else {
                std::fs::rename(&self.path, &dest)?;
            }
            self.file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
            Ok(())
        })();
        if let Err(e) = res {
            eprintln!("midnad: rotating events.jsonl failed: {e}");
        }
        self.file_month = month.to_string();
    }

    fn trim(&mut self) {
        let cutoff = time::now_unix() - self.retain_secs;
        let mut drop = self.events.partition_point(|e| time::parse_rfc3339(&e.at).is_some_and(|t| t < cutoff));
        if self.events.len() - drop > self.max_events {
            drop = self.events.len() - self.max_events;
        }
        if drop > 0 {
            self.events.drain(..drop);
        }
    }

    /// Matching events with `since < seq < first in-memory seq`, read from the files; the
    /// newest `limit` of them, oldest first. Empty when the window covers `since`.
    fn older_than_window(&self, since: u64, limit: usize, filter: &EventFilter) -> Vec<Event> {
        let first = self.events.first().map(|e| e.seq).unwrap_or(self.seq + 1);
        if since + 1 >= first || limit == 0 {
            return vec![];
        }
        let mut out: std::collections::VecDeque<Event> = Default::default();
        let mut files = archives(&self.dir);
        files.push(self.path.clone());
        for p in files {
            for e in read_file(&p) {
                if e.seq > since && e.seq < first && filter.matches(&e) {
                    out.push_back(e);
                    if out.len() > limit {
                        out.pop_front();
                    }
                }
            }
        }
        out.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, at: &str) -> Event {
        Event { seq, at: at.into(), kind: "settings.changed".into(), actor: Actor::system(), project_id: None, session_id: None, data: json!({ "n": seq }) }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("midna-evlog-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn splits_legacy_file_into_months_and_windows_memory() {
        let home = tmp("split");
        let path = home.join("events.jsonl");
        let now = time::now_unix();
        let old = [ev(1, "2020-01-05T00:00:00Z"), ev(2, "2020-01-06T00:00:00Z"), ev(3, "2020-02-01T00:00:00Z")];
        let recent = [ev(4, &time::format_unix(now - 60)), ev(5, &time::format_unix(now - 30))];
        append_lines(&path, &old).unwrap();
        append_lines(&path, &recent).unwrap();
        let log = EventLog::open(&path).unwrap();
        // Old months are archived; the current file holds only this month.
        assert_eq!(read_file(&home.join("events/2020-01.jsonl")).len(), 2);
        assert_eq!(read_file(&home.join("events/2020-02.jsonl")).len(), 1);
        assert!(read_file(&path).iter().all(|e| e.seq >= 4));
        // Only recent events are in memory, but seq continues and old reads hit the disk.
        assert_eq!(log.window_start(), Some(4));
        assert_eq!(log.with_events(|e| e.len()), 2);
        let e = log.append("x", Actor::system(), None, None, json!({}));
        assert_eq!(e.seq, 6);
        let all = log.list(0, 100, &EventFilter::default());
        assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
        // limit keeps the newest; since cuts the oldest.
        assert_eq!(log.list(0, 3, &EventFilter::default()).iter().map(|e| e.seq).collect::<Vec<_>>(), vec![4, 5, 6]);
        assert_eq!(log.list(1, 4, &EventFilter::default()).iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5, 6]);
        // A subscriber with an old since gets the archived events replayed first.
        let (tx, rx) = crate::conn::OutTx::pair();
        log.subscribe(Some(1), EventFilter::default(), tx);
        let seqs: Vec<u64> = rx
            .try_iter()
            .filter_map(|o| match o {
                Out::Line(l) => serde_json::from_str::<Value>(&l).ok().and_then(|v| v["params"]["seq"].as_u64()),
                _ => None,
            })
            .collect();
        assert_eq!(seqs, vec![2, 3, 4, 5, 6]);
        // Reopening keeps everything and the seq.
        drop(log);
        let log = EventLog::open(&path).unwrap();
        assert_eq!(log.seq(), 6);
        assert_eq!(log.list(0, 100, &EventFilter::default()).len(), 6);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn seq_survives_when_only_archives_exist() {
        let home = tmp("onlyarch");
        let path = home.join("events.jsonl");
        append_lines(&path, &[ev(1, "2020-01-05T00:00:00Z"), ev(7, "2020-03-05T00:00:00Z")]).unwrap();
        let log = EventLog::open(&path).unwrap();
        assert_eq!(log.with_events(|e| e.len()), 0);
        assert_eq!(log.append("x", Actor::system(), None, None, json!({})).seq, 8);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cap_trims_memory_not_disk() {
        let home = tmp("cap");
        let path = home.join("events.jsonl");
        let log = EventLog::open_with(&path, RETAIN_DAYS * 86_400, 10).unwrap();
        for _ in 0..25 {
            log.append("x", Actor::system(), None, None, json!({}));
        }
        assert!(log.with_events(|e| e.len()) <= 10);
        assert_eq!(log.list(0, 1000, &EventFilter::default()).len(), 25);
        let f = EventFilter { kinds: Some(vec!["x".into()]), ..Default::default() };
        assert_eq!(log.list(3, 5, &f).first().map(|e| e.seq), Some(21));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rotates_when_the_month_changes() {
        let home = tmp("rotate");
        let path = home.join("events.jsonl");
        let log = EventLog::open(&path).unwrap();
        log.append("x", Actor::system(), None, None, json!({}));
        {
            // Pretend the open file belongs to an earlier month.
            let mut g = log.inner.lock().unwrap();
            g.file_month = "2020-05".into();
        }
        log.append("x", Actor::system(), None, None, json!({}));
        assert_eq!(read_file(&home.join("events/2020-05.jsonl")).len(), 1);
        assert_eq!(read_file(&path).len(), 1);
        assert_eq!(log.list(0, 10, &EventFilter::default()).len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }
}
