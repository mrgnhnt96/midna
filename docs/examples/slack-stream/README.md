# Slack channel stream

`slack-stream.mjs` reads one Slack channel over Socket Mode and prints each message as a midna
stream event, so a local trigger can act on it. Socket Mode needs no public URL: the script opens
the connection to Slack. It needs Node 22 or later and no packages.

Run under `midna stream add slack …`, each message in the channel becomes the event
`stream.slack.message`:

```json
{"kind":"message","id":"Ev07ABCDEF","data":{"channel":"C0123ABCD","user":null,"bot_id":"B07CIBOT","username":"Azure Pipelines","subtype":"bot_message","text":"roon_mobile failed to build on pipeline roon_mobile on branch master\nBuild 20261120.3","ts":"1795120000.000100","thread_ts":null}}
```

- `id` is Slack's `event_id`, or the envelope's id when there is none. Slack sends an event
  again when an ack is late, with the same `event_id`, and midna drops the repeat.
- `data.text` is plain text built from the message's `text`, its attachments (pretext, author,
  title, text, fields, footer) and its blocks (section, header, context and rich text), one
  piece per line, with repeated pieces left out. CI bots often post with `subtype: bot_message`
  and an empty `text`, with the words in attachments or blocks. Slack's formatting is removed:
  `*master*` becomes `master`, `<https://…|View logs>` becomes `View logs (https://…)`.
- Plain messages, thread replies (and replies also sent to the channel), bot messages, file
  shares and `/me` messages are forwarded. Joins, topic changes and deletions aren't.
- An edit that changes a message's words comes as kind `message_changed`, with the new words.
  A bot that posts "building" and edits it to "failed" shows up only this way.

## Setup

1. Make the Slack app in [Slack's app settings](https://api.slack.com/apps) (Create New App, From
   scratch):
   - **Socket Mode**: turn it on and make an app-level token with the `connections:write` scope.
     That's `SLACK_APP_TOKEN` (`xapp-…`).
   - **Event Subscriptions**: turn them on and subscribe to the bot event `message.channels`, or
     `message.groups` for a private channel.
   - **OAuth & Permissions**: add the bot scope `channels:history` (`groups:history` for a
     private channel) and install the app to the workspace. The bot token is `SLACK_BOT_TOKEN`
     (`xoxb-…`).
   - In the channel, `/invite @<app name>`. The channel's ID (`C0…`) is at the bottom of its
     details.
2. Paste each token into a midna terminal and store them as `SLACK_APP_TOKEN` and
   `SLACK_BOT_TOKEN` (midna offers to when it sees one).
3. Get the script and add the stream from that terminal:

   ```sh
   mkdir -p ~/.midna-streams && cd ~/.midna-streams && \
     for f in slack-stream.mjs bot-message.json; do curl -fsSLO \
       https://raw.githubusercontent.com/mrgnhnt96/midna/main/docs/examples/slack-stream/$f; done
   midna stream add slack --secret SLACK_APP_TOKEN --secret SLACK_BOT_TOKEN \
     -- env CHANNEL=C0123ABCD node ~/.midna-streams/slack-stream.mjs
   ```

   An agent's stream waits for the human to approve it. `midna stream list` then shows it
   `running`, and its log (`$MIDNA_HOME/streams/slack.log`) says `connected`.
4. Add a trigger. This one types a line into the terminal named "build angel" when a
   `roon_mobile` build on `master` fails:

   ```sh
   midna triggers add --name "roon_mobile master failed" --event stream.slack.message \
     --match 'text=*failed to build on pipeline roon_mobile*' --match 'text=*master*' \
     --to "build angel" --send 'CI failed on master: {{data.text}}' --enable
   ```

   The same field given twice needs both globs, in any order.

## Environment

| Variable | What it is |
| --- | --- |
| `SLACK_APP_TOKEN` | Required. The app-level token (`xapp-…`). |
| `CHANNEL` | Required. The channel's ID. |
| `SLACK_BOT_TOKEN` | Optional. When set, the script first checks that the app is in the channel, and exits if it isn't. |
| `SLACK_API_URL` | Optional. Another API base than `https://slack.com/api`, for a stand-in Slack in tests. |

## Connection

The script acks every envelope as soon as it arrives. It connects again at once when Slack sends
`disconnect` (Slack does this every few hours), and after 1s, doubling to 60s, when the socket
drops or Slack can't be reached. When Node reports Slack's pings and none comes for 90 seconds,
it connects again. It logs to stderr, which midna keeps in the stream's log.

It exits with status 1 when retrying can't help: a token Slack refuses, the app missing from the
channel, or Socket Mode turned off for the app (`link_disabled`). midna starts it again, and after
5 exits in 10 minutes marks the stream `failing` and raises one needs-you item.
`midna explain stream slack` shows the log's last lines.

## Test a trigger without Slack

`--data` turns a saved Slack message event into the data the script would publish, so the
trigger test runs the same text building:

```sh
node ~/.midna-streams/slack-stream.mjs --data < ~/.midna-streams/bot-message.json \
  | midna triggers test <trigger-id> --stream slack --kind message --data -
```

`bot-message.json` is a CI bot's `bot_message`: an empty `text`, with the failure in an
attachment and in blocks.
