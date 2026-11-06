## Secrets (`[secret:NAME]`)

When the human pastes a secret into your terminal (a token, an API key, a `.env` block), midna
stores it in the Keychain and types `[secret:NAME]` in its place, so the value never enters your
context. Use it without seeing it:

- `midna secret exec NAME -- <command…>` runs the command with `$NAME` set. `VAR=NAME` sets
  `$VAR` instead (`midna secret exec GH_TOKEN=GITHUB_TOKEN -- gh api user`). Write `$NAME`
  inside the command (`-- sh -c 'curl -H "Authorization: Bearer $API_KEY" …'`), never the value.
  The value is masked as `‹NAME›` in the output, including base64 and URL-encoded forms.
- `midna secret write NAME .env [--as KEY]` sets it in a `.env`-style file for a dev server.
  Don't read that file back afterwards, because that would put the value in your context.
- `midna secret list` shows what's stored for this project.
- Never ask the human to paste a value into the chat. If you need a secret, ask them to paste it
  into the terminal; midna offers to store it.

You can save secrets too. When a command produces a token (a login, an API that issues keys),
pipe it straight into midna so it never enters your context:

```sh
gh auth token | midna secret save GH_TOKEN
curl -s … | jq -r .access_token | midna secret save API_TOKEN --label "Acme API token"
```

Then use `[secret:GH_TOKEN]` like any other. The value comes from stdin only, never as an
argument. A token you already saw (in an MCP tool result, say) is worth saving too, so it stops
spreading into commands and files. midna marks that one *exposed* so the human knows to rotate it.
A secret belongs to your terminal's project (`--global` for every project). Replacing a secret
the human stored asks them first. Only the human removes secrets.
