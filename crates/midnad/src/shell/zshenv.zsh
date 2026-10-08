# midna shell integration (written by midnad; setting agents.adopt_typed).
# zsh reads this first because midna points ZDOTDIR here: put ZDOTDIR back, run the user's
# own .zshenv (zsh then reads their other startup files from there as usual), and before
# each prompt (so after those files ran) keep midna's shims first on PATH and send `claude`
# / `codex` aliases and full paths through them.
if [[ -n "${MIDNA_ZDOTDIR-}" ]]; then
  ZDOTDIR="$MIDNA_ZDOTDIR"
else
  unset ZDOTDIR
fi
unset MIDNA_ZDOTDIR
[[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]] && source "${ZDOTDIR:-$HOME}/.zshenv"
if [[ -o interactive && -n "${MIDNA_SHIMS-}" ]]; then
  typeset -g _midna_routed=""
  _midna_precmd() {
    [[ "${path[1]}" == "$MIDNA_SHIMS" ]] || path=("$MIDNA_SHIMS" ${path:#$MIDNA_SHIMS})
    # Aliases only change in startup files or by hand, and the agents on PATH with PATH.
    [[ "$_midna_routed" == "$PATH|${aliases[claude]-}|${aliases[codex]-}" ]] && return
    local a real first rest tilde
    for a in claude codex; do
      # `alias claude=~/.claude/local/claude --flag`: run that binary through the shim.
      if (( ${+aliases[$a]} )) && [[ "${aliases[$a]}" != MIDNA_SHIM_REAL=* ]]; then
        first="${aliases[$a]%% *}"
        rest="${aliases[$a]#"$first"}"
        real="${first/#\~/$HOME}"
        if [[ "${real:t}" == "$a" && "$real" == /* && -x "$real" ]]; then
          aliases[$a]="MIDNA_SHIM_REAL=${(q)real} ${(q)MIDNA_SHIMS}/$a$rest"
          aliases[$real]="MIDNA_SHIM_REAL=${(q)real} ${(q)MIDNA_SHIMS}/$a"
          # (A quoted subscript would keep its quotes in the key, hence the variable.)
          tilde="~${real#$HOME}"
          [[ "$real" == "$HOME"/* ]] && aliases[$tilde]="MIDNA_SHIM_REAL=${(q)real} ${(q)MIDNA_SHIMS}/$a"
        fi
      fi
      # A binary typed in full (`~/.local/bin/claude`): zsh aliases may contain slashes.
      for real in ${(f)"$(whence -pa "$a" 2>/dev/null)"}; do
        [[ "${real:h}" == "$MIDNA_SHIMS" || -z "$real" ]] && continue
        aliases[$real]="MIDNA_SHIM_REAL=${(q)real} ${(q)MIDNA_SHIMS}/$a"
        tilde="~${real#$HOME}"
        [[ "$real" == "$HOME"/* ]] && aliases[$tilde]="MIDNA_SHIM_REAL=${(q)real} ${(q)MIDNA_SHIMS}/$a"
      done
    done
    _midna_routed="$PATH|${aliases[claude]-}|${aliases[codex]-}"
  }
  autoload -Uz add-zsh-hook && add-zsh-hook precmd _midna_precmd
fi
if [[ -o interactive ]]; then
  # Prompt marks (OSC 133), so midna can tell the input from the prompt and the output (a click
  # moves the cursor anywhere in the input): A before the prompt, B where the input starts, C
  # when the command runs. B is sent when zle starts on a line, hooked at the first prompt so
  # it's added after the user's own zle-line-init (oh-my-zsh defines one in .zshrc).
  _midna_mark_prompt() {
    print -n '\e]133;A\a'
    if [[ -z "${_midna_marking-}" ]]; then
      typeset -g _midna_marking=1
      autoload -Uz add-zle-hook-widget && add-zle-hook-widget line-init _midna_mark_input
    fi
  }
  _midna_mark_input() { print -n '\e]133;B\a' }
  _midna_mark_output() { print -n '\e]133;C\a' }
  autoload -Uz add-zsh-hook && add-zsh-hook precmd _midna_mark_prompt && add-zsh-hook preexec _midna_mark_output
fi
