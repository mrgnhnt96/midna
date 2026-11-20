#!/usr/bin/env node
// A midna stream producer: forwards the messages posted in one Slack channel, read over Slack's
// Socket Mode, as one JSON line each on stdout. Needs Node 22 or later and no packages.
//
//   SLACK_APP_TOKEN  app-level token (xapp-…, scope connections:write). Required.
//   CHANNEL          the channel's ID (C0123ABCD). Required.
//   SLACK_BOT_TOKEN  bot token (xoxb-…, channels:history or groups:history). Optional: when set,
//                    the client first checks that the app is in CHANNEL, and exits if it isn't.
//
// Each line is {"kind":"message","id":<event_id>,"data":{channel, user, bot_id, username,
// subtype, text, ts, thread_ts}}. A message whose words change later (a bot editing its
// post) comes again as kind "message_changed" with the new words. data.text is plain text
// made from the message's text, attachments and blocks, since CI bots often post with an empty
// text. Run it under `midna stream add` (README.md beside this file); its stderr is the log.

import { channel as diagnostics } from 'node:diagnostics_channel';

const API = process.env.SLACK_API_URL || 'https://slack.com/api'; // a stand-in Slack, for tests
const APP_TOKEN = process.env.SLACK_APP_TOKEN || '';
const BOT_TOKEN = process.env.SLACK_BOT_TOKEN || '';
const CHANNEL = (process.env.CHANNEL || '').trim();

// Message subtypes worth forwarding; joins, topic changes, deletions and the rest are skipped.
const KEEP = new Set(['bot_message', 'thread_broadcast', 'file_share', 'me_message']);
// midna refuses event data over 64 KB, so long text is cut well before that.
const MAX_TEXT = 16000;
// Slack pings every connection; one that goes this long without a ping or a message is dead.
const SILENT_MS = 90_000;
// Slack errors that retrying can't fix: midna's backoff and its failing state take over.
const FATAL = new Set(['invalid_auth', 'not_authed', 'account_inactive', 'token_revoked', 'token_expired', 'not_allowed_token_type', 'missing_scope', 'no_permission', 'not_in_channel', 'channel_not_found']);

const log = (...a) => console.error(new Date().toISOString(), ...a);

function quit(why) {
  log(why);
  process.exit(1);
}

async function slack(method, token, params = {}) {
  const res = await fetch(`${API}/${method}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams(params),
  });
  if (!res.ok) throw new Error(`${method}: HTTP ${res.status}`);
  const body = await res.json();
  if (!body.ok) {
    const e = new Error(`${method}: ${body.error}`);
    e.slack = body.error;
    throw e;
  }
  return body;
}

// ---------------------------------------------------------------------------- message text

// Slack's mrkdwn as plain text: links become their label and URL, mentions stay readable,
// and *bold*, _italic_, ~strike~ and `code` lose their marks, so a glob can match the words.
function plain(s) {
  if (typeof s !== 'string') return '';
  return s
    .replace(/<([^<>|]+)\|([^<>]+)>/g, (_, target, label) => (/^[@#!]/.test(target) ? `${target[0] === '#' ? '#' : '@'}${label}` : `${label} (${target})`))
    .replace(/<([^<>]+)>/g, (_, t) => (t.startsWith('!') ? `@${t.slice(1).split('^')[0]}` : t))
    .replace(/`+([^`]+?)`+/g, '$1')
    .replace(/\*([^*\n]+)\*/g, '$1')
    .replace(/~([^~\n]+)~/g, '$1')
    .replace(/(^|[^\p{L}\p{N}_])_([^_\n]+)_(?![\p{L}\p{N}_])/gu, '$1$2')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&amp;/g, '&');
}

// The words in rich_text elements (what Slack's composer and many apps send).
function richText(elements) {
  let out = '';
  for (const e of elements || []) {
    switch (e.type) {
      case 'text': out += e.text || ''; break;
      case 'link': out += e.text ? `${e.text} (${e.url})` : e.url || ''; break;
      case 'user': out += `<@${e.user_id}>`; break;
      case 'channel': out += `<#${e.channel_id}>`; break;
      case 'usergroup': out += `<!subteam^${e.usergroup_id}>`; break;
      case 'broadcast': out += `@${e.range}`; break;
      case 'emoji': out += `:${e.name}:`; break;
      case 'date': out += e.fallback || ''; break;
      case 'rich_text_list': out += (e.elements || []).map((item) => `- ${richText(item.elements)}`).join('\n') + '\n'; break;
      default: if (Array.isArray(e.elements)) out += richText(e.elements) + '\n';
    }
  }
  return out;
}

// Each block's words, one entry per block or field.
function blockTexts(blocks) {
  const out = [];
  for (const b of blocks || []) {
    switch (b.type) {
      case 'section':
        out.push(plain(b.text?.text));
        for (const f of b.fields || []) out.push(plain(f.text));
        break;
      case 'header': out.push(b.text?.text || ''); break;
      case 'context': for (const e of b.elements || []) out.push(plain(e.text)); break;
      case 'rich_text': out.push(richText(b.elements)); break;
    }
  }
  return out;
}

function attachmentTexts(a) {
  // A link preview Slack added (an unfurl) isn't the poster's words.
  if (a.from_url || a.is_app_unfurl) return [];
  const own = [a.pretext, a.author_name, a.title, a.text, ...(a.fields || []).map((f) => [f.title, f.value].filter(Boolean).join(': ')), a.footer]
    .filter(Boolean)
    .map(plain);
  const blocks = blockTexts(a.blocks);
  return own.length || blocks.some(Boolean) ? [...own, ...blocks] : [plain(a.fallback)];
}

// The message as plain text: its text, then each attachment's and block's words, leaving
// out a piece whose words are already there (bots repeat their blocks in text).
function textOf(m) {
  const parts = [plain(m.text), ...(m.attachments || []).flatMap(attachmentTexts), ...blockTexts(m.blocks)];
  let text = '';
  for (const p of parts.map((p) => p.trim()).filter(Boolean)) {
    if (!text.includes(p)) text += (text ? '\n' : '') + p;
  }
  return text.length > MAX_TEXT ? `${text.slice(0, MAX_TEXT)}…` : text;
}

// ---------------------------------------------------------------------------- events

function dataFor(channel, m) {
  return {
    channel,
    user: m.user ?? null,
    bot_id: m.bot_id ?? null,
    username: m.username ?? m.bot_profile?.name ?? null,
    subtype: m.subtype ?? null,
    text: textOf(m),
    ts: m.ts ?? null,
    thread_ts: m.thread_ts ?? null,
  };
}

let emit = (kind, id, channel, m) => process.stdout.write(JSON.stringify({ kind, id, data: dataFor(channel, m) }) + '\n');

// One Events API payload: a message in the channel becomes a line.
function onEvent(payload, envelopeId, channel = CHANNEL) {
  const ev = payload?.event;
  if (ev?.type !== 'message' || ev.channel !== channel) return;
  const id = payload.event_id || envelopeId;
  if (ev.subtype === 'message_changed') {
    // Slack also sends these when a reply count or a link preview changes: only new words count.
    if (ev.message && textOf(ev.message) !== textOf(ev.previous_message || {})) emit('message_changed', id, ev.channel, ev.message);
  } else if (!ev.subtype || KEEP.has(ev.subtype)) {
    emit('message', id, ev.channel, ev);
  }
}

// ---------------------------------------------------------------------------- the connection

let current = null; // the socket events are read from
let backoff = 1000;
let lastSeen = 0;
let pinged = false; // whether this Node reports pings; the watchdog only runs if it does

// Node's WebSocket answers Slack's pings itself and reports them here (Node 22+).
diagnostics('undici:websocket:ping').subscribe(() => {
  pinged = true;
  lastSeen = Date.now();
});

function watch() {
  setInterval(() => {
    if (pinged && current && Date.now() - lastSeen > SILENT_MS) {
      log(`no word from Slack for ${SILENT_MS / 1000}s; connecting again`);
      reconnect();
    }
  }, 15_000);
}

function reconnect() {
  const old = current;
  current = null;
  try { old?.close(); } catch {}
  connect();
}

function retry(why) {
  log(`${why}; trying again in ${backoff / 1000}s`);
  setTimeout(connect, backoff);
  backoff = Math.min(backoff * 2, 60_000);
}

async function connect() {
  let url;
  try {
    ({ url } = await slack('apps.connections.open', APP_TOKEN));
  } catch (e) {
    if (FATAL.has(e.slack)) quit(`Slack refused SLACK_APP_TOKEN (${e.message}): it must be the app-level xapp- token with connections:write`);
    return retry(`can't reach Slack (${e.message})`);
  }
  const ws = new WebSocket(url);
  current = ws;
  lastSeen = Date.now();
  ws.addEventListener('message', (m) => {
    lastSeen = Date.now();
    let msg;
    try { msg = JSON.parse(m.data); } catch { return log('Slack sent something that isn\'t JSON'); }
    // Ack every envelope at once, or Slack sends it again.
    if (msg.envelope_id) ws.send(JSON.stringify({ envelope_id: msg.envelope_id }));
    if (ws !== current) return;
    switch (msg.type) {
      case 'hello':
        backoff = 1000;
        log(`connected (${msg.num_connections ?? 1} connection(s) open for this app)`);
        break;
      case 'disconnect':
        if (msg.reason === 'link_disabled') quit('Slack turned Socket Mode off for this app (link_disabled): turn it on again in the app settings');
        log(`Slack asked to reconnect (${msg.reason})`);
        reconnect();
        break;
      case 'events_api':
        onEvent(msg.payload, msg.envelope_id);
        break;
    }
  });
  ws.addEventListener('close', (e) => {
    if (ws === current) {
      current = null;
      retry(`connection closed (${e.code}${e.reason ? ` ${e.reason}` : ''})`);
    }
  });
  ws.addEventListener('error', () => {}); // a close follows
}

async function main() {
  if (typeof WebSocket === 'undefined') quit(`needs Node 22 or later (this is ${process.version})`);
  if (!APP_TOKEN.startsWith('xapp-')) quit('set SLACK_APP_TOKEN to the app-level token (xapp-…)');
  if (!/^[CGD][A-Z0-9]+$/.test(CHANNEL)) quit('set CHANNEL to the channel ID (C0123ABCD, at the bottom of the channel details)');
  if (BOT_TOKEN) {
    try {
      await slack('conversations.history', BOT_TOKEN, { channel: CHANNEL, limit: '1' });
    } catch (e) {
      if (e.slack === 'not_in_channel') quit(`the app isn't in channel ${CHANNEL}: invite it there (/invite @your-app), or Slack sends it nothing`);
      if (FATAL.has(e.slack)) quit(`Slack refused SLACK_BOT_TOKEN for channel ${CHANNEL} (${e.message})`);
      log(`couldn't check channel ${CHANNEL} (${e.message}); going on`);
    }
  }
  watch();
  connect();
}

// `node slack-stream.mjs --data < message.json` prints the data a saved Slack message event
// becomes, for `midna triggers test --stream slack --kind message --data -`.
async function dataOf() {
  let raw = '';
  for await (const chunk of process.stdin) raw += chunk;
  const m = JSON.parse(raw);
  emit = (kind, id, channel, msg) => process.stdout.write(JSON.stringify(dataFor(channel, msg), null, 2) + '\n');
  onEvent({ event: m }, 'sample', m.channel);
}

if (process.argv[2] === '--data') dataOf();
else main();
