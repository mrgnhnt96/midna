#!/bin/sh
printf '\033[1mbold\033[0m \033[3mitalic\033[0m \033[1;3mbolditalic\033[0m \033[4munder\033[0m \033[9mstrike\033[0m \033[2mfaint\033[0m \033[7minverse\033[0m\n'
i=0; while [ $i -lt 256 ]; do printf "\033[48;5;${i}m  "; i=$((i+1)); [ $((i % 32)) -eq 0 ] && printf '\033[0m\n'; done
c=0; while [ $c -lt 64 ]; do r=$((c*4)); printf "\033[48;2;${r};$((255-r));128m "; c=$((c+1)); done; printf '\033[0m\n'
printf 'emoji: 😀 🚀 👍🏽 👨‍👩‍👧 🇯🇵 | CJK: 日本語 中文 한국어 | combining: e\xcc\x81 a\xcc\x8a n\xcc\x83 | ascii after\n'
printf '┌──┬──┐ ╔══╗ ▁▂▃▄▅▆▇█ ⠿ │\n├──┼──┤ ║  ║ ░▒▓ \n└──┴──┘ ╚══╝  →←↑↓ λ π Ω ≠ ≤\n'
printf '\033[31mred \033[32mgreen \033[33myellow \033[34mblue \033[35mmagenta \033[36mcyan \033[91mbright-red \033[0m\n'
