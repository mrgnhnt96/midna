## Links

- `midna links` lists the links, PRs, artifacts and files that came up in your conversation;
  the human sees them under the header's links button (⌘L). Pin what they will want to come back
  to (`midna links pin <id|url>`: the PR you opened, the design you published, the doc they sent),
  and `midna links add <url|path> --title T --why "note"` for something important that never
  showed up as a link. Don't pin everything: a few pins are useful, twenty aren't.
  Links remember the prompts they came up in: `midna links --kind file --turn last` lists the
  files edited since the human's last prompt (`--turn N` for prompt N of `midna prompts`).
  After you merge, close or mark ready a PR, or re-run a workflow, `midna links refresh <url>` so
  its link's color and hover card update now instead of in a minute or two.
- An app that owns refs (like `T42`) makes them clickable in terminal text by registering its
  own link patterns: `midna links patterns set <app> --rule '<regex> = <url template> [agents]'`
  (RPC `links.patterns.register {owner, patterns}`; each call replaces that app's set, `[]` or
  `clear` removes it). They persist, apply alongside `terminal.link_patterns`, and show read-only
  in Settings ▸ Terminal ▸ Links and paths.
- A link pattern (yours in `terminal.link_patterns`, or an app's) can fill its hover card: end
  the rule with `preview:"<command> $1"`, e.g.
  `\b(RRD-\d+)\b = https://acme.atlassian.net/browse/$1 agents preview:"jira-card $1"`. The
  command (no shell; `$1` … filled from the match) prints JSON
  `{"title": "RRD-12 Fix login", "lines": ["In Progress", "Morgan Hunt"]}` and the card shows it
  instead of only the URL. It runs on the login shell's PATH with a 5s limit and its answer is
  kept for 15s; on failure or bad JSON the card falls back to the plain URL.
- An app can also add its own items to the menu a click on a link opens:
  `midna links actions register --owner <app> --file menus.json` (RPC `links.actions.register
  {owner, menus}`; each call replaces that app's menus, `[]` or `clear` removes them). A menu is
  `{"links": "<regex over the URL or a pattern's match>", "own": true | "section": "<label>",
  "icon"?, "title"?, "subtitle"?, "items": [...]}`; captures `$0 $1 ${name}` fill its fields. `own`
  makes it the link's whole menu (midna keeps Open, Copy link and Add to links); `section` adds a
  labelled group to midna's menu. An item is `{label, keys?, detail?, style?: "danger", confirm?}`
  plus one of `open` (URL), `copy` (text), `run` (argv, no shell, the program can't come from a
  capture), `terminal` (argv in a new terminal in the link's project) or `prompt` (text for a new
  agent terminal), or `{"divider": true}`. Open and copy items work at once; the first time an
  app registers run/terminal/prompt items (and whenever those commands change) the human is asked
  to approve them, and they stay hidden until then. A pattern's `preview` JSON may add `"actions":
  [...]` (same item shape) to replace an own menu's items for that one link.
