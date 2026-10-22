//! App link menus (`links.actions.register`). A menu's `links` regex runs over a link's URL, or
//! over the text a link pattern matched; its captures (`$0`, `$1`, `${name}`) fill the items'
//! fields. An `own` menu is the whole menu for those links; a `section` menu adds a labelled
//! group to midna's. `run` and `terminal` items are argv without a shell, and their program never
//! comes from a capture, like a link pattern's `preview:`. midnad checks what an app registers
//! with [`validate`]; the app finds a link's menus with [`menus_for`] and fills each item with
//! [`expand_item`].
use crate::methods::{AppLinkActions, AppLinkMenu, LinkMenuItem};
use regex::{Captures, Regex};

pub const MAX_MENUS: usize = 32;
pub const MAX_ITEMS: usize = 24;
const MAX_LABEL: usize = 80;
const MAX_TEXT: usize = 4000;

/// Every menu is valid, or the error for the first that isn't (`menus[1].items[0]: …`).
pub fn validate(menus: &[AppLinkMenu]) -> Result<(), String> {
    if menus.len() > MAX_MENUS {
        return Err(format!("at most {MAX_MENUS} menus"));
    }
    for (i, m) in menus.iter().enumerate() {
        validate_menu(m).map_err(|e| format!("menus[{i}]{e}"))?;
    }
    Ok(())
}

fn validate_menu(m: &AppLinkMenu) -> Result<(), String> {
    if m.links.is_empty() || m.links.len() > 1000 {
        return Err(".links: a regex over the link, 1–1000 characters".into());
    }
    Regex::new(&m.links).map_err(|e| format!(".links: bad regex: {}", e.to_string().lines().last().unwrap_or("")))?;
    match (m.own, m.section.as_deref()) {
        (true, Some(_)) => return Err(": `own` and `section` don't go together; pick one".into()),
        (false, None) => return Err(": needs `own: true` (the whole menu) or `section: \"<label>\"` (a group in midna's)".into()),
        (false, Some(s)) if s.trim().is_empty() || s.chars().count() > 40 => return Err(".section: a label of 1–40 characters".into()),
        (false, Some(_)) if m.items.is_empty() => return Err(".items: a section needs at least one item".into()),
        _ => {}
    }
    if let Some(icon) = &m.icon {
        valid_icon(icon).map_err(|e| format!(".icon: {e}"))?;
    }
    for (k, v) in [("title", &m.title), ("subtitle", &m.subtitle)] {
        if v.as_ref().is_some_and(|v| v.chars().count() > MAX_LABEL) {
            return Err(format!(".{k}: at most {MAX_LABEL} characters"));
        }
    }
    validate_items(&m.items).map_err(|e| format!(".items{e}"))
}

/// The items of a menu, or of a preview command's `actions`.
pub fn validate_items(items: &[LinkMenuItem]) -> Result<(), String> {
    if items.len() > MAX_ITEMS {
        return Err(format!(": at most {MAX_ITEMS} items"));
    }
    for (i, item) in items.iter().enumerate() {
        validate_item(item).map_err(|e| format!("[{i}]: {e}"))?;
    }
    Ok(())
}

pub fn validate_item(item: &LinkMenuItem) -> Result<(), String> {
    let actions = [item.open.is_some(), item.copy.is_some(), item.run.is_some(), item.terminal.is_some(), item.prompt.is_some()].iter().filter(|a| **a).count();
    if item.divider {
        let bare = LinkMenuItem { divider: true, ..Default::default() };
        return if *item == bare { Ok(()) } else { Err("a divider has nothing else".into()) };
    }
    if item.label.trim().is_empty() || item.label.chars().count() > MAX_LABEL {
        return Err(format!("`label` is 1–{MAX_LABEL} characters"));
    }
    if actions != 1 {
        return Err("needs exactly one of open, copy, run, terminal, prompt".into());
    }
    for argv in [&item.run, &item.terminal].into_iter().flatten() {
        match argv.first() {
            None => return Err("a command needs a program, like [\"tb\", \"done\", \"$1\"]".into()),
            Some(p) if p.trim().is_empty() => return Err("the program is empty".into()),
            Some(p) if p.contains('$') => return Err(format!("the program `{p}` can't come from a capture")),
            _ => {}
        }
        if argv.iter().map(String::len).sum::<usize>() > MAX_TEXT {
            return Err(format!("the command is over {MAX_TEXT} characters"));
        }
    }
    for (k, v) in [("open", &item.open), ("copy", &item.copy), ("prompt", &item.prompt), ("confirm", &item.confirm)] {
        match v {
            Some(v) if v.trim().is_empty() => return Err(format!("`{k}` is empty")),
            Some(v) if v.len() > MAX_TEXT => return Err(format!("`{k}` is over {MAX_TEXT} characters")),
            _ => {}
        }
    }
    if item.keys.as_ref().is_some_and(|k| k.is_empty() || k.chars().count() > 12) {
        return Err("`keys` is a shortcut like ⌘D".into());
    }
    if item.detail.as_ref().is_some_and(|d| d.chars().count() > 40) {
        return Err("`detail` is at most 40 characters".into());
    }
    Ok(())
}

/// A built-in icon's name (`checklist`), or an absolute path to an .svg or .png.
fn valid_icon(icon: &str) -> Result<(), String> {
    if icon.starts_with('/') {
        let lower = icon.to_ascii_lowercase();
        return if lower.ends_with(".svg") || lower.ends_with(".png") { Ok(()) } else { Err("a file icon is an .svg or .png".into()) };
    }
    let name = !icon.is_empty() && icon.len() <= 32 && icon.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if name { Ok(()) } else { Err(format!("`{icon}` is neither an icon name (checklist, tag, bug) nor an absolute .svg/.png path")) }
}

/// The captures of `m.links` over the link: its URL first, else the text a link pattern matched.
pub fn captures<'t>(m: &AppLinkMenu, url: &'t str, text: Option<&'t str>) -> Option<Captures<'t>> {
    let re = Regex::new(&m.links).ok()?;
    re.captures(url).or_else(|| text.and_then(|t| re.captures(t)))
}

/// A link's menus, filled from its captures: the own menu that takes it over (the first owner's,
/// by name), and the sections apps add to midna's, in owner order.
pub struct Matched {
    pub own: Option<(String, AppLinkMenu)>,
    pub sections: Vec<(String, AppLinkMenu)>,
}

pub fn menus_for(apps: &[AppLinkActions], url: &str, text: Option<&str>) -> Matched {
    let mut out = Matched { own: None, sections: vec![] };
    for app in apps {
        for m in &app.menus {
            let Some(caps) = captures(m, url, text) else { continue };
            let filled = AppLinkMenu {
                title: m.title.as_deref().map(|t| fill(t, &caps)),
                subtitle: m.subtitle.as_deref().map(|t| fill(t, &caps)),
                items: m.items.iter().map(|i| expand_item(i, &caps)).collect(),
                ..m.clone()
            };
            if m.own {
                out.own.get_or_insert((app.owner.clone(), filled));
            } else {
                out.sections.push((app.owner.clone(), filled));
            }
        }
    }
    out
}

fn fill(template: &str, caps: &Captures) -> String {
    let mut out = String::new();
    caps.expand(template, &mut out);
    out
}

/// An item with every field filled from the captures, but a command's program, which stays as
/// registered.
pub fn expand_item(item: &LinkMenuItem, caps: &Captures) -> LinkMenuItem {
    let f = |v: &Option<String>| v.as_deref().map(|t| fill(t, caps));
    let argv = |v: &Option<Vec<String>>| v.as_ref().map(|a| a.iter().enumerate().map(|(i, w)| if i == 0 { w.clone() } else { fill(w, caps) }).collect());
    LinkMenuItem {
        label: fill(&item.label, caps),
        detail: f(&item.detail),
        open: f(&item.open),
        copy: f(&item.copy),
        run: argv(&item.run),
        terminal: argv(&item.terminal),
        prompt: f(&item.prompt),
        confirm: f(&item.confirm),
        ..item.clone()
    }
}

/// Whether an item runs something the human has to approve first (run, terminal, prompt).
pub fn needs_approval(item: &LinkMenuItem) -> bool {
    item.run.is_some() || item.terminal.is_some() || item.prompt.is_some()
}

/// The commands the human approves for an app, one line each in a stable order: `run tb done $1`,
/// `terminal …`, `prompt …`. Empty when it has none.
pub fn commands(menus: &[AppLinkMenu]) -> Vec<String> {
    let mut out = vec![];
    for i in menus.iter().flat_map(|m| &m.items) {
        if let Some(a) = &i.run {
            out.push(format!("run {}", shell_words(a)));
        }
        if let Some(a) = &i.terminal {
            out.push(format!("terminal {}", shell_words(a)));
        }
        if let Some(p) = &i.prompt {
            out.push(format!("prompt {p:?}"));
        }
    }
    out.sort();
    out.dedup();
    out
}

fn shell_words(argv: &[String]) -> String {
    let quote = |w: &String| if !w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || "-_./:=@$%+,{}".contains(c)) { w.clone() } else { format!("'{}'", w.replace('\'', r"'\''")) };
    argv.iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// A preview command's `actions` for a link an approved app owns may only run what that app's
/// registered items run: a run or terminal item whose program one of them uses, a prompt item
/// when one of them prompts. Open and copy always may.
pub fn preview_item_allowed(app: &AppLinkActions, item: &LinkMenuItem) -> bool {
    if !needs_approval(item) {
        return true;
    }
    if !app.approved {
        return false;
    }
    let items = || app.menus.iter().flat_map(|m| &m.items);
    let program = |i: &LinkMenuItem| i.run.as_ref().or(i.terminal.as_ref()).and_then(|a| a.first()).cloned();
    match program(item) {
        Some(p) => items().any(|i| program(i).as_ref() == Some(&p)),
        None => items().any(|i| i.prompt.is_some()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> LinkMenuItem {
        LinkMenuItem { label: label.into(), ..Default::default() }
    }

    fn run(label: &str, argv: &[&str]) -> LinkMenuItem {
        LinkMenuItem { run: Some(argv.iter().map(|s| s.to_string()).collect()), ..item(label) }
    }

    fn task_menu() -> AppLinkMenu {
        AppLinkMenu {
            links: r"taskboard://#/\?task=(T\d+)".into(),
            own: true,
            icon: Some("checklist".into()),
            items: vec![
                run("Go to its terminal", &["tb", "focus", "$1"]),
                LinkMenuItem { divider: true, ..Default::default() },
                LinkMenuItem { confirm: Some("Cancel $1?".into()), ..run("Cancel task…", &["tb", "cancel", "$1"]) },
            ],
            ..Default::default()
        }
    }

    fn pr_section() -> AppLinkMenu {
        AppLinkMenu {
            links: r"github\.com/[^/]+/[^/]+/pull/\d+".into(),
            section: Some("Taskboard".into()),
            items: vec![run("Add to board as a task", &["tb", "task", "new", "--pr", "$0"])],
            ..Default::default()
        }
    }

    #[test]
    fn validates_menus() {
        assert_eq!(validate(&[task_menu(), pr_section()]), Ok(()));
        let bad = |m: AppLinkMenu| validate(&[m]).unwrap_err();
        assert!(bad(AppLinkMenu { links: "(".into(), ..task_menu() }).contains("menus[0].links: bad regex"));
        assert!(bad(AppLinkMenu { section: Some("X".into()), ..task_menu() }).contains("don't go together"));
        assert!(bad(AppLinkMenu { own: false, ..task_menu() }).contains("needs `own: true`"));
        assert!(bad(AppLinkMenu { items: vec![], ..pr_section() }).contains("at least one item"));
        assert!(bad(AppLinkMenu { icon: Some("~/x.svg".into()), ..task_menu() }).contains("neither an icon name"));
        assert!(bad(AppLinkMenu { icon: Some("/x/y.gif".into()), ..task_menu() }).contains(".svg or .png"));
        assert_eq!(validate(&[AppLinkMenu { icon: Some("/x/Y.PNG".into()), ..task_menu() }]), Ok(()));
        assert_eq!(validate(&[AppLinkMenu { items: vec![], ..task_menu() }]), Ok(()));
    }

    #[test]
    fn validates_items() {
        let bad = |i: LinkMenuItem| validate_item(&i).unwrap_err();
        assert!(bad(run("x", &["$1", "a"])).contains("can't come from a capture"));
        assert!(bad(run("x", &[])).contains("needs a program"));
        assert!(bad(item("x")).contains("exactly one"));
        assert!(bad(LinkMenuItem { copy: Some("a".into()), ..run("x", &["tb"]) }).contains("exactly one"));
        assert!(bad(LinkMenuItem { copy: Some("a".into()), ..Default::default() }).contains("label"));
        assert!(bad(LinkMenuItem { divider: true, label: "x".into(), ..Default::default() }).contains("divider"));
        assert!(bad(LinkMenuItem { keys: Some(String::new()), ..run("x", &["tb"]) }).contains("keys"));
        assert_eq!(validate_item(&LinkMenuItem { prompt: Some("Review $0".into()), keys: Some("⌘D".into()), ..item("Ask") }), Ok(()));
        assert!(validate_items(&vec![item("x"); MAX_ITEMS + 1]).unwrap_err().contains("at most"));
    }

    #[test]
    fn matches_and_fills() {
        let apps = vec![AppLinkActions { owner: "Taskboard".into(), menus: vec![task_menu(), pr_section()], approved: false, approval: None }];
        let m = menus_for(&apps, "taskboard://#/?task=T42", Some("T42"));
        let (owner, own) = m.own.unwrap();
        assert_eq!(owner, "Taskboard");
        assert_eq!(own.items[0].run, Some(vec!["tb".into(), "focus".into(), "T42".into()]));
        assert_eq!(own.items[2].confirm.as_deref(), Some("Cancel T42?"));
        assert!(m.sections.is_empty());
        let pr = menus_for(&apps, "https://github.com/o/r/pull/10", None);
        assert!(pr.own.is_none());
        assert_eq!(pr.sections[0].1.items[0].run.as_ref().unwrap()[4], "github.com/o/r/pull/10");
        assert!(menus_for(&apps, "https://example.com", None).own.is_none());
        // The pattern's text when the URL doesn't match; the URL first when both do.
        let by_text = AppLinkMenu { links: r"^(T\d+)$".into(), ..task_menu() };
        assert!(captures(&by_text, "taskboard://task/T4", Some("T4")).is_some_and(|c| &c[1] == "T4"));
        // A capture never becomes the program.
        let named = AppLinkMenu { links: r"(?P<id>T\d+)".into(), items: vec![run("${id}", &["tb", "${id}"])], ..task_menu() };
        let caps = captures(&named, "T7", None).unwrap();
        assert_eq!(expand_item(&named.items[0], &caps).run.unwrap(), ["tb", "T7"]);
        assert_eq!(expand_item(&LinkMenuItem { run: Some(vec!["$1".into()]), ..item("x") }, &caps).run.unwrap(), ["$1"]);
    }

    #[test]
    fn commands_and_preview_gate() {
        let menus = vec![task_menu(), pr_section()];
        assert_eq!(commands(&menus), ["run tb cancel $1", "run tb focus $1", "run tb task new --pr $0"]);
        assert_eq!(commands(&[AppLinkMenu { items: vec![LinkMenuItem { open: Some("x://$1".into()), ..item("Open") }], ..task_menu() }]), Vec::<String>::new());
        assert_eq!(shell_words(&["a b".into(), "it's".into()]), r"'a b' 'it'\''s'");
        let mut app = AppLinkActions { owner: "Taskboard".into(), menus, approved: false, approval: None };
        let done = run("Mark done", &["tb", "done", "T42"]);
        assert!(!preview_item_allowed(&app, &done));
        assert!(preview_item_allowed(&app, &LinkMenuItem { copy: Some("T42".into()), ..item("Copy") }));
        app.approved = true;
        assert!(preview_item_allowed(&app, &done));
        assert!(!preview_item_allowed(&app, &run("x", &["rm", "-rf", "/"])));
        assert!(!preview_item_allowed(&app, &LinkMenuItem { prompt: Some("hi".into()), ..item("Ask") }));
    }
}
