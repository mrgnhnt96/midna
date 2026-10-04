#!/bin/sh
sleep 0.5
awk 'BEGIN{for(i=1;i<=2000000;i++) printf "%d\n", i}'
sleep 30
