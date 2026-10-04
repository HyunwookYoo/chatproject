// Shared by send.mjs and collect.mjs: adb access to the probe app's private files.
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

export const PKG = 'dev.chatproject.spike.fcmprobe';
export const LAST_BURST = new URL('./last-burst.json', import.meta.url);

const sdk = process.env.ANDROID_HOME ?? join(process.env.LOCALAPPDATA ?? '', 'Android', 'Sdk');
const sdkAdb = join(sdk, 'platform-tools', process.platform === 'win32' ? 'adb.exe' : 'adb');
export const ADB = existsSync(sdkAdb) ? sdkAdb : 'adb';

export const serialArgs = (serial) => (serial ? ['-s', serial] : []);

export function die(message) {
  console.error(message);
  process.exit(1);
}

/** A file in the app's private dir (via `adb shell run-as`), or '' if it does not exist yet. */
export function readAppFile(serial, path) {
  try {
    return execFileSync(ADB, [...serialArgs(serial), 'shell', 'run-as', PKG, 'cat', path], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  } catch (e) {
    if (String(e.stderr).includes('No such file')) return '';
    return die(`adb failed: ${String(e.stderr || e.message).trim()}`);
  }
}

/** The FCM registration token the app wrote. Never print it. */
export function readToken(serial) {
  const token = readAppFile(serial, 'files/token.txt').trim();
  if (!token) die('No FCM token on the device. Launch the app, then check `adb logcat -s FcmProbe`.');
  return token;
}
