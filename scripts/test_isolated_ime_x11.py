"""Tests for the opt-in-only, fixed-sequence private X11/XIM harness."""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import pty
import socket
from pathlib import Path
import subprocess
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock


SCRIPT = Path(__file__).with_name('isolated-ime-x11.py')
SPEC = importlib.util.spec_from_file_location('isolated_ime_x11_tests', SCRIPT)
assert SPEC is not None and SPEC.loader is not None
IME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(IME)


class IsolatedImePlanTests(unittest.TestCase):
    def test_default_mode_is_plan_only_and_never_spawns(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')) as popen, \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')) as run, \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main([]), 0)
        value = json.loads(output.getvalue())
        self.assertEqual(value['status'], 'plan-only-no-process-no-write')
        self.assertEqual(value['run_requires_explicit_flag'], '--run')
        self.assertFalse(value['input']['arbitrary_text_argument'])
        popen.assert_not_called(); run.assert_not_called()

    def test_publication_trace_adds_only_the_fixed_app_diagnostic_flag(self):
        self.assertEqual(IME.app_command(), ['/pigade/app', '/artifacts/state/host.sock'])
        self.assertEqual(IME.app_command(True), ['/pigade/app', '/artifacts/state/host.sock',
                                                 '--diagnostic-publication-trace'])
        self.assertEqual(IME.app_command('arbitrary'), IME.app_command())

    def test_context_prompt_flow_is_fixed_plan_only_and_never_targets_terminal_input(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'context']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'context')
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        self.assertIn('native WM_DELETE_WINDOW close review cancels with Escape and explicitly discards an unsaved draft with Ctrl+Enter',
                      plan['input']['context'])
        self.assertIn('Ctrl+Shift+P inserts its exact text into the frozen saved workspace draft',
                      plan['input']['context'])
        self.assertIn('no terminal input', plan['input']['context'])

        source = Path(__file__).with_name('x11-ime-input.c').read_text()
        harness = SCRIPT.read_text()
        stages = ('context-close', 'context-close-confirm', 'wm-close',
                  'prompts-open', 'prompt-new', 'prompt-create-fields',
                  'prompt-save', 'prompt-select-first', 'prompt-edit', 'prompt-rename',
                  'prompt-save-edit', 'prompt-close', 'prompt-insert-open', 'prompt-use-first',
                  'prompt-delete', 'prompt-delete-cancel', 'prompt-confirm-delete')
        for stage in stages:
            self.assertIn(f'"{stage}"', source)
            self.assertIn(f"helper_call(config, '{stage}'", harness)
        self.assertIn("'revision') == 1", harness)
        self.assertIn("'revision') == 2", harness)
        self.assertIn("'instructions'] == appended", harness)
        self.assertIn("fixture_journal_hashes([generation]) == baseline_journal", harness)
        context_flow = harness.split('def run_context_fixture(', 1)[1].split(
            '\ndef run_latency_fixture(', 1)[0]
        dirty_close = context_flow.split("helper_call(config, 'context-edit')", 1)[1].split(
            "helper_call(config, 'context-save')", 1)[0]
        self.assertLess(dirty_close.index("helper_call(config, 'wm-close')"),
                        dirty_close.index("helper_call(config, 'context-close')"))
        self.assertIn("'/artifacts/context-close-cancel-review.ppm'", dirty_close)
        self.assertIn("'/artifacts/context-close-cancelled.ppm'", dirty_close)
        discard_close = context_flow.split('# End with a second unsaved draft.', 1)[1]
        self.assertLess(discard_close.index("helper_call(config, 'wm-close')"),
                        discard_close.index("helper_call(config, 'context-close-confirm')"))
        self.assertIn('app.wait(timeout=12)', discard_close)
        self.assertIn('process_identity(host.pid) == discard_host_identity', discard_close)
        self.assertIn('get_context() == inserted', discard_close)
        self.assertIn("retained_view.get('state') == 'running'", discard_close)
        self.assertIn('fixture_panel_pin(session) == baseline_pin', discard_close)
        self.assertIn("'/artifacts/context-close-discard-review.ppm'", discard_close)
        close_helper = source.split('static void request_wm_delete(', 1)[1].split('\n}', 1)[0]
        self.assertIn('XGetWMProtocols(dpy, app', close_helper)
        self.assertIn('WM_DELETE_WINDOW', close_helper)
        self.assertIn('event.xclient.message_type = protocols', close_helper)
        self.assertIn('event.xclient.data.l[0] = (long)delete_window', close_helper)
        self.assertIn('XSendEvent(dpy, app, False, NoEventMask, &event)', close_helper)
        self.assertIn('request_wm_delete(dpy, app_window)', source)
        self.assertNotIn('control.sendall', context_flow)
        self.assertIn("'context-prompt-inserted.ppm'", harness)
        self.assertIn("'context-prompt-delete-review.ppm'", harness)

    def test_history_state_ignores_idle_checkpoint_timing_but_retains_terminal_changes(self):
        import copy
        baseline = {'generation': {'sequence': 5, 'frame': {'geometry': {'rows': 2, 'cols': 3}},
                                  'pid': 42, 'metrics': {'events': 5, 'output_bytes': 100,
                                                       'checkpoints': 1, 'checkpoint_ns': 7},
                                  'actor_diagnostic': {'stage': 'waiting'}}}
        original = copy.deepcopy(baseline)
        idle = copy.deepcopy(baseline)
        idle['generation']['metrics']['checkpoint_ns'] = 99
        idle['generation']['metrics']['checkpoints'] = 2
        idle['generation']['actor_diagnostic'] = {'stage': 'checkpoint'}
        self.assertEqual(IME.history_model_state(baseline), IME.history_model_state(idle))
        for path, value in ((('sequence',), 6), (('pid',), 43),
                            (('frame', 'geometry', 'cols'), 4),
                            (('metrics', 'events'), 6), (('metrics', 'output_bytes'), 101)):
            changed = copy.deepcopy(idle)
            target = changed['generation']
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            self.assertNotEqual(IME.history_model_state(baseline), IME.history_model_state(changed))
        self.assertEqual(baseline, original)

    def test_history_fixture_covers_colored_scrollback_and_live_return_without_native_run(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'history']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'history')
        self.assertIn('256 ANSI truecolor/named-color Unicode rows', plan['input']['history'])
        self.assertIn('reaches the live PTY exactly once after return', plan['input']['history'])
        self.assertIn('96-column far-right marker/link', plan['input']['history'])
        self.assertIn('Alt+Left/Right/Home/End horizontal panning', plan['input']['history'])
        self.assertIn('double-click the path token, triple-click the logical line', plan['input']['history'])
        self.assertIn('spaced over 600 ms apart', plan['input']['history'])

        harness = SCRIPT.read_text()
        helper = Path(__file__).with_name('x11-ime-input.c').read_text()
        self.assertIn('history_rows = int(sys.argv[3]) if len(sys.argv) > 3 else 0', harness)
        self.assertIn('history_rows in (0, 256)', harness)
        self.assertIn('\\x1b[38;2;231;72;86mSCROLLBACK_', harness)
        self.assertIn('\\x1b[31mANSI_RED', harness)
        self.assertIn('FAR_RIGHT_096', harness)
        self.assertIn('https://example.test/history-far-right', harness)
        self.assertIn("b'xyz' if config.get('fixture') == 'history'", harness)
        self.assertIn("'256' if config.get('fixture') == 'history' else '0'", harness)
        self.assertIn("'SCROLLBACK_255' in screen_text", harness)
        self.assertIn('line = "SCROLLBACK_000 path/to-file.rs\\r\\n"', harness)
        self.assertIn("'Named(Red)' in foregrounds", harness)
        self.assertIn("'r: 231' in fg and 'g: 72' in fg and 'b: 86' in fg", harness)

        history_flow = harness.split('def run_history_fixture(', 1)[1].split(
            '\ndef run_resume_fixture(', 1)[0]
        stages = ('scrollback-open', 'scrollback-probe-input', 'scrollback-wheel',
                  'scrollback-top', 'scrollback-newer', 'scrollback-close',
                  'scrollback-end', 'scrollback-live-input',
                  'scrollback-resize-narrow', 'scrollback-resize-wide',
                  'scrollback-pan-right', 'scrollback-pan-left',
                  'scrollback-pan-home', 'scrollback-pan-end')
        for stage in stages:
            self.assertIn(f'helper_call(config, \'{stage}\')', history_flow)
            self.assertIn(f'"{stage}"', helper)
        selection_flow = history_flow.split("helper_call(config, 'scrollback-top')", 1)[1].split(
            "helper_call(config, 'scrollback-end')", 1)[0]
        for stage in ('scrollback-select-word', 'scrollback-select-line',
                      'scrollback-drag-word-forward', 'scrollback-drag-word-reverse'):
            self.assertIn(f"helper_call(config, '{stage}')", selection_flow)
            self.assertIn(f'"{stage}"', helper)
        for expected in ("b'path/to-file.rs'", "b'SCROLLBACK_000 path/to-file.rs'"):
            self.assertIn(expected, selection_flow)
        self.assertIn("'/artifacts/scrollback-selection-line.ppm'", selection_flow)
        self.assertIn('scrollback-selection-line.ppm', harness)
        self.assertIn('time.sleep(.7)', selection_flow)
        self.assertIn('def wait_selection_clipboard(expected, gesture):', selection_flow)
        self.assertIn('time.monotonic() + 3', selection_flow)
        self.assertIn('before forward word drag', selection_flow)
        self.assertIn('before reverse word drag', selection_flow)
        self.assertIn('fixed_history_selection(dpy, app_window, a.stage)', helper)
        self.assertIn('pointer_position(dpy, app, x, y)', helper)
        gesture = helper.split('static void fixed_history_selection(', 1)[1].split(
            '\nstatic void scrollback_wheel(', 1)[0]
        self.assertIn('attr.width != 1280 || attr.height != 800', gesture)
        self.assertIn('fixed_click_at(dpy, app, start, word_y)', gesture)
        self.assertIn('int start = reverse ? 280 : 420;', gesture)
        self.assertIn('int end = reverse ? 420 : 280;', gesture)
        self.assertIn("b'SCROLLBACK_000 path/to-file.rs'", selection_flow)
        self.assertLess(history_flow.index("helper_call(config, 'scrollback-wheel')"),
                        history_flow.index("helper_call(config, 'scrollback-open')"))
        probe_at = history_flow.index("helper_call(config, 'scrollback-probe-input')")
        self.assertGreater(history_flow.index('check_cli_input_absent()', probe_at), probe_at)
        self.assertIn("control.sendall(b'COLLECT\\n')", history_flow)
        self.assertIn("bytes.fromhex(live_result.get('synthetic_fixture_payload_hex', '')) == b'xyz'",
                      history_flow)
        for capture in ('scrollback-open.ppm', 'scrollback-wheel.ppm', 'scrollback-top.ppm',
                        'scrollback-newer.ppm', 'scrollback-escape-live.ppm',
                        'scrollback-live.ppm', 'scrollback-pan-left-edge.ppm',
                        'scrollback-pan-right-8.ppm', 'scrollback-pan-return-left.ppm',
                        'scrollback-pan-end.ppm', 'scrollback-pan-home.ppm',
                        'scrollback-pan-wide-frozen.ppm'):
            self.assertIn(capture, harness)
        self.assertIn('post-resize model, frame, journal, or layout', harness)
        self.assertIn('scrollback-resize-narrow', helper)
        self.assertIn('XResizeWindow(dpy, w, target_width, 800u)', helper)
        self.assertIn('attr.width != (int)target_width || attr.height != 800', helper)
        self.assertIn("'journal': fixture_journal_hashes([generation]) == expected_journals", history_flow)
        self.assertIn('XTestFakeButtonEvent(dpy, 4, True, CurrentTime)', helper)
        self.assertIn('pointer_position(dpy, app, 640, 400)', helper)

        link_flow = history_flow.split("helper_call(config, 'links-open')", 1)[1].split(
            "# Unmodified wheel is exercised", 1)[0]
        for stage in ('links-open', 'links-list-ctrl-enter', 'links-enter-review',
                      'links-review-enter', 'links-copy', 'links-clipboard-report',
                      'links-back', 'links-down', 'links-refresh', 'links-confirm', 'links-cancel'):
            self.assertIn(f'"{stage}"', helper)
            self.assertIn(f"helper_call(config, '{stage}'", link_flow)
        self.assertLess(link_flow.index("helper_call(config, 'links-list-ctrl-enter')"),
                        link_flow.index("helper_call(config, 'links-enter-review')"))
        self.assertLess(link_flow.index("helper_call(config, 'links-review-enter')"),
                        link_flow.index("helper_call(config, 'links-copy')"))
        self.assertIn("b'https://example.test/pigade?fixture=osc8'", link_flow)
        self.assertIn('("fixture-javascript", "javascript:alert(1)", "blocked-javascript.example")', harness)
        self.assertIn('("fixture-file", "file:///etc/passwd", "blocked-file.example")', harness)
        self.assertIn("launcher_records == [{'program': 'xdg-open'", link_flow)
        self.assertIn("app_env['PATH'] = '/artifacts/bin:/usr/bin'", harness)
        self.assertIn("sys.argv[4] == \"links\"", harness)
        self.assertIn("link_targets == ['https://example.test/pigade?fixture=osc8'", harness)
        self.assertIn("b'https://example.test/history-only'", history_flow)
        self.assertIn("'terminal-links-history-review.ppm'", harness)
        self.assertIn("'historical_osc8_target_distinct_from_live': True", history_flow)

    def test_resume_fixture_drives_workspace_overview_and_gates_exact_native_resume(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'resume']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'resume')
        self.assertIn('Ctrl+Shift+Space,U opens workspace session overview', plan['input']['resume'])
        self.assertIn('Ctrl+Enter prepares only', plan['input']['resume'])
        self.assertFalse(plan['input']['arbitrary_text_argument'])

        harness = SCRIPT.read_text()
        helper = Path(__file__).with_name('x11-ime-input.c').read_text()
        flow = harness.split('def run_resume_fixture(', 1)[1].split(
            '\n\ndef run_controls_fixture(', 1)[0]
        for stage in ('workspace-sessions-open', 'workspace-sessions-row-down',
                      'workspace-sessions-enter', 'workspace-sessions-prepare',
                      'workspace-sessions-refresh', 'workspace-sessions-cancel',
                      'resume-confirm'):
            self.assertIn(f'"{stage}"', helper)
            self.assertIn(f"helper_call(config, '{stage}'", flow)
        self.assertIn("{'command': 'workspace_sessions'", flow)
        self.assertIn('codex_index <= 31 and raw_index <= 31', flow)
        self.assertIn('for _ in range(codex_index)', flow)
        self.assertIn("raw_entry.get('resume_state') != 'check_available'", flow)
        self.assertIn('require_overview_read_only()', flow)
        self.assertIn('require_review_read_only(first_review_plans)', flow)
        self.assertIn("expected_resume_layout['selection']['revision'] += 1", flow)
        self.assertIn("expected_resume_layout['selection']['window']['panel_generations'].pop(session, None)", flow)
        self.assertIn('catalog_snapshot() == expected_resume_layout', flow)
        self.assertIn("calls[-1].count(fixture['conversation_id']) == 1", flow)
        self.assertIn("(after_entry.get('conversation') or {}).get('source_generation') == generation",
                      flow)
        self.assertIn("'workspace-session-resume-cancelled.ppm'", flow)
        self.assertIn("'workspace-session-resumed.ppm'", flow)
        self.assertIn("'workspace-sessions-cancelled.ppm'", harness)
        self.assertIn("'workspace-session-resumed.ppm'", harness)
        self.assertIn('modified_key(dpy, XK_Control_L, XK_Return)', helper)
        self.assertIn('modified_key(dpy, XK_Control_L, XK_r)', helper)

    def test_setup_fixture_reviews_and_runs_exact_custom_cli_argv_only_after_confirmation(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'setup']), 0)
        plan = json.loads(output.getvalue())
        self.assertIn('Ctrl+Enter from the selected empty ready tab launches exactly one real shell PTY',
                      plan['input']['setup'])
        self.assertIn('read-only launch review', plan['input']['setup'])
        self.assertIn('Ctrl+Enter runs the exact argv once', plan['input']['setup'])
        self.assertIn('hand off its visible output through prepare then explicit Ctrl+Enter',
                      plan['input']['setup'])
        self.assertIn('duplicate it with independent settings, pin it', plan['input']['setup'])

        harness = SCRIPT.read_text()
        helper = Path(__file__).with_name('x11-ime-input.c').read_text()
        stages = ('custom-open', 'custom-fields', 'custom-save', 'custom-select',
                  'custom-review-enter', 'custom-review-cancel', 'custom-reopen-profiles',
                  'custom-confirm', 'setup-ctrl-enter-shell', 'handoff-open',
                  'handoff-type-ask', 'handoff-prepare', 'handoff-cancel', 'handoff-confirm')
        for stage in stages:
            self.assertIn(f'"{stage}"', helper)
            self.assertIn(f"helper_call(config, '{stage}'", harness)
        self.assertIn("helper_call(config, 'custom-fields', timeout=15)", harness)
        custom_flow = harness.split('def run_custom_cli_profile_fixture(', 1)[1].split(
            '\ndef run_context_fixture(', 1)[0]
        self.assertIn("'args': ['--label', 'two words', '$(literal);']", custom_flow)
        self.assertIn("not log.exists()", custom_flow)
        self.assertIn("records == [{'argv': command['args'], 'cwd': '/home/pigade/demo'}]", custom_flow)
        self.assertIn("'CUSTOM_CLI_MARKER' in view_text(view)", custom_flow)
        self.assertLess(custom_flow.index("helper_call(config, 'custom-review-enter')"),
                        custom_flow.index("helper_call(config, 'custom-review-cancel')"))
        self.assertLess(custom_flow.index("helper_call(config, 'custom-review-cancel')"),
                        custom_flow.index("helper_call(config, 'custom-confirm')"))
        self.assertIn("all(value is False for value in preview.get('capabilities', {}).values())",
                      custom_flow)
        self.assertIn('custom_ascii(dpy, "--label \\\"two words\\\" \'$(literal);\'")', helper)
        self.assertIn("custom_tools.mkdir(mode=0o700, exist_ok=True)", harness)
        self.assertIn("tools_info = custom_tools.lstat()", harness)
        self.assertIn("os.O_EXCL |", harness)
        self.assertIn("os.O_NOFOLLOW | os.O_CLOEXEC, 0o700", harness)
        self.assertIn('original_journal_prefixes = fixture_journal_prefixes(', custom_flow)
        self.assertIn('fixture_journal_prefixes_preserved(original_journal_prefixes)', custom_flow)
        self.assertIn("'existing_journal_prefix_preserved': True", harness)
        self.assertIn('config, report, app, save, session, focused, tab', harness)
        setup_flow = harness.split('def run_setup_fixture(', 1)[1].split(
            '\ndef run_custom_cli_profile_fixture(', 1)[0]
        handoff_flow = setup_flow.split("helper_call(config, 'capture', '/artifacts/agent-direct.ppm')", 1)[1]
        first_open = handoff_flow.index("helper_call(config, 'handoff-open')")
        cancel = handoff_flow.index("helper_call(config, 'handoff-cancel')", first_open)
        second_open = handoff_flow.index("helper_call(config, 'handoff-open')", first_open + 1)
        prepare = handoff_flow.index("helper_call(config, 'handoff-prepare')", second_open)
        confirm = handoff_flow.index("helper_call(config, 'handoff-confirm')", prepare)
        self.assertLess(first_open, cancel)
        self.assertLess(cancel, second_open)
        self.assertLess(second_open, prepare)
        self.assertLess(prepare, confirm)
        for evidence in ('OLD_RESUME_FRAME', 'handoff_source_unchanged()',
                         "'include_memory') is False", "'deliver_context') is True",
                         "'developer_instructions='", 'expected_ask', 'handoff_context_text',
                         'instruction_blocks == [handoff_instructions]',
                         'fixture_generations() == handoff_generations'):
            self.assertIn(evidence, handoff_flow)
        for capture in ('handoff-open.ppm', 'handoff-preview.ppm'):
            self.assertIn(capture, setup_flow)
            self.assertGreaterEqual(harness.count(f"'{capture}'"), 2)
        self.assertIn('live_input_session in {key[0] for key in final_models}', custom_flow)
        self.assertIn("view_text(final_models[(live_input_session,", custom_flow)

        profile_flow = harness.split('def run_pinned_profile_access_fixture(', 1)[1].split(
            '\ndef run_context_fixture(', 1)[0]
        for stage in ('profile-open', 'profile-select-first', 'profile-duplicate',
                      'profile-duplicate-rename', 'profile-save', 'profile-select-copy',
                      'profile-pin-copy', 'profile-library-close', 'profile-sidebar-copy',
                      'profile-review-enter', 'profile-review-cancel', 'setup-launcher',
                      'launcher-select-first-pin', 'profile-confirm'):
            self.assertIn(f'"{stage}"', helper)
        self.assertIn("helper_call(config, 'profile-sidebar-copy')", profile_flow)
        self.assertIn('setup_click(dpy, app_window, 100, 666)', helper)
        self.assertIn("helper_call(config, 'launcher-select-first-pin')", profile_flow)
        self.assertIn("records() == [{'argv': command['args'], 'cwd': '/home/pigade/demo'}]",
                      profile_flow)
        self.assertIn("'phase') == 'prepared'", profile_flow)
        self.assertIn("'phase') == 'not_requested'", profile_flow)
        self.assertIn("'copy_pinned_and_visible_in_sidebar_and_launcher': True", profile_flow)
        self.assertIn("'profile-launcher-confirmed.ppm'", harness)

    def test_journal_prefix_contract_allows_append_but_rejects_rewrite(self):
        self.assertTrue(IME.journal_prefix_matches(b'old journal bytes',
                                                   b'old journal bytes + resize record'))
        self.assertFalse(IME.journal_prefix_matches(b'old journal bytes',
                                                    b'new journal bytes'))
        self.assertFalse(IME.journal_prefix_matches(b'', b'new journal bytes'))

    def test_private_host_rejection_reports_bounded_escaped_protocol_error(self):
        reader, writer = socket.socketpair()
        try:
            payload = json.dumps({'protocol': 2, 'error': 'bad\n' + ('x' * 900)}).encode()
            writer.sendall(len(payload).to_bytes(4, 'little') + payload)
            with self.assertRaises(RuntimeError) as raised:
                IME.read_framed(reader)
        finally:
            reader.close(); writer.close()
        message = str(raised.exception)
        self.assertIn('protocol=2', message)
        error_text = message.split(', error=', 1)[1]
        self.assertLessEqual(len(error_text), 512)
        self.assertIn('\\n', error_text)
        self.assertNotIn('\nbad', message)

    def test_real_run_requires_a_new_external_output_directory(self):
        result = subprocess.run([os.environ.get('PYTHON', '/usr/bin/python3'), str(SCRIPT), '--run'],
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True, timeout=3, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn('requires a new private --output path', result.stderr)

    def test_output_guard_rejects_repository_and_existing_or_symlink_paths(self):
        with self.assertRaisesRegex(RuntimeError, 'outside the repository'):
            IME.private_path(IME.ROOT / 'tmp')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            existing = root / 'existing'; existing.mkdir()
            with self.assertRaisesRegex(RuntimeError, 'already exists'):
                IME.private_path(existing, must_not_exist=True)
            target = root / 'target'; target.mkdir()
            link = root / 'link'; link.symlink_to(target, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, 'symlink'):
                IME.private_path(link / 'new')

    def test_private_config_maps_current_sdk_extraction_not_fixed_old_hash(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            sdk = base / 'sdk'; extraction = sdk / 'root/x11-ime-new-package-set'; extraction.mkdir(parents=True)
            platform = base / 'platform'; (platform / 'root').mkdir(parents=True)
            app, host, helper = (base / name for name in ('app', 'host', 'helper'))
            app.write_bytes(b'app'); host.write_bytes(b'host'); helper.write_bytes(b'helper')
            args = SimpleNamespace(app=app, host=host, x11_sdk=sdk, platform_sdk=platform)
            config = IME.inside_config(args, base, helper,
                                       {'extraction_root': str(extraction), 'manifest_sha256': 'x'},
                                       {'manifest_sha256': 'y'})
            self.assertEqual(config['x11_root'], '/pigade/x11-sdk')
            self.assertEqual(config['x11_extraction_root'], str(extraction))
            self.assertEqual(config['platform_root'], str(platform / 'root'))

    def test_bwrap_builder_clears_host_display_and_mounts_only_extracted_payloads(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            sdk = base / 'sdk'; extracted = sdk / 'root/x11-ime-set'; extracted.mkdir(parents=True)
            platform = base / 'platform'; (platform / 'root').mkdir(parents=True)
            output = base / 'output'; output.mkdir()
            app, host, helper = (base / name for name in ('app', 'host', 'helper'))
            app.write_bytes(b'app'); host.write_bytes(b'host'); helper.write_bytes(b'helper')
            config_path = base / 'config.json'
            config_path.write_text(json.dumps({
                'x11_root': '/pigade/x11-sdk', 'x11_extraction_root': str(extracted),
                'platform_root': str(platform / 'root'),
                'private_icd': '/pigade/platform-sdk/lavapipe-icd.json',
                'private_icd_host': str(output / 'lavapipe-icd.json'),
            }))
            args = SimpleNamespace(x11_sdk=sdk, platform_sdk=platform, output=output,
                                   app=app, host=host)
            argv = IME.bwrap_command(args, config_path, helper)
            self.assertIn('--unshare-all', argv); self.assertIn('--clearenv', argv)
            self.assertIn(str(extracted), argv)
            self.assertNotIn(str(sdk / 'downloads'), argv)
            self.assertNotIn(str(platform / 'downloads'), argv)
            self.assertNotIn('/run/user', argv)
            self.assertNotIn('/dev/input', argv)
            self.assertNotIn('/dev/dri', argv)
            environment = {argv[index + 1] for index, value in enumerate(argv[:-1])
                           if value == '--setenv'}
            self.assertTrue({'HOME', 'XDG_CONFIG_HOME', 'XDG_RUNTIME_DIR',
                             'LD_LIBRARY_PATH', 'LIBIME_MODEL_DIRS'} <= environment)
            self.assertFalse({'DISPLAY', 'WAYLAND_DISPLAY', 'DBUS_SESSION_BUS_ADDRESS',
                              'XAUTHORITY', 'HYPRLAND_INSTANCE_SIGNATURE'} & environment)

    def test_outer_wrapper_retains_bounded_stderr_on_startup_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            exit_status = IME.run_bwrap(['/usr/bin/python3', '-I', '-c',
                                        'import os;os.write(2,b"x"*100000);raise SystemExit(3)'], output)
            self.assertEqual(exit_status, 3)
            retained = (output / 'bwrap.stderr.log').read_bytes()
            self.assertEqual(len(retained), IME.LOG_LIMIT)
            self.assertEqual(retained, b'x' * IME.LOG_LIMIT)

    def test_private_loopback_preflight_observes_flags_without_reconfiguring_network(self):
        for flags, accepted in ((0x49, True), (0x8, False), (0x1, False)):
            with self.subTest(flags=flags), mock.patch.object(IME.socket, 'socket'), \
                    mock.patch.object(IME.fcntl, 'ioctl', return_value=IME.struct.pack(
                        '16sH14x', b'lo', flags)) as ioctl:
                if accepted:
                    IME.verify_private_loopback()
                else:
                    with self.assertRaises(RuntimeError):
                        IME.verify_private_loopback()
                self.assertEqual(ioctl.call_count, 1)
                self.assertEqual(ioctl.call_args.args[1], 0x8913)

    def test_process_identity_reads_real_proc_start_time(self):
        boot, start = IME.process_identity(os.getpid())
        self.assertEqual(boot, Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        self.assertGreater(start, 0)

    def test_window_probe_requires_positive_verified_result(self):
        self.assertTrue(IME.window_verified({'window_verified': True}))
        self.assertFalse(IME.window_verified({'window_verified': False}))
        self.assertFalse(IME.window_verified({}))

    def test_private_identity_files_include_actual_host_uid_and_gid(self):
        passwd, group = IME.private_identity_files(1234, 2345)
        self.assertIn('pigade:x:1234:2345:', passwd)
        self.assertIn('pigade:x:2345:', group)

    def test_pinyin_selection_is_private_and_confirmed_without_key_events(self):
        config = {'display': ':3', 'x11_root': '/pigade/x11-sdk'}
        with mock.patch.object(IME.subprocess, 'run', side_effect=[
                SimpleNamespace(returncode=0, stdout='', stderr=''),
                SimpleNamespace(returncode=0, stdout='', stderr=''),
                SimpleNamespace(returncode=0, stdout='pinyin\n', stderr=''),
        ]) as run:
            self.assertEqual(IME.select_private_pinyin(
                config, Path('/home/pigade'), Path('/run/pigade-test'),
                Path('/run/pigade-test/Xauthority')), 'pinyin')
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            ['/pigade/x11-sdk/usr/bin/fcitx5-remote', '-s', 'pinyin'],
            ['/pigade/x11-sdk/usr/bin/fcitx5-remote', '-o'],
            ['/pigade/x11-sdk/usr/bin/fcitx5-remote', '-n'],
        ])
        for call in run.call_args_list:
            env = call.kwargs['env']
            self.assertEqual(env['DISPLAY'], ':3')
            self.assertEqual(env['XAUTHORITY'], '/run/pigade-test/Xauthority')
            self.assertEqual(env['DBUS_SESSION_BUS_ADDRESS'], 'unix:path=/run/pigade-test/bus')

    def test_keyboard_mode_is_fixed_plan_only_and_loads_only_private_map(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'keyboard']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'keyboard')
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        self.assertEqual(IME.KEYBOARD_EXPECTED,
                         b'ab1Z\x03\x04\x1a\x12\x0c\x01\x1bx\x1b[D\x1b[H\r' + 'привет'.encode())
        env = {'DISPLAY': ':3', 'XAUTHORITY': '/run/pigade-test/Xauthority'}
        with mock.patch.object(IME.subprocess, 'run', return_value=SimpleNamespace(
                returncode=0, stdout='', stderr='')) as run:
            IME.prepare_private_keyboard({'x11_root': '/pigade/x11-sdk', 'display': ':3'}, env)
        self.assertEqual(run.call_args.args[0], ['/pigade/x11-sdk/usr/bin/setxkbmap',
                         '-display', ':3', '-layout', 'us,ru', '-option', ''])
        self.assertEqual(run.call_args.kwargs['env'], env)

    def test_private_fcitx_callback_preedit_is_enabled_only_outside_keyboard_fallback(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary) / 'private-home'
            home.mkdir(mode=0o700)
            config = home / '.config/fcitx5/conf/xim.conf'
            self.assertEqual(IME.prepare_private_fcitx_preedit_config(home, 'pointer'),
                             'on-the-spot')
            self.assertEqual(config.read_text(), 'UseOnTheSpot=True\n')
            self.assertEqual(config.stat().st_mode & 0o777, 0o600)
            self.assertEqual(IME.prepare_private_fcitx_preedit_config(home, 'keyboard'),
                             'server-default-external')
            self.assertFalse(config.exists())

    def test_opencode_mode_is_fixed_plan_only_with_no_enter_or_arbitrary_input(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'opencode']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'opencode')
        self.assertIn('/usr/bin/opencode 1.19.3', plan['input']['opencode'])
        self.assertIn('unsent prompt', plan['input']['opencode'])
        self.assertIn('loopback session creation', plan['input']['opencode'])
        self.assertIn('exact-ID resume', plan['input']['opencode'])
        self.assertIn('public export/import of a synthetic text-only exchange', plan['input']['opencode'])
        self.assertIn('UI profile create/review/launch/edit/delete', plan['input']['opencode'])
        self.assertIn('fixed model-option transport checks (no account/model acceptance claim)',
                      plan['input']['opencode'])
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        source = Path(__file__).with_name('x11-ime-input.c').read_text()
        sequence = source.split('static void opencode_input_sequence', 1)[1].split('\n}', 1)[0]
        self.assertIn('for (int row = 0; row < 8; ++row)', sequence)
        self.assertIn('for (int column = 0; column < 16; ++column)', sequence)
        self.assertNotIn('XK_Return', sequence)
        self.assertNotIn('--text', source)
        harness = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        self.assertIn("'/usr/bin/opencode', 'serve', '--hostname', '127.0.0.1'", harness)
        self.assertIn("run_owned_opencode_command('export', session_id, project, env)", harness)
        self.assertIn("run_owned_opencode_command(\n            'import', str(import_path), project, env)", harness)
        self.assertIn("require(session_id.encode() in import_stdout", harness)
        self.assertNotIn("require(assistant_message_id.encode() in import_stdout", harness)
        self.assertIn("'POST', '/session', {'title': title}", harness)
        self.assertIn("'command': 'prepare_resume', 'session': session", harness)
        self.assertIn("['--session', conversation_id]", harness)
        self.assertIn("['/usr/bin/opencode', '--session', missing_id]", harness)
        self.assertIn("'failed_before_tui': True", harness)
        self.assertIn("'synthetic': True", harness)
        self.assertIn("'parentID': user_message_id", harness)
        self.assertIn("'public_import_exact_two_messages': True", harness)
        self.assertIn("'resumed_synthetic_response_visible_in_cells': True", harness)
        self.assertIn("'GET /session/{id}/message'", harness)
        self.assertIn("'opencode-resume-review.ppm'", harness)
        self.assertIn("'opencode-resumed-session.ppm'", harness)
        self.assertIn("'opencode-profile-launch-review.ppm'", harness)
        self.assertIn("'opencode-profile-model-edit.ppm'", harness)
        self.assertIn("'model': 'opencode/big-pickle'", harness)
        self.assertIn("'model': 'opencode/test-other-model'", harness)
        self.assertIn("before_catalog.get('engine_options_schema') == 1", harness)
        self.assertIn("'model_transport_only_no_account_acceptance_claim': True", harness)
        self.assertIn("preview.get('engine') == expected_engine", harness)
        self.assertIn("launched_argv == ['/usr/bin/opencode', '--model', 'opencode/big-pickle']", harness)
        self.assertIn("process_argv((after_edit_view or {}).get('pid')) == launched_argv", harness)
        profile_argv = harness.split('    def process_argv(pid):', 1)[1].split('    def profile_by_name', 1)[0]
        self.assertIn("Path(f'/proc/{pid}/cmdline')", profile_argv)
        self.assertNotIn('/environ', profile_argv)
        self.assertIn('def run_opencode_profile_fixture(', harness)
        for assertion in (
                "'command': 'detect_providers'", "'preview', {}).get('profile') or {}).get('id') == profile['id']",
                "'role_override') == expected_settings['role']",
                "'agent_instructions') == expected_settings['instructions']",
                "launch.get('args') == ['--model', 'opencode/big-pickle']",
                "launch.get('env', {}) == {}",
                "'frame_unchanged': (after_edit_view or {}).get('frame') == profile_frame",
                "'1.19.3' not in text or text.count('█') < 8",
                "final_plans.get(profile_plan_id, {}).get('payload') == payload",
                "'delete_cancel_preserved_profile': True"):
            self.assertIn(assertion, harness)
        for stage in ('profile-open', 'profile-new', 'profile-create-fields',
                      'profile-context-off', 'profile-save', 'profile-select-first',
                      'profile-launch', 'profile-confirm', 'profile-edit', 'profile-rename',
                      'profile-save-edit', 'profile-delete', 'profile-cancel-delete',
                      'profile-confirm-delete'):
            self.assertIn(f'"{stage}"', source)
            self.assertIn(f"helper_call(config, '{stage}'", harness)
        profile_input = source.split('} else if (!strcmp(a.stage, "profile-create-fields")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "profile-context-off")) {', 1)[0]
        self.assertIn('XK_3', profile_input)
        self.assertIn('XK_slash, XK_b, XK_i, XK_g', profile_input)
        self.assertIn('XK_Tab', profile_input)
        self.assertNotIn('modified_key(dpy, XK_Control_L, XK_m)', profile_input)
        self.assertIn('modified_key(dpy, XK_Control_L, XK_m)', source)
        self.assertIn("'public_metadata_directory': '/home/pigade/.local/share/opencode'", harness)
        self.assertIn("'private_loopback_enabled_for_fixture': config.get('fixture') == 'opencode'", harness)
        self.assertNotIn("'/global/auth'", harness)
        self.assertNotIn("'POST', '/message'", harness)
        self.assertNotIn("'POST', '/session/", harness)

    def test_only_keyboard_fallback_accepts_no_fcitx_context(self):
        with mock.patch.object(IME.subprocess, 'run', return_value=SimpleNamespace(
                returncode=0, stdout='0\n', stderr='')):
            self.assertEqual(IME.deactivate_private_fcitx(
                {'x11_root': '/private', 'fixture': 'keyboard'}, {}), 0)
            with self.assertRaises(RuntimeError):
                IME.deactivate_private_fcitx({'x11_root': '/private', 'fixture': 'pointer'}, {})

    def test_controls_mode_is_fixed_plan_only_and_contains_only_private_fixed_stages(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'controls']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'controls')
        self.assertIn('four-shell', plan['input']['controls'])
        self.assertIn('Alt+F', plan['input']['controls'])
        self.assertIn('Appearance persists independent terminal Dark/Light/Follow app palette',
                      plan['input']['controls'])
        self.assertIn('verify hidden output/end metadata with PollPanels returning no frames',
                      plan['input']['controls'])
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        source = Path(__file__).with_name('x11-ime-input.c').read_text()
        for stage in ('controls-focus-toggle', 'controls-focus-right', 'controls-alt-focus-toggle', 'controls-move',
                      'controls-resize-cancel', 'controls-resize-commit', 'controls-close',
                      'controls-open', 'controls-select-alt', 'controls-select-control',
                      'controls-select-disabled', 'appearance-open', 'appearance-light',
                      'appearance-motion', 'appearance-dark', 'appearance-terminal-dark',
                      'appearance-terminal-light', 'appearance-terminal-follow',
                      'appearance-font-increase',
                      'appearance-font-decrease', 'appearance-font-reset',
                      'appearance-reload', 'appearance-close', 'activity-open',
                      'activity-close', 'activity-mark-all', 'activity-mark-current',
                      'activity-enter-target', 'activity-next',
                      'activity-select-source-tab', 'pointer-create-transfer-tab',
                      'pointer-select-first-tab', 'setup-shell', 'setup-ctrl-enter-shell'):
            self.assertIn(f'"{stage}"', source)
        helper_sequences = {
            'appearance-terminal-dark': 'XK_3',
            'appearance-terminal-light': 'XK_4',
            'appearance-terminal-follow': 'XK_5',
        }
        for stage, key in helper_sequences.items():
            sequence = source.split(f'}} else if (!strcmp(a.stage, "{stage}")) {{', 1)[1].split(
                '} else if', 1)[0]
            self.assertIn(f'key(dpy, {key})', sequence)
        harness = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        appearance_flow = harness.split("appearance_path = Path('/home/pigade/.config/pigade/appearance.json')", 1)[1].split(
            "report['controls'].update", 1)[0]
        self.assertIn("'version', 'revision', 'theme', 'reduced_motion', 'terminal_font_size',", appearance_flow)
        self.assertIn("'terminal_theme'}", appearance_flow)
        self.assertIn("value.get('terminal_theme') in ('dark', 'light', 'follow_app')", appearance_flow)
        self.assertIn("wait_appearance(1, 'light', False)", appearance_flow)
        self.assertIn("wait_appearance(2, 'light', True)", appearance_flow)
        self.assertIn("wait_appearance(3, 'dark', True)", appearance_flow)
        self.assertIn("wait_appearance(6, 'dark', True, 'light')", appearance_flow)
        self.assertIn("wait_appearance(7, 'dark', True, 'follow_app')", appearance_flow)
        self.assertIn("wait_appearance(8, 'light', True, 'follow_app')", appearance_flow)
        self.assertIn("wait_appearance(9, 'dark', True, 'follow_app')", appearance_flow)
        self.assertIn("wait_appearance(10, 'dark', True, 'dark')", appearance_flow)
        self.assertIn("'terminal_font_size': 15", appearance_flow)
        self.assertIn("'terminal_font_size': 14", appearance_flow)
        self.assertIn("def probe_kernel_pty_geometries(label)", appearance_flow)
        self.assertIn("'command': 'input'", appearance_flow)
        self.assertIn("stty size", appearance_flow)
        self.assertIn("host frame geometry did not match the actual kernel PTY size", appearance_flow)
        self.assertIn("decreasing terminal font size did not restore the exact actual kernel PTY geometry", appearance_flow)
        self.assertIn("step('appearance-font-increase')", appearance_flow)
        self.assertIn("step('appearance-font-decrease')", appearance_flow)
        self.assertIn("step('appearance-font-reset')", appearance_flow)
        self.assertIn("terminal_geometries() == original_terminal_geometries", appearance_flow)
        self.assertIn("increased_geometries[key] != original_terminal_geometries[key]", appearance_flow)
        self.assertIn("config_path.read_bytes() == controls_bytes", appearance_flow)
        self.assertIn('appearance_snapshot() == terminal_before_appearance', appearance_flow)
        self.assertIn('journals_before_appearance', appearance_flow)
        self.assertIn("appearance_restart_recovery_tested': False", harness)
        self.assertIn("appearance-light.ppm", harness)
        self.assertIn("appearance-dark.ppm", harness)
        self.assertIn("def screenshot_pixel(name, x=430, y=150)", harness)
        self.assertIn("zip(light_pixel, (255, 255, 255))", harness)
        self.assertIn("zip(dark_pixel, (45, 45, 49))", harness)
        self.assertIn("'appearance_modal_surface_pixel': {'position': [430, 150]", harness)
        self.assertIn("terminal_pixel_position = (1260, 750)", appearance_flow)
        self.assertIn("'appearance_terminal_padding_pixel'", harness)
        self.assertIn('terminal_theme_snapshot = appearance_snapshot()', appearance_flow)
        self.assertIn('terminal_theme_journals = fixture_journal_hashes(', appearance_flow)
        self.assertIn('def terminal_theme_invariants(message):', appearance_flow)
        self.assertIn("/artifacts/appearance-terminal-controls.ppm", appearance_flow)
        self.assertIn("'reselecting Light terminal theme changed its preference revision'", appearance_flow)
        self.assertIn("'reselecting Dark terminal theme changed its preference revision'", appearance_flow)
        for stage in ('appearance-terminal-light', 'appearance-terminal-follow',
                      'appearance-terminal-dark'):
            self.assertIn(f"step('{stage}')", appearance_flow)
        self.assertLess(appearance_flow.index("step('appearance-terminal-light')"),
                        appearance_flow.index("step('appearance-terminal-follow')"))
        self.assertIn("capture_terminal_theme('appearance-terminal-light.ppm', (250, 250, 252))",
                      appearance_flow)
        self.assertIn("'appearance-terminal-follow-light.ppm', (250, 250, 252)", appearance_flow)
        self.assertIn("'appearance-terminal-follow-dark.ppm', (17, 19, 23)", appearance_flow)
        terminal_capture = appearance_flow.split('def capture_terminal_theme(', 1)[1].split(
            'return pixel', 1)[0]
        self.assertLess(terminal_capture.index("step('appearance-close')"),
                        terminal_capture.index("helper_call(config, 'capture'"))
        self.assertLess(terminal_capture.index("helper_call(config, 'capture'"),
                        terminal_capture.index("step('appearance-open')"))
        activity_flow = harness.split('def run_background_activity_fixture(', 1)[1].split(
            '\ndef split_ids(', 1)[0]
        self.assertIn("'command': 'poll_panels', 'panels': [], 'known': []", activity_flow)
        self.assertIn("reply.get('views') == []", activity_flow)
        self.assertIn("b\"printf 'PIGADE_ACTIVITY_OUTPUT_A", activity_flow)
        self.assertIn("b\"printf 'PIGADE_ACTIVITY_EXIT_MARKER", activity_flow)
        self.assertIn("wait_saved(target_pair, first_entry, host_instance, True)",
                      activity_flow)
        self.assertIn("wait_saved(target_pair, second_entry, host_instance, True)",
                      activity_flow)
        self.assertIn("wait_saved(target_pair, ended_entry, host_instance, True)",
                      activity_flow)
        self.assertIn("def wait_saved(pair, entry, host_instance, require_source_sequence=False):",
                      activity_flow)
        self.assertIn("document.get('host_instance') == host_instance and matches", activity_flow)
        self.assertIn("(prior_sessions - placed) - {bootstrap_session}", activity_flow)
        self.assertIn("cleared_seen[pair]['output_bytes'] >= entry['output_bytes']", activity_flow)
        self.assertIn("ordered_pairs = unread_pairs", activity_flow)
        self.assertIn("second_unread == [target_pair]", activity_flow)
        self.assertIn("'activity-selected-read.ppm'", activity_flow)
        self.assertIn("'activity-visible-current-head.ppm'", harness)
        self.assertIn('appearance_terminal_light_independent_of_dark_chrome', harness)
        self.assertIn('appearance_follow_app_changed_with_chrome_both_directions', harness)
        self.assertIn('appearance_terminal_dark_explicitly_restored', harness)
        for action in ('appearance-light', 'appearance-motion', 'appearance-reload',
                       'appearance-close', 'appearance-open', 'appearance-dark'):
            self.assertIn(f"step('{action}')", appearance_flow)
        self.assertLess(appearance_flow.index("wait_appearance(1, 'light', False)"),
                        appearance_flow.index("wait_appearance(2, 'light', True)"))
        self.assertLess(appearance_flow.index("wait_appearance(2, 'light', True)"),
                        appearance_flow.index("step('appearance-reload')"))
        first_open = appearance_flow.index("step('appearance-open')")
        second_open = appearance_flow.index("step('appearance-open')", first_open + 1)
        self.assertLess(second_open, appearance_flow.index("wait_appearance(3, 'dark', True)"))
        self.assertIn('int start_x = button == 1 ? 485 : 720;', source)
        self.assertIn('int end_x = button == 1 ? 1015 : 660;', source)
        self.assertNotIn('--coordinates', source)

    def test_pointer_mode_is_fixed_plan_only_and_whitelists_held_drag_and_hover_stages(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'pointer']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'pointer')
        self.assertIn('last-hover unsent text', plan['input']['pointer'])
        self.assertIn('held zero-motion Ctrl+LMB visibly picks up', plan['input']['pointer'])
        self.assertIn('five-pixel modifier drag preview is cancelled with Escape', plan['input']['pointer'])
        self.assertIn('focus-mode drag preview/cancel from nonzero pane index', plan['input']['pointer'])
        self.assertIn('cross-tab pane drag preview/cancel and atomic transfer undo', plan['input']['pointer'])
        self.assertIn('Ctrl+Shift+2 and searchable-prefix keyboard transfers', plan['input']['pointer'])
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        source = Path(__file__).with_name('x11-ime-input.c').read_text()
        for stage in ('pointer-hover-left', 'pointer-hover-right', 'pointer-hover-input',
                      'pointer-hover-header',
                      'pointer-no-motion-release',
                      'pointer-subthreshold-hold', 'pointer-focus-toggle',
                      'pointer-focus-mode-preview',
                      'pointer-hover-source', 'pointer-keyboard-away', 'pointer-move-right-near',
                      'pointer-hold-preview', 'pointer-drop', 'pointer-drop-gap', 'pointer-undo',
                      'pointer-create-transfer-tab', 'pointer-select-first-tab',
                      'pointer-tab-drop-cancel', 'pointer-tab-drop',
                      'pointer-tab-drop-populated-preview',
                      'pointer-hotkey-transfer', 'pointer-picker-transfer',
                      'pointer-picker-confirm',
                      'pointer-selection-hold', 'pointer-ime-input', 'pointer-ime-cancel',
                      'pointer-dialog-hover', 'pointer-inactive-hold'):
            self.assertIn(f'"{stage}"', source)
        header_stage = source.split(
            '} else if (!strcmp(a.stage, "pointer-hover-header")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-hover-right")) {', 1)[0]
        self.assertIn('pointer_hover(dpy, app_window, 485, 90, 0)', header_stage)
        self.assertNotIn('XTestFakeButtonEvent', header_stage)
        self.assertIn('capture(dpy, a.output)', header_stage)
        self.assertIn('"/artifacts/pointer-hover-header.ppm"', source)
        subthreshold_stage = source.split(
            '} else if (!strcmp(a.stage, "pointer-subthreshold-hold")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-focus-toggle")) {', 1)[0]
        self.assertLess(subthreshold_stage.index('XTestFakeButtonEvent(dpy, 1, True'),
                        subthreshold_stage.index('pointer_position(dpy, app_window, 355, 250)'))
        self.assertIn('XTestFakeButtonEvent(dpy, 1, False, CurrentTime)', source)
        self.assertIn('modified_key(dpy, XK_Control_L, XK_f)', source)
        focus_stage = source.split(
            '} else if (!strcmp(a.stage, "pointer-focus-mode-preview")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-drop")) {', 1)[0]
        self.assertIn('pointer_drag_begin(dpy, app_window)', focus_stage)
        self.assertIn('pointer_drag_release(dpy, 1)', focus_stage)
        self.assertIn('/artifacts/pointer-focus-mode-drag.ppm', source)
        no_motion_stage = source.split(
            '} else if (!strcmp(a.stage, "pointer-no-motion-release")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-hover-source")) {', 1)[0]
        self.assertIn('pointer_no_motion_begin(dpy, app_window)', no_motion_stage)
        self.assertIn('capture(dpy, a.output)', no_motion_stage)
        self.assertIn('puts("HELD")', no_motion_stage)
        self.assertIn('pointer_drag_release(dpy, 0)', no_motion_stage)
        self.assertNotIn('pointer_position', no_motion_stage)
        no_motion_begin = source.split('static void pointer_no_motion_begin', 1)[1].split(
            'static void pointer_drag_release', 1)[0]
        press_end = no_motion_begin.index('XTestFakeButtonEvent(dpy, 1, True')
        self.assertNotIn('pointer_position', no_motion_begin[press_end:])
        self.assertIn('"/artifacts/pointer-no-motion-held.ppm"', source)
        self.assertIn('"/artifacts/pointer-small-motion-held.ppm"', source)
        fixture = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        self.assertIn("step('pointer-hover-source')", fixture)
        self.assertIn("ppm_changed_pixels('pointer-no-motion-before.ppm', 'pointer-no-motion-held.ppm') > 1000", fixture)
        self.assertIn("ppm_changed_pixels('pointer-subthreshold-before.ppm', 'pointer-small-motion-held.ppm') > 1000", fixture)
        self.assertIn("held_stage('pointer-no-motion-release', 'HELD', 'RELEASE'", fixture)
        self.assertIn("held_stage('pointer-subthreshold-hold', 'HELD', 'CANCEL'", fixture)
        gap_stage = source.split(
            '} else if (!strcmp(a.stage, "pointer-drop-gap")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-create-transfer-tab")) {', 1)[0]
        gap_drag = source.split('static void pointer_gap_drag', 1)[1].split(
            'static void pointer_tab_drag_begin', 1)[0]
        self.assertIn('pointer_position(dpy, app, 350, 250)', gap_drag)
        self.assertIn('pointer_position(dpy, app, 754, 250)', gap_drag)
        self.assertIn('pointer_drag_release(dpy, 0)', gap_stage)
        self.assertIn("step('pointer-drop-gap')", fixture)
        self.assertIn("direct_move_sibling_split(gap_dropped.get('root'), right_pane, left_pane)", fixture)
        self.assertIn("gap_siblings == ('Horizontal', [left_pane, right_pane])", fixture)
        self.assertIn("step('pointer-undo')", fixture.split("step('pointer-drop-gap')", 1)[1])
        self.assertIn('pointer-gap-dropped.ppm', fixture)
        self.assertIn('pointer-gap-undo.ppm', fixture)
        self.assertIn('five_pixel_modifier_drag_preview_cancelled_to_exact_layout_without_pty_or_process_changes', fixture)
        self.assertIn('hovering_top_left_pane_header_from_right_focus_changed_focus_without_click_or_input',
                      fixture)
        self.assertIn("helper_call(config, 'pointer-hover-header', '/artifacts/pointer-hover-header.ppm')",
                      fixture)
        self.assertIn('header_after_views == header_before_views', fixture)
        header_flow = fixture.split("report['stage'] = 'pointer-hover-header'", 1)[1].split(
            "step('pointer-hover-right')", 1)[0]
        self.assertLess(header_flow.index('header_layout.get(\'root\')'),
                        header_flow.index('time.sleep(0.25)'))
        self.assertLess(header_flow.index('time.sleep(0.25)'),
                        header_flow.index("helper_call(config, 'capture', str(header_capture))"))
        self.assertIn('header_capture.unlink()', header_flow)
        for evidence in ('tab_drop_cancel_preserved_both_tab_snapshots_selection_and_live_processes',
                         'tab_drop_committed_one_atomic_history_step_and_activated_empty_destination',
                         'tab_transfer_history_contains_exact_before_and_after_for_both_tabs',
                         'tab_transfer_undo_restored_both_trees_and_kept_destination_selected',
                         'ctrl_shift_2_transferred_to_occupied_tab_without_following_selection_and_undo_restored_exact_trees',
                         'searchable_transfer_picker_transferred_to_occupied_tab_without_following_selection_and_undo_restored_exact_trees'):
            self.assertIn(evidence, fixture)
        hotkey_stage = source.split('} else if (!strcmp(a.stage, "pointer-hotkey-transfer")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-picker-transfer")) {', 1)[0]
        self.assertIn('control_shift_key(dpy, XK_2)', hotkey_stage)
        self.assertIn("step('pointer-select-first-tab')", fixture)
        picker_stage = source.split('} else if (!strcmp(a.stage, "pointer-picker-transfer")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-picker-confirm")) {', 1)[0]
        self.assertIn('setup_prefix(dpy, XK_m, 0)', picker_stage)
        self.assertIn('key(dpy, XK_Down)', picker_stage)
        self.assertIn('key(dpy, XK_Up)', picker_stage)
        self.assertIn('modified_key(dpy, XK_Control_L, XK_r)', picker_stage)
        confirm_stage = source.split('} else if (!strcmp(a.stage, "pointer-picker-confirm")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-tab-drop-cancel")', 1)[0]
        self.assertIn('key(dpy, XK_Return)', confirm_stage)
        self.assertIn("cwd_snapshot(before_processes) == before_cwds", fixture)
        self.assertIn("'/artifacts/pointer-transfer-hotkey.ppm'", fixture)
        self.assertIn("'/artifacts/pointer-transfer-picker-search.ppm'", fixture)
        self.assertIn("'/artifacts/pointer-transfer-picker.ppm'", fixture)
        self.assertIn("prepare_stage='pointer-picker-transfer'", fixture)
        self.assertIn("pointer_position(dpy, app, 405, 57)", source)
        tab_drag = source.split('static void pointer_tab_drag_begin', 1)[1].split(
            'static void pointer_no_motion_begin', 1)[0]
        self.assertIn('pointer_position(dpy, app, 350, 250)', tab_drag)
        self.assertIn('pointer_position(dpy, app, 405, 57)', tab_drag)
        self.assertIn('after_no_motion_layout == no_motion_layout', fixture)
        self.assertIn('after_no_motion_tab.get(\'layout_json\')', fixture)
        self.assertIn('strcmp(response, "CANCEL\\n")', source)
        self.assertIn('XK_p, XK_i, XK_g, XK_a, XK_d, XK_e', source)
        self.assertIn('pointer_position(dpy, app, 350, 250)', source)
        self.assertIn("move_siblings == ('Horizontal', [right_pane, left_pane])",
                      Path(__file__).with_name('isolated-ime-x11.py').read_text())
        fixture = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        self.assertIn("'pointer-small-motion-held.ppm'", fixture)
        self.assertIn("'pointer-no-motion-held.ppm'", fixture)
        self.assertIn("'focus_mode_drag_preview_and_cancel_preserved_nonzero_index_source_and_layout': True", fixture)
        self.assertIn("focus_layout.get('focused') == right_pane", fixture)
        ime_stage = source.split('} else if (!strcmp(a.stage, "pointer-ime-input")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "pointer-ime-cancel")) {', 1)[0]
        self.assertLess(ime_stage.index('nanosleep'), ime_stage.index('capture(dpy, a.output)'))
        self.assertLess(ime_stage.index('capture(dpy, a.output)'), ime_stage.index('pointer_position'))
        self.assertIn('/artifacts/pointer-ime-preedit.ppm', source)
        self.assertNotIn('--coordinates', source)
        self.assertNotIn('--text', source)

    def test_pointer_file_drop_uses_fixed_private_xdnd_and_waits_for_review_confirmation(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'pointer']), 0)
        plan = json.loads(output.getvalue())
        self.assertIn('native XDND fixed URI list opens review', plan['input']['pointer'])
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        source = Path(__file__).with_name('x11-ime-input.c').read_text()
        for token in ('XdndAware', 'XdndSelection', 'XdndEnter', 'XdndPosition', 'XdndDrop',
                      'XdndStatus', 'XdndFinished', 'text/uri-list',
                      'file:///home/pigade/demo/a%20space.txt',
                      'file:///home/pigade/demo/it%27s%24%28no%29.txt',
                      'https://example.invalid/file.txt', 'URI_PAYLOAD_TRUNCATED',
                      'pointer-file-drop-invalid-uri', 'pointer-file-drop-truncated'):
            self.assertIn(token, source)
        drop = source.split('static void pointer_file_drop', 1)[1].split('\n}', 1)[0]
        self.assertIn('XGetSelectionOwner(dpy, selection) != source', drop)
        self.assertIn('XGetWindowProperty(dpy, target, aware, 0, 1, False, XA_ATOM', drop)
        self.assertIn('actual_type == XA_ATOM && actual_format == 32', drop)
        self.assertIn('XDND Status deadline expired', drop)
        self.assertIn('XDND Finished deadline expired', drop)
        self.assertIn('serve_xdnd_request(dpy, c, source, app_pid', drop)
        self.assertIn('while (!status_received || !uri_transferred)', drop)
        self.assertLess(drop.index('uri_transferred |= serve_xdnd_request'),
                        drop.index('message_type = drop'))
        self.assertIn('nanosleep(&entered, NULL)', drop)
        self.assertIn('nanosleep(&review_paint, NULL)', drop)
        self.assertIn('Finished did not match fixed URI-list validity', drop)
        self.assertIn('payload != URI_PAYLOAD_VALID && !accepted', drop)
        selection_service = source.split('static int serve_xdnd_request', 1)[1].split(
            'static void pointer_file_drop', 1)[0]
        self.assertIn('request->owner != source', selection_service)
        self.assertIn('xres_pid(c, (uint32_t)request->requestor) != app_pid', selection_service)
        harness = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        file_flow = harness.split('    # Native external XDND path:', 1)[1].split(
            "    report['pointer'] = {", 1)[0]
        self.assertLess(file_flow.index("helper_call(config, 'pointer-file-drop-cancel')"),
                        file_flow.index("helper_call(config, 'pointer-file-drop-confirm')"))
        self.assertIn("process_snapshot() == drop_processes", file_flow)
        self.assertIn("cwd_snapshot(drop_processes) == drop_cwds", file_flow)
        self.assertIn("expected_insert = first_quoted + ' ' + second_quoted + ' '", file_flow)
        self.assertIn('pointer-file-drop-review-cancel.ppm', harness)
        self.assertIn('pointer-file-drop-review-confirm.ppm', harness)
        self.assertIn('pointer-file-drop-inserted.ppm', harness)
        self.assertIn('pointer-review-baseline.ppm', harness)
        self.assertIn('pointer-file-drop-review-cancelled.ppm', harness)
        self.assertIn('pointer-file-drop-invalid-uri.ppm', harness)
        self.assertIn('pointer-file-drop-truncated.ppm', harness)
        self.assertIn("pointer_chrome_pixel('pointer-file-drop-review-cancel.ppm') != pointer_review_baseline",
                      file_flow)
        self.assertIn("pointer_chrome_pixel('pointer-file-drop-review-confirm.ppm') != pointer_review_baseline",
                      file_flow)
        self.assertIn("pointer_chrome_pixel('pointer-file-drop-review-cancelled.ppm') == pointer_review_baseline",
                      file_flow)
        self.assertIn('pointer_chrome_pixel(Path(capture_name).name) == pointer_review_baseline', file_flow)
        self.assertIn('xdnd_nonfile_and_over_64k_uri_lists_rejected_atomically_without_review_or_input',
                      harness)

    def test_private_clipboard_multiline_payload_is_exact_and_review_gated(self):
        source_path = Path(__file__).with_name('x11-ime-input.c')
        source = source_path.read_text()
        for token in ('CLIPBOARD', 'TARGETS', 'UTF8_STRING', 'text/plain',
                      'PIGADE_MULTILINE_A\\nPIGADE_MULTILINE_B',
                      'pointer-clipboard-paste', 'clipboard transfer deadline expired',
                      'requestor_pid != app_pid', 'owner_match=%d selection_match=%d',
                      'requestor_pid=%ld expected_pid=%ld target=%s target_allowed=%d',
                      'snprintf(target_name, sizeof(target_name), "%.*s", 64, atom_name)',
                      'xres_pid(c, (uint32_t)request->requestor)',
                      'fcitx_pid > 0 && fcitx_pid != app_pid && requestor_pid == fcitx_pid',
                      'refusal.xselection.property = None',
                      'pointer_clipboard_paste(dpy, c, a.app, a.fcitx)'):
            self.assertIn(token, source)
        clipboard_source = source.split('static void pointer_clipboard_paste', 1)[1].split(
            'static void setup_create_sequence', 1)[0]
        fcitx_refusal = clipboard_source.split('if (fcitx_pid > 0 && fcitx_pid != app_pid &&', 1)[1].split(
            'if (requestor_pid != app_pid || !target_match)', 1)[0]
        self.assertIn('XSendEvent(dpy, request->requestor, False, 0, &refusal)', fcitx_refusal)
        self.assertIn('continue;', fcitx_refusal)
        self.assertNotIn('XChangeProperty', fcitx_refusal)
        self.assertNotIn('printf("{\\"stage\\":\\"pointer-clipboard-paste', clipboard_source)
        self.assertEqual(source.count('selection_transfer'), 1)
        self.assertIn('if (!strcmp(a.stage, "pointer-clipboard-paste")) {', source)
        self.assertIn('"\\\"selection_transfer\\\":true,\\\"payload_bytes\\\":%zu}', source)
        with tempfile.TemporaryDirectory() as temporary:
            helper = Path(temporary) / 'x11-ime-input'
            IME.compile_helper(source_path, helper)
            result = subprocess.run([str(helper), '--self-test-clipboard-payload'],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, text=True, timeout=3, check=False)
            uri_result = subprocess.run([str(helper), '--self-test-uri-payloads'],
                                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, timeout=3, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), b'PIGADE_MULTILINE_A\nPIGADE_MULTILINE_B'.hex())
        self.assertEqual(uri_result.returncode, 0, uri_result.stderr)
        uri_lengths = json.loads(uri_result.stdout)
        self.assertGreater(uri_lengths['truncated_uri_bytes'], 65536)
        self.assertGreater(uri_lengths['invalid_uri_bytes'], 0)
        harness = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        flow = harness.split('    # Clipboard paste is a separate multiline preview.', 1)[1].split(
            "    report['pointer'] = {", 1)[0]
        self.assertLess(flow.index("helper_call(config, 'pointer-clipboard-enter')"),
                        flow.index("helper_call(config, 'pointer-clipboard-cancel')"))
        self.assertLess(flow.index("helper_call(config, 'pointer-clipboard-cancel')"),
                        flow.index("helper_call(config, 'pointer-clipboard-confirm')"))
        self.assertLess(flow.index('target_modes & 16'),
                        flow.index("helper_call(config, 'pointer-clipboard-confirm')"))
        self.assertIn("pointer_chrome_pixel('pointer-clipboard-review-enter.ppm') != pointer_review_baseline",
                      flow)
        self.assertIn("pointer_chrome_pixel('pointer-clipboard-review-cancelled.ppm') == pointer_review_baseline",
                      flow)
        self.assertIn("pointer_chrome_pixel('pointer-clipboard-review-confirm.ppm') != pointer_review_baseline",
                      flow)
        self.assertIn("clipboard_before[1][target_key][3].get('modes')", flow)
        self.assertIn("pointer_state_snapshot() == clipboard_before", flow)
        self.assertIn('pointer-clipboard-review-enter.ppm', flow)
        self.assertIn('pointer-clipboard-review-confirm.ppm', flow)

    def test_catalog_mode_is_fixed_plan_only_and_has_bounded_library_sequences(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'catalog']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'catalog')
        self.assertIn('save/apply/undo/delete geometry', plan['input']['catalog'])
        self.assertIn('rename/find a pane', plan['input']['catalog'])
        self.assertIn('exact-generation stop action', plan['input']['catalog'])
        self.assertIn('stop hidden bootstrap through Sessions', plan['input']['catalog'])
        self.assertIn('seed metadata-only agent profiles and pin/reorder them through the library UI',
                      plan['input']['catalog'])
        self.assertIn('open/refresh private workspace Git', plan['input']['catalog'])
        self.assertIn('stage/unstage four fixed per-file diffs through Git UI', plan['input']['catalog'])
        self.assertIn('review and confirm a fixed native Git commit with actual hooks',
                      plan['input']['catalog'])
        self.assertIn('review/cancel/confirm Close Tab', plan['input']['catalog'])
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        source = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        for action in ('open-library', 'close-library', 'open-save-dialog', 'save-template',
                       'apply-first-template', 'undo-layout', 'delete-template',
                       'mutate-layout', 'rename-tab', 'focus-right'):
            self.assertIn(f"'{action}'", source)
        self.assertNotIn('from Xlib', source)
        self.assertIn('tab_rename_persisted', source)
        self.assertIn('pane_alias_saved_and_found_by_palette', source)
        self.assertIn('confirmed_stop_exited_only_named_generation', source)
        self.assertIn('stop_confirmation_cancel_preserved_named_generation', source)
        self.assertIn('hidden_sessions_stop_exited_only_unplaced_bootstrap', source)
        self.assertIn('other_four_process_states_and_pane_tree_preserved_after_hidden_stop', source)
        self.assertIn('agent_profile_pin_and_partition_reorder_persisted_exact_roster', source)
        self.assertIn('agent_profile_display_order_survived_library_close_and_reopen', source)
        self.assertIn('agent_profile_actions_preserved_profile_revisions_selection_layout_frames_and_journals',
                      source)
        profile_flow = source.split("report['stage'] = 'catalog-agent-profiles-seed'", 1)[1].split(
            'git_catalog, git_layout = catalog_layout()', 1)[0]
        for stage in ('catalog-profiles-open', 'catalog-profiles-query',
                      'catalog-profiles-select-first', 'catalog-profiles-select-next',
                      'catalog-profiles-pin', 'catalog-profiles-move-up',
                      'catalog-profiles-move-down', 'catalog-profiles-close'):
            self.assertIn(f"helper_call(config, '{stage}')", profile_flow)
        self.assertLess(profile_flow.index("helper_call(config, 'catalog-profiles-close')"),
                        profile_flow.index("helper_call(config, 'catalog-profiles-open')",
                                           profile_flow.index("helper_call(config, 'catalog-profiles-close')") + 1))
        self.assertIn("roster.get('ordered_ids') == order", source)
        self.assertIn("'settings': profile_settings", source)
        self.assertIn("control.sendall(b'CHECK\\n')", source)
        self.assertNotIn('profile-launch', profile_flow)
        self.assertNotIn('detect_providers', profile_flow)
        seed = source.split("seeded_catalog = host_request({'command': 'catalog_apply'", 1)[1].split(
            "profile_catalog, profile_layout = catalog_layout()", 1)[0]
        self.assertIn("'name': name", seed)
        self.assertIn("'revision': 0", seed)
        self.assertIn("'provider': 'codex'", source)
        self.assertIn("'model': None, 'reasoning_effort': None", source)
        expected_actions = ('catalog-profiles-open', 'catalog-profiles-query',
                            'catalog-profiles-select-first', 'catalog-profiles-pin',
                            'catalog-profiles-select-next', 'catalog-profiles-pin',
                            'catalog-profiles-move-up', 'catalog-profiles-pin',
                            'catalog-profiles-move-down', 'catalog-profiles-close',
                            'catalog-profiles-open', 'catalog-profiles-query',
                            'catalog-profiles-close')
        cursor = 0
        for stage in expected_actions:
            cursor = profile_flow.index(f"helper_call(config, '{stage}')", cursor) + 1
        helper = Path(__file__).with_name('x11-ime-input.c').read_text()
        open_sequence = helper.split('} else if (!strcmp(a.stage, "catalog-profiles-open")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-profiles-query")) {', 1)[0]
        self.assertIn('setup_prefix(dpy, XK_p, 0)', open_sequence)
        query_sequence = helper.split('} else if (!strcmp(a.stage, "catalog-profiles-query")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-profiles-select-first")) {', 1)[0]
        self.assertIn('XK_p, XK_r, XK_o, XK_f, XK_i, XK_l, XK_e, XK_s', query_sequence)
        self.assertIn('modified_key(dpy, XK_Control_L, XK_p)', helper)
        self.assertIn('modified_key(dpy, XK_Alt_L, XK_Up)', helper)
        self.assertIn('modified_key(dpy, XK_Alt_L, XK_Down)', helper)
        self.assertIn('current_panes == process_sessions - {bootstrap_key[0]}', source)
        self.assertIn('workspace_git_snapshot_covers_staged_unstaged_and_untracked', source)
        self.assertIn('workspace_git_refresh_reflects_private_file_removal', source)
        self.assertIn('workspace_git_index_actions_used_fixed_ui_and_restored_exact_index', source)
        self.assertIn("private_git(['ls-files', '--stage', '-z']) == index_baseline", source)
        self.assertIn("private_git(['diff', '--cached', '--raw', '-z']) == cached_baseline", source)
        index_flow = source.split('# Exercise each one-file index ticket only through the dialog\'s fixed UI', 1)[1].split(
            "# The fixture commands suppress hooks", 1)[0]
        self.assertEqual(index_flow.count("helper_call(config, 'catalog-git-index-apply')"), 4)
        second_open = index_flow.index("helper_call(config, 'catalog-git-diff-second')")
        self.assertLess(
            second_open,
            index_flow.index("helper_call(config, 'catalog-git-index-apply')", second_open),
        )
        batch_flow = source.split('# The same review dialog also exposes frozen Stage All / Unstage All', 1)[1].split(
            "# The fixture commands suppress hooks", 1)[0]
        for stage in ('catalog-git-batch-stage', 'catalog-git-batch-unstage',
                      'catalog-git-batch-confirm', 'catalog-git-batch-enter',
                      'catalog-git-batch-back'):
            self.assertIn(f"helper_call(config, '{stage}')", batch_flow)
        self.assertLess(batch_flow.index("helper_call(config, 'catalog-git-batch-enter')"),
                        batch_flow.index("helper_call(config, 'catalog-git-batch-back')"))
        self.assertIn('require_batch_cancelled_index(batch_index_before', batch_flow)
        self.assertIn("private_git(['ls-files', '-z'])", batch_flow)
        self.assertIn("batch_staged_paths == expected_batch_index_paths", batch_flow)
        self.assertIn("private_git(['add', '--', 'staged-only.txt'])", batch_flow)
        self.assertIn("batch-all.txt", batch_flow)
        batch_helper = Path(__file__).with_name('x11-ime-input.c').read_text()
        for stage in ('catalog-git-batch-stage', 'catalog-git-batch-unstage',
                      'catalog-git-batch-confirm', 'catalog-git-batch-enter',
                      'catalog-git-batch-back'):
            self.assertIn(f'"{stage}"', batch_helper)
        self.assertIn('control_shift_key(dpy, XK_s)', batch_helper)
        self.assertIn('control_shift_key(dpy, XK_BackSpace)', batch_helper)
        self.assertIn('modified_key(dpy, XK_Control_L, XK_Return)', batch_helper)
        self.assertIn('modified_key(dpy, XK_Alt_L, XK_Left)', batch_helper)
        for artifact in ('catalog-git-batch-stage-review.ppm',
                         'catalog-git-batch-stage-result.ppm',
                         'catalog-git-batch-unstage-review.ppm',
                         'catalog-git-batch-unstage-result.ppm'):
            self.assertIn(artifact, source)
        self.assertIn("'refresh_invoked_by_fixed_keyboard_stage': 'catalog-git-refresh'", source)
        self.assertIn("'command': 'workspace_git_snapshot'", source)
        self.assertIn("untracked_name = 'untracked space\\nline.txt'", source)
        self.assertIn("mount_filesystem_type('/home') == 'tmpfs'", source)
        self.assertIn("'--tmpfs', '/home'", source)
        self.assertIn("'tab_close_schema') == 1", source)
        self.assertIn("'close_tab_confirm_removed_tab_and_reselected_adjacent_fallback': True", source)
        self.assertIn("'close_tab_removed_closed_tab_from_all_undo_redo_snapshots': True", source)
        self.assertIn("'closed_session_reattached_via_existing_layout_edit_without_launch': attach_session", source)
        commit_flow = source.split("report['stage'] = 'catalog-git-commit-review'", 1)[1].split(
            "report['stage'] = 'catalog-git-close'", 1)[0]
        self.assertLess(commit_flow.index("helper_call(config, 'catalog-git-commit-open')"),
                        commit_flow.index("helper_call(config, 'catalog-git-commit-message')"))
        self.assertLess(commit_flow.index('require_commit_review_unchanged('),
                        commit_flow.index("helper_call(config, 'catalog-git-commit-confirm')"))
        self.assertIn("committed_tree == frozen_index_tree", commit_flow)
        self.assertIn("committed_parent == head_baseline", commit_flow)
        self.assertIn("all(path.read_bytes() == b'x' for path in hook_counts.values())", commit_flow)
        close_flow = source.split("report['stage'] = 'catalog-close-tab-review'", 1)[1].split(
            "report['catalog'] =", 1)[0]
        self.assertLess(close_flow.index("helper_call(config, 'catalog-close-cancel')"),
                        close_flow.index("helper_call(config, 'catalog-close-confirm')"))
        self.assertIn("close_layout_revision = before_close_catalog['layout'].get('revision')", source)
        self.assertIn("close_selection_revision = fallback_selection.get('revision')", source)
        self.assertIn("'root': None, 'focused': None, 'focus_mode': False", source)
        self.assertIn("helper_call(config, 'catalog-close-inspect')", close_flow)
        self.assertIn("'action': {'action': 'apply', 'command': {'Insert': {", close_flow)
        self.assertIn("all(isinstance(state, dict) and tab_id not in state.get('tabs', {})", close_flow)
        self.assertIn("'private_home_tmpfs': True", source)
        self.assertIn("project['canonical_path'] != str(expected_project_path)", source)
        self.assertIn("expected_git_repo = (private_home / 'demo').resolve(strict=True)", source)
        self.assertIn("git_repo.parent == private_home", source)
        hidden_stop_close = source.split("'/artifacts/catalog-hidden-stop-complete.ppm'", 1)[1].split(
            "report['catalog'] =", 1)[0]
        self.assertLess(hidden_stop_close.index("helper_call(config, 'catalog-stop-cancel')"),
                        hidden_stop_close.index("helper_call(config, 'catalog-git-open')"))
        self.assertIn("label.get('name') == 'Review'", source)
        self.assertIn("'pane_labels'", source)
        helper = Path(__file__).with_name('x11-ime-input.c').read_text()
        for stage in ('catalog-open-library', 'catalog-close-library', 'catalog-open-save',
                      'catalog-save', 'catalog-apply', 'catalog-undo', 'catalog-delete',
                      'catalog-mutate', 'catalog-rename-tab', 'catalog-pane-rename-open',
                      'catalog-pane-rename-action', 'catalog-pane-rename-type',
                      'catalog-pane-rename-save', 'catalog-pane-palette-open',
                      'catalog-pane-palette-query', 'catalog-pane-palette-select',
                      'catalog-stop-open', 'catalog-stop-action', 'catalog-stop-cancel',
                      'catalog-stop-confirm', 'catalog-sessions-open',
                      'catalog-sessions-action', 'catalog-hidden-stop-review',
                      'catalog-hidden-stop-confirm', 'catalog-git-open',
                      'catalog-profiles-open', 'catalog-profiles-query',
                      'catalog-profiles-select-first', 'catalog-profiles-select-next',
                      'catalog-profiles-pin', 'catalog-profiles-move-up',
                      'catalog-profiles-move-down', 'catalog-profiles-close',
                      'catalog-git-query', 'catalog-git-refresh', 'catalog-git-diff-first',
                      'catalog-git-diff-second', 'catalog-git-index-apply',
                      'catalog-git-commit-open', 'catalog-git-commit-message',
                      'catalog-git-commit-confirm', 'catalog-git-close',
                      'catalog-close-open', 'catalog-close-query', 'catalog-close-inspect',
                      'catalog-close-cancel', 'catalog-close-confirm'):
            self.assertIn(f'\"{stage}\"', helper)
        self.assertIn('modified_key(dpy, XK_Shift_L, XK_r)', helper)
        rename_action = helper.split('} else if (!strcmp(a.stage, "catalog-pane-rename-action")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-pane-rename-type"', 1)[0]
        self.assertLess(rename_action.index('nanosleep'), rename_action.index('key(dpy, XK_Return)'))
        self.assertIn('.tv_nsec = 400000000', rename_action)
        self.assertIn("time.sleep(.4)\n    helper_call(config, 'capture', '/artifacts/catalog-pane-palette.ppm')", source)
        self.assertIn("'/artifacts/catalog-pane-renamed.ppm'", source)
        self.assertIn("'/artifacts/catalog-pane-palette.ppm'", source)
        self.assertIn("'/artifacts/catalog-stop-review.ppm'", source)
        self.assertIn("'/artifacts/catalog-stop-complete.ppm'", source)
        between = source.split("'capture', '/artifacts/catalog-stop-complete.ppm'", 1)[1].split("'catalog-sessions-open'", 1)[0]
        self.assertIn("helper_call(config, 'catalog-stop-cancel')", between)
        stop_action = helper.split('} else if (!strcmp(a.stage, "catalog-stop-action")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-stop-cancel")) {', 1)[0]
        self.assertIn('XK_s, XK_t, XK_o, XK_p', stop_action)
        self.assertIn('.tv_nsec = 400000000', stop_action)
        self.assertLess(stop_action.index('nanosleep'), stop_action.index('key(dpy, XK_Return)'))
        stop_confirm = helper.split('} else if (!strcmp(a.stage, "catalog-stop-confirm")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "recipe-select-first")) {', 1)[0]
        self.assertLess(stop_confirm.index('key(dpy, XK_Tab)'), stop_confirm.index('key(dpy, XK_Return)'))
        self.assertIn('!strcmp(a.stage, "catalog-stop-confirm") ? "2"', helper)
        sessions_action = helper.split('} else if (!strcmp(a.stage, "catalog-sessions-action")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-hidden-stop-review")', 1)[0]
        self.assertIn('XK_s, XK_e, XK_s, XK_s, XK_i, XK_o, XK_n, XK_s', sessions_action)
        self.assertIn('.tv_nsec = 400000000', sessions_action)
        self.assertLess(sessions_action.index('nanosleep'), sessions_action.index('key(dpy, XK_Return)'))
        hidden_stop = helper.split('} else if (!strcmp(a.stage, "catalog-hidden-stop-review") ||', 1)[1].split(
            '} else if (!strcmp(a.stage, "recipe-select-first")) {', 1)[0]
        self.assertIn('key(dpy, XK_Tab)', hidden_stop)
        self.assertIn('key(dpy, XK_Return)', hidden_stop)
        self.assertIn("'/artifacts/catalog-hidden-stop-review.ppm'", source)
        self.assertIn("'/artifacts/catalog-hidden-stop-complete.ppm'", source)
        for artifact in ('catalog-agent-profiles-initial.ppm', 'catalog-agent-profiles-pinned.ppm',
                         'catalog-agent-profiles-final.ppm', 'catalog-agent-profiles-reopened.ppm'):
            self.assertIn(artifact, source)
        self.assertIn('!strcmp(a.stage, "catalog-hidden-stop-confirm") ? "2"', helper)
        git_query = helper.split('} else if (!strcmp(a.stage, "catalog-git-query")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-git-refresh")) {', 1)[0]
        self.assertIn('XK_g, XK_i, XK_t', git_query)
        self.assertIn('.tv_nsec = 400000000', git_query)
        self.assertLess(git_query.index('nanosleep'), git_query.index('capture('))
        self.assertLess(git_query.index('capture('), git_query.index('key(dpy, XK_Return)'))
        git_close = helper.split('} else if (!strcmp(a.stage, "catalog-git-close")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "recipe-select-first")) {', 1)[0]
        self.assertIn('key(dpy, XK_Escape)', git_close)
        self.assertNotIn('key(dpy, XK_Tab)', git_close)
        git_refresh = helper.split('} else if (!strcmp(a.stage, "catalog-git-refresh")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-git-diff-first")) {', 1)[0]
        self.assertIn('modified_key(dpy, XK_Control_L, XK_r)', git_refresh)
        git_diff_first = helper.split('} else if (!strcmp(a.stage, "catalog-git-diff-first")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-git-diff-second")) {', 1)[0]
        self.assertIn('key(dpy, XK_Return)', git_diff_first)
        git_diff_second = helper.split('} else if (!strcmp(a.stage, "catalog-git-diff-second")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-git-index-apply")) {', 1)[0]
        self.assertLess(git_diff_second.index('key(dpy, XK_Down)'), git_diff_second.index('key(dpy, XK_Return)'))
        git_index_apply = helper.split('} else if (!strcmp(a.stage, "catalog-git-index-apply")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-git-commit-open")) {', 1)[0]
        self.assertIn('modified_key(dpy, XK_Control_L, XK_s)', git_index_apply)
        git_commit_message = helper.split(
            '} else if (!strcmp(a.stage, "catalog-git-commit-message")) {', 1)[1].split(
                '} else if (!strcmp(a.stage, "catalog-git-close")) {', 1)[0]
        self.assertIn('XK_Shift_L, XK_p', git_commit_message)
        self.assertIn('XK_c, XK_o, XK_m, XK_m, XK_i, XK_t', git_commit_message)
        for artifact in ('catalog-git-commit-review.ppm', 'catalog-git-commit-complete.ppm'):
            self.assertIn(artifact, source)
        for artifact in ('catalog-git-query.ppm', 'catalog-git-snapshot.ppm',
                         'catalog-git-refreshed.ppm', 'catalog-close-tab-review.ppm',
                         'catalog-close-tab-fallback.ppm'):
            self.assertIn(artifact, source)
        for artifact in ('catalog-git-index-staged-review.ppm',
                         'catalog-git-worktree-review.ppm',
                         'catalog-git-index-tracked-review.ppm',
                         'catalog-git-untracked-review.ppm'):
            self.assertIn(artifact, source)
        close_query = helper.split('} else if (!strcmp(a.stage, "catalog-close-query")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-close-cancel")) {', 1)[0]
        self.assertIn('XK_c, XK_l, XK_o, XK_s, XK_e, XK_space', close_query)
        self.assertIn('XK_t, XK_a, XK_b', close_query)
        self.assertLess(close_query.index('nanosleep'), close_query.index('key(dpy, XK_Return)'))
        close_inspect = helper.split('} else if (!strcmp(a.stage, "catalog-close-inspect")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "catalog-close-cancel")) {', 1)[0]
        self.assertIn('modified_key(dpy, XK_Control_L, XK_r)', close_inspect)
        self.assertLess(close_flow.index("helper_call(config, 'catalog-close-query')"),
                        close_flow.index("helper_call(config, 'catalog-close-inspect')"))
        self.assertLess(close_flow.index("helper_call(config, 'catalog-close-inspect')"),
                        close_flow.index("'/artifacts/catalog-close-tab-review.ppm'"))
        close_confirm = helper.split('} else if (!strcmp(a.stage, "catalog-close-confirm")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "recipe-select-first")) {', 1)[0]
        self.assertIn('modified_key(dpy, XK_Control_L, XK_Return)', close_confirm)
        self.assertIn("!strcmp(a.stage, \"catalog-close-query\") ? \"10\"", helper)
        self.assertIn("'stage'] = 'catalog-pane-rename-action'", source)
        self.assertIn("'stage'] = 'catalog-pane-rename-input'", source)
        self.assertIn("time.sleep(.25)\n    helper_call(config, 'capture', '/artifacts/catalog-pane-rename-dialog.ppm')", source)
        self.assertIn("time.sleep(.25)\n    helper_call(config, 'capture', '/artifacts/catalog-pane-rename-input.ppm')", source)
        self.assertIn("'stage'] = 'catalog-pane-palette-select'", source)
        self.assertIn("'stage'] = 'catalog-pane-palette-verify'", source)
        alias_checks = source.split('rename_layout = catalog_layout()[1]', 1)[1].split(
            "report['catalog'] =", 1)[0]
        self.assertNotIn("original_layout.get('root')", alias_checks)
        self.assertGreaterEqual(alias_checks.count("get('root') == rename_layout.get('root')"), 3)
        self.assertIn('initial_five_process_identities_unchanged_until_stop', source)
        self.assertIn('remaining_four_process_identities_and_states_unchanged_after_stop', source)

    def test_recipes_mode_is_plan_only_and_uses_only_fixed_input_stages(self):
        output = io.StringIO()
        with mock.patch.object(IME.subprocess, 'Popen', side_effect=AssertionError('spawned')), \
                mock.patch.object(IME.subprocess, 'run', side_effect=AssertionError('spawned')), \
                contextlib.redirect_stdout(output):
            self.assertEqual(IME.main(['--fixture', 'recipes']), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan['fixture'], 'recipes')
        self.assertEqual(plan['status'], 'plan-only-no-process-no-write')
        self.assertIn('explicitly confirm', plan['input']['recipes'])
        self.assertFalse(plan['input']['arbitrary_text_argument'])
        source = Path(__file__).with_name('isolated-ime-x11.py').read_text()
        for action in ('select-first', 'open-editor', 'save-editor', 'new-tab', 'name-tab',
                       'submit-name', 'confirm-run'):
            self.assertIn(f"'{action}'", source)
        for evidence in ('original_five_process_identities_unchanged',
                         'no_process_or_tab_before_explicit_confirm',
                         'revision_observer_allows_skipped_acknowledgements',
                         'active_run_list_empty_after_completion'):
            self.assertIn(evidence, source)
        observer = source.split("recipe_key_action(config, 'confirm-run')", 1)[1].split(
            "expected_pairs =", 1)[0]
        self.assertNotIn('fixture_generations()', observer)
        self.assertNotIn('added_identities', observer)
        self.assertIn('set(original_identities).issubset(set(final_identities))', source)
        helper = Path(__file__).with_name('x11-ime-input.c').read_text()
        for stage in ('recipe-select-first', 'recipe-open-editor', 'recipe-save-editor', 'recipe-new-tab',
                      'recipe-name-tab', 'recipe-submit-name', 'recipe-confirm-run',
                      'recipe-slot-open', 'recipe-slot-dirty', 'recipe-profile-open',
                      'recipe-profile-no-match', 'recipe-profile-cancel', 'recipe-profile-select',
                      'recipe-slot-save', 'recipe-name-custom-tab', 'recipe-custom-confirm'):
            self.assertIn(f'"{stage}"', helper)
        for proof in ('dirty_slot_survives_picker_cancel', 'source_profile_deleted_after_copy',
                      'no_process_or_plan_advance_before_confirm', 'custom_cli_invocations'):
            self.assertIn(proof, source)
        self.assertIn("config.get('fixture') in ('setup', 'recipes')", source)
        self.assertIn("'recipe-custom-run-preview.ppm'", source)
        self.assertIn("'projects': [], 'workspaces': [], 'tabs': [], 'selection': None, 'layout': None", source)
        dirty_stage = helper.split('} else if (!strcmp(a.stage, "recipe-slot-dirty")) {', 1)[1].split(
            '} else if (!strcmp(a.stage, "recipe-profile-open")) {', 1)[0]
        self.assertIn('XK_Shift_L, XK_d', dirty_stage)
        self.assertIn('XK_Shift_L, XK_k', dirty_stage)
        self.assertNotIn('--text', helper)
        self.assertNotIn('--coordinates', helper)

    def run_raw_fixture(self, expected, delivered):
        with tempfile.TemporaryDirectory() as temporary:
            path = str(Path(temporary) / 'control.sock')
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            master, slave = pty.openpty()
            process = None
            control = None
            try:
                listener.bind(path); listener.listen(1); listener.settimeout(3)
                process = subprocess.Popen(['/usr/bin/python3', '-I', '-c', IME.FIXTURE,
                                            path, expected.hex()], stdin=slave, stdout=slave,
                                           stderr=subprocess.DEVNULL, close_fds=True)
                os.close(slave); slave = -1
                control, _ = listener.accept()
                self.assertEqual(IME.json_line(control, timeout=2), 'READY')
                control.sendall(b'CHECK\n')
                self.assertEqual(IME.json_line(control, timeout=2), 'NO_PRECOMMIT_INPUT')
                os.write(master, delivered)
                control.sendall(b'COLLECT\n')
                line = IME.json_line(control, timeout=5)
                self.assertTrue(line.startswith('RESULT:'))
                result = json.loads(line[len('RESULT:'):])
                self.assertEqual(process.wait(timeout=3), 0)
                return result
            finally:
                if control is not None:
                    control.close()
                listener.close()
                os.close(master)
                if slave >= 0:
                    os.close(slave)
                if process is not None and process.poll() is None:
                    # Still-owned direct child; no PID discovery or desktop access.
                    process.kill(); process.wait(timeout=3)

    def test_raw_pty_oracle_checks_ime_and_keyboard_bytes_without_echo_or_duplicates(self):
        for expected in ('你好'.encode(), IME.KEYBOARD_EXPECTED):
            with self.subTest(expected_bytes=len(expected)):
                result = self.run_raw_fixture(expected, expected)
                self.assertEqual(result['received_bytes'], len(expected))
                self.assertTrue(result['exact_once'])
                self.assertEqual(result['extra_bytes'], 0)

    def test_raw_pty_oracle_refuses_duplicate_trailing_bytes(self):
        expected = '你好'.encode()
        result = self.run_raw_fixture(expected, expected + expected)
        self.assertFalse(result['exact_once'])
        self.assertEqual(result['extra_bytes'], len(expected))

    def test_latency_child_checks_ordered_input_and_rejects_trailing_duplicate(self):
        for duplicate in (False, True):
            with self.subTest(duplicate=duplicate), tempfile.TemporaryDirectory() as temporary:
                path = str(Path(temporary) / 'control.sock')
                listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                master, slave = pty.openpty()
                process = None
                control = None
                try:
                    listener.bind(path); listener.listen(1); listener.settimeout(3)
                    process = subprocess.Popen(['/usr/bin/python3', '-I', '-c', IME.LATENCY_FIXTURE,
                                                path, IME.LATENCY_KEYS.hex()], stdin=slave,
                                               stdout=slave, stderr=subprocess.DEVNULL, close_fds=True)
                    os.close(slave); slave = -1
                    control, _ = listener.accept()
                    self.assertEqual(IME.json_line(control, timeout=2), 'READY')
                    control.sendall(b'START\n')
                    os.write(master, IME.LATENCY_KEYS + (IME.LATENCY_KEYS[-1:] if duplicate else b''))
                    if duplicate:
                        with self.assertRaises(RuntimeError):
                            IME.json_line(control, timeout=3)
                        self.assertNotEqual(process.wait(timeout=3), 0)
                    else:
                        self.assertEqual(IME.json_line(control, timeout=3), 'FIFTY_EXACT_KEYS')
                        self.assertEqual(process.wait(timeout=3), 0)
                finally:
                    if control is not None:
                        control.close()
                    listener.close(); os.close(master)
                    if slave >= 0:
                        os.close(slave)
                    if process is not None and process.poll() is None:
                        process.kill(); process.wait(timeout=3)

    def test_helper_contains_only_fixed_input_stages_and_server_validation(self):
        source = Path(__file__).with_name('x11-ime-input.c').read_text()
        self.assertIn('const KeySym sequence[] = { XK_n, XK_i, XK_h, XK_a, XK_o }', source)
        self.assertIn('key(dpy, XK_space)', source)
        self.assertIn('"@server=fcitx"', source)
        self.assertIn('xcb_res_client_id_value_value_length(it.data)', source)
        self.assertIn('SO_PEERCRED', source)
        self.assertIn('XTestQueryExtension', source)
        focus_stage = source.split('else if (!strcmp(a.stage, "focus")) {', 1)[1].split('} else {', 1)[0]
        self.assertIn('verify_app_focus_target(&a, dpy, c, 1)', focus_stage)
        self.assertNotIn('key(dpy,', focus_stage)
        self.assertNotIn('--text', source)
        self.assertNotIn('XStringToKeysym(argv', source)

    def test_helper_build_and_identity_selftests_never_open_x(self):
        with tempfile.TemporaryDirectory() as temporary:
            helper = Path(temporary) / 'x11-ime-input'
            evidence = IME.compile_helper(Path(__file__).with_name('x11-ime-input.c'), helper)
            self.assertEqual(evidence['libraries'], ['X11', 'Xtst', 'xcb', 'xcb-res'])
            result = subprocess.run([str(helper), '--self-test-proc'], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                    timeout=2, check=True, env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            self.assertGreater(int(result.stdout.strip()), 0)
            self.assertEqual(result.stderr, '')
            length = subprocess.run([str(helper), '--self-test-xres-length'],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, text=True, timeout=2, check=True,
                                    env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            self.assertEqual(length.stdout.strip(), 'wire_bytes=4 decoded_values=1')
            missing = subprocess.run([str(helper), '--self-test-window-missing'],
                                     stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True, timeout=2, check=False,
                                     env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            self.assertEqual(missing.returncode, 2)
            self.assertIn('verified app window is absent', missing.stderr)
            diagnostic = subprocess.run([str(helper), '--self-test-latency-diagnostic'],
                                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, timeout=2, check=True,
                                        env={'PATH': '/usr/bin', 'DISPLAY': ':99999'})
            parsed = IME.parse_latency_helper_diagnostic(diagnostic.stdout)
            self.assertEqual(parsed['sample_count'], 7)
            self.assertEqual(parsed['failed_index'], 7)
            self.assertEqual(parsed['samples_ns'], [100, 110, 120, 130, 140, 150, 160])
            self.assertEqual(parsed['expected_rgb'], [45, 239, 27])
            self.assertEqual(parsed['observed_rgb'], [41, 239, 27])
            self.assertEqual(parsed['reason'], 'color_deadline')

    def latency_diagnostic_payload(self):
        return {
            'version': 1, 'stage': 'latency',
            'scope': 'XTEST-submit to private root pixel readback', 'pixel': [20, 120],
            'sample_count': 7, 'samples_ns': list(range(100, 107)), 'failed_index': 7,
            'expected_rgb': [45, 239, 27], 'observed_rgb': [41, 239, 27],
            'elapsed_ns': 1_000_100_000, 'reason': 'color_deadline',
        }

    def test_latency_diagnostic_rejects_oversize_malformed_and_nonfixture_values(self):
        valid = self.latency_diagnostic_payload()
        self.assertEqual(IME.parse_latency_helper_diagnostic(json.dumps(valid)),
                         {**valid, 'samples_ns': list(range(100, 107))})
        cases = []
        cases.append(b'x' * (IME.MAX_LATENCY_DIAGNOSTIC_BYTES + 1))
        cases.append(b'{not json')
        for change in (
                {'unexpected_raw_text': 'do not retain'},
                {'version': True},
                {'failed_index': 8},
                {'observed_rgb': [256, 0, 0]},
                {'reason': 'arbitrary-provider-output'},
                {'samples_ns': list(range(100, 106))},
                {'expected_rgb': [46, 239, 27]}):
            altered = dict(valid); altered.update(change)
            cases.append(json.dumps(altered))
        for candidate in cases:
            with self.subTest(candidate_type=type(candidate).__name__):
                with self.assertRaises((ValueError, TypeError, json.JSONDecodeError)):
                    IME.parse_latency_helper_diagnostic(candidate)

    def test_latency_failure_helper_preserves_only_validated_fixed_diagnostic(self):
        with mock.patch.dict(IME.os.environ, {'LD_LIBRARY_PATH': ''}), \
                mock.patch.object(IME.subprocess, 'run', return_value=SimpleNamespace(
                returncode=2, stdout=json.dumps(self.latency_diagnostic_payload()),
                stderr='arbitrary helper stderr must not be retained')):
            with self.assertRaisesRegex(RuntimeError, 'no key retry') as caught:
                IME.helper_call({'display': ':3', 'boot_id': 'boot', 'xvfb_pid': 1,
                                 'xvfb_start': 2, 'xvfb_socket_inode': 3,
                                 'fcitx_pid': 4, 'fcitx_start': 5,
                                 'app_pid': 6, 'app_start': 7}, 'latency')
        self.assertEqual(caught.exception.latency_diagnostic_status, 'validated')
        self.assertEqual(caught.exception.latency_diagnostic['failed_index'], 7)
        self.assertNotIn('arbitrary helper stderr', str(caught.exception))

    def test_latency_failure_diagnostic_collection_cannot_mask_original_exception(self):
        original = RuntimeError('fixed latency color deadline; no key retry')
        with mock.patch.object(IME, 'collect_latency_failure_diagnostics',
                               side_effect=OSError('private diagnostic failure')):
            snapshot = IME.capture_latency_failure_diagnostics({}, {}, Path('/nonexistent'), original)
        self.assertEqual(snapshot, {'status': 'unavailable', 'error_type': 'OSError'})
        self.assertEqual(str(original), 'fixed latency color deadline; no key retry')

    def test_latency_host_snapshot_redacts_content_and_bounds_palette_and_cells(self):
        sentinel = 'PRIVATE_SENTINEL_MUST_NOT_ESCAPE'
        cells = [{'text': chr(97 + index % 26), 'bg': 'Spec(Rgb { r: 45, g: 239, b: 27 })',
                  'fg': sentinel, 'hyperlink': 1} for index in range(8)]
        cells[7]['text'] = sentinel
        reply = {'views': [{
            'session': 'fixture-session', 'generation': 'private-generation', 'sequence': 10,
            'state': 'running', 'pid': 999, 'storage_error': None,
            'frame': {'geometry': {'rows': 2, 'cols': 4}, 'cells': cells,
                      'cursor': [1, 2], 'modes': 7, 'history_lines': 0,
                      'sync_pending_bytes': 0, 'title': sentinel, 'colors': sentinel,
                      'palette_overrides': [[7, [45, 239, 27]]]},
            'actor_diagnostic': {'version': 1, 'status': 'available',
                                 'stage': 'model_apply', 'candidate_sequence': 11,
                                 'entered_monotonic_ns': 123,
                                 'reader': {'read_calls': 3, 'bytes_read': 8,
                                            'last_read_bytes': 4, 'enqueued_calls': 2,
                                            'bytes_enqueued': 8,
                                            'last_read_monotonic_ns': 100,
                                            'last_enqueued_monotonic_ns': 101}},
        }]}
        summary = IME.sanitize_latency_host_snapshot(reply, 'fixture-session', [45, 239, 27])
        encoded = json.dumps(summary)
        self.assertNotIn(sentinel, encoded)
        self.assertNotIn('private-generation', encoded)
        self.assertNotIn('fixture-session', encoded)
        self.assertEqual(summary['sequence'], 10)
        self.assertEqual(summary['cell_samples'][7]['text_class'], 'redacted')
        self.assertTrue(summary['cell_samples'][7]['matches_failed_expected_rgb'])
        self.assertEqual(summary['palette_overrides'], [[7, [45, 239, 27]]])
        self.assertEqual(summary['actor_diagnostic']['stage'], 'model_apply')
        reply['views'][0]['frame']['cells'] = cells * 9000
        with self.assertRaisesRegex(ValueError, 'bounds'):
            IME.sanitize_latency_host_snapshot(reply, 'fixture-session', [45, 239, 27])
        reply['views'][0]['frame']['cells'] = cells
        reply['views'][0]['frame']['palette_overrides'] = [[999, [0, 0, 0]]]
        with self.assertRaisesRegex(ValueError, 'palette'):
            IME.sanitize_latency_host_snapshot(reply, 'fixture-session', [45, 239, 27])

    def test_xim_selection_name_matches_pinned_upstream_registration_form(self):
        # xcb-imdkit constructs its registered atom as "@server=<serverName>".
        # The harness only accepts the serverName chosen by XMODIFIERS=@im=fcitx.
        self.assertIn("XMODIFIERS': '@im=fcitx'", SCRIPT.read_text())
        self.assertIn('"@server=fcitx"', Path(__file__).with_name('x11-ime-input.c').read_text())


if __name__ == '__main__':
    unittest.main()
