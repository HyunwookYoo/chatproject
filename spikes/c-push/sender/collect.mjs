// Pulls files/recv.jsonl from the probe app and reports on the last burst sent by send.mjs.
//   node collect.mjs [--serial S] [--verbose]
import { spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { ADB, LAST_BURST, die, readAppFile, serialArgs } from './common.mjs';

const { values: opt } = parseArgs({
  options: { serial: { type: 'string' }, verbose: { type: 'boolean', short: 'v' } },
});
if (!existsSync(LAST_BURST)) die('No last-burst.json: run `node send.mjs burst ...` first.');
const burst = JSON.parse(readFileSync(LAST_BURST, 'utf8'));
const serial = opt.serial ?? burst.serial;

const offset = await deviceClockOffset();
const recv = pullRecv();

// A received line belongs to this burst if it echoes one of its (seq, sent_ms) pairs.
const key = (m) => `${m.seq}:${m.sent_ms}`;
const sentKeys = new Set(burst.sent.map(key));
const got = new Map();
let duplicates = 0;
let other = 0;
for (const r of recv) {
  if (r.deleted) continue;
  if (!sentKeys.has(key(r))) other++;
  else if (got.has(key(r))) duplicates++;
  else got.set(key(r), r);
}
const accepted = burst.sent.filter((s) => s.status === 200);
const missing = accepted.filter((s) => !got.has(key(s))).map((s) => s.seq);
const msgs = [...got.values()].sort((a, b) => a.seq - b.seq);
const latency = (r) => r.recv_ms - offset - r.sent_ms;
const lat = msgs.map(latency).sort((a, b) => a - b);
const pct = (q) => lat[Math.ceil(q * lat.length) - 1];
const deleted = recv.filter((r) => r.deleted && r.recv_ms - offset >= burst.started_ms);

console.log(`clock:    device - PC = ${offset} ms (latencies are corrected by this)`);
console.log(
  `burst:    ${burst.count} x ${burst.priority}, ${burst.size} B, ttl ${burst.ttl ?? 'default'}, ` +
    `collapse ${burst.collapse ?? 'none'}, started ${clock(burst.started_ms)}`,
);
console.log(`FCM:      ${accepted.length}/${burst.count} accepted (HTTP 200)`);
console.log(`received: ${msgs.length}/${accepted.length}  ${ranges(msgs.map((r) => r.seq))}`);
console.log(`missing:  ${missing.length ? `${missing.length}  ${ranges(missing)}` : 'none'}`);
if (lat.length) console.log(`latency:  p50 ${pct(0.5)} ms, p95 ${pct(0.95)} ms, max ${lat.at(-1)} ms (min ${lat[0]} ms)`);
console.log(`priority: ${tally(msgs.map((r) => `${r.priority}/${r.originalPriority}`))}  (delivered/original)`);
console.log(`payload:  ${tally(msgs.map((r) => `${r.payload_bytes} B`))}`);
console.log(`deleted:  ${deleted.map((r) => `onDeletedMessages at ${clock(r.recv_ms - offset)}`).join(', ') || 'none'}`);
if (duplicates) console.log(`(${duplicates} duplicate deliveries)`);
if (other) console.log(`(${other} lines in recv.jsonl are from other bursts)`);
if (opt.verbose) {
  for (const r of msgs) {
    console.log(
      `  seq ${r.seq}  ${latency(r)} ms  recv ${clock(r.recv_ms - offset)}  ` +
        `${r.priority}/${r.originalPriority}  ${r.payload_bytes} B`,
    );
  }
}

/**
 * Device clock minus PC clock, in ms. The device prints its time and the PC stamps each line
 * on arrival, so (device - arrival) undershoots by the one-way adb delay; the max over
 * samples is within a few ms of the true offset.
 */
function deviceClockOffset() {
  const script = 'for i in 1 2 3 4 5 6 7 8 9 10; do date +%s%3N; sleep 0.05; done';
  return new Promise((resolve) => {
    const p = spawn(ADB, [...serialArgs(serial), 'shell', script]);
    let best = -Infinity;
    let rest = '';
    let stderr = '';
    p.stdout.on('data', (chunk) => {
      const now = Date.now();
      const lines = (rest + chunk).split('\n');
      rest = lines.pop();
      for (const line of lines) if (/^\d{13}\s*$/.test(line)) best = Math.max(best, Number(line) - now);
    });
    p.stderr.on('data', (chunk) => (stderr += chunk));
    p.on('error', (e) => die(`adb failed: ${e.message}`));
    p.on('close', () => (best > -Infinity ? resolve(best) : die(`Could not read the device clock: ${stderr.trim()}`)));
  });
}

function pullRecv() {
  return readAppFile(serial, 'files/recv.jsonl').split('\n').flatMap((line) => {
    try {
      return [JSON.parse(line)];
    } catch {
      return []; // blank or half-written line
    }
  });
}

function clock(ms) {
  return `${new Date(ms).toTimeString().slice(0, 8)}.${String(ms % 1000).padStart(3, '0')}`;
}

function tally(values) {
  const counts = Object.groupBy(values, (v) => v);
  return Object.entries(counts).map(([v, list]) => `${v} x${list.length}`).join(', ') || '-';
}

/** [1,2,3,7,9,10] -> "1-3, 7, 9-10" (input sorted). */
function ranges(nums) {
  const runs = [];
  for (const n of nums) {
    const last = runs.at(-1);
    if (last && n === last[1] + 1) last[1] = n;
    else runs.push([n, n]);
  }
  return runs.map(([a, b]) => (a === b ? `${a}` : `${a}-${b}`)).join(', ');
}
