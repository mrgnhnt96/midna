//! App links: `<scheme>://…` URLs whose scheme an app on this Mac registered (the task board's
//! `taskboard://#/?task=T6`). Launch Services says which app opens a scheme; schemes no app
//! claims stay plain text, so a `foo://` in a log isn't a link.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Scheme -> the app that opens it (None: no app does). Apps rarely come and go, and the
/// daemon restarts with each release.
static APPS: LazyLock<Mutex<HashMap<String, Option<String>>>> = LazyLock::new(Default::default);

/// The name of the app that opens `scheme://` links, None when no app registered it. http(s)
/// and file are the browser's and Finder's, so they're never app links.
pub fn app_for(scheme: &str) -> Option<String> {
    let scheme = scheme.to_ascii_lowercase();
    if !valid(&scheme) || matches!(scheme.as_str(), "http" | "https" | "file") {
        return None;
    }
    if let Some(app) = APPS.lock().unwrap_or_else(|e| e.into_inner()).get(&scheme) {
        return app.clone();
    }
    let app = ls::default_app(&format!("{scheme}://"));
    APPS.lock().unwrap_or_else(|e| e.into_inner()).insert(scheme, app.clone());
    app
}

/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` (RFC 3986).
pub fn valid(scheme: &str) -> bool {
    let mut c = scheme.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic()) && c.all(|x| x.is_ascii_alphanumeric() || matches!(x, '+' | '-' | '.'))
}

/// The `<scheme>://…` app links in `text`, as (byte start, url, app). A
/// scheme glued to a word is part of it (`seetaskboard://` asks about `seetaskboard`); trailing
/// punctuation, and brackets the URL didn't open, are left off by `trim`.
pub fn app_links<'a>(text: &'a str, trim: impl Fn(&str) -> &str, app_for: &dyn Fn(&str) -> Option<String>) -> Vec<(usize, &'a str, String)> {
    let mut out = vec![];
    for (sep, _) in text.match_indices("://") {
        let start = text[..sep].char_indices().rev().take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')).last().map(|(i, _)| i);
        let Some(start) = start else { continue };
        let scheme = &text[start..sep];
        if !valid(scheme) {
            continue;
        }
        let end = text[sep..].find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | '|' | '\\' | '{' | '}')).map(|e| sep + e).unwrap_or(text.len());
        let url = trim(&text[start..end]);
        if url.len() <= scheme.len() + 3 {
            continue;
        }
        if let Some(app) = app_for(scheme) {
            out.push((start, url, app));
        }
    }
    out
}

mod ls {
    use core_foundation::base::TCFType;
    use core_foundation::url::{CFURL, CFURLRef};
    use core_foundation_sys::base::{CFTypeRef, kCFAllocatorDefault};

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSCopyDefaultApplicationURLForURL(url: CFURLRef, roles: u32, err: *mut CFTypeRef) -> CFURLRef;
    }

    /// kLSRolesAll.
    const ROLES_ALL: u32 = 0xFFFF_FFFF;

    /// The default app for `url`, by its bundle's name (`Taskboard.app` -> `Taskboard`).
    pub fn default_app(url: &str) -> Option<String> {
        let s = core_foundation::string::CFString::new(url);
        let url = unsafe { core_foundation_sys::url::CFURLCreateWithString(kCFAllocatorDefault, s.as_concrete_TypeRef(), std::ptr::null()) };
        if url.is_null() {
            return None;
        }
        let url = unsafe { CFURL::wrap_under_create_rule(url) };
        let app = unsafe { LSCopyDefaultApplicationURLForURL(url.as_concrete_TypeRef(), ROLES_ALL, std::ptr::null_mut()) };
        if app.is_null() {
            return None;
        }
        let app = unsafe { CFURL::wrap_under_create_rule(app) };
        let path = app.to_path()?;
        let name = path.file_stem()?.to_string_lossy().into_owned();
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes() {
        assert!(valid("taskboard") && valid("x-man+1.2"));
        assert!(!valid("") && !valid("1x") && !valid("a_b"));
        // Never app links, whatever opens them.
        assert_eq!(app_for("https"), None);
        assert_eq!(app_for("file"), None);
        // No app registers this one.
        assert_eq!(app_for("midna-test-unregistered-scheme"), None);
    }
}
