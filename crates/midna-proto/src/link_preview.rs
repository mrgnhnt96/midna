//! What a link's hover card shows (`links.preview`), read where the link lives: the app reads
//! this Mac's files itself, and midnad reads them for a window that drives another Mac.
use crate::methods::{DirEntry, LinkPreview};
use std::io::{BufRead, Read};
use std::path::Path;

/// Lines a file's card shows.
pub const FILE_LINES: usize = 12;
/// Lines above the target line.
pub const BEFORE: usize = 4;
/// Entries a folder's card shows.
pub const DIR_ROWS: usize = 8;

/// A file's lines, a folder's entries, an image's or binary's size, or why there's nothing.
pub fn read_path(path: &Path, line: Option<u32>) -> LinkPreview {
    let note = |text: String| LinkPreview::Note { text };
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return note(format!("Couldn't read it: {e}")),
    };
    if meta.is_dir() {
        return read_dir(path);
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
        return LinkPreview::Image { size: meta.len() };
    }
    if meta.len() == 0 {
        return note("Empty file".into());
    }
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return note(format!("Couldn't read it: {e}")),
    };
    let mut head = vec![0u8; 8192];
    let n = f.read(&mut head).unwrap_or(0);
    if head[..n].contains(&0) {
        return LinkPreview::Binary { size: meta.len() };
    }
    match std::fs::File::open(path) {
        Ok(f) => {
            let (lines, focus) = excerpt(std::io::BufReader::new(f), line);
            if lines.is_empty() { note("Empty file".into()) } else { LinkPreview::Lines { lines, focus } }
        }
        Err(e) => note(format!("Couldn't read it: {e}")),
    }
}

/// `FILE_LINES` numbered lines around `line` (the top of the file without one), tabs expanded
/// and very long lines cut. Reads no further than it needs (and gives up 64 MB in).
pub fn excerpt(mut r: impl BufRead, line: Option<u32>) -> (Vec<(u32, String)>, Option<u32>) {
    let start = line.map(|l| (l as usize).saturating_sub(1 + BEFORE)).unwrap_or(0);
    let mut out = vec![];
    let (mut n, mut read) = (0usize, 0usize);
    let mut buf = vec![];
    while out.len() < FILE_LINES && read < 64 << 20 {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(k) => read += k,
        }
        n += 1;
        if n > start {
            let s = String::from_utf8_lossy(&buf);
            let s = s.trim_end_matches(['\n', '\r']).replace('\t', "    ");
            let s: String = s.chars().take(500).collect();
            out.push((n as u32, s));
        }
    }
    // A line past the end: show the file's last lines instead of nothing.
    let focus = line.filter(|l| out.iter().any(|(n, _)| n == l));
    (out, focus)
}

fn read_dir(path: &Path) -> LinkPreview {
    let rd = match std::fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => return LinkPreview::Note { text: format!("Couldn't list it: {e}") },
    };
    let mut all: Vec<DirEntry> = rd
        .flatten()
        .filter(|e| e.file_name() != ".DS_Store")
        .map(|e| {
            let dir = e.file_type().is_ok_and(|t| t.is_dir());
            let size = (!dir).then(|| e.metadata().ok().map(|m| m.len())).flatten();
            DirEntry { name: e.file_name().to_string_lossy().into_owned(), dir, size }
        })
        .collect();
    all.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let total = all.len();
    all.truncate(DIR_ROWS);
    LinkPreview::Dir { entries: all, total }
}

/// Which GitHub page a link is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GithubKind {
    Pr,
    Issue,
    Run,
}

/// A GitHub link: what it is, its owner, repository and number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubLink {
    pub kind: GithubKind,
    pub owner: String,
    pub repo: String,
    pub n: u64,
}

impl GithubLink {
    /// `https://github.com/<owner>/<repo>/pull/<n>…`, `…/issues/<n>…` or
    /// `…/actions/runs/<id>…` (a job or an attempt in it too).
    pub fn parse(url: &str) -> Option<GithubLink> {
        let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        let mut seg = rest.strip_prefix("github.com/")?.split(['/', '?', '#']);
        let (owner, repo) = (seg.next().filter(|s| !s.is_empty())?, seg.next().filter(|s| !s.is_empty())?);
        let (kind, n) = match seg.next()? {
            "pull" => (GithubKind::Pr, seg.next()?),
            "issues" => (GithubKind::Issue, seg.next()?),
            "actions" if seg.next()? == "runs" => (GithubKind::Run, seg.next()?),
            _ => return None,
        };
        Some(GithubLink { kind, owner: owner.to_string(), repo: repo.to_string(), n: n.parse().ok()? })
    }

    /// `gh`'s arguments for what a card shows: `pr|issue|run view <n> -R <owner>/<repo> --json …`.
    pub fn gh_args(&self) -> Vec<String> {
        let (what, fields) = match self.kind {
            GithubKind::Pr => ("pr", "title,state,isDraft,headRefName,baseRefName,additions,deletions,statusCheckRollup"),
            GithubKind::Issue => ("issue", "title,state,body"),
            GithubKind::Run => ("run", "displayTitle,workflowName,headBranch,status,conclusion,startedAt,updatedAt,jobs"),
        };
        [what, "view", &self.n.to_string(), "-R", &format!("{}/{}", self.owner, self.repo), "--json", fields].map(str::to_string).to_vec()
    }

    /// How much of `gh`'s output to read: a run lists every job and step.
    pub fn gh_limit(&self) -> usize {
        if self.kind == GithubKind::Run { 4 << 20 } else { 1 << 20 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_centers_on_the_line() {
        let text: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        let (lines, focus) = excerpt(text.as_bytes(), Some(20));
        assert_eq!(lines.first().map(|l| l.0), Some(16));
        assert_eq!(lines.len(), FILE_LINES);
        assert_eq!(focus, Some(20));
        let (lines, focus) = excerpt(text.as_bytes(), None);
        assert_eq!((lines[0].0, focus), (1, None));
        // Near the top, the window starts at line 1.
        assert_eq!(excerpt(text.as_bytes(), Some(2)).0[0].0, 1);
        // Tabs expand; CRLF goes.
        assert_eq!(excerpt("\ta\r\n".as_bytes(), Some(1)).0[0].1, "    a");
    }

    #[test]
    fn reads_files_folders_and_what_it_cant_show() {
        let dir = std::env::temp_dir().join(format!("midna-link-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("empty.txt"), "").unwrap();
        std::fs::write(dir.join("bin"), [1u8, 0, 2]).unwrap();
        std::fs::write(dir.join("p.png"), [9u8; 10]).unwrap();
        std::fs::write(dir.join(".DS_Store"), "x").unwrap();
        assert_eq!(read_path(&dir.join("a.rs"), Some(1)), LinkPreview::Lines { lines: vec![(1, "fn main() {}".into())], focus: Some(1) });
        assert_eq!(read_path(&dir.join("empty.txt"), None), LinkPreview::Note { text: "Empty file".into() });
        assert_eq!(read_path(&dir.join("bin"), None), LinkPreview::Binary { size: 3 });
        assert_eq!(read_path(&dir.join("p.png"), None), LinkPreview::Image { size: 10 });
        assert!(matches!(read_path(&dir.join("gone"), None), LinkPreview::Note { text } if text.starts_with("Couldn't read it")));
        let LinkPreview::Dir { entries, total } = read_path(&dir, None) else { panic!("not a folder") };
        // Folders first, then by name; .DS_Store is left out.
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["sub", "a.rs", "bin", "empty.txt", "p.png"]);
        assert_eq!((total, entries[0].size, entries[1].size), (5, None, Some(13)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn github_links() {
        let g = |url: &str| GithubLink::parse(url).map(|g| (g.kind, g.owner, g.repo, g.n));
        assert_eq!(g("https://github.com/mrgnhnt96/midna/pull/42"), Some((GithubKind::Pr, "mrgnhnt96".into(), "midna".into(), 42)));
        assert_eq!(g("https://github.com/a/b/pull/7/files#diff"), Some((GithubKind::Pr, "a".into(), "b".into(), 7)));
        assert_eq!(g("https://github.com/a/b/issues/7#issuecomment-1"), Some((GithubKind::Issue, "a".into(), "b".into(), 7)));
        assert_eq!(g("https://github.com/a/b/actions/runs/9/job/3"), Some((GithubKind::Run, "a".into(), "b".into(), 9)));
        assert_eq!(g("https://github.com/a/b/issues"), None);
        assert_eq!(g("https://github.com/a/b/actions/workflows/ci.yml"), None);
        assert_eq!(g("https://github.com//b/pull/1"), None);
        assert_eq!(g("https://gitlab.com/a/b/pull/1"), None);
        let args = GithubLink::parse("https://github.com/a/b/pull/7").unwrap().gh_args();
        assert_eq!(args[..5], ["pr", "view", "7", "-R", "a/b"]);
    }
}
