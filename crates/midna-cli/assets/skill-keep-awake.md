## Keeping the Mac awake

The human's Mac idle-sleeps after a minute. Keep-awake holds it awake during their work hours
while there is work (an agent working, queued input, an agent waiting to resume, a schedule
trigger or wakeup due), so your queued and scheduled work still runs when they step away. It only
stops idle sleep: the display sleeps, the screen locks, closing the lid still sleeps the Mac.
It is off until the human (or an agent they asked) turns it on.

- `midna keep-awake` says whether it is held now, why or why not, and when the hours open or close.
- `midna keep-awake on|off`, `hours 8am 6pm weekdays`, `day fri 9am-3pm`, `day sat off`,
  `today off|on|until 5pm|for 5h|clear` (`for 5h` / `until 1am` alone too), `mode with_work|always`, `battery 20`, `linger 5`.
- **Waking a sleeping Mac** (`keep_awake.wake`, `midna keep-awake wake on|off`): midnad asks
  macOS to wake the Mac 2 minutes before the next work due inside the hours (a schedule
  trigger, a message queued `--at` a time, an agent's wakeup) and holds it until that work runs,
  so a 7 AM trigger runs on a Mac that slept all night. It needs a one-time admin grant,
  `midna keep-awake wake setup` (human only: macOS asks for their password). A closed lid still
  keeps a laptop asleep. To start work at a time while they're away: a schedule trigger at that
  time, hours that include it, and wake on.
- The same through RPC (`midna call`, MCP `keep_awake_status` / `keep_awake_set`), the contract
  other tools build on:

```
keep_awake.status {}  ->
  { held, reason: work|always|disabled|outside_hours|day_off|today_off|battery_low|no_work|failed,
    line, window_open, next_on?, next_off? (RFC 3339), work: ["2 agents working", …],
    held_since?, battery?: {percent, on_ac, low}, schedule: "9 AM–6 PM weekdays; Fri 9 AM–3 PM",
    settings: {enabled, mode: with_work|always, start: "HH:MM", end: "HH:MM", days: ["mon", …],
               hours: {"fri": "09:00-15:00", "sat": "off", "sun": "all day"}, min_battery, linger_mins, wake},
    today?: {date: "YYYY-MM-DD", on, until?: "HH:MM", until_date?: "YYYY-MM-DD", line},
    wake: {ready, next? (RFC 3339), reason?: "Morning kickoff at 7 AM", line, error?} }

keep_awake.set { enabled? (alias on), mode?, start?, end?, days?, hours?, min_battery?, linger_mins?, wake?, today? }
  -> keep_awake.status
  start/end: 8am | 8:30 PM | 17:30 (end at or before start runs past midnight; equal = all day)
  days:      weekdays | weekends | daily | mon-fri | "mon,wed,fri" | ["sat","sun"]
  hours:     {"fri": "9am-3pm", "sat": "off", "sun": "all day", "mon": null}  merges by day
             (null = back to the schedule); a list or string of `day = hours` rules replaces them all
  today:     off | on | until 5pm | for 5h | {"on": true, "until": "17:00"} | {"on": true, "for": "5h"} | clear
             (off = the rest of today, on = until midnight; on until a time, even past midnight
             (`for 5h` at 8 PM, `until 1am`; 24 h at most), then the schedule again. Needs
             keep-awake enabled)

keep_awake.wake_setup { remove? }  -> keep_awake.status   (human only: installs or removes the grant)
```

Only the fields given change, and one bad field saves none (error -32602 names it). The settings
are also ordinary `keep_awake.*` settings. Event `keep_awake.changed` (data = the status) fires
when it is taken or released.
