import 'dart:io';

/// Fixed public state of one owned emulator and its app, read with adb after an
/// account scenario. Every value is an enum name, a number or a boolean that
/// these parsers produce; no command output, id or log text is ever kept.
///
/// The value sets are shared with tools/ci/write_e2e_summary.py through
/// tools/e2e/diagnostic_details.json (`android`).
const androidScreens = {'awake', 'asleep', 'dozing', 'dreaming', 'unknown'};
const androidTops = {
  'app',
  'permission_controller',
  'settings',
  'launcher',
  'systemui',
  'other',
  'none',
  'unknown',
};
const androidProcesses = {'alive_same', 'alive_other', 'none', 'unknown'};

/// `ApplicationExitInfo` reason codes (API 30+), by number.
const androidExitReasons = {
  0: 'UNKNOWN',
  1: 'EXIT_SELF',
  2: 'SIGNALED',
  3: 'LOW_MEMORY',
  4: 'CRASH',
  5: 'CRASH_NATIVE',
  6: 'ANR',
  7: 'INITIALIZATION_FAILURE',
  8: 'PERMISSION_CHANGE',
  9: 'EXCESSIVE_RESOURCE_USAGE',
  10: 'USER_REQUESTED',
  11: 'USER_STOPPED',
  12: 'DEPENDENCY_DIED',
  13: 'OTHER',
  14: 'FREEZER',
  15: 'PACKAGE_STATE_CHANGE',
  16: 'PACKAGE_UPDATED',
};

/// Event-log tags of the app's own lifecycle, plus the screen turning off/on.
const androidLifecycle = {
  'am_proc_start',
  'am_proc_bound',
  'am_proc_died',
  'am_kill',
  'am_anr',
  'am_crash',
  'am_freeze',
  'am_unfreeze',
  'wm_on_create_called',
  'wm_on_start_called',
  'wm_on_restart_called',
  'wm_on_resume_called',
  'wm_on_top_resumed_gained_called',
  'wm_on_top_resumed_lost_called',
  'wm_on_paused_called',
  'wm_on_stop_called',
  'wm_on_destroy_called',
  'wm_finish_activity',
  'wm_destroy_activity',
  'screen_off',
  'screen_on',
};

/// The newest lifecycle entries kept.
const androidLifecycleLimit = 32;

/// `dumpsys power`: the global wakefulness.
String screenState(String dumpsysPower) {
  final match = RegExp(
    r'^\s*mWakefulness=(\w+)',
    multiLine: true,
  ).firstMatch(dumpsysPower);
  final value = match?.group(1)?.toLowerCase();
  return androidScreens.contains(value) ? value! : 'unknown';
}

/// `dumpsys activity activities`: whose activity is the top resumed one.
String topActivity(String dumpsysActivities, String package) {
  final lines = dumpsysActivities.split('\n');
  final line =
      lines.where((l) => l.contains('topResumedActivity=')).firstOrNull ??
      lines.where((l) => l.contains('ResumedActivity:')).firstOrNull;
  if (line == null) return 'unknown';
  final owner = RegExp(r'\{\S+ u\d+ ([^/\s}]+)/').firstMatch(line)?.group(1);
  if (owner == null) return 'none';
  if (owner == package) return 'app';
  if (owner.contains('permissioncontroller')) return 'permission_controller';
  if (owner == 'com.android.settings') return 'settings';
  if (owner.contains('launcher') || owner.contains('quickstep')) {
    return 'launcher';
  }
  if (owner == 'com.android.systemui') return 'systemui';
  return 'other';
}

/// `pidof <package>` against the pid the harness owns.
String processState(ProcessResult pidof, String ownedPid) {
  final out = (pidof.stdout as String).trim();
  if (out.isEmpty) return pidof.exitCode <= 1 ? 'none' : 'unknown';
  if (pidof.exitCode != 0 || !RegExp(r'^\d+( \d+)*$').hasMatch(out)) {
    return 'unknown';
  }
  return out == ownedPid ? 'alive_same' : 'alive_other';
}

/// `dumpsys activity processes <package>`: process state, oom adj and freeze.
({int? procState, int? oomAdj, bool? frozen}) processRecord(String dump) {
  int? number(RegExp pattern) {
    final value = pattern.firstMatch(dump)?.group(1);
    return value == null ? null : int.tryParse(value);
  }

  final frozen = RegExp(r'\bisFrozen=(true|false)').firstMatch(dump)?.group(1);
  return (
    procState: number(RegExp(r'\bcurProcState=(\d+)')),
    oomAdj: number(RegExp(r'oom adj:.*?\bcur=(-?\d+)')),
    frozen: frozen == null ? null : frozen == 'true',
  );
}

/// `dumpsys activity exit-info <package>`: the newest record (listed first).
({String? reason, int? subreason, int? importance, bool? owned}) lastExit(
  String dump,
  String ownedPid,
) {
  final start = dump.indexOf('ApplicationExitInfo #');
  if (start < 0) {
    return (reason: null, subreason: null, importance: null, owned: null);
  }
  final next = dump.indexOf('ApplicationExitInfo #', start + 1);
  final record = dump.substring(start, next < 0 ? dump.length : next);
  int? number(String key) {
    final value = RegExp('\\b$key=(\\d+)').firstMatch(record)?.group(1);
    return value == null ? null : int.tryParse(value);
  }

  final reason = number('reason');
  final pid = number('pid');
  final subreason = number('subreason');
  final importance = number('importance');
  return (
    reason: reason == null ? null : androidExitReasons[reason] ?? 'UNKNOWN',
    subreason: subreason != null && subreason <= 1000 ? subreason : null,
    importance: importance != null && importance <= 1000 ? importance : null,
    owned: pid == null ? null : '$pid' == ownedPid,
  );
}

/// `logcat -b events -d`: the app's lifecycle tags and the screen toggles, in
/// order, repeats folded, the newest [androidLifecycleLimit].
List<String> lifecycle(String events, String package) {
  final tags = <String>[];
  final line = RegExp(r'\s([a-z_]+): (.*)$');
  for (final text in events.split('\n')) {
    final match = line.firstMatch(text.trimRight());
    if (match == null) continue;
    final tag = match.group(1)!;
    final value = match.group(2)!;
    String? entry;
    if (tag == 'screen_toggled') {
      entry = value.trim() == '0'
          ? 'screen_off'
          : value.trim() == '1'
          ? 'screen_on'
          : null;
    } else if (androidLifecycle.contains(tag) &&
        RegExp(
          '[\\[,{ ]${RegExp.escape(package)}[,/.:\\] }]',
        ).hasMatch('$value ')) {
      entry = tag;
    }
    if (entry != null && (tags.isEmpty || tags.last != entry)) tags.add(entry);
  }
  return tags.length > androidLifecycleLimit
      ? tags.sublist(tags.length - androidLifecycleLimit)
      : tags;
}

/// The system reported low memory (`am_low_memory`) or lmkd killed something.
bool lowMemory(String events, String lmk) =>
    RegExp(r'\sam_low_memory:').hasMatch(events) ||
    RegExp(r'\bkill', caseSensitive: false).hasMatch(lmk);

/// lmkd killed this app.
bool lmkKilledApp(String lmk, String package) => lmk
    .split('\n')
    .any(
      (line) =>
          RegExp(r'\bkill', caseSensitive: false).hasMatch(line) &&
          line.contains("'$package'"),
    );

/// `/proc/meminfo`: total and available memory in MiB.
({int? total, int? available}) memory(String meminfo) {
  int? mib(String key) {
    final value = RegExp(
      '^$key:\\s+(\\d+) kB',
      multiLine: true,
    ).firstMatch(meminfo)?.group(1);
    return value == null ? null : int.parse(value) ~/ 1024;
  }

  return (total: mib('MemTotal'), available: mib('MemAvailable'));
}

/// A `settings get` number, or null.
int? setting(String output) => int.tryParse(output.trim());

/// Reads the fixed state of one owned emulator. [adb] runs one adb command
/// against that emulator and never throws for a non-zero exit.
Future<Map<String, Object?>> probeAndroid(
  Future<ProcessResult> Function(List<String>) adb, {
  required String name,
  required String package,
  required String ownedPid,
}) async {
  Future<String?> output(List<String> args) async {
    try {
      final result = await adb(args);
      return result.exitCode == 0 ? result.stdout as String : null;
    } catch (_) {
      return null;
    }
  }

  final power = await output(['shell', 'dumpsys', 'power']);
  final activities = await output([
    'shell',
    'dumpsys',
    'activity',
    'activities',
  ]);
  String process;
  try {
    process = processState(await adb(['shell', 'pidof', package]), ownedPid);
  } catch (_) {
    process = 'unknown';
  }
  final processes = await output([
    'shell',
    'dumpsys',
    'activity',
    'processes',
    package,
  ]);
  final exits = await output([
    'shell',
    'dumpsys',
    'activity',
    'exit-info',
    package,
  ]);
  final events = await output(['logcat', '-b', 'events', '-d']);
  final lmk = await output([
    'logcat',
    '-b',
    'main',
    '-d',
    '-s',
    'lowmemorykiller',
    'lmkd',
  ]);
  final meminfo = await output(['shell', 'cat', '/proc/meminfo']);
  final timeout = await output([
    'shell',
    'settings',
    'get',
    'system',
    'screen_off_timeout',
  ]);
  final record = processes == null ? null : processRecord(processes);
  final exit = exits == null ? null : lastExit(exits, ownedPid);
  final mem = meminfo == null ? null : memory(meminfo);
  return {
    'name': name,
    'screen': power == null ? 'unknown' : screenState(power),
    'top': activities == null ? 'unknown' : topActivity(activities, package),
    'process': process,
    'proc_state': record?.procState,
    'oom_adj': record?.oomAdj,
    'frozen': record?.frozen,
    'exit_reason': exit?.reason,
    'exit_subreason': exit?.subreason,
    'exit_importance': exit?.importance,
    'exit_owned': exit?.owned,
    'lifecycle': events == null ? null : lifecycle(events, package),
    'low_memory': events == null || lmk == null ? null : lowMemory(events, lmk),
    'lmk_killed_app': lmk == null ? null : lmkKilledApp(lmk, package),
    'mem_total_mib': mem?.total,
    'mem_available_mib': mem?.available,
    'screen_off_timeout_ms': timeout == null ? null : setting(timeout),
  };
}
