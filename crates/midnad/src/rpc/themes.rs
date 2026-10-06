//! `themes.*`: the app resolves the theme (it alone knows whether macOS is dark) and reports
//! the terminal colors it shows with `themes.report`; midnad applies them to every engine
//! (`engine::set_theme_colors`) and keeps them in `$MIDNA_HOME/terminal-colors.json`, so
//! terminals keep the theme's colors across a daemon restart before the app reconnects.
use super::{R, ok};
use crate::daemon::Daemon;
use crate::engine::{ThemeColors, set_theme_colors};
use midna_proto::themes;
use midna_proto::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What the app last reported (also the source of `themes.list`'s `showing`).
static SHOWN: Mutex<Option<TerminalColors>> = Mutex::new(None);

fn path(home: &Path) -> PathBuf {
    home.join("terminal-colors.json")
}

fn to_engine(c: &TerminalColors) -> Result<ThemeColors, String> {
    let hex = |what: &str, v: &str| themes::parse_hex(v).ok_or_else(|| format!("{what} `{v}` is not #RRGGBB"));
    if c.ansi.len() != 16 {
        return Err(format!("ansi must have 16 colors, got {}", c.ansi.len()));
    }
    let mut ansi = [[0u8; 3]; 16];
    for (i, a) in c.ansi.iter().enumerate() {
        ansi[i] = hex("ansi", a)?;
    }
    Ok(ThemeColors {
        fg: hex("foreground", &c.foreground)?,
        bg: hex("background", &c.background)?,
        cursor: c.cursor.as_deref().map(|v| hex("cursor", v)).transpose()?,
        ansi,
    })
}

/// Apply the saved colors at startup (before any engine exists).
pub fn load(home: &Path) {
    let Ok(text) = std::fs::read_to_string(path(home)) else { return };
    let Ok(c) = serde_json::from_str::<TerminalColors>(&text) else { return };
    if let Ok(e) = to_engine(&c) {
        set_theme_colors(Some(e));
        *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
    }
}

pub fn report(d: &Daemon, p: ThemesReportParams) -> R {
    let c = p.colors;
    let e = to_engine(&c).map_err(RpcError::bad_params)?;
    let changed = {
        let mut cur = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
        let changed = cur.as_ref() != Some(&c);
        *cur = Some(c.clone());
        changed
    };
    if changed {
        set_theme_colors(Some(e));
        if let Ok(bytes) = serde_json::to_vec_pretty(&c) {
            let p = path(&d.cfg.home);
            let tmp = p.with_extension("json.tmp");
            let _ = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &p));
        }
        // Idle terminals build no frames: poke each engine so it redraws in the new colors.
        let handles: Vec<_> = d.core().rt.values().cloned().collect();
        for h in handles {
            let _ = h.tx.try_send(crate::term::EngineMsg::With(Box::new(|e| e.sync_colors())));
        }
    }
    ok(OkResult { ok: true })
}

pub fn list(d: &Daemon) -> R {
    let dir = themes::themes_dir(&d.cfg.home);
    let (all, errors) = themes::load_all(&dir);
    let (setting, dark, light) = {
        let c = d.core();
        (c.state.setting_str("theme"), c.state.setting_str("theme.dark"), c.state.setting_str("theme.light"))
    };
    let showing = SHOWN.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|c| c.theme.clone());
    ok(ThemesListResult {
        themes: all.iter().map(|t| t.info()).collect(),
        errors,
        dir: dir.display().to_string(),
        dark: themes::choose("system", &dark, &light, true),
        light: themes::choose("system", &dark, &light, false),
        setting,
        showing,
        format: themes::FILE_FORMAT.to_string(),
    })
}
