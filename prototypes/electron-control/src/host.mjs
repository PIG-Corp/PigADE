import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { randomUUID } from 'node:crypto';
import pty from 'node-pty';
import { pathToFileURL } from 'node:url';
import { createModel, apply, frame, output } from './terminal.mjs';
import { Journal, readJournal } from './journal.mjs';

export const DEFAULT_STATE = path.join(process.env.XDG_STATE_HOME || path.join(os.homedir(), '.local/state'), 'pigade-electron-control');
export const MAX_IPC = 1024 * 1024;
export const MAX_WRITE = 4 * 1024 * 1024;
export class Session {
  constructor(directory, executable, argv = [], cols = 80, rows = 24) {
    this.session = randomUUID(); this.generation = randomUUID();
    this.model = createModel(cols, rows); this.status = 'starting'; this.pending = 0; this.queuedOperations = 0;
    this.listeners = new Set(); this.chain = Promise.resolve();
    this.journal = new Journal(path.join(directory, `${this.session}.jsonl`),
      { session: this.session, generation: this.generation });
    // A bounded control prototype, not the production durable launch protocol.
    this.journal.append({ kind: 'initial', cols, rows, executable, argv });
    this.process = pty.spawn(executable, argv, { name: 'xterm-256color', cols, rows,
      encoding: null, cwd: process.cwd(), env: { ...process.env, TERM: 'xterm-256color', COLORTERM: 'truecolor' } });
    this.status = 'running';
    this.model.terminal.onData(bytes => { if (this.status === 'running') this.process.write(bytes); });
    this.process.onData(bytes => {
      this.process.pause();
      const event = output(bytes);
      this.pending += bytes.length;
      this.queue(async () => {
        this.journal.append(event); // fsync completes before parse and publication.
        await apply(this.model.terminal, event);
        this.pending -= bytes.length;
        this.publish();
        if (this.status === 'running' && this.pending === 0) this.process.resume();
      });
    });
    this.exitPromise = new Promise(resolve => this.process.onExit(({ exitCode, signal }) => {
      this.queue(async () => {
        this.journal.append({ kind: 'exit', exitCode, signal });
        this.status = 'exited'; this.publish();
      }).then(resolve);
    }));
  }
  queue(operation) {
    if (this.queuedOperations >= 1024) throw new Error('Per-session operation queue is full');
    this.queuedOperations++;
    this.chain = this.chain.then(operation).catch(error => {
      this.status = 'storage-error'; this.error = String(error); this.process.pause(); this.publish();
    }).finally(() => { this.queuedOperations--; });
    return this.chain;
  }
  resize(cols, rows) {
    if (!Number.isInteger(cols) || !Number.isInteger(rows) || cols < 2 || rows < 2 || cols > 500 || rows > 200) throw new Error('Invalid geometry');
    return this.queue(async () => {
      if (this.model.terminal.cols === cols && this.model.terminal.rows === rows) return;
      const event = { kind: 'resize', cols, rows };
      // Ordered with output. The PTY call is synchronous. This comparative prototype
      // has no crash-atomic ResizeApplied protocol; see README limitations.
      this.process.resize(cols, rows);
      this.journal.append(event);
      await apply(this.model.terminal, event); this.publish();
    });
  }
  input(bytes) { if (this.status === 'running') this.process.write(bytes); }
  getFrame() {
    return { protocol: 1, session: this.session, generation: this.generation,
      seq: this.journal.seq, durableSeq: this.journal.seq, status: this.status,
      pid: this.process.pid, error: this.error, frame: frame(this.model.terminal) };
  }
  publish() { for (const listener of this.listeners) listener(this.getFrame()); }
  async close() {
    if (this.status === 'running' || this.status === 'storage-error') this.process.kill();
    await this.exitPromise; await this.chain; this.journal.close(); this.model.terminal.dispose();
  }
}
export async function restore(pathname) {
  const { records, isolatedTail } = readJournal(pathname);
  const initial = records[0]?.event;
  if (!initial || initial.kind !== 'initial') throw new Error('Missing initial geometry');
  const model = createModel(initial.cols, initial.rows);
  // No output handler is connected here: queries/bell/OSC have no external effects.
  for (const { event } of records.slice(1)) if (['output', 'resize'].includes(event.kind)) await apply(model.terminal, event);
  return { model, records, isolatedTail };
}
export async function startHost(directory = DEFAULT_STATE) {
  fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
  fs.chmodSync(directory, 0o700);
  const socketPath = path.join(directory, 'host.sock');
  const sessions = new Map(), clients = new Set();
  // Refuse an existing endpoint; do not unlink a possible live host.
  if (fs.existsSync(socketPath)) throw new Error(`Endpoint exists: ${socketPath}. Inspect the previous host before removing a stale socket.`);
  const server = net.createServer(socket => {
    socket.setEncoding('utf8');
    clients.add(socket); let incoming = '';
    const send = value => {
      if (socket.destroyed) return;
      // Render notifications may be omitted when the UI is slow. The next
      // request always returns a full authoritative frame, never a delta.
      const encoded = JSON.stringify(value) + '\n';
      if (socket.writableLength + Buffer.byteLength(encoded) > MAX_WRITE) { socket.destroy(); return; }
      socket.write(encoded);
    };
    const subscriptions = new Map();
    socket.on('data', chunk => {
      incoming += chunk.toString('utf8');
      if (Buffer.byteLength(incoming) > MAX_IPC) { socket.destroy(); return; }
      let index;
      while ((index = incoming.indexOf('\n')) !== -1) {
        const raw = incoming.slice(0, index); incoming = incoming.slice(index + 1);
        try {
          const command = JSON.parse(raw);
          if (command.protocol !== 1) throw new Error('Protocol mismatch');
          if (command.kind === 'list') send({ kind: 'list', sessions: [...sessions.values()].map(s => s.getFrame()) });
          else if (command.kind === 'start') {
            if (sessions.size >= 4) throw new Error('Four-session prototype limit');
            if (typeof command.executable !== 'string' || !Array.isArray(command.argv) || command.argv.some(v => typeof v !== 'string')) throw new Error('Invalid argv');
            const session = new Session(directory, command.executable, command.argv);
            sessions.set(session.session, session); send({ kind: 'started', ...session.getFrame() });
          } else {
            const session = sessions.get(command.session);
            if (!session || session.generation !== command.generation) throw new Error('Unknown session or stale generation');
            if (command.kind === 'subscribe') {
              if (!subscriptions.has(session.session)) {
                const notify = data => send({ kind: 'frame', ...data });
                subscriptions.set(session.session, notify); session.listeners.add(notify);
              }
              send({ kind: 'frame', ...session.getFrame() });
            } else if (command.kind === 'input' && typeof command.bytes === 'string' && command.bytes.length < 64 * 1024) session.input(command.bytes);
            else if (command.kind === 'resize') session.resize(command.cols, command.rows);
            else throw new Error('Unknown command');
          }
        } catch (error) { send({ kind: 'error', error: String(error) }); }
      }
    });
    socket.on('error', () => {});
    socket.on('close', () => { clients.delete(socket); for (const [id, listener] of subscriptions) sessions.get(id)?.listeners.delete(listener); });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, resolve); });
  fs.chmodSync(socketPath, 0o600);
  return { server, sessions, socketPath, async close() {
    for (const client of clients) client.destroy();
    await Promise.all([...sessions.values()].map(session => session.close()));
    await new Promise(resolve => server.close(resolve));
  } };
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const host = await startHost(process.env.PIGADE_CONTROL_STATE || DEFAULT_STATE);
  console.log(`Control host ${process.pid}: ${host.socketPath}`);
  let closing = false;
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, async () => {
    if (!closing) { closing = true; await host.close(); process.exit(0); }
  });
}
