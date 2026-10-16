import 'dart:async';

import 'package:chat_e2e/src/owned_resources.dart';
import 'package:test/test.dart';

void main() {
  test('cleanup tries all resources in reverse and reports failures', () async {
    final calls = <String>[];
    final owned = OwnedResources();
    owned.add('first', () async {
      calls.add('first');
    });
    owned.add('second', () async {
      calls.add('second');
      throw StateError('stop');
    });
    final failures = await owned.close();
    expect(calls, ['second', 'first']);
    expect(failures, [contains('second: Bad state: stop')]);
    expect(await owned.close(), failures);
    expect(calls, ['second', 'first']);
  });

  test('concurrent close shares cleanup and rejects late admission', () async {
    final release = Completer<void>();
    var calls = 0;
    final owned = OwnedResources();
    owned.add('child', () async {
      calls++;
      await release.future;
    });
    final first = owned.close();
    final second = owned.close();
    expect(() => owned.add('late', () async {}), throwsStateError);
    release.complete();
    expect(await first, isEmpty);
    expect(await second, isEmpty);
    expect(calls, 1);
  });

  test('cleanup callbacks cannot acquire new resources reentrantly', () async {
    final owned = OwnedResources();
    owned.add('first', () async {
      expect(() => owned.add('late', () async {}), throwsStateError);
    });
    expect(await owned.close(), isEmpty);
  });
}
