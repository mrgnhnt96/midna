#!/bin/zsh
# usage: ./bench.sh <a|braw|bsnap> <suite: main|tui>
D=${0:A:h}; cd $D
export MIDNA_TRANSPORT=$1 MIDNA_LABEL=$1 MIDNA_SHOTS=$D/shots
if [[ $2 == main ]]; then
  export MIDNA_W=900 MIDNA_H=600
  export MIDNA_BENCH="wait:500,latency:160,flood:cat data/big.txt,type:clear\r,wait:300,flood:seq 1 5000000,type:clear\r,wait:300,idle:|10"
else
  export MIDNA_W=1700 MIDNA_H=1040
  export MIDNA_BENCH="wait:300,tui:top -s 1|6,wait:500,tui:vim -u NONE -N data/big.txt|6|\f\f\b,wait:300,tui:python3 data/rainbow.py 9|6,shot:big"
fi
./target/release/gterm 2>&1 | grep -v '^cell'
