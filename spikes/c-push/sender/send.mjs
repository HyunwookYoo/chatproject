// Sends FCM HTTP v1 data messages to the probe app. Prints HTTP statuses and error bodies only,
// never the FCM token, the OAuth access token or anything from the service-account file.
//   node send.mjs token [--serial S]
//   node send.mjs burst --count N --size BYTES --priority high|normal [--ttl SECONDS] [--collapse KEY] [--serial S]
import { createHash, randomBytes, sign } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';
import { parseArgs } from 'node:util';
import { LAST_BURST, die, readToken } from './common.mjs';

const SA_FILE = new URL('../secrets/fcm-service-account.json', import.meta.url);
const TOKEN_URL = 'https://oauth2.googleapis.com/token';
const SCOPE = 'https://www.googleapis.com/auth/firebase.messaging';
const PACE_MS = 300; // FCM accepts at most 240 messages/minute for one Android device
const USAGE = `usage: node send.mjs token [--serial S]
       node send.mjs burst --count N --size BYTES --priority high|normal [--ttl SECONDS] [--collapse KEY] [--serial S]`;

const { values: opt, positionals: [cmd] } = parseArgs({
  allowPositionals: true,
  options: {
    serial: { type: 'string' },
    count: { type: 'string' },
    size: { type: 'string' },
    priority: { type: 'string' },
    ttl: { type: 'string' },
    collapse: { type: 'string' },
  },
});

if (cmd === 'token') {
  const token = readToken(opt.serial);
  const hash = createHash('sha256').update(token).digest('hex').slice(0, 12);
  console.log(`FCM token on device: ${token.length} chars, sha256 ${hash}... (the token itself is never printed)`);
} else if (cmd === 'burst') {
  try {
    await burst();
  } catch (e) {
    // Not die(): process.exit() right after a fetch can crash Node on Windows (libuv assertion).
    console.error(e.message);
    process.exitCode = 1;
  }
} else {
  die(USAGE);
}

async function burst() {
  const count = Number(opt.count);
  const size = Number(opt.size);
  if (!(count >= 1) || !(size >= 0) || !['high', 'normal'].includes(opt.priority)) die(USAGE);
  const token = readToken(opt.serial);
  const sa = loadServiceAccount();
  const accessToken = await getAccessToken(sa);
  const redact = (text) => text.replaceAll(token, '<token>').replaceAll(accessToken, '<access-token>');

  const url = `https://fcm.googleapis.com/v1/projects/${sa.project_id}/messages:send`;
  const android = { priority: opt.priority.toUpperCase() };
  if (opt.ttl) android.ttl = `${Number(opt.ttl)}s`;
  if (opt.collapse) android.collapse_key = opt.collapse;

  const record = {
    serial: opt.serial ?? null,
    count,
    size,
    priority: opt.priority,
    ttl: opt.ttl ?? null,
    collapse: opt.collapse ?? null,
    started_ms: Date.now(),
    sent: [],
  };
  for (let seq = 1; seq <= count; seq++) {
    const sentMs = Date.now();
    const data = { seq: String(seq), sent_ms: String(sentMs), pad: '' };
    data.pad = randomBytes(size).toString('base64').slice(0, Math.max(0, size - kvBytes(data)));
    if (seq === 1) {
      console.log(`data payload: ${kvBytes(data)} B as keys+values, ${Buffer.byteLength(JSON.stringify(data))} B as JSON`);
    }
    let status = 0;
    let detail = '';
    try {
      const res = await fetch(url, {
        method: 'POST',
        headers: { authorization: `Bearer ${accessToken}`, 'content-type': 'application/json' },
        body: JSON.stringify({ message: { token, data, android } }),
      });
      status = res.status;
      const body = await res.text();
      if (!res.ok) detail = redact(body.replace(/\s+/g, ' '));
    } catch (e) {
      detail = redact(String(e.cause ?? e));
    }
    const httpMs = Date.now() - sentMs;
    console.log(`seq ${seq}/${count}  HTTP ${status || 'error'}  ${httpMs} ms  ${detail}`.trimEnd());
    record.sent.push({ seq, sent_ms: sentMs, status, http_ms: httpMs });
    await sleep(Math.max(0, PACE_MS - httpMs));
  }
  writeFileSync(LAST_BURST, JSON.stringify(record, null, 1) + '\n');
  const accepted = record.sent.filter((s) => s.status === 200).length;
  console.log(`FCM accepted ${accepted}/${count}. When delivery should be done: node collect.mjs`);
}

function kvBytes(data) {
  return Object.entries(data).reduce((n, [k, v]) => n + Buffer.byteLength(k) + Buffer.byteLength(v), 0);
}

function loadServiceAccount() {
  try {
    return JSON.parse(readFileSync(SA_FILE, 'utf8'));
  } catch {
    // no error details: they could quote the file
    return die('Cannot read secrets/fcm-service-account.json (see secrets/README.md).');
  }
}

/** Service-account JWT (RS256) exchanged for an OAuth access token. */
async function getAccessToken(sa) {
  const now = Math.floor(Date.now() / 1000);
  const b64 = (obj) => Buffer.from(JSON.stringify(obj)).toString('base64url');
  const unsigned = `${b64({ alg: 'RS256', typ: 'JWT' })}.${b64({
    iss: sa.client_email,
    scope: SCOPE,
    aud: TOKEN_URL,
    iat: now,
    exp: now + 3600,
  })}`;
  const jwt = `${unsigned}.${sign('RSA-SHA256', Buffer.from(unsigned), sa.private_key).toString('base64url')}`;
  const res = await fetch(TOKEN_URL, {
    method: 'POST',
    body: new URLSearchParams({ grant_type: 'urn:ietf:params:oauth:grant-type:jwt-bearer', assertion: jwt }),
  });
  if (!res.ok) throw new Error(`OAuth token exchange failed: HTTP ${res.status} ${(await res.text()).replace(/\s+/g, ' ')}`);
  return (await res.json()).access_token;
}
