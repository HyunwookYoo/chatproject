import 'package:test/test.dart';
import 'package:path/path.dart' as p;

import '../bin/run.dart' as cli;

void main() {
  final environment = {
    'GITHUB_ACTIONS': 'true',
    'RUNNER_TEMP': '/tmp/runner',
    'GITHUB_RUN_ID': '123',
    'GITHUB_RUN_ATTEMPT': '1',
    'GITHUB_JOB': 'android',
    'GRADLE_USER_HOME': p.join('/tmp/runner', 'chatproject-123-1-android'),
  };
  test('only GitHub Android profile stops its exact job-owned Gradle home', () {
    expect(cli.usesOwnedCiGradle('android', environment), isTrue);
    expect(cli.usesOwnedCiGradle('mixed', environment), isFalse);
    expect(cli.usesOwnedCiGradle('windows', environment), isFalse);
    expect(cli.usesOwnedCiGradle('android', {}), isFalse);
  });
  test(
    'CI shared, missing, traversal and wrong-run homes fail before building',
    () {
      for (final home in [
        '',
        '/home/user/.gradle',
        '/tmp/runner/chatproject-122-1-android',
        '/tmp/runner/../chatproject-123-1-android',
      ]) {
        expect(
          () => cli.usesOwnedCiGradle('android', {
            ...environment,
            'GRADLE_USER_HOME': home,
          }),
          throwsStateError,
        );
      }
      expect(
        () => cli.usesOwnedCiGradle(
          'android',
          {...environment}..remove('RUNNER_TEMP'),
        ),
        throwsStateError,
      );
    },
  );
}
