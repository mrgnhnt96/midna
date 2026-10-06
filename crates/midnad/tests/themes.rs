//! themes.list (built-ins, custom files, errors, settings) and themes.report (human only,
//! persisted, recolors a running terminal) against a real in-process daemon.
mod common;
use common::*;
use midna_proto::error::HUMAN_ONLY;
use serde_json::json;

#[test]
fn themes_list_and_report() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let mut a = d.agent(None);
    std::fs::create_dir_all(d.home.join("themes")).unwrap();
    std::fs::write(d.home.join("themes/pinky.json"), r##"{"name":"Pinky","extends":"nord","colors":{"accent":"#FF79C6"}}"##).unwrap();
    std::fs::write(d.home.join("themes/broken.json"), "{").unwrap();

    let v = call(&mut a, "themes.list", json!({}));
    let ids: Vec<&str> = v["themes"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 9, "{ids:?}");
    assert!(ids.contains(&"pinky") && ids.contains(&"tokyo-night"));
    let pinky = v["themes"].as_array().unwrap().iter().find(|t| t["id"] == "pinky").unwrap();
    assert_eq!(pinky["colors"]["accent"], "#FF79C6");
    assert_eq!(pinky["kind"], "dark");
    assert_eq!(v["errors"].as_array().unwrap().len(), 1);
    assert_eq!((v["setting"].as_str(), v["dark"].as_str(), v["light"].as_str()), (Some("system"), Some("twilight"), Some("daylight")));
    assert!(v["showing"].is_null());

    // Agents switch with settings; the legacy values still work.
    call(&mut a, "settings.set", json!({ "key": "theme", "value": "pinky" }));
    call(&mut a, "settings.set", json!({ "key": "theme", "value": "light" }));
    call(&mut a, "settings.set", json!({ "key": "theme.colors", "value": ["accent = #FF79C6", "nord:need = #EBCB8B"] }));

    // The GUI reports the terminal colors it shows; agents can't.
    let mut ansi: Vec<String> = (0..16).map(|_| "#101010".to_string()).collect();
    ansi[1] = "#010203".into();
    let colors = json!({ "theme": "pinky", "foreground": "#EEEEEE", "background": "#111111", "ansi": ansi });
    assert_eq!(call_err(&mut a, "themes.report", json!({ "colors": colors })).code, HUMAN_ONLY);
    call(&mut h, "themes.report", json!({ "colors": colors }));
    assert_eq!(call(&mut a, "themes.list", json!({}))["showing"], "pinky");
    assert!(d.home.join("terminal-colors.json").exists());
    assert_eq!(call_err(&mut h, "themes.report", json!({ "colors": { "theme": "x", "foreground": "#EEEEEE", "background": "#111111", "ansi": ["#000000"] } })).code, -32602);
}
