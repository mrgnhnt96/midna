#!/usr/bin/env python3
"""PreToolUse guard: agents working on midna never touch the production install.

Production = /Applications/Midna.app, bundle id com.mrgnhnt.midna, LaunchAgent
com.mrgnhnt.midna.daemon, home ~/Library/Application Support/com.mrgnhnt.midna, the
~/.local/bin/midna link and the Finder Quick Action. The human uses it as their daily terminal;
a bad build or a stopped daemon hangs up every live shell. Agents use Midna Dev
(scripts/dev-app.sh, com.mrgnhnt.midna.dev) or a temp MIDNA_HOME instead. See AGENTS.md.

Reading production files (logs, settings) is fine; anything that writes, launches, quits,
signals or reconfigures production is refused (exit 2, the reason goes back to the agent).
This is a guard against mistakes, not a sandbox.
"""
import json, os, re, shlex, subprocess, sys

HOME = os.path.expanduser("~")
PROD_HOME = f"{HOME}/Library/Application Support/com.mrgnhnt.midna"
# The production id, label or home, but not com.mrgnhnt.midna.dev / .test / .cli / .secret…
PROD_ID = re.compile(r"com\.mrgnhnt\.midna(\.daemon)?(?![\w.-])")
PROD_PATHS = ["/Applications/Midna.app", "~/.local/bin/midna", f"{HOME}/.local/bin/midna",
              "Library/Services/Open in Midna.workflow"]
READ_ONLY = {"cat", "head", "tail", "less", "more", "grep", "egrep", "rg", "ag", "ls", "stat", "file",
             "wc", "jq", "diff", "cmp", "echo", "printf", "mdls", "spctl", "otool", "strings", "shasum",
             "md5", "readlink", "realpath", "dirname", "basename", "test", "[", "git", "cargo",
             "sed", "awk", "find", "plutil", "codesign", "defaults", "pgrep", "ps", "log", "du"}
MUTATING_FLAGS = {"sed": ("-i",), "awk": ("-i",), "find": ("-delete", "-exec", "-execdir"),
                  "plutil": ("-convert", "-insert", "-replace", "-remove"), "codesign": ("-s", "--sign", "--remove-signature"),
                  "defaults": ("write", "delete", "import"), "git": ("clean",)}
DAEMON_CONTROL = re.compile(r"\bmidna\s+daemon\s+(stop|restart|upgrade|start)\b|\bmidna\s+call\s+daemon\.(stop|restart|upgrade)"
                            r"|midna-app\b.*--uninstall|\bmidnad\b.*--install-self")


def deny(why):
    print(f"Blocked: {why}\nThe production Midna (/Applications/Midna.app, its daemon and home) is the human's "
          "live terminal; agents never touch it. Use Midna Dev (scripts/dev-app.sh) or a temp MIDNA_HOME, "
          "or ask the human to do this themselves. See AGENTS.md.", file=sys.stderr)
    sys.exit(2)


HEREDOC = re.compile(r"<<-?\s*(['\"]?)(\w+)\1[^\n]*\n.*?\n\s*\2[ \t]*(?=\n|$)", re.S)
OPERATORS = {";", "&&", "||", "|", "&", ";;", "|&"}


def strip_heredocs(cmd):
    """Heredoc bodies are data (commit messages, notes), not commands. The `<<EOF` line stays."""
    return HEREDOC.sub(lambda m: m.group(0).split("\n", 1)[0], cmd)


def segments(cmd):
    """(text, words) per simple command, split on ; && || | & and newlines outside quotes."""
    out = []
    for line in strip_heredocs(cmd).split("\n"):
        lex = shlex.shlex(line, posix=True, punctuation_chars=";&|")
        lex.whitespace_split = True
        try:
            toks = list(lex)
        except ValueError:  # unbalanced quotes (a multi-line string): fall back to a plain split
            toks = re.split(r"(\|\||&&|[;|&])|\s+", line)
            toks = [t for t in toks if t]
        cur = []
        for t in toks + [";"]:
            if t in OPERATORS or set(t) <= set(";&|"):
                if cur:
                    out.append((" ".join(cur), cur))
                cur = []
            else:
                cur.append(t)
    return out


def words(w):
    w = list(w)
    while w and re.match(r"^\w+=", w[0]):  # leading VAR=value
        w = w[1:]
    while w and w[0] in ("sudo", "env", "command", "exec", "nohup", "time", "xargs"):
        w = w[1:]
        while w and (re.match(r"^\w+=", w[0]) or w[0].startswith("-")):
            w = w[1:]
    return w


def touches_prod(seg):
    return bool(PROD_ID.search(seg)) or any(p in seg for p in PROD_PATHS) or PROD_HOME in seg


def targets_dev_or_temp(cmd):
    """The command itself points MIDNA_HOME / MIDNA_SOCKET away from production."""
    for m in re.finditer(r"MIDNA_(?:HOME|SOCKET)=(\"[^\"]*\"|'[^']*'|\S+)", cmd):
        v = os.path.expanduser(m.group(1).strip("\"'").replace("$HOME", HOME))
        if PROD_HOME not in v or "com.mrgnhnt.midna." in v:
            return True
    return False


def env_is_prod():
    home, sock = os.environ.get("MIDNA_HOME", ""), os.environ.get("MIDNA_SOCKET", "")
    if not home and not sock:
        return True  # the default is production
    return any(v.startswith(PROD_HOME) and not v.startswith(PROD_HOME + ".") for v in (home, sock) if v)


def is_prod_daemon(pid):
    try:
        out = subprocess.run(["ps", "-o", "command=", "-p", pid], capture_output=True, text=True).stdout
    except OSError:
        return False
    return (PROD_HOME + "/") in out or "/Applications/Midna.app/" in out


def check_bash(cmd):
    cmd = strip_heredocs(cmd)
    if re.search(r"\b(pkill|killall)\b[^;&|\n]*\bmidna", cmd, re.I):
        deny("pkill/killall by name also hits the production midnad. Stop test daemons by PID.")
    if re.search(r"\bopen\b[^;&|\n]*-a\s+['\"]?Midna(?!\s*Dev)(?:\.app)?['\"]?(\s|$)", cmd):
        deny("opening the production app.")
    if DAEMON_CONTROL.search(cmd) and not targets_dev_or_temp(cmd) and env_is_prod():
        deny("this daemon command would act on the production daemon (MIDNA_HOME/MIDNA_SOCKET are unset or production).")
    for seg, toks in segments(cmd):
        w = words(toks)
        if not w:
            continue
        prog = os.path.basename(w[0])
        script = os.path.basename(w[1]) if prog in ("bash", "sh", "zsh") and len(w) > 1 else prog
        if script == "reinstall.sh":
            deny("scripts/reinstall.sh rebuilds and restarts the production app. Use scripts/dev-app.sh.")
        if prog == "kill":
            for pid in (a for a in w[1:] if a.isdigit()):
                if is_prod_daemon(pid):
                    deny(f"pid {pid} is the production Midna app or daemon.")
        if not touches_prod(seg):
            continue
        for i, t in enumerate(toks):  # > file, >>file, 2> file, &> file
            m = re.match(r"^[0-9&]?>>?(.*)$", t)
            target = m and (m.group(1) or (toks[i + 1] if i + 1 < len(toks) else ""))
            if target and touches_prod(target):
                deny("redirecting output into a production path.")
        flags = MUTATING_FLAGS.get(prog, ())
        mutates = any(a in flags or (a.startswith("-i") and "-i" in flags) for a in w[1:])  # sed -i.bak
        if prog in READ_ONLY and not mutates:
            continue
        deny(f"`{prog}` on a production path or id (`{seg[:120]}`).")


def check_path(path):
    p = os.path.realpath(os.path.expanduser(path or ""))
    prod = [os.path.realpath(x) for x in ("/Applications/Midna.app", PROD_HOME, f"{HOME}/.local/bin/midna",
                                          f"{HOME}/Library/Services/Open in Midna.workflow")]
    if any(p == x or p.startswith(x + "/") for x in prod):
        deny(f"writing {path}.")


def main():
    try:
        data = json.load(sys.stdin)
    except ValueError:
        return
    tool, inp = data.get("tool_name", ""), data.get("tool_input") or {}
    if tool == "Bash":
        check_bash(inp.get("command", ""))
    elif tool in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
        check_path(inp.get("file_path") or inp.get("notebook_path"))


if __name__ == "__main__":
    main()
