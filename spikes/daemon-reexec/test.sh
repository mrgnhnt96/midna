#!/bin/zsh
# End-to-end re-exec test. Usage: ./test.sh
set -u
D=${0:A:h}; R=$D/run; S=$R/s.sock
rm -rf $R; mkdir -p $R
cp $D/target/release/midnad $R/midnad-v1; cp $D/target/release/midnad $R/midnad-v2
printf '#!/bin/sh\necho "v2 cannot parse state: simulated" >&2\nexit 3\n' > $R/midnad-broken; chmod +x $R/midnad-broken
seq 1 200 | sed 's/^/vim file line /' > $R/file.txt
c() { $R/midnad-v1 client $S cmd "$1"; }
pss() { ps -o pid,ppid,pgid,sess,tpgid,tty,stat,command -p "$1" | sed 1d; }
TERM=xterm-256color $R/midnad-v1 serve $S 2> $R/daemon.log &
DPID=$!; sleep 0.3
echo "daemon pid=$DPID"
c "SPAWN zsh -c 'trap \"echo GOT_HUP \$\$ >> $R/hup.log\" HUP; echo ZPID \$\$; i=0; while true; do echo C \$i; i=\$((i+1)); sleep 0.1; done'"
c "SPAWN vim -u NONE -N -c 'set nu' $R/file.txt"
command -v claude >/dev/null && c "SPAWN sh -c 'claude --version; echo CLAUDE_DONE; sleep 30'"
$R/midnad-v1 client $S attach 1 > $R/client1.out 2> $R/client1.err &
CPID=$!
sleep 1.5
c "SEND 2 $(printf '50G' | xxd -p)"   # move vim cursor to line 50
sleep 0.5
c LIST | tee $R/list0.txt
Z=$(awk '/^id=1 /{print $2}' $R/list0.txt | cut -d= -f2); V=$(awk '/^id=2 /{print $2}' $R/list0.txt | cut -d= -f2)
echo "--- ps before"; pss $DPID; pss $Z; pss $V
c "DUMP 2" > $R/vim_before.txt
echo "=== UPGRADE to broken v2 (expect abort)"
c "UPGRADE $R/midnad-broken"
c LIST
sleep 1
c "DUMP 2" > $R/vim_after_abort.txt
echo "=== UPGRADE to v2"
c "UPGRADE $R/midnad-v2" ; echo "(upgrade cmd returned rc=$?)"
sleep 0.5
c LIST | tee $R/list1.txt
c "DUMP 2" > $R/vim_after.txt
echo "--- ps after"; pss $DPID; pss $Z; pss $V
echo "=== send keys to vim after upgrade"
c "SEND 2 $(printf 'oTYPED AFTER UPGRADE\033' | xxd -p)"
sleep 0.5
c "DUMP 2" > $R/vim_typed.txt
echo "=== upgrade again v2 -> v1"
c "UPGRADE $R/midnad-v1"
sleep 1.5
c LIST
c "DUMP 3" > $R/claude.txt 2>/dev/null
kill $CPID
echo "=== hup log:"; cat $R/hup.log 2>/dev/null || echo "(none - no SIGHUP)"
# cleanup: quit vim, kill zsh, kill daemon
pkill -P $DPID; sleep 0.2; kill $DPID; wait 2>/dev/null
echo "leftover:"; pgrep -fl "$R" || echo none
