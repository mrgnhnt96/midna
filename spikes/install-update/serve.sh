#!/bin/zsh
# Usage: ./serve.sh <version>  -> writes srv/latest.json pointing at dist/<version>
# (python http.server on 127.0.0.1:8765 serves srv/; start once with: python3 -m http.server -d srv -b 127.0.0.1 8765)
set -eu
D=${0:A:h}; V=$1; mkdir -p $D/srv
cp $D/dist/$V/Midna.app.tar.gz $D/srv/Midna-$V.app.tar.gz
python3 - "$V" "$D" <<'P'
import json, sys, datetime
v, d = sys.argv[1], sys.argv[2]
sig = open(f"{d}/dist/{v}/Midna.app.tar.gz.sig").read().strip()
json.dump({"version": v, "notes": f"spike {v}", "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
  "platforms": {"macos-aarch64": {"url": f"http://127.0.0.1:8765/Midna-{v}.app.tar.gz", "signature": sig, "format": "app"}}},
  open(f"{d}/srv/latest.json", "w"), indent=2)
P
cat $D/srv/latest.json
