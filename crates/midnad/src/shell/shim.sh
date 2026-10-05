#!/bin/sh
# midna: `@AGENT@` typed into a midna shell terminal runs under midna (setting agents.adopt_typed).
# Outside midna, or without midna, this is the binary an alias named (MIDNA_SHIM_REAL) or the
# next `@AGENT@` on PATH.
cli=@CLI@
if [ -n "$MIDNA_SESSION" ] && [ -x "$cli" ]; then exec "$cli" shim @AGENT@ "$@"; fi
if [ -n "$MIDNA_SHIM_REAL" ] && [ -x "$MIDNA_SHIM_REAL" ]; then
  real=$MIDNA_SHIM_REAL
  unset MIDNA_SHIM_REAL
  exec "$real" "$@"
fi
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
IFS=:
for d in $PATH; do
  [ -n "$d" ] || continue
  [ "$(CDPATH= cd -- "$d" 2>/dev/null && pwd -P)" = "$here" ] && continue
  [ -x "$d/@AGENT@" ] && exec "$d/@AGENT@" "$@"
done
echo "@AGENT@: command not found" >&2
exit 127
