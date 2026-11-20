//! Stream intake: what `streams.publish` lets in. Per stream it drops an id it took in the last
//! 24 hours (of its last 1000 ids), takes 60 events a minute in bursts of 20 (a token bucket),
//! and refuses everything while 1000 published events wait for the local trigger worker, so a
//! flood never blocks a connection's thread or grows midnad's memory. Every publish gets a
//! verdict, kept in the stream's ring of the last 200 (`streams.events`) and counted.
//!
//! The ids and the ring of accepted events are seeded from the event log at start, so a
//! producer that replays after a daemon restart isn't logged twice.
use midna_proto::streams::EVENTS_KEPT;
use midna_proto::{Event, StreamCounts, StreamPublishRecord, StreamRef, StreamVerdict, time};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Ids a stream remembers for dedupe, and for how long.
pub const DEDUPE_IDS: usize = 1000;
pub const DEDUPE_SECS: i64 = 24 * 60 * 60;
/// The rate limit: tokens per second and the bucket's size.
pub const RATE_PER_SEC: f64 = 1.0;
pub const BURST: f64 = 20.0;
/// Published events waiting for the local trigger worker before publishes answer busy.
pub const QUEUE_MAX: usize = 1000;
/// Streams tracked at once; past it the one published to longest ago is forgotten, so names
/// made up on the fly can't grow memory without end.
pub const MAX_STREAMS: usize = 256;

/// What [`Intake::admit`] decided.
#[derive(Debug, PartialEq)]
pub struct Admitted {
    pub verdict: StreamVerdict,
    /// The logged event: accepted, its own; duplicate, the first one's.
    pub seq: Option<u64>,
    /// Rate limited: when the next publish would be taken.
    pub retry_after: Option<Duration>,
}

#[derive(Default)]
pub struct Intake {
    book: Mutex<HashMap<String, Stream>>,
    /// Accepted events the local trigger worker hasn't taken yet.
    queued: AtomicUsize,
}

struct Stream {
    /// (id, unix time, seq) in the order taken; `seen` indexes it.
    ids: VecDeque<(String, i64, u64)>,
    seen: HashMap<String, u64>,
    tokens: f64,
    refilled: Instant,
    ring: VecDeque<StreamPublishRecord>,
    counts: StreamCounts,
    used: Instant,
}

impl Stream {
    fn new(now: Instant) -> Stream {
        Stream { ids: VecDeque::new(), seen: HashMap::new(), tokens: BURST, refilled: now, ring: VecDeque::new(), counts: StreamCounts::default(), used: now }
    }

    /// Forget ids past the window or the count.
    fn prune(&mut self, now_unix: i64) {
        while let Some((id, at, _)) = self.ids.front() {
            if self.ids.len() <= DEDUPE_IDS && now_unix - at < DEDUPE_SECS {
                break;
            }
            self.seen.remove(id);
            self.ids.pop_front();
        }
    }

    fn remember(&mut self, id: &str, at: i64, seq: u64) {
        self.ids.push_back((id.to_string(), at, seq));
        self.seen.insert(id.to_string(), seq);
    }

    /// Refill the bucket, then take a token if there is one; else how long until there is.
    fn take_token(&mut self, now: Instant) -> Result<(), Duration> {
        let secs = now.saturating_duration_since(self.refilled).as_secs_f64();
        self.tokens = (self.tokens + secs * RATE_PER_SEC).min(BURST);
        self.refilled = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - self.tokens) / RATE_PER_SEC))
        }
    }

    fn record(&mut self, r: StreamPublishRecord) {
        self.counts.add(r.verdict);
        self.push(r);
    }

    fn push(&mut self, r: StreamPublishRecord) {
        if self.ring.len() >= EVENTS_KEPT {
            self.ring.pop_front();
        }
        self.ring.push_back(r);
    }
}

impl Intake {
    fn book(&self) -> MutexGuard<'_, HashMap<String, Stream>> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Decide on a valid publish and, when it is accepted, log it with `append` (called with
    /// the intake locked, so two publishes of one id can't both get in).
    pub fn admit(&self, s: &StreamRef, append: impl FnOnce() -> Event) -> Admitted {
        self.admit_at(s, Instant::now(), time::now_unix(), append)
    }

    fn admit_at(&self, s: &StreamRef, now: Instant, now_unix: i64, append: impl FnOnce() -> Event) -> Admitted {
        let mut book = self.book();
        let st = stream_mut(&mut book, &s.name, now);
        st.used = now;
        st.prune(now_unix);
        let record = |verdict, seq, detail: Option<String>| StreamPublishRecord {
            at: time::format_unix(now_unix),
            kind: s.kind.clone(),
            id: s.id.clone(),
            verdict,
            seq,
            detail,
        };
        let dup = s.id.as_ref().and_then(|id| st.seen.get(id).copied());
        let (verdict, seq, retry_after, detail) = if let Some(seq) = dup {
            (StreamVerdict::Duplicate, Some(seq), None, Some(format!("same id as event {seq}")))
        } else if self.queued.load(Ordering::Acquire) >= QUEUE_MAX {
            (StreamVerdict::Busy, None, None, Some(format!("{QUEUE_MAX} events are waiting for the trigger worker")))
        } else {
            match st.take_token(now) {
                Err(wait) => (StreamVerdict::RateLimited, None, Some(wait), Some(format!("over {} a minute (bursts of {BURST})", RATE_PER_SEC * 60.0))),
                Ok(()) => {
                    self.queued.fetch_add(1, Ordering::AcqRel);
                    let seq = append().seq;
                    if let Some(id) = &s.id {
                        st.remember(id, now_unix, seq);
                    }
                    (StreamVerdict::Accepted, Some(seq), None, None)
                }
            }
        };
        st.record(record(verdict, seq, detail));
        Admitted { verdict, seq, retry_after }
    }

    /// A publish dropped before it got this far (a payload over 64 KB).
    pub fn reject(&self, stream: &str, kind: &str, id: Option<String>, verdict: StreamVerdict, detail: String) {
        let now = Instant::now();
        let mut book = self.book();
        let st = stream_mut(&mut book, stream, now);
        st.used = now;
        st.record(StreamPublishRecord { at: time::now_rfc3339(), kind: kind.into(), id, verdict, seq: None, detail: Some(detail) });
    }

    /// The local trigger worker took an accepted event off the queue.
    pub fn taken(&self) {
        let _ = self.queued.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }

    /// Accepted events still waiting for the trigger worker.
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    /// A stream's recent publishes, newest first, and its counts.
    pub fn events(&self, stream: &str, limit: usize) -> (Vec<StreamPublishRecord>, StreamCounts) {
        let book = self.book();
        let Some(st) = book.get(stream) else { return (vec![], StreamCounts::default()) };
        (st.ring.iter().rev().take(limit).cloned().collect(), st.counts.clone())
    }

    /// Seed ids and accepted events from the log (oldest first), at daemon start.
    pub fn seed(&self, events: &[Event]) {
        let now = Instant::now();
        let now_unix = time::now_unix();
        let mut book = self.book();
        for e in events {
            let Some(s) = &e.stream else { continue };
            let st = stream_mut(&mut book, &s.name, now);
            let at = time::parse_rfc3339(&e.at).unwrap_or(0);
            if let Some(id) = &s.id
                && now_unix - at < DEDUPE_SECS
                && !st.seen.contains_key(id)
            {
                st.remember(id, at, e.seq);
            }
            st.push(StreamPublishRecord { at: e.at.clone(), kind: s.kind.clone(), id: s.id.clone(), verdict: StreamVerdict::Accepted, seq: Some(e.seq), detail: None });
        }
        for st in book.values_mut() {
            st.prune(now_unix);
        }
    }
}

/// The stream's book, made (and the longest-unused one forgotten past [`MAX_STREAMS`]) if new.
fn stream_mut<'a>(book: &'a mut HashMap<String, Stream>, name: &str, now: Instant) -> &'a mut Stream {
    if !book.contains_key(name) && book.len() >= MAX_STREAMS
        && let Some(oldest) = book.iter().min_by_key(|(_, s)| s.used).map(|(k, _)| k.clone())
    {
        book.remove(&oldest);
    }
    book.entry(name.to_string()).or_insert_with(|| Stream::new(now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use midna_proto::Actor;
    use serde_json::json;
    use std::cell::Cell;

    fn sref(name: &str, id: Option<&str>) -> StreamRef {
        StreamRef { name: name.into(), kind: "message".into(), id: id.map(Into::into), caused_by: None }
    }

    fn ev(seq: u64, at: &str, s: StreamRef) -> Event {
        Event { seq, at: at.into(), kind: s.event_kind(), actor: Actor::system(), project_id: None, session_id: None, data: json!({}), stream: Some(Box::new(s)) }
    }

    /// Admit at `now` (+`secs`), logging accepted events with rising seqs.
    struct T {
        i: Intake,
        t0: Instant,
        seq: Cell<u64>,
    }

    impl T {
        fn new() -> T {
            T { i: Intake::default(), t0: Instant::now(), seq: Cell::new(0) }
        }
        fn at(&self, secs: f64, s: &StreamRef) -> Admitted {
            let now = self.t0 + Duration::from_secs_f64(secs);
            self.i.admit_at(s, now, 1_700_000_000 + secs as i64, || {
                self.seq.set(self.seq.get() + 1);
                ev(self.seq.get(), "2023-11-14T22:13:20Z", s.clone())
            })
        }
    }

    #[test]
    fn a_repeated_id_is_a_duplicate_of_the_first_event() {
        let t = T::new();
        let a = t.at(0.0, &sref("slack", Some("1700.1")));
        assert_eq!((a.verdict, a.seq), (StreamVerdict::Accepted, Some(1)));
        let d = t.at(1.0, &sref("slack", Some("1700.1")));
        assert_eq!((d.verdict, d.seq), (StreamVerdict::Duplicate, Some(1)));
        assert_eq!(t.seq.get(), 1, "a duplicate isn't logged");
        // Another stream, or no id, isn't a duplicate.
        assert_eq!(t.at(2.0, &sref("ci", Some("1700.1"))).verdict, StreamVerdict::Accepted);
        assert_eq!(t.at(3.0, &sref("slack", None)).verdict, StreamVerdict::Accepted);
        assert_eq!(t.at(4.0, &sref("slack", None)).verdict, StreamVerdict::Accepted);
        // After 24 hours the id is forgotten.
        assert_eq!(t.at(DEDUPE_SECS as f64, &sref("slack", Some("1700.1"))).verdict, StreamVerdict::Accepted);
    }

    #[test]
    fn dedupe_keeps_the_last_thousand_ids() {
        let mut st = Stream::new(Instant::now());
        for n in 0..=DEDUPE_IDS {
            st.remember(&n.to_string(), 100, n as u64);
            st.prune(100);
        }
        assert_eq!(st.ids.len(), DEDUPE_IDS);
        assert!(!st.seen.contains_key("0") && st.seen.contains_key("1") && st.seen.contains_key(&DEDUPE_IDS.to_string()));
    }

    #[test]
    fn the_rate_limit_takes_a_burst_of_20_then_one_a_second() {
        let t = T::new();
        let s = sref("slack", None);
        for n in 0..20 {
            assert_eq!(t.at(0.0, &s).verdict, StreamVerdict::Accepted, "burst {n}");
        }
        let over = t.at(0.0, &s);
        assert_eq!(over.verdict, StreamVerdict::RateLimited);
        assert!(over.retry_after.is_some_and(|w| w <= Duration::from_secs(1) && w > Duration::from_millis(900)), "{over:?}");
        assert_eq!(t.at(0.5, &s).verdict, StreamVerdict::RateLimited);
        assert_eq!(t.at(1.0, &s).verdict, StreamVerdict::Accepted, "a token a second");
        assert_eq!(t.at(1.0, &s).verdict, StreamVerdict::RateLimited);
        // Another stream has its own bucket.
        assert_eq!(t.at(1.0, &sref("ci", None)).verdict, StreamVerdict::Accepted);
        // A rate-limited id isn't remembered, so its retry gets in.
        assert_eq!(t.at(1.0, &sref("slack", Some("x"))).verdict, StreamVerdict::RateLimited);
        assert_eq!(t.at(2.0, &sref("slack", Some("x"))).verdict, StreamVerdict::Accepted);
        // The bucket refills to 20, no further.
        for _ in 0..20 {
            assert_eq!(t.at(1000.0, &s).verdict, StreamVerdict::Accepted);
        }
        assert_eq!(t.at(1000.0, &s).verdict, StreamVerdict::RateLimited);
        let (_, c) = t.i.events("slack", 200);
        assert_eq!((c.rate_limited, c.dropped()), (5, 5), "{c:?}");
    }

    #[test]
    fn a_full_queue_answers_busy_until_the_worker_takes_events() {
        let t = T::new();
        t.i.queued.store(QUEUE_MAX - 1, Ordering::Release);
        assert_eq!(t.at(0.0, &sref("a", None)).verdict, StreamVerdict::Accepted);
        assert_eq!(t.i.queued(), QUEUE_MAX);
        let b = t.at(0.0, &sref("b", Some("1")));
        assert_eq!((b.verdict, b.seq), (StreamVerdict::Busy, None));
        assert_eq!(t.seq.get(), 1, "a busy publish isn't logged");
        t.i.taken();
        assert_eq!(t.at(0.0, &sref("b", Some("1"))).verdict, StreamVerdict::Accepted, "a busy id can be published again");
        // Taking never goes below zero.
        t.i.queued.store(0, Ordering::Release);
        t.i.taken();
        assert_eq!(t.i.queued(), 0);
    }

    #[test]
    fn the_ring_keeps_the_last_200_newest_first_with_counts() {
        let t = T::new();
        for n in 0..250 {
            t.at(n as f64, &sref("slack", Some(&n.to_string())));
        }
        t.at(250.0, &sref("slack", Some("249")));
        t.i.reject("slack", "message", Some("big".into()), StreamVerdict::TooLarge, "70000 bytes".into());
        let (rows, c) = t.i.events("slack", 500);
        assert_eq!(rows.len(), EVENTS_KEPT);
        assert_eq!((rows[0].verdict, rows[0].id.as_deref(), rows[0].detail.as_deref()), (StreamVerdict::TooLarge, Some("big"), Some("70000 bytes")));
        assert_eq!((rows[1].verdict, rows[1].seq), (StreamVerdict::Duplicate, Some(250)));
        assert_eq!((rows[2].verdict, rows[2].id.as_deref(), rows[2].seq), (StreamVerdict::Accepted, Some("249"), Some(250)));
        assert_eq!(c, StreamCounts { accepted: 250, duplicate: 1, too_large: 1, ..Default::default() });
        assert_eq!(t.i.events("slack", 3).0.len(), 3);
        assert_eq!(t.i.events("nobody", 10), (vec![], StreamCounts::default()));
    }

    #[test]
    fn seeding_from_the_log_restores_ids_and_accepted_events() {
        let i = Intake::default();
        let recent = time::format_unix(time::now_unix() - 60);
        i.seed(&[
            ev(1, "2020-01-01T00:00:00Z", sref("slack", Some("old"))),
            ev(2, &recent, sref("slack", Some("new"))),
            ev(3, &recent, sref("ci", None)),
        ]);
        let (rows, c) = i.events("slack", 10);
        assert_eq!(rows.iter().map(|e| e.seq).collect::<Vec<_>>(), [Some(2), Some(1)]);
        assert_eq!(c, StreamCounts::default(), "counts are since the daemon started");
        let dup = i.admit(&sref("slack", Some("new")), || unreachable!("a duplicate isn't logged"));
        assert_eq!((dup.verdict, dup.seq), (StreamVerdict::Duplicate, Some(2)));
        let old = i.admit(&sref("slack", Some("old")), || ev(4, &recent, sref("slack", Some("old"))));
        assert_eq!(old.verdict, StreamVerdict::Accepted, "an id older than 24 hours is forgotten");
    }

    #[test]
    fn past_256_streams_the_longest_unused_is_forgotten() {
        let t = T::new();
        for n in 0..MAX_STREAMS {
            t.at(n as f64, &sref(&format!("s{n}"), Some("1")));
        }
        t.at(1000.0, &sref("s0", Some("2")));
        t.at(1001.0, &sref("new", None));
        assert_eq!(t.i.book().len(), MAX_STREAMS);
        assert!(t.i.book().contains_key("s0"), "s0 was used again");
        assert!(!t.i.book().contains_key("s1"));
    }
}
