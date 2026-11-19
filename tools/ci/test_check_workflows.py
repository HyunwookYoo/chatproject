import pathlib
import contextlib
import io
import tempfile
import unittest

from check_workflows import check, main
import check_workflows

PINNED = "apple-actions/upload-testflight-build@14df18fee5a4ff3b76971dd382f033494a0194e9"


def testflight(**overrides):
    doc = {
        "on": {"workflow_dispatch": None, "push": {"tags": ["testflight-*"]}},
        "permissions": {"contents": "read"},
        "env": {"RUST_TOOLCHAIN": "1.92.0", "CARGOKIT_TOOLCHAIN": "1.92.0"},
        "jobs": {"testflight": {
            "environment": "testflight",
            "steps": [
                {"name": "Refuse re-runs", "run": 'if [ "$GITHUB_RUN_ATTEMPT" != 1 ]; then exit 1; fi'},
                {"name": "Install provisioning profiles", "env": {"APP_PROFILE_B64": "${{ secrets.APPSTORE_PROFILE_APP_BASE64 }}"},
                 "run": "bash tools/ci/testflight_profile.sh"},
                {"name": "Locate IPA and check its contents", "run": 'bash tools/ci/check_ios_app.sh --signed "$app_dir"'},
                {"name": "Upload to TestFlight", "uses": PINNED},
            ],
        }},
    }
    doc.update(overrides)
    return doc


def ci(**overrides):
    doc = {
        "on": {"push": {"branches": ["main"]}, "pull_request": None},
        "permissions": {"contents": "read"},
        "env": {"RUST_TOOLCHAIN": "1.92.0", "FLUTTER_VERSION": "3.44.6"},
        "jobs": {"windows": {"steps": [{"run": "cargo test --locked -p chat_core"}]}, "ios-release": {"steps": [
            {"name": "Build the app for a device, unsigned", "run": "flutter pub get\nflutter build ios --release --no-codesign"},
            {"name": "Check the app and its extension", "run": "bash tools/ci/check_ios_app.sh app/build/ios/iphoneos/Runner.app"},
        ]}},
    }
    doc.update(overrides)
    return doc


class CheckTest(unittest.TestCase):
    path = pathlib.Path("testflight.yml")

    def test_android_cannot_omit_native_worker_and_restart_processes(self):
        doc = ci()
        doc['jobs']['android'] = {'steps': [{'run': 'perl -v\nmake --version\ncc --version'}]}
        self.assertTrue(any('native worker' in p for p in check(pathlib.Path('ci.yml'), doc)))

    def test_instrumentation_rejects_exit_zero_without_selected_success(self):
        for output in ('', 'OK (1 test)\n', 'INSTRUMENTATION_FAILED: crashed\nOK (1 test)',
                       'INSTRUMENTATION_STATUS: test=restartPersistence\nINSTRUMENTATION_STATUS_CODE: -4\nOK (1 test)\nINSTRUMENTATION_CODE: -1\n'):
            with self.subTest(output=output):
                self.assertTrue(check_workflows.check_android_instrumentation(output, 'read'))

    def test_instrumentation_requires_started_and_completed_expected_method(self):
        output = ''.join(f'INSTRUMENTATION_STATUS: class=dev.chatproject.chat_app.AccountKeyStoreTest\nINSTRUMENTATION_STATUS: test=restartPersistence\nINSTRUMENTATION_STATUS_CODE: {code}\n' for code in (1, 0)) + 'OK (1 test)\nINSTRUMENTATION_CODE: -1\n'
        self.assertEqual(check_workflows.check_android_instrumentation(output, 'read'), [])
        for bad in (output.replace('restartPersistence', 'otherMethod'), output.replace('CODE: 0', 'CODE: -2'), output.replace('CODE: 1', 'CODE: 0'), output.replace('OK (1 test)', 'OK (0 tests)'), output.replace('INSTRUMENTATION_CODE: -1', 'INSTRUMENTATION_CODE: 0')):
            self.assertTrue(check_workflows.check_android_instrumentation(bad, 'read'))

    def test_full_instrumentation_requires_worker_and_only_expected_assumption(self):
        methods = ('ownedTestPathAcceptsOnlyAbsoluteChildren', 'ownedTestPathRejectsChildSymlinkEscape',
                   'ownedTestPathRejectsRootSymlinkEscape', 'nativeWorkerUsesCachedHelperAndThreadLocalEnvironment',
                   'roundTripScopeTamperAndDelete', 'noBackupRootPreservesLegacyIncompleteState',
                   'invalidInputsAndCapacityDoNotCreateKeys', 'restartPersistence')
        output = ''.join(f'INSTRUMENTATION_STATUS: class=dev.chatproject.chat_app.AccountKeyStoreTest\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS_CODE: {code}\n' for method in methods for code in (1, -4 if method == 'restartPersistence' else 0)) + 'OK (8 tests)\nINSTRUMENTATION_CODE: -1\n'
        self.assertEqual(check_workflows.check_android_instrumentation(output, 'full'), [])
        self.assertTrue(check_workflows.check_android_instrumentation(output.replace('nativeWorkerUsesCachedHelperAndThreadLocalEnvironment', 'notWorker'), 'full'))
        self.assertTrue(check_workflows.check_android_instrumentation(output.replace('CODE: 0', 'CODE: -4'), 'full'))

    def push_output(self, methods, skipped=()):
        return ''.join(f'INSTRUMENTATION_STATUS: class=dev.chatproject.chat_app.PushReceiverTest\nINSTRUMENTATION_STATUS: test={method}\nINSTRUMENTATION_STATUS_CODE: {code}\n'
                       for method in methods for code in (1, -4 if method in skipped else 0)) + f'OK ({len(methods)} tests)\nINSTRUMENTATION_CODE: -1\n'

    def test_push_instrumentation_requires_its_class_and_only_the_cold_assumption(self):
        methods = ('unknownTagPostsNothing', 'storageRootSameFromNativeAndFlutter', 'brokenBindingFailsEnsureInitialized',
                   'notifierPostsUpdatesAndCancels', 'pushTheCoreCannotTakeStillNotifies', 'readClearFetchHasItsOwnWorkName',
                   'coldPushShowedNotification')
        output = self.push_output(methods, skipped=('coldPushShowedNotification',))
        self.assertEqual(check_workflows.check_android_instrumentation(output, 'push'), [])
        self.assertTrue(check_workflows.check_android_instrumentation(output.replace('unknownTagPostsNothing', 'other'), 'push'))
        self.assertTrue(check_workflows.check_android_instrumentation(output.replace('PushReceiverTest', 'AccountKeyStoreTest'), 'push'))
        self.assertTrue(check_workflows.check_android_instrumentation(output, 'full'))
        cold = self.push_output(('coldPushShowedNotification',))
        self.assertEqual(check_workflows.check_android_instrumentation(cold, 'cold'), [])
        self.assertTrue(check_workflows.check_android_instrumentation(self.push_output(('coldPushShowedNotification',), skipped=('coldPushShowedNotification',)), 'cold'))

    def test_android_cannot_omit_push_instrumentation(self):
        doc = ci()
        doc['jobs']['android'] = {'steps': [{'run': ':app:assembleDebugAndroidTest AccountKeyStoreTest for phase in write read accountPhase --android-instrumentation adb -s emulator-5554'}]}
        self.assertTrue(any('push receiver' in p for p in check(pathlib.Path('ci.yml'), doc)))
        doc['jobs']['android']['steps'][0]['run'] += ' PushReceiverTest pushPhase TestPushReceiver'
        self.assertFalse(any('push receiver' in p for p in check(pathlib.Path('ci.yml'), doc)))

    def test_a_good_testflight_workflow_passes(self):
        self.assertEqual(check(self.path, testflight()), [])

    def test_pull_request_trigger_is_rejected(self):
        problems = check(self.path, testflight(on={"pull_request": None}))
        self.assertTrue(any("triggers" in p for p in problems), problems)

    def test_missing_environment_is_rejected(self):
        doc = testflight()
        del doc["jobs"]["testflight"]["environment"]
        self.assertTrue(any("environment" in p for p in check(self.path, doc)))

    def test_missing_rerun_guard_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"].pop(0)
        self.assertTrue(any("Refuse re-runs" in p for p in check(self.path, doc)))

    def test_artifact_upload_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"].append(
            {"uses": "actions/upload-artifact@0123456789012345678901234567890123456789"})
        self.assertTrue(any("upload-artifact" in p for p in check(self.path, doc)))

    def test_unpinned_action_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"][-1]["uses"] = "apple-actions/upload-testflight-build@v5"
        self.assertTrue(any("not pinned" in p for p in check(self.path, doc)))

    def test_testflight_yaml_is_checked_like_testflight_yml(self):
        # GitHub runs .yaml workflows too, so the rules must follow the file stem, not the extension.
        doc = testflight()
        del doc["jobs"]["testflight"]["environment"]
        problems = check(pathlib.Path("testflight.yaml"), doc)
        self.assertTrue(any("environment" in p for p in problems), problems)

    def test_rerun_guard_must_test_the_attempt_and_fail(self):
        for body in ("true", 'test "$GITHUB_RUN_ATTEMPT" = 1', "exit 1"):
            with self.subTest(body=body):
                doc = testflight()
                doc["jobs"]["testflight"]["steps"][0]["run"] = body
                self.assertTrue(any("GITHUB_RUN_ATTEMPT" in p for p in check(self.path, doc)))

    def test_bare_push_is_rejected(self):
        # A push trigger without a filter starts the job for every branch and tag.
        shapes = {
            "push: (null)": {"workflow_dispatch": None, "push": None},
            "push: {}": {"push": {}},
            "on: push": "push",
            "on: [push]": ["push"],
            "on: [push, workflow_dispatch]": ["push", "workflow_dispatch"],
        }
        for label, on in shapes.items():
            with self.subTest(on=label):
                problems = check(self.path, testflight(on=on))
                self.assertTrue(any("push may only be for tags" in p for p in problems), problems)

    def test_push_for_branches_or_other_tags_is_rejected(self):
        for push in ({"branches": ["main"]}, {"tags": ["v*"]}, {"tags": ["testflight-*"], "branches": ["main"]}):
            with self.subTest(push=push):
                problems = check(self.path, testflight(on={"push": push}))
                self.assertTrue(any("push may only be for tags" in p for p in problems), problems)

    def test_dispatch_only_and_tags_only_triggers_are_accepted(self):
        for on in ({"workflow_dispatch": None}, "workflow_dispatch", {"push": {"tags": ["testflight-*"]}}):
            with self.subTest(on=on):
                self.assertEqual(check(self.path, testflight(on=on)), [])

    def test_missing_profile_step_is_rejected(self):
        doc = testflight()
        del doc["jobs"]["testflight"]["steps"][1]
        self.assertTrue(any("validate the profiles" in p for p in check(self.path, doc)))

    def test_profile_step_must_run_the_tested_script(self):
        # The checks themselves live in tools/ci/testflight_profile.sh (test_testflight_profile.py).
        doc = testflight()
        doc["jobs"]["testflight"]["steps"][1]["run"] = "ProvisionedDevices application-groups aps-environment"
        problems = check(self.path, doc)
        self.assertTrue(any("testflight_profile.sh" in p for p in problems), problems)

    def test_the_ipa_check_must_run_the_shared_script_on_the_signed_app(self):
        for body in ("echo ok", 'bash tools/ci/check_ios_app.sh "$app_dir"'):
            with self.subTest(body=body):
                doc = testflight()
                doc["jobs"]["testflight"]["steps"][2]["run"] = body
                problems = check(self.path, doc)
                self.assertTrue(any("check_ios_app.sh --signed" in p for p in problems), problems)

    def test_missing_ipa_check_step_is_rejected(self):
        doc = testflight()
        del doc["jobs"]["testflight"]["steps"][2]
        self.assertTrue(any("check_ios_app.sh --signed" in p for p in check(self.path, doc)))

    def test_the_ipa_must_be_checked_before_it_is_uploaded(self):
        doc = testflight()
        steps = doc["jobs"]["testflight"]["steps"]
        steps[2], steps[3] = steps[3], steps[2]
        self.assertTrue(any("before it uploads" in p for p in check(self.path, doc)))


class CiTest(unittest.TestCase):
    path = pathlib.Path("ci.yml")

    def test_a_good_ci_workflow_passes(self):
        self.assertEqual(check(self.path, ci()), [])

    def test_windows_requires_explicit_core_tests(self):
        doc = ci()
        doc['jobs']['windows'] = {'runs-on': 'windows-latest', 'steps': []}
        self.assertTrue(any('Windows cargo test -p chat_core' in p for p in check(self.path, doc)))
        doc['jobs']['windows']['steps'].append({'run': 'cargo test --locked -p chat_core'})
        self.assertFalse(any('Windows cargo test -p chat_core' in p for p in check(self.path, doc)))

    def test_ci_pins_rust_and_flutter_versions(self):
        for key, value in (('RUST_TOOLCHAIN', 'stable'), ('FLUTTER_VERSION', 'stable')):
            doc = ci()
            doc['env'] = {'RUST_TOOLCHAIN': '1.92.0', 'FLUTTER_VERSION': '3.44.6'}
            doc['env'][key] = value
            self.assertTrue(any(key in p for p in check(self.path, doc)))

    def test_native_sql_prerequisites_are_job_scoped(self):
        doc = ci()
        doc['jobs']['rust'] = {'runs-on': 'ubuntu-latest', 'steps': [{'run': 'cargo test --workspace'}]}
        self.assertTrue(any('SQL build prerequisites' in p for p in check(self.path, doc)))
        doc['jobs']['rust']['steps'].insert(0, {'run': 'perl -v\nmake --version\ncc --version'})
        self.assertFalse(any('SQL build prerequisites' in p for p in check(self.path, doc)))
        doc['jobs']['workflows'] = {'runs-on': 'ubuntu-latest', 'steps': []}
        self.assertFalse(any('SQL build prerequisites' in p for p in check(self.path, doc)))

    def test_bindings_job_cannot_accept_generated_drift(self):
        doc = ci()
        doc['jobs']['bindings'] = {'steps': [{'run': 'perl -v\nmake --version\ncc --version\nbash tools/gen_bindings.sh\nbash tools/gen_bindings.sh'}]}
        self.assertTrue(any('generated drift' in p for p in check(self.path, doc)))
        doc['jobs']['bindings']['steps'].append({'run': 'git status --porcelain\nexit 1'})
        self.assertFalse(any('generated drift' in p for p in check(self.path, doc)))

    def test_missing_ios_release_job_is_rejected(self):
        doc = ci()
        del doc["jobs"]["ios-release"]
        problems = check(self.path, doc)
        self.assertTrue(any("ios-release is missing" in p for p in problems), problems)

    def test_ios_release_must_build_the_release_configuration_unsigned(self):
        for build in ("flutter build ios --debug --no-codesign", "flutter build ios --release", "flutter build ipa --release"):
            with self.subTest(build=build):
                doc = ci()
                doc["jobs"]["ios-release"]["steps"][0]["run"] = build
                problems = check(self.path, doc)
                self.assertTrue(any("--no-codesign" in p for p in problems), problems)

    def test_ios_release_must_check_the_built_app(self):
        doc = ci()
        del doc["jobs"]["ios-release"]["steps"][1]
        problems = check(self.path, doc)
        self.assertTrue(any("check_ios_app.sh" in p for p in problems), problems)

    def test_ci_yaml_is_checked_like_ci_yml(self):
        # GitHub runs .yaml workflows too, so the rules must follow the file stem, not the extension.
        doc = ci()
        del doc["jobs"]["ios-release"]
        problems = check(pathlib.Path("ci.yaml"), doc)
        self.assertTrue(any("ios-release is missing" in p for p in problems), problems)


class PolicyTest(unittest.TestCase):
    def workflow(self, **overrides):
        doc = {"on": "push", "permissions": {"contents": "read"}, "env": {"RUST_TOOLCHAIN": "1.92.0", "FLUTTER_VERSION": "3.44.6"}, "jobs": {"test": {"steps": []}}}
        doc.update(overrides)
        return doc

    def test_default_trigger_allowlist_and_e2e_schedule_exception(self):
        for event in ("push", "pull_request", "workflow_dispatch", "schedule", "issues",
                      "pull_request_target", "workflow_run", "workflow_call"):
            for filename in ("ordinary.yml", "e2e.yml", "E2E.YAML"):
                with self.subTest(event=event, filename=filename):
                    allowed = (event in ("push", "pull_request", "workflow_dispatch")
                               if filename == "ordinary.yml" else event in ("schedule", "workflow_dispatch"))
                    problems = check(pathlib.Path(filename), self.workflow(on={event: None}))
                    self.assertEqual(problems == [], allowed, problems)

    def test_runner_context_is_unavailable_in_job_env_but_allowed_in_step_env(self):
        for value in ('${{ runner.temp }}/chatproject-123-1-android', "${{ runner['temp'] }}"):
            with self.subTest(value=value):
                doc = self.workflow()
                job = doc['jobs']['test']
                job['env'] = {'GRADLE_USER_HOME': value}
                problems = check(pathlib.Path('ordinary.yml'), doc)
                self.assertTrue(any('runner context is unavailable in job env' in p for p in problems), problems)
                del job['env']
                job['steps'] = [{'env': {'GRADLE_USER_HOME': value}, 'run': 'echo ready'}]
                self.assertEqual(check(pathlib.Path('ordinary.yml'), doc), [])

    def test_job_permissions_may_only_narrow_top_level_read(self):
        for permissions, allowed in ((None, True), ({}, True), ({"contents": "read"}, True),
                                     ({"contents": "none"}, True), ({"contents": "write"}, False),
                                     ({"issues": "read"}, False), ("read-all", False), ("write-all", False)):
            with self.subTest(permissions=permissions):
                doc = self.workflow()
                if permissions is not None:
                    doc["jobs"]["test"]["permissions"] = permissions
                self.assertEqual(check(pathlib.Path("ordinary.yml"), doc) == [], allowed)

    def test_local_actions_and_pinned_reusable_workflows(self):
        for uses, reusable, allowed in (("./tools/action", False, True),
                                        ("owner/action@v1", False, False),
                                        ("owner/action@" + "a" * 40, False, True),
                                        ("./.github/workflows/shared.yml", True, True),
                                        ("owner/repo/.github/workflows/shared.yml@main", True, False),
                                        ("owner/repo/.github/workflows/shared.yml@" + "a" * 40, True, True)):
            with self.subTest(uses=uses):
                job = {"uses": uses} if reusable else {"steps": [{"uses": uses}]}
                doc = self.workflow(jobs={"test": job})
                self.assertEqual(check(pathlib.Path("ordinary.yml"), doc) == [], allowed)

    def test_checkout_requires_disabled_credentials_even_when_pinned(self):
        doc = self.workflow(jobs={"test": {"steps": [{"uses": "actions/checkout@" + "a" * 40}]}})
        self.assertTrue(any("persist-credentials" in p for p in check(pathlib.Path("ordinary.yml"), doc)))

    def test_top_level_permissions_and_toolchain_contracts(self):
        for overrides in ({"permissions": {}}, {"permissions": {"contents": "read", "issues": "read"}},
                          {"env": {"CARGOKIT_TOOLCHAIN": "stable", "RUST_TOOLCHAIN": "1.92.0"}}):
            with self.subTest(overrides=overrides):
                self.assertTrue(check(pathlib.Path("ordinary.yml"), self.workflow(**overrides)))

    def test_non_mapping_documents_have_one_filename_diagnostic(self):
        for doc in (None, "workflow", ["push"], 42):
            with self.subTest(doc=doc):
                problems = check(pathlib.Path("broken.yaml"), doc)
                self.assertEqual(len(problems), 1)
                self.assertIn("broken.yaml", problems[0])

    def test_discovery_accepts_yaml_extensions_case_insensitively(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            workflows = root / ".github/workflows"
            workflows.mkdir(parents=True)
            for suffix in ("yml", "yaml", "YML", "YAML"):
                (workflows / ("ordinary." + suffix)).write_text(
                    "on: push\npermissions: {contents: read}\njobs: {}\n", encoding="utf-8")
                with contextlib.redirect_stdout(io.StringIO()) as output:
                    self.assertEqual(main(root), 0)
                self.assertEqual(output.getvalue(), "workflows ok\n")
                (workflows / ("ordinary." + suffix)).unlink()

    def test_missing_files_and_malformed_yaml_report_without_traceback(self):
        with tempfile.TemporaryDirectory() as folder:
            root = pathlib.Path(folder)
            workflows = root / ".github/workflows"
            workflows.mkdir(parents=True)
            with contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(main(root), 1)
            self.assertIn("no workflow files", output.getvalue())
            for contents in ("", "42", "- push", "on: [push"):
                with self.subTest(contents=contents):
                    (workflows / "broken.yaml").write_text(contents, encoding="utf-8")
                    with contextlib.redirect_stdout(io.StringIO()) as output:
                        self.assertEqual(main(root), 1)
                    self.assertEqual(len(output.getvalue().splitlines()), 1)
                    self.assertIn("broken.yaml:", output.getvalue())
                    self.assertNotIn("Traceback", output.getvalue())


SECRET = "${{ secrets.SOME_TOKEN }}"
ORDINARY = pathlib.Path("ordinary.yml")


def ordinary(job, **overrides):
    doc = {"on": "workflow_dispatch", "permissions": {"contents": "read"}, "jobs": {"test": job}}
    doc.update(overrides)
    return doc


def secret_problems(doc, path=ORDINARY):
    return [p for p in check(path, doc) if "secret" in p]


class SecretPlacementTest(unittest.TestCase):
    """Rule (a): a secret reaches only the step that needs it, in a job that declares an Environment."""

    def test_step_env_and_action_inputs_of_an_environment_job_are_allowed(self):
        for step in ({"env": {"TOKEN": SECRET}, "run": 'use "$TOKEN"'},
                     {"env": {"TOKEN_SET": "${{ secrets.SOME_TOKEN != '' }}"}, "run": 'test "$TOKEN_SET" = true'},
                     {"uses": "owner/action@" + "a" * 40, "with": {"token": SECRET}}):
            for environment in ("release", {"name": "release", "url": "https://example.invalid"}):
                with self.subTest(step=step, environment=environment):
                    doc = ordinary({"environment": environment, "steps": [step]})
                    self.assertEqual(check(ORDINARY, doc), [])

    def test_the_word_secrets_outside_an_expression_is_not_a_reference(self):
        doc = ordinary({"steps": [{"name": "Write secrets.json", "run": 'cp "$RUNNER_TEMP/secrets.json" out'}]})
        self.assertEqual(check(ORDINARY, doc), [])

    def test_secrets_elsewhere_are_rejected(self):
        cases = {
            "workflow env": ordinary({"environment": "release", "steps": []}, env={"TOKEN": SECRET}),
            "job env": ordinary({"environment": "release", "env": {"TOKEN": SECRET}, "steps": []}),
            "run text": ordinary({"environment": "release", "steps": [{"run": "use " + SECRET}]}),
            "whole context": ordinary({"environment": "release", "steps": [{"run": "echo '${{ toJSON(secrets) }}'"}]}),
            "step if": ordinary({"environment": "release", "steps": [{"if": "secrets.SOME_TOKEN != ''", "run": "true"}]}),
            "no environment": ordinary({"steps": [{"env": {"TOKEN": SECRET}, "run": 'use "$TOKEN"'}]}),
            "no environment, action": ordinary({"steps": [{"uses": "owner/action@" + "a" * 40, "with": {"token": SECRET}}]}),
            "service credentials": ordinary({"environment": "release", "services": {"db": {
                "image": "db", "credentials": {"password": SECRET}}}, "steps": []}),
        }
        for label, doc in cases.items():
            with self.subTest(case=label):
                self.assertTrue(secret_problems(doc), check(ORDINARY, doc))

    def test_reusable_workflow_secrets_are_rejected(self):
        for secrets in ("inherit", {"token": SECRET}):
            with self.subTest(secrets=secrets):
                doc = ordinary({"uses": "./.github/workflows/shared.yml", "secrets": secrets})
                self.assertTrue(secret_problems(doc), check(ORDINARY, doc))

    def test_secrets_context_is_matched_case_insensitively(self):
        for value in ("${{ Secrets.SOME_TOKEN }}", "${{ SECRETS['SOME_TOKEN'] }}"):
            with self.subTest(value=value):
                doc = ordinary({"environment": "release", "env": {"TOKEN": value}, "steps": []})
                self.assertTrue(secret_problems(doc), check(ORDINARY, doc))

    def test_environment_name_must_be_literal(self):
        doc = ordinary({"environment": "${{ inputs.target }}", "steps": []})
        self.assertTrue(any("literal" in p for p in check(ORDINARY, doc)))

    def test_the_committed_workflows_pass(self):
        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(main(), 0, output.getvalue())


class TestflightEnvironmentTest(unittest.TestCase):
    """Rule (b): the TestFlight rules follow `environment: testflight`, not the file name."""

    def release(self, environment="testflight", **overrides):
        doc = testflight(**overrides)
        doc["jobs"]["testflight"]["environment"] = environment
        return doc

    def test_a_good_testflight_job_passes_in_any_file(self):
        for environment in ("testflight", {"name": "testflight"}):
            with self.subTest(environment=environment):
                self.assertEqual(check(pathlib.Path("release.yml"), self.release(environment)), [])

    def test_job_rules_apply_in_any_file(self):
        for environment in ("testflight", {"name": "testflight"}):
            with self.subTest(environment=environment):
                doc = self.release(environment)
                doc["jobs"]["testflight"]["steps"].pop(0)
                self.assertTrue(any("Refuse re-runs" in p for p in check(pathlib.Path("release.yml"), doc)))
                doc = self.release(environment)
                doc["jobs"]["testflight"]["steps"].append(
                    {"uses": "actions/upload-artifact@0123456789012345678901234567890123456789"})
                self.assertTrue(any("upload-artifact" in p for p in check(pathlib.Path("release.yml"), doc)))

    def test_environment_name_is_matched_case_insensitively(self):
        for environment in ("TestFlight", {"name": "TESTFLIGHT"}):
            with self.subTest(environment=environment):
                doc = self.release(environment)
                doc["jobs"]["testflight"]["steps"].pop(0)
                self.assertTrue(any("Refuse re-runs" in p for p in check(pathlib.Path("release.yml"), doc)))

    def test_trigger_rules_apply_in_any_file(self):
        doc = self.release(on={"push": {"branches": ["main"]}})
        self.assertTrue(any("push may only be for tags" in p for p in check(pathlib.Path("release.yml"), doc)))

    def test_jobs_with_other_environments_are_not_testflight_jobs(self):
        doc = ordinary({"environment": "release", "steps": []})
        self.assertEqual(check(ORDINARY, doc), [])

    def test_build_ipa_step_holds_no_secret(self):
        # cargokit's build_pod.sh runs `env`, so a failed build would print the secret.
        doc = testflight()
        doc["jobs"]["testflight"]["steps"].insert(2, {"name": "Build IPA", "env": {"KEY": SECRET}, "run": "flutter build ipa"})
        self.assertTrue(any("Build IPA" in p for p in check(CheckTest.path, doc)))
        del doc["jobs"]["testflight"]["steps"][2]["env"]
        self.assertEqual(check(CheckTest.path, doc), [])


class PushE2eEnvironmentTest(unittest.TestCase):
    """Rule (c): a `push-e2e` job uploads no artifact and writes its secret files under $RUNNER_TEMP only."""

    path = pathlib.Path("e2e.yml")

    def nightly(self, *steps):
        return {
            "on": {"schedule": [{"cron": "0 18 * * *"}], "workflow_dispatch": None},
            "permissions": {"contents": "read"},
            "env": {"RUST_TOOLCHAIN": "1.92.0", "FLUTTER_VERSION": "3.44.6"},
            "jobs": {"android-fcm": {"environment": "push-e2e", "steps": [
                {"name": "FCM secret present", "id": "secret",
                 "env": {"FCM_SET": "${{ secrets.FCM_SERVICE_ACCOUNT_JSON != '' }}"},
                 "run": 'if [ "$FCM_SET" != true ]; then echo "skipped: no FCM secret" >> "$GITHUB_STEP_SUMMARY"; fi\n'
                        'echo "present=$FCM_SET" >> "$GITHUB_OUTPUT"'},
                {"name": "Write the service account",
                 "env": {"FCM_JSON": "${{ secrets.FCM_SERVICE_ACCOUNT_JSON }}"},
                 "run": 'umask 077\nprintf \'%s\' "$FCM_JSON" > "$RUNNER_TEMP/fcm.json"\n'
                        'printf \'%s\' "$FCM_JSON" | tee -a "${RUNNER_TEMP}/fcm-copy.json" >/dev/null 2>&1\n'
                        'printf \'%s\' "$FCM_JSON" >"$RUNNER_TEMP"/fcm-quoted.json\n'
                        'test -s "$RUNNER_TEMP/fcm.json" >&2'},
                *steps,
            ]}},
        }

    def test_a_good_push_e2e_job_passes(self):
        self.assertEqual(check(self.path, self.nightly()), [])

    def test_artifact_upload_is_rejected(self):
        doc = self.nightly({"uses": "actions/upload-artifact@0123456789012345678901234567890123456789"})
        self.assertTrue(any("upload-artifact" in p for p in check(self.path, doc)))

    def test_secret_files_outside_runner_temp_are_rejected(self):
        for write in ('> fcm.json', '> "$HOME/fcm.json"', '>> "$GITHUB_ENV"', '> "$GITHUB_STEP_SUMMARY"',
                      '| tee fcm.json', '| tee -a "$GITHUB_OUTPUT"', '> "$RUNNER_TEMP/../fcm.json"',
                      '> /tmp/fcm.json', '&> out.log', '> "$RUNNER_TEMP_X/fcm.json"'):
            with self.subTest(write=write):
                doc = self.nightly({"env": {"FCM_JSON": "${{ secrets.FCM_SERVICE_ACCOUNT_JSON }}"},
                                    "run": 'printf \'%s\' "$FCM_JSON" ' + write})
                self.assertTrue(any("RUNNER_TEMP" in p for p in check(self.path, doc)), write)

    def test_secret_action_inputs_are_rejected(self):
        # An action writes wherever it likes; push-e2e secrets go through a run step that the checker can read.
        doc = self.nightly({"uses": "owner/action@" + "a" * 40, "with": {"key": "${{ secrets.FCM_SERVICE_ACCOUNT_JSON }}"}})
        self.assertTrue(any("push-e2e" in p for p in check(self.path, doc)))

    def test_secret_env_on_an_action_step_is_rejected(self):
        doc = self.nightly({"uses": "owner/action@" + "a" * 40,
                            "env": {"FCM_JSON": "${{ secrets.FCM_SERVICE_ACCOUNT_JSON }}"}})
        self.assertTrue(any("push-e2e secrets go in a run step" in p for p in check(self.path, doc)))
        doc["jobs"]["android-fcm"]["steps"][-1]["env"] = {"FCM_SET": "${{ secrets.FCM_SERVICE_ACCOUNT_JSON != '' }}"}
        self.assertEqual(check(self.path, doc), [])

    def test_environment_name_is_matched_case_insensitively(self):
        for environment in ("Push-E2E", {"name": "PUSH-E2E"}):
            with self.subTest(environment=environment):
                doc = self.nightly({"uses": "actions/upload-artifact@0123456789012345678901234567890123456789"})
                doc["jobs"]["android-fcm"]["environment"] = environment
                self.assertTrue(any("upload-artifact" in p for p in check(self.path, doc)))

    def test_the_rules_follow_the_environment_not_the_file(self):
        doc = self.nightly({"uses": "actions/upload-artifact@0123456789012345678901234567890123456789"})
        doc["on"] = "workflow_dispatch"
        self.assertTrue(any("upload-artifact" in p for p in check(ORDINARY, doc)))
        doc["jobs"]["android-fcm"]["environment"] = {"name": "push-e2e"}
        self.assertTrue(any("upload-artifact" in p for p in check(ORDINARY, doc)))


if __name__ == "__main__":
    unittest.main()
