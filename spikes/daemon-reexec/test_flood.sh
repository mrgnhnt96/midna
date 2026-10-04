#!/bin/zsh
# Upgrade mid-flood: does any byte get lost/duplicated when the kernel buffer is full?
D=${0:A:h}; R=$D/run-flood; S=$R/s.sock
rm -rf $R; mkdir -p $R; cp $D/target/release/midnad $R/midnad-v1; cp $D/target/release/midnad $R/midnad-v2
c() { $R/midnad-v1 client $S cmd "$1"; }
$R/midnad-v1 serve $S 2> $R/daemon.log & DPID=$!; sleep 0.3
c "$R/midnad-v2 --preflight /dev/null" >/dev/null 2>&1  # warm nothing; ignore
c "SPAWN $D/gen.sh"
$R/midnad-v1 client $S attach 1 > $R/out.txt 2> $R/client.err & CPID=$!
sleep 0.8
for i in 1 2 3; do c "UPGRADE $R/midnad-v$(( i % 2 == 1 ? 2 : 1 ))" >/dev/null; sleep 0.3; done
sleep 4
kill $CPID; pkill -P $DPID; kill $DPID; wait 2>/dev/null
grep -E 'pending|preflight ok|resumed' $R/daemon.log
cat $R/client.err
tr -d '\r' < $R/out.txt | awk '{ if ($1 != NR) { print "MISMATCH at line", NR, "got", $1; bad++; if (bad>5) exit } } END { print "lines:", NR, "last:", $1 }'
