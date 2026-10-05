# midna shell integration (written by midnad; setting agents.adopt_typed).
# fish reads this vendor_conf.d file because midna puts its directory first in XDG_DATA_DIRS:
# put XDG_DATA_DIRS back, and keep midna's shims first on PATH. fish reads vendor files before
# the user's config.fish, so the shims go back to the front whenever PATH changes (and before
# each prompt). An `alias claude=/full/path …` (a function fish describes as that alias) is
# redefined to run that binary through the shim. (fish function names can't contain a slash,
# so a binary typed in full runs as typed.)
if set -q MIDNA_XDG_DATA_DIRS
    set -gx XDG_DATA_DIRS (string split : -- $MIDNA_XDG_DATA_DIRS)
    set -e MIDNA_XDG_DATA_DIRS
else
    set -e XDG_DATA_DIRS
end
if status is-interactive; and set -q MIDNA_SHIMS
    function __midna_shims_first --on-variable PATH --on-event fish_prompt
        if test "$PATH[1]" != "$MIDNA_SHIMS"
            set -gx PATH $MIDNA_SHIMS (string match -v -- $MIDNA_SHIMS $PATH)
        end
    end
    function __midna_route_aliases --on-event fish_prompt --on-event fish_preexec
        for a in claude codex
            functions -q $a; or continue
            set -l def (functions --details --verbose $a)[5]
            string match -q -- "alias $a=*" $def; or continue
            set -l line (string replace -- "alias $a=" "" $def)
            set -l first (string split -m1 ' ' -- $line)[1]
            set -l rest (string sub -s (math (string length -- $first) + 1) -- $line)
            set -l real (string replace -r '^~' $HOME -- $first)
            test (path basename -- $real) = $a; and string match -q '/*' -- $real; and test -x $real; or continue
            eval "function $a --wraps $a --description "(string escape -- "midna: alias $a=$line")"; MIDNA_SHIM_REAL="(string escape -- $real)" "(string escape -- $MIDNA_SHIMS/$a)"$rest \$argv; end"
        end
    end
    __midna_shims_first
end
