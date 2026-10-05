//! midna's shell startup files (`adopt.rs`, `src/shell/*`) against real shells: a home whose
//! startup files rebuild PATH and alias the agents to full paths, as people's do. The shims
//! here are stand-ins that print what they were asked to run. A shell that isn't installed
//! (fish, often) is skipped, unless `MIDNA_TEST_FISH` names one.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Home {
    root: PathBuf,
    /// Type the input slowly (a shell on a terminal).
    slow: bool,
}

impl Home {
    fn new(tag: &str) -> Home {
        let root = PathBuf::from(format!("/tmp/midna-sh-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["home/tools", "realbin", "shims", "mid"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let h = Home { root, slow: false };
        for a in ["claude", "codex"] {
            h.exe(&format!("shims/{a}"), &format!("#!/bin/sh\necho \"SHIM {a} real=$MIDNA_SHIM_REAL args=$*\"\n"));
            h.exe(&format!("realbin/{a}"), &format!("#!/bin/sh\necho \"REAL {a} $*\"\n"));
        }
        h.exe("home/tools/codex", "#!/bin/sh\necho \"TOOLS codex $*\"\n");
        h
    }

    fn p(&self, rel: &str) -> String {
        self.root.join(rel).to_string_lossy().into_owned()
    }

    fn exe(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }

    fn file(&self, rel: &str, body: &str) {
        std::fs::write(self.root.join(rel), body).unwrap();
    }

    /// Run `shell args` interactively with `input` on stdin; its stdout+stderr.
    fn run(&self, shell: &str, args: &[&str], env: &[(&str, String)], input: &str) -> String {
        let mut c = Command::new(shell);
        c.args(args).env_clear().env("HOME", self.p("home")).env("PATH", "/usr/bin:/bin").env("TERM", "dumb").env("MIDNA_SHIMS", self.p("shims"));
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        // A line at a time, as typed: a terminal (`script`) passes an early end of input on.
        for line in input.split_inclusive('\n') {
            std::thread::sleep(std::time::Duration::from_millis(if self.slow { 400 } else { 0 }));
            let _ = stdin.write_all(line.as_bytes());
        }
        if self.slow {
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `MIDNA_TEST_<SHELL>` names a shell that isn't installed (a fish unpacked somewhere).
fn installed(shell: &str) -> Option<String> {
    if let Ok(p) = std::env::var(format!("MIDNA_TEST_{}", shell.to_uppercase())) {
        return Some(p);
    }
    ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"].iter().map(|d| format!("{d}/{shell}")).find(|p| Path::new(p).is_file())
}

#[test]
fn zsh_keeps_the_shims_first_and_routes_aliases_and_full_paths() {
    let Some(zsh) = installed("zsh") else { return eprintln!("no zsh: skipped") };
    let h = Home::new("zsh");
    h.file("mid/.zshenv", midnad::adopt::ZSHENV);
    h.file("home/.zshenv", "export FROM_USER_ZSHENV=1\n");
    h.file("home/.zshrc", &format!("export PATH={}:/usr/bin:/bin\nalias codex='~/tools/codex --yolo'\n", h.p("realbin")));
    let input = format!("claude a\ncodex b\n{}/claude c\n~/tools/codex d\nprint -r -- \"env=$FROM_USER_ZSHENV zdotdir=${{ZDOTDIR-unset}}\"\nexit\n", h.p("realbin"));
    let out = h.run(&zsh, &["-il"], &[("ZDOTDIR", h.p("mid"))], &input);
    assert!(out.contains("SHIM claude real= args=a"), "PATH, after .zshrc rebuilt it:\n{out}");
    assert!(out.contains(&format!("SHIM codex real={} args=--yolo b", h.p("home/tools/codex"))), "an alias to a full path:\n{out}");
    assert!(out.contains(&format!("SHIM claude real={} args=c", h.p("realbin/claude"))), "a full path typed:\n{out}");
    assert!(out.contains(&format!("SHIM codex real={} args=d", h.p("home/tools/codex"))), "a ~ path typed:\n{out}");
    assert!(out.contains("env=1 zdotdir=unset"), "the user's .zshenv ran and ZDOTDIR is theirs again:\n{out}");
    assert!(!out.contains("REAL ") && !out.contains("TOOLS "), "{out}");
}

#[test]
fn bash_runs_the_login_files_keeps_the_shims_first_and_routes_aliases() {
    let Some(bash) = installed("bash") else { return eprintln!("no bash: skipped") };
    let h = Home::new("bash");
    h.file("mid/init.bash", midnad::adopt::BASH_INIT);
    h.file("home/.bash_profile", &format!("export PATH={}:/usr/bin:/bin\nexport FROM_PROFILE=1\nalias codex='~/tools/codex --yolo'\n", h.p("realbin")));
    let input = "claude a\ncodex b\necho \"profile=$FROM_PROFILE\"\nexit\n";
    let out = h.run(&bash, &["--init-file", &h.p("mid/init.bash"), "-i"], &[("MIDNA_BASH_LOGIN", "1".into())], input);
    assert!(out.contains("SHIM claude real= args=a"), "PATH, after .bash_profile rebuilt it:\n{out}");
    assert!(out.contains(&format!("SHIM codex real={} args=--yolo b", h.p("home/tools/codex"))), "an alias to a full path:\n{out}");
    assert!(out.contains("profile=1"), "the login files ran:\n{out}");
    // Not a login shell: ~/.bashrc, as bash would.
    h.file("home/.bashrc", "export FROM_BASHRC=1\n");
    let out = h.run(&bash, &["--init-file", &h.p("mid/init.bash"), "-i"], &[], "echo \"rc=$FROM_BASHRC\"\nclaude e\nexit\n");
    assert!(out.contains("rc=1") && out.contains("SHIM claude real= args=e"), "{out}");
}

#[test]
fn fish_keeps_the_shims_first_and_routes_aliases() {
    let Some(fish) = installed("fish") else { return eprintln!("no fish: skipped") };
    let mut h = Home::new("fish");
    h.slow = true;
    let conf = h.root.join("mid/fish/vendor_conf.d");
    std::fs::create_dir_all(&conf).unwrap();
    std::fs::write(conf.join("midna.fish"), midnad::adopt::FISH_CONF).unwrap();
    std::fs::create_dir_all(h.root.join("home/.config/fish")).unwrap();
    h.file("home/.config/fish/config.fish", &format!("set -gx PATH {} /usr/bin /bin\nalias codex='~/tools/codex --yolo'\n", h.p("realbin")));
    // fish fires its prompt events only on a terminal: `script` gives it one.
    let out = h.run("/usr/bin/script", &["-q", "/dev/null", &fish, "-il"], &[("XDG_DATA_DIRS", format!("{}:/usr/local/share:/usr/share", h.p("mid")))], "claude a\ncodex b\necho xdg=(set -q XDG_DATA_DIRS; and echo set; or echo unset)\nexit\n");
    assert!(out.contains("SHIM claude real= args=a"), "PATH, after config.fish rebuilt it:\n{out}");
    assert!(out.contains(&format!("SHIM codex real={} args=--yolo b", h.p("home/tools/codex"))), "an alias to a full path:\n{out}");
    assert!(out.contains("xdg=unset"), "XDG_DATA_DIRS is the user's again:\n{out}");
}

#[test]
fn shim_without_midna_runs_what_the_alias_named_or_the_next_on_path() {
    let h = Home::new("shim");
    let shim = h.root.join("shims/claude");
    std::fs::write(&shim, midnad::adopt::shim_script("/nonexistent/midna", "claude")).unwrap();
    std::fs::set_permissions(&shim, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let run = |env: &[(&str, String)]| {
        let mut c = Command::new(&shim);
        c.arg("x").env_clear().env("PATH", format!("{}:{}:/usr/bin:/bin", h.p("shims"), h.p("realbin")));
        for (k, v) in env {
            c.env(k, v);
        }
        String::from_utf8_lossy(&c.output().unwrap().stdout).into_owned()
    };
    assert_eq!(run(&[]), "REAL claude x\n", "skips itself on PATH");
    h.exe("home/tools/claude", "#!/bin/sh\necho \"TOOLS claude $*\"\n");
    assert_eq!(run(&[("MIDNA_SHIM_REAL", h.p("home/tools/claude"))]), "TOOLS claude x\n");
}
