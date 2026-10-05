//! Pasted secrets: spotting them in text, the `[secret:NAME]` reference an agent sees instead,
//! and scrubbing stored values out of command output.
//!
//! The GUI scans a paste into an agent terminal with [`scan`]. Anything it finds can be stored
//! (`secret.set`) and replaced by [`reference`], so the value never reaches the agent's prompt
//! (and so never reaches the model). `midna secret exec` runs a command with the values in its
//! environment and passes its output through a [`Scrubber`].

/// What an agent sees in place of a stored secret.
pub fn reference(name: &str) -> String {
    format!("[secret:{name}]")
}

/// A secret name doubles as the environment variable `midna secret exec` sets, so it must be
/// one: `[A-Z_][A-Z0-9_]*`, at most 64 characters.
pub fn valid_name(name: &str) -> bool {
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_uppercase() || c == '_')
        && cs.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && name.len() <= 64
}

/// Turn free text ("github token", "my-key") into a valid name (`GITHUB_TOKEN`, `MY_KEY`).
pub fn normalize_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let mut out = out.trim_matches('_').to_string();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out.truncate(64);
    out
}

/// One secret found in a piece of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Byte range of the value in the scanned text.
    pub start: usize,
    pub end: usize,
    /// What it looks like, for the human ("GitHub token").
    pub label: &'static str,
    /// Suggested name (`GITHUB_TOKEN`; the key for `KEY=value` lines).
    pub name: String,
}

impl Found {
    pub fn value<'a>(&self, text: &'a str) -> &'a str {
        &text[self.start..self.end]
    }
}

/// Known token shapes: (prefix, minimum length after the prefix, label, suggested name).
/// Longer prefixes come first so `sk-ant-` wins over `sk-`.
const PREFIXES: &[(&str, usize, &str, &str)] = &[
    ("sk-ant-", 20, "Anthropic API key", "ANTHROPIC_API_KEY"),
    ("sk-proj-", 20, "OpenAI API key", "OPENAI_API_KEY"),
    ("sk-", 32, "OpenAI API key", "OPENAI_API_KEY"),
    ("sk_live_", 16, "Stripe secret key", "STRIPE_SECRET_KEY"),
    ("sk_test_", 16, "Stripe test key", "STRIPE_SECRET_KEY"),
    ("rk_live_", 16, "Stripe restricted key", "STRIPE_SECRET_KEY"),
    ("whsec_", 16, "Webhook signing secret", "WEBHOOK_SECRET"),
    ("github_pat_", 30, "GitHub token", "GITHUB_TOKEN"),
    ("ghp_", 30, "GitHub token", "GITHUB_TOKEN"),
    ("gho_", 30, "GitHub token", "GITHUB_TOKEN"),
    ("ghu_", 30, "GitHub token", "GITHUB_TOKEN"),
    ("ghs_", 30, "GitHub token", "GITHUB_TOKEN"),
    ("ghr_", 30, "GitHub token", "GITHUB_TOKEN"),
    ("glpat-", 20, "GitLab token", "GITLAB_TOKEN"),
    ("ATBB", 28, "Bitbucket app password", "BITBUCKET_TOKEN"),
    ("ATATT", 28, "Atlassian API token", "ATLASSIAN_API_TOKEN"),
    ("xoxb-", 20, "Slack bot token", "SLACK_BOT_TOKEN"),
    ("xoxp-", 20, "Slack user token", "SLACK_TOKEN"),
    ("xoxa-", 20, "Slack token", "SLACK_TOKEN"),
    ("xapp-", 20, "Slack app token", "SLACK_APP_TOKEN"),
    ("AKIA", 16, "AWS access key", "AWS_ACCESS_KEY_ID"),
    ("ASIA", 16, "AWS access key", "AWS_ACCESS_KEY_ID"),
    ("AIza", 30, "Google API key", "GOOGLE_API_KEY"),
    ("npm_", 30, "npm token", "NPM_TOKEN"),
    ("pypi-", 30, "PyPI token", "PYPI_TOKEN"),
    ("hf_", 30, "Hugging Face token", "HF_TOKEN"),
    ("dop_v1_", 30, "DigitalOcean token", "DIGITALOCEAN_TOKEN"),
    ("SG.", 30, "SendGrid key", "SENDGRID_API_KEY"),
    ("pk_live_", 16, "Stripe publishable key", "STRIPE_PUBLISHABLE_KEY"),
    ("shpat_", 24, "Shopify token", "SHOPIFY_TOKEN"),
    ("lin_api_", 24, "Linear API key", "LINEAR_API_KEY"),
];

/// Words in a `KEY=value` key that mark the value as secret.
const SECRET_KEY_WORDS: &[&str] = &["KEY", "SECRET", "TOKEN", "PASSWORD", "PASSWD", "PASS", "PWD", "AUTH", "CREDENTIAL", "PRIVATE", "DSN", "COOKIE", "SESSION"];

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'+' | b'/' | b'=')
}

/// Every secret-looking value in `text`, in order, never overlapping. Recognizes known token
/// prefixes, PEM private keys, JWTs, `KEY=value` lines whose key names a secret, and long
/// random-looking words (mixed case and digits, high entropy; git hashes and UUIDs don't count).
pub fn scan(text: &str) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let b = text.as_bytes();
    // PEM private keys (multi-line).
    let mut from = 0;
    while let Some(i) = text[from..].find("-----BEGIN ") {
        let s = from + i;
        let Some(hdr_end) = text[s + 11..].find("-----").map(|j| s + 11 + j + 5) else { break };
        let kind = &text[s + 11..hdr_end - 5];
        if !kind.ends_with("PRIVATE KEY") {
            from = hdr_end;
            continue;
        }
        let footer = format!("-----END {kind}-----");
        let Some(e) = text[hdr_end..].find(&footer).map(|j| hdr_end + j + footer.len()) else { break };
        found.push(Found { start: s, end: e, label: "Private key", name: "PRIVATE_KEY".into() });
        from = e;
    }
    let overlaps = |found: &[Found], s: usize, e: usize| found.iter().any(|f| s < f.end && f.start < e);
    // KEY=value lines (dotenv, `export KEY=…`, shell assignments).
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        if let Some(f) = assignment(line, line_start)
            && !overlaps(&found, f.start, f.end)
        {
            found.push(f);
        }
        line_start += line.len();
    }
    // Single tokens.
    let mut i = 0;
    while i < b.len() {
        if !token_char(b[i]) || (i > 0 && token_char(b[i - 1])) {
            i += 1;
            continue;
        }
        let mut e = i;
        while e < b.len() && token_char(b[e]) {
            e += 1;
        }
        // Trailing punctuation is never part of a token.
        let mut end = e;
        while end > i && matches!(b[end - 1], b'.' | b'/' | b'-') {
            end -= 1;
        }
        let word = &text[i..end];
        if let Some(f) = inline_assignment(word, i).or_else(|| classify(word, i)) {
            if let Some(x) = found.iter_mut().find(|x| x.start <= f.start && f.end <= x.end) {
                // Inside a `KEY=value`: keep the key as the name, but a known token's label is better.
                if x.label == "Secret" && f.label != "Possible secret" {
                    x.label = f.label;
                }
            } else if !overlaps(&found, f.start, f.end) {
                found.push(f);
            }
        }
        i = e;
    }
    found.sort_by_key(|f| f.start);
    found
}

/// `KEY=value` / `export KEY="value"` where KEY names a secret and the value isn't trivial.
fn assignment(line: &str, offset: usize) -> Option<Found> {
    let trimmed = line.trim_start();
    let lead = line.len() - trimmed.len();
    let (rest, lead) = match trimmed.strip_prefix("export ") {
        Some(r) => (r.trim_start(), lead + (trimmed.len() - r.trim_start().len())),
        None => (trimmed, lead),
    };
    let eq = rest.find('=')?;
    let key = &rest[..eq];
    if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') || key.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let upper = key.to_ascii_uppercase();
    if !upper.split('_').any(|w| SECRET_KEY_WORDS.contains(&w)) {
        return None;
    }
    let raw = rest[eq + 1..].trim_end_matches(['\n', '\r']);
    let mut vs = lead + eq + 1;
    let mut value = raw.trim_end();
    if let Some(q) = value.chars().next().filter(|c| *c == '"' || *c == '\'')
        && value.len() >= 2
        && value.ends_with(q)
    {
        value = &value[1..value.len() - 1];
        vs += 1;
    } else if let Some(h) = value.find(" #") {
        value = value[..h].trim_end();
    }
    if value.len() < 8 || value.contains("${") || value.starts_with('$') || value.starts_with("[secret:") || is_placeholder(value) {
        return None;
    }
    Some(Found { start: offset + vs, end: offset + vs + value.len(), label: "Secret", name: normalize_name(key) })
}

/// `KEY=value` inside a line (`use STRIPE_KEY=sk_live_… here`): the value, named after KEY.
/// A known token keeps its label; otherwise KEY must name a secret.
fn inline_assignment(word: &str, start: usize) -> Option<Found> {
    let (key, value) = word.split_once('=')?;
    if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') || key.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let vs = start + key.len() + 1;
    let named = |label| Some(Found { start: vs, end: vs + value.len(), label, name: normalize_name(key) });
    match classify(value, vs) {
        Some(f) if f.end == vs + value.len() => named(f.label),
        Some(f) => Some(f),
        None => assignment(word, start).and_then(|f| named(f.label)),
    }
}

/// `changeme`, `your-api-key-here`, `xxxxxxxx`, `<token>`…
fn is_placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    l.starts_with('<') || l.contains("your") || l.contains("changeme") || l.contains("example") || l.contains("placeholder") || v.chars().all(|c| c == v.chars().next().unwrap_or(' '))
}

fn classify(word: &str, start: usize) -> Option<Found> {
    let mk = |end: usize, label: &'static str, name: &str| Some(Found { start, end: start + end, label, name: name.into() });
    for (p, min, label, name) in PREFIXES {
        if let Some(rest) = word.strip_prefix(p) {
            let body = rest.bytes().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')).count();
            if body >= *min {
                return mk(p.len() + body, label, name);
            }
        }
    }
    if let Some(len) = jwt_len(word) {
        return mk(len, "JWT", "JWT");
    }
    if random_looking(word) {
        return mk(word.len(), "Possible secret", "SECRET");
    }
    None
}

fn jwt_len(w: &str) -> Option<usize> {
    if !w.starts_with("eyJ") {
        return None;
    }
    let b64 = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    let parts: Vec<&str> = w.split('.').collect();
    (parts.len() == 3 && parts[0].len() >= 10 && parts[1].len() >= 10 && parts.iter().all(|p| b64(p)) && parts[1].starts_with("eyJ")).then_some(w.len())
}

/// A long word with upper case, lower case and digits and high per-character entropy.
/// Excludes paths, URLs, hex (git hashes) and base64 that's obviously data (very long).
fn random_looking(w: &str) -> bool {
    if w.len() < 24 || w.len() > 512 || w.contains('/') || w.contains("..") {
        return false;
    }
    let (mut up, mut lo, mut dig) = (false, false, false);
    for c in w.bytes() {
        up |= c.is_ascii_uppercase();
        lo |= c.is_ascii_lowercase();
        dig |= c.is_ascii_digit();
    }
    if !(up && lo && dig) || w.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
        return false;
    }
    // CamelCaseIdentifiers123 have long letter runs; tokens don't.
    let mut counts = [0u32; 256];
    for c in w.bytes() {
        counts[c as usize] += 1;
    }
    let n = w.len() as f64;
    let entropy: f64 = counts.iter().filter(|&&k| k > 0).map(|&k| {
        let p = k as f64 / n;
        -p * p.log2()
    }).sum();
    let longest_alpha = w.split(|c: char| !c.is_ascii_lowercase()).map(str::len).max().unwrap_or(0);
    entropy >= 4.0 && longest_alpha < 9
}

/// Replace each found value with its stored name's reference. `names[i]` is the name chosen
/// for `found[i]`; `None` keeps that value as it is.
pub fn replace(text: &str, found: &[Found], names: &[Option<String>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (f, n) in found.iter().zip(names) {
        let Some(n) = n else { continue };
        out.push_str(&text[at..f.start]);
        out.push_str(&reference(n));
        at = f.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Masks stored secret values in a byte stream (command output), including their base64 and
/// URL-encoded forms, as `‹NAME›`. Feed chunks as they arrive; a possible start of a value at
/// the end of a chunk is held back until the next one (or [`Scrubber::finish`]).
pub struct Scrubber {
    /// (needle, replacement), longest first so the full value wins over a shorter form.
    needles: Vec<(Vec<u8>, Vec<u8>)>,
    held: Vec<u8>,
}

/// Values shorter than this aren't scrubbed (they'd match ordinary output).
pub const MIN_SCRUB_LEN: usize = 6;

impl Scrubber {
    pub fn new<'a>(secrets: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Scrubber {
        let mut needles: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (name, value) in secrets {
            if value.len() < MIN_SCRUB_LEN {
                continue;
            }
            let rep = format!("‹{name}›").into_bytes();
            let mut forms = vec![value.to_vec(), base64(value, false), base64(value, true), percent(value)];
            // A value inside JSON output has its quotes and backslashes escaped.
            if let Ok(s) = std::str::from_utf8(value) {
                let j = serde_json::to_string(s).unwrap_or_default();
                forms.push(j.as_bytes()[1..j.len() - 1].to_vec());
            }
            forms.sort();
            forms.dedup();
            for f in forms {
                if f.len() >= MIN_SCRUB_LEN {
                    needles.push((f, rep.clone()));
                }
            }
        }
        needles.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
        Scrubber { needles, held: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    /// Scrub `chunk`; returns what can be written now.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.held.extend_from_slice(chunk);
        let buf = std::mem::take(&mut self.held);
        let mut out = Vec::with_capacity(buf.len());
        let mut i = 0;
        'outer: while i < buf.len() {
            for (n, rep) in &self.needles {
                if buf[i..].starts_with(n) {
                    out.extend_from_slice(rep);
                    i += n.len();
                    continue 'outer;
                }
            }
            // Could a needle start here and continue in the next chunk?
            if self.needles.iter().any(|(n, _)| n.len() > buf.len() - i && n.starts_with(&buf[i..])) {
                self.held = buf[i..].to_vec();
                return out;
            }
            out.push(buf[i]);
            i += 1;
        }
        out
    }

    /// The end of the stream: whatever was held back.
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }
}

fn base64(data: &[u8], url: bool) -> Vec<u8> {
    let table: &[u8; 64] = if url { b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_" } else { b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/" };
    let mut out = Vec::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..=c.len() {
            out.push(table[(n >> (18 - 6 * k)) as usize & 63]);
        }
    }
    // Without padding: the padded form starts with the same bytes.
    out
}

fn percent(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &c in data {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~') {
            out.push(c);
        } else {
            out.extend_from_slice(format!("%{c:02X}").as_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<(String, &'static str, String)> {
        scan(text).into_iter().map(|f| (f.value(text).to_string(), f.label, f.name)).collect()
    }

    #[test]
    fn known_prefixes() {
        let gh = "ghp_aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF3hJ6";
        assert_eq!(names(&format!("use {gh} please")), vec![(gh.into(), "GitHub token", "GITHUB_TOKEN".into())]);
        let ant = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        assert_eq!(names(ant)[0].1, "Anthropic API key");
        assert_eq!(names("AKIAIOSFODNN7EXAMPLE")[0].2, "AWS_ACCESS_KEY_ID");
        // Trailing punctuation isn't part of it.
        assert_eq!(names(&format!("({gh}).")), vec![(gh.into(), "GitHub token", "GITHUB_TOKEN".into())]);
    }

    #[test]
    fn dotenv_lines_keep_their_key() {
        let t = "PORT=3000\nSTRIPE_SECRET_KEY=\"sk_live_51HxQ2bCdEfGhIjKlMnOp\"\nexport DB_PASSWORD=hunter2hunter2\nAPI_TOKEN=your-token-here\n";
        let f = names(t);
        assert_eq!(f.len(), 2, "{f:?}");
        assert_eq!(f[0], ("sk_live_51HxQ2bCdEfGhIjKlMnOp".into(), "Stripe secret key", "STRIPE_SECRET_KEY".into()));
        assert_eq!(f[1], ("hunter2hunter2".into(), "Secret", "DB_PASSWORD".into()));
        let found = scan(t);
        let out = replace(t, &found, &[Some("STRIPE_SECRET_KEY".into()), Some("DB_PASSWORD".into())]);
        assert_eq!(out, "PORT=3000\nSTRIPE_SECRET_KEY=\"[secret:STRIPE_SECRET_KEY]\"\nexport DB_PASSWORD=[secret:DB_PASSWORD]\nAPI_TOKEN=your-token-here\n");
    }

    #[test]
    fn inline_assignments_use_the_key() {
        let t = "use STRIPE_SECRET_KEY=sk_live_51HxQ2bCdEfGhIjKlMnOpQr and DB_PASSWORD=hunter2hunter2 and PORT=3000";
        let f = names(t);
        assert_eq!(f, vec![
            ("sk_live_51HxQ2bCdEfGhIjKlMnOpQr".into(), "Stripe secret key", "STRIPE_SECRET_KEY".into()),
            ("hunter2hunter2".into(), "Secret", "DB_PASSWORD".into()),
        ]);
    }

    #[test]
    fn pem_and_jwt() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmU\n-----END OPENSSH PRIVATE KEY-----";
        let t = format!("key:\n{pem}\nthanks");
        assert_eq!(names(&t), vec![(pem.into(), "Private key", "PRIVATE_KEY".into())]);
        assert!(scan("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----").is_empty());
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(names(jwt)[0].1, "JWT");
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        for t in [
            "fix the bug in crates/midna-app/src/terminal.rs please",
            "commit 4176a04b2e0f1c9d8e7a6b5c4d3e2f1a0b9c8d7e",
            "id 550e8400-e29b-41d4-a716-446655440000",
            "https://github.com/mrgnhnt96/midna/pull/12?tab=files",
            "TerminalViewControllerDelegate2 handles it",
            "PORT=3000 and HOST=localhost",
            "API_KEY=${API_KEY}",
        ] {
            assert!(scan(t).is_empty(), "{t}: {:?}", scan(t));
        }
        assert_eq!(names("token q8Zr2LmX9vB4nK7tY1wE5cH3jP6sD0fG")[0].1, "Possible secret");
    }

    #[test]
    fn name_rules() {
        assert!(valid_name("GITHUB_TOKEN") && valid_name("_X1"));
        assert!(!valid_name("github") && !valid_name("1X") && !valid_name("") && !valid_name("A-B"));
        assert_eq!(normalize_name(" my github-token "), "MY_GITHUB_TOKEN");
        assert_eq!(normalize_name("1password"), "_1PASSWORD");
    }

    #[test]
    fn scrubber_masks_forms_across_chunks() {
        let v: &[u8] = b"s3cr3t/value+x";
        let mut s = Scrubber::new([("TOK", v)]);
        let mut out = s.feed(b"a s3cr");
        out.extend(s.feed(b"3t/value+x b "));
        out.extend(s.feed(base64(v, false).as_slice()));
        out.extend(s.feed(b" "));
        out.extend(s.feed(&percent(v)));
        out.extend(s.feed(b" s3c"));
        out.extend(s.finish());
        assert_eq!(String::from_utf8(out).unwrap(), "a ‹TOK› b ‹TOK› ‹TOK› s3c");
        // Short values are left alone.
        assert!(Scrubber::new([("X", &b"abc"[..])]).is_empty());
    }
}
