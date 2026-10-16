import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import { performance } from 'node:perf_hooks';
import { createHash } from 'node:crypto';
import { createModel, apply, snapshot, output, write } from '../src/terminal.mjs';

const fixtures = [
  { name: 'plain-completed', prefix: [output('hello\r\nПривет')], tail: [output('\r\nnext')], expectedEquivalent: true },
  { name: 'utf8-mid-codepoint', prefix: [output(Buffer.from([0xd0]))], tail: [output(Buffer.from([0x96]))], expectedEquivalent: false },
  { name: 'csi-mid-parameter', prefix: [output('before\x1b[38;2;255;')], tail: [output('32;0mRED')], expectedEquivalent: false },
  { name: 'osc-mid-payload', prefix: [output('\x1b]8;;https://example.invalid')], tail: [output('\x1b\\LINK\x1b]8;;\x1b\\')], expectedEquivalent: false },
  { name: 'normal-alt-normal', prefix: [output('NORMAL\x1b[?1049hALT\x1b[31m')], tail: [output('red\x1b[?1049lN')], expectedEquivalent: false },
  { name: 'saved-cursor', prefix: [output('one\x1b7\r\nmore')], tail: [output('\x1b8!')], expectedEquivalent: false },
  { name: 'custom-tabstop', prefix: [output('\x1b[3g\x1b[5G\x1bH\r')], tail: [output('\tX')], expectedEquivalent: false },
  { name: 'scroll-region', prefix: [output('1\r\n2\r\n3\r\n4\x1b[2;4r\x1b[4;1H')], tail: [output('\nTAIL')], expectedEquivalent: false },
  { name: 'modes', prefix: [output('\x1b[?2004h\x1b[?1003h\x1b[?1004h\x1b[?1h\x1b[4h')], tail: [output('X')], expectedEquivalent: true },
  { name: 'unicode-resize', prefix: [output('é 中 👩‍💻 │ ─ Привет'), { kind: 'resize', cols: 42, rows: 10 }], tail: [output('\r\nEND')], expectedEquivalent: true }
];
const results = [];
const digest = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
function differences(left, right, path = '', output = []) {
  if (output.length >= 20) return output;
  if (typeof left !== 'object' || left === null || typeof right !== 'object' || right === null) {
    if (left !== right) output.push({ path, live: left, serializeTail: right });
    return output;
  }
  for (const key of new Set([...Object.keys(left), ...Object.keys(right)])) differences(left[key], right[key], `${path}.${key}`, output);
  return output;
}
for (const fixture of fixtures) {
  test(`full replay and serialize probe: ${fixture.name}`, async () => {
    const live = createModel(40, 8), replay = createModel(40, 8);
    for (const event of fixture.prefix) await apply(live.terminal, event);
    const checkpoint = live.addon.serialize();
    const resumed = createModel(live.terminal.cols, live.terminal.rows);
    await write(resumed.terminal, checkpoint);
    for (const event of fixture.tail) await apply(live.terminal, event);
    for (const event of [...fixture.prefix, ...fixture.tail]) await apply(replay.terminal, event);
    for (const event of fixture.tail) await apply(resumed.terminal, event);
    const liveValue = snapshot(live.terminal), replayValue = snapshot(replay.terminal), restoredValue = snapshot(resumed.terminal);
    assert.deepEqual(replayValue, liveValue, 'full replay must equal live cells, buffers, modes, wraps and cursors');
    const equivalent = JSON.stringify(liveValue) === JSON.stringify(restoredValue);
    results.push({ fixture: fixture.name, fullReplayEquivalent: true, serializeTailEquivalent: equivalent,
      expectation: fixture.expectedEquivalent, checkpointBytes: Buffer.byteLength(checkpoint),
      liveHash: digest(liveValue), replayHash: digest(replayValue), serializeTailHash: digest(restoredValue),
      firstDifferences: differences(liveValue, restoredValue) });
    // Failure of candidate recovery is a successful *diagnostic*, never stack approval.
    assert.equal(equivalent, fixture.expectedEquivalent, 'changed upstream behavior: investigate the recorded model diff');
    live.terminal.dispose(); replay.terminal.dispose(); resumed.terminal.dispose();
  });
}
test.after(() => {
  fs.mkdirSync('evidence', { recursive: true });
  fs.writeFileSync('evidence/checkpoint-comparisons.json', JSON.stringify({
    observedAt: new Date().toISOString(), node: process.version, platform: os.platform(),
    versions: { headless: '6.0.0', serialize: '0.14.0' },
    scope: 'Observable API state plus behavioral continuations; not proof of full hidden state.', results
  }, null, 2) + '\n');
});
test('bounded short corpus replay timing (diagnostic only)', async () => {
  const model = createModel(120, 36);
  const event = output('Привет 中 é ─ row\r\n'.repeat(10_000));
  const start = performance.now();
  await apply(model.terminal, event);
  const elapsedMs = performance.now() - start;
  fs.mkdirSync('evidence', { recursive: true });
  fs.writeFileSync('evidence/short-replay.json', JSON.stringify({ observedAt: new Date().toISOString(),
    elapsedMs, bytes: Buffer.from(event.bytes, 'base64').length, rssBytes: process.memoryUsage().rss,
    node: process.version, cpu: os.cpus()[0]?.model, logicalCPUs: os.cpus().length,
    releaseGate: false, note: 'One Node host parse, no fsync/IPC/GPU; not SPEC performance acceptance.' }, null, 2) + '\n');
  model.terminal.dispose();
});
