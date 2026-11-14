import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/android_probe.dart';
import 'package:test/test.dart';

const package = 'dev.chatproject.chat_app';

void main() {
  test('screen wakefulness', () {
    expect(
      screenState('Power Manager State:\n  mWakefulness=Awake\n'),
      'awake',
    );
    expect(screenState('  mWakefulness=Asleep\n'), 'asleep');
    expect(screenState('  mWakefulness=Dozing\n'), 'dozing');
    expect(screenState('  mWakefulness=PRIVATE\n'), 'unknown');
    expect(screenState(''), 'unknown');
  });

  test('top resumed activity owner as a fixed category', () {
    String top(String owner) => topActivity(
      'ACTIVITY MANAGER ACTIVITIES\n'
      '  topResumedActivity=ActivityRecord{7d1d3c4 u0 $owner/.Main t12}\n'
      '  ResumedActivity: ActivityRecord{1 u0 other.pkg/.X t3}\n',
      package,
    );
    expect(top(package), 'app');
    expect(
      top('com.google.android.permissioncontroller'),
      'permission_controller',
    );
    expect(top('com.android.settings'), 'settings');
    expect(top('com.google.android.apps.nexuslauncher'), 'launcher');
    expect(top('com.android.systemui'), 'systemui');
    expect(top('com.private.thing'), 'other');
    expect(topActivity('  topResumedActivity=null\n', package), 'none');
    expect(
      topActivity(
        '  ResumedActivity: ActivityRecord{1 u0 $package/.M t3}\n',
        package,
      ),
      'app',
    );
    expect(topActivity('', package), 'unknown');
  });

  test('process alive against the owned pid', () {
    expect(
      processState(ProcessResult(1, 0, '4321\n', ''), '4321'),
      'alive_same',
    );
    expect(
      processState(ProcessResult(1, 0, '9999\n', ''), '4321'),
      'alive_other',
    );
    expect(processState(ProcessResult(1, 1, '', ''), '4321'), 'none');
    // adb itself failed (device offline): not "exited".
    expect(
      processState(ProcessResult(1, 255, '', 'error: device offline'), '4321'),
      'unknown',
    );
  });

  test(
    'the pid monitor needs two misses in a row; an adb failure counts nothing',
    () {
      final same = ProcessResult(1, 0, '4321\n', '');
      final gone = ProcessResult(1, 1, '', '');
      final other = ProcessResult(1, 0, '9999\n', '');
      final adbFailed = ProcessResult(1, 255, '', 'error: device offline');
      expect(ownedMisses(gone, '4321', 0), 1);
      expect(ownedMisses(other, '4321', 1), 2);
      expect(ownedMisses(same, '4321', 1), 0);
      // Says nothing about the app: the count stays.
      expect(ownedMisses(adbFailed, '4321', 0), 0);
      expect(ownedMisses(adbFailed, '4321', 1), 1);
      expect(ownedExitedAfter, 2);
    },
  );

  test('process record numbers and freeze state', () {
    final record = processRecord(
      '  *APP* UID 10190 ProcessRecord{1 4321:$package/u0a190}\n'
      '    oom adj: max=1001 curRaw=0 setRaw=0 cur=0 set=0\n'
      '    curProcState=2 mRepProcState=2 setProcState=2\n'
      '    isFreezeExempt=false isPendingFreeze=false isFrozen=false\n',
    );
    expect(record.procState, 2);
    expect(record.oomAdj, 0);
    expect(record.frozen, false);
    final cached = processRecord(
      '    oom adj: max=1001 curRaw=900 setRaw=900 cur=900 set=900\n'
      '    curProcState=19\n    isFrozen=true\n',
    );
    expect((cached.procState, cached.oomAdj, cached.frozen), (19, 900, true));
    final none = processRecord('');
    expect((none.procState, none.oomAdj, none.frozen), (null, null, null));
  });

  test('newest exit record: reason name, subreason, importance, owned pid', () {
    const dump =
        'ApplicationExitInfo records:\n'
        '  package: dev.chatproject.chat_app\n'
        '    Historical Process Exit for uid=10190\n'
        '        ApplicationExitInfo #0:\n'
        '          timestamp=2026-11-08 08:30:00.123 pid=4321 realUid=10190 packageUid=10190 definingUid=10190 user=0\n'
        '          process=dev.chatproject.chat_app reason=3 (LOW MEMORY) subreason=0 (UNKNOWN) status=0\n'
        '          importance=400 pss=1KB rss=2KB description=lmk state=empty\n'
        '        ApplicationExitInfo #1:\n'
        '          timestamp=2026-11-08 08:29:00.123 pid=1111 realUid=10190\n'
        '          process=dev.chatproject.chat_app reason=10 (USER REQUESTED) subreason=23 (FORCE STOP) status=0\n'
        '          importance=100\n';
    final exit = lastExit(dump, '4321');
    expect(
      (exit.reason, exit.subreason, exit.importance, exit.owned),
      ('LOW_MEMORY', 0, 400, true),
    );
    final other = lastExit(dump, '1111');
    expect(other.owned, false);
    final none = lastExit('ApplicationExitInfo records:\n', '1');
    expect((none.reason, none.owned), (null, null));
  });

  test('app lifecycle tags in order, other apps and text dropped', () {
    const events =
        '11-08 08:30:00.000  500  520 I am_proc_start: [0,4321,10190,$package,next-top-activity,{$package/$package.MainActivity}]\n'
        '11-08 08:30:00.100  500  520 I am_proc_start: [0,5000,10100,com.other,service,{com.other/x}]\n'
        '11-08 08:30:00.200 4321 4321 I wm_on_create_called: [1,$package.MainActivity,performCreate,10]\n'
        '11-08 08:30:00.300 4321 4321 I wm_on_resume_called: [1,$package.MainActivity,RESUME_ACTIVITY,3]\n'
        '11-08 08:30:00.301 4321 4321 I wm_on_resume_called: [1,$package.MainActivity,RESUME_ACTIVITY,3]\n'
        '11-08 08:31:00.000  500  600 I screen_toggled: 0\n'
        '11-08 08:31:00.100 4321 4321 I wm_on_paused_called: [1,$package.MainActivity,performPause,1]\n'
        '11-08 08:31:00.200 4321 4321 I wm_on_stop_called: [1,$package.MainActivity,STOP_ACTIVITY_ITEM,2]\n'
        '11-08 08:31:10.000  500  700 I am_freeze: [4321,$package]\n'
        '11-08 08:31:11.000  500  700 I am_low_memory: 3\n'
        '11-08 08:31:12.000  500  700 I am_kill: [0,4321,$package,900,PRIVATE reason]\n'
        '11-08 08:31:12.100  500  700 I am_proc_died: [0,4321,$package,900,19]\n'
        '11-08 08:31:12.200  500  700 I wm_on_resume_called: [1,${package}_other.Main,x,1]\n';
    expect(lifecycle(events, package), [
      'am_proc_start',
      'wm_on_create_called',
      'wm_on_resume_called',
      'screen_off',
      'wm_on_paused_called',
      'wm_on_stop_called',
      'am_freeze',
      'am_kill',
      'am_proc_died',
    ]);
    expect(lowMemory(events, ''), true);
    expect(lowMemory('', ''), false);
  });

  test('lifecycle keeps only the newest entries', () {
    final events = [
      for (var i = 0; i < 40; i++)
        'I ${i.isEven ? 'wm_on_resume_called' : 'wm_on_paused_called'}: [1,$package.MainActivity,x,1]',
    ].join('\n');
    expect(lifecycle(events, package), hasLength(androidLifecycleLimit));
  });

  test('configuration change masks and clock skew', () {
    const events =
        '11-08 08:30:00.000  500  520 I configuration_changed: 536870912\n'
        '11-08 08:30:00.100 4321 4321 I wm_relaunch_resume_activity: [0,1,$package.MainActivity,3,0]\n'
        '11-08 08:30:01.000  500  520 I configuration_changed: 3\n'
        '11-08 08:30:01.000  500  520 I configuration_changed: PRIVATE\n';
    expect(configMasks(events), [536870912, 3]);
    expect(lifecycle(events, package), ['wm_relaunch_resume_activity']);
    expect(
      configMasks(List.filled(12, 'I configuration_changed: 4').join('\n')),
      hasLength(androidConfigLimit),
    );
    expect(clockSkew('1700000100\n', 1700000000), 100);
    expect(clockSkew('PRIVATE', 1), isNull);
  });

  test('load, app CPU and rendered frames', () {
    expect(loadX100('3.52 2.10 1.00 4/900 1234\n'), 352);
    expect(loadX100('PRIVATE'), isNull);
    // utime 300 + stime 200 ticks over (2000 - 1000) ticks of age: 50 %.
    const stat =
        '4321 (a b) S 1 2 3 4 5 6 7 8 9 10 300 200 0 0 20 0 30 0 1000 0 0';
    expect(appCpuPercent(stat, '20.00 10.00'), 50);
    expect(appCpuPercent('PRIVATE', '20'), isNull);
    expect(appCpuPercent(stat, 'PRIVATE'), isNull);
    expect(framesRendered('  Total frames rendered: 1234\n'), 1234);
    expect(framesRendered('none'), isNull);
    expect(cpuCount('4\n'), 4);
    expect(cpuCount('0\n'), isNull);
    expect(cpuCount('PRIVATE'), isNull);
  });

  test('lmkd kills and memory', () {
    const lmk =
        "11-08 08:31:12.000  300  300 I lowmemorykiller: Kill '$package' (4321), uid 10190, oom_score_adj 900 to free 100000kB\n";
    expect(lmkKilledApp(lmk, package), true);
    expect(
      lmkKilledApp("I lowmemorykiller: Kill 'com.other' (1)", package),
      false,
    );
    expect(lowMemory('', lmk), true);
    final mem = memory(
      'MemTotal:        2014444 kB\nMemFree: 1 kB\nMemAvailable:     512000 kB\n',
    );
    expect((mem.total, mem.available), (1967, 500));
    expect(setting('60000\n'), 60000);
    expect(setting('null\n'), isNull);
  });

  test('probe keeps fixed keys and fixed values only', () async {
    final outputs = <String, ProcessResult>{
      'shell dumpsys power': ProcessResult(1, 0, '  mWakefulness=Asleep\n', ''),
      'shell dumpsys activity activities': ProcessResult(
        1,
        0,
        '  topResumedActivity=ActivityRecord{1 u0 com.android.systemui/.Keyguard t1}\n',
        '',
      ),
      'shell pidof $package': ProcessResult(1, 1, '', ''),
      'shell dumpsys activity processes $package': ProcessResult(1, 0, '', ''),
      'shell dumpsys activity exit-info $package': ProcessResult(
        1,
        0,
        'ApplicationExitInfo #0:\n pid=4321 reason=14 (FREEZER) subreason=0 importance=400 description=PRIVATE_TOKEN\n',
        '',
      ),
      'logcat -b events -d': ProcessResult(1, 0, 'I screen_toggled: 0\n', ''),
      'logcat -b main -d -s lowmemorykiller lmkd': ProcessResult(1, 0, '', ''),
      'shell cat /proc/meminfo': ProcessResult(1, 0, 'MemTotal: 2048 kB\n', ''),
      'shell date +%s': ProcessResult(1, 0, '1000\n', ''),
      'shell cat /proc/loadavg': ProcessResult(
        1,
        0,
        '3.52 2.10 1.00 4/900 1234\n',
        '',
      ),
      'shell cat /proc/4321/stat': ProcessResult(
        1,
        0,
        '4321 (PRIVATE app) S 1 2 3 4 5 6 7 8 9 10 300 200 0 0 20 0 30 0 1000 0 0\n',
        '',
      ),
      'shell cat /proc/uptime': ProcessResult(1, 0, '20.00 10.00\n', ''),
      'shell nproc': ProcessResult(1, 0, '2\n', ''),
      'shell dumpsys gfxinfo $package': ProcessResult(
        1,
        0,
        'Total frames rendered: 77\n',
        '',
      ),
      'shell settings get system screen_off_timeout': ProcessResult(
        1,
        1,
        '',
        'PRIVATE_TOKEN',
      ),
    };
    final probe = await probeAndroid(
      (args) async => outputs[args.join(' ')]!,
      name: 'android-2',
      package: package,
      ownedPid: '4321',
      launchSkew: -3,
      launchMsMax: 12000,
      hostLoad: 410,
      hostSeconds: () => 1090,
    );
    expect(probe, {
      'name': 'android-2',
      'screen': 'asleep',
      'top': 'systemui',
      'process': 'none',
      'proc_state': null,
      'oom_adj': null,
      'frozen': null,
      'exit_reason': 'FREEZER',
      'exit_subreason': 0,
      'exit_importance': 400,
      'exit_owned': true,
      'lifecycle': ['screen_off'],
      'config_masks': [],
      'low_memory': false,
      'lmk_killed_app': false,
      'mem_total_mib': 2,
      'mem_available_mib': null,
      'screen_off_timeout_ms': null,
      'clock_skew_s': -90,
      'launch_clock_skew_s': -3,
      'launch_ms_max': 12000,
      'load_x100': 352,
      'host_load_x100': 410,
      'app_cpu_pct': 50,
      'frames': 77,
      'cpus': 2,
    });
    expect(jsonEncode(probe), isNot(contains('PRIVATE')));
  });

  test('value sets match the shared summary allowlist', () {
    final shared =
        (jsonDecode(File('diagnostic_details.json').readAsStringSync())
                as Map<String, dynamic>)['android']
            as Map<String, dynamic>;
    Set<String> list(String key) =>
        (shared[key] as List).cast<String>().toSet();
    expect(list('screen'), androidScreens);
    expect(list('top'), androidTops);
    expect(list('process'), androidProcesses);
    expect(list('exit_reason'), androidExitReasons.values.toSet());
    expect(list('lifecycle'), androidLifecycle);
    expect(shared['lifecycle_limit'], androidLifecycleLimit);
    expect(shared['config_limit'], androidConfigLimit);
  });
}
