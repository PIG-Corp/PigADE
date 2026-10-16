import fs from 'node:fs';
import pathModule from 'node:path';
import { createHash } from 'node:crypto';
export function checksum(record) {
  return createHash('sha256').update(JSON.stringify(record)).digest('hex');
}
export class Journal {
  constructor(path, identity) {
    this.identity = identity;
    this.seq = 0;
    this.fd = fs.openSync(path, 'ax', 0o600);
    const parent = fs.openSync(pathModule.dirname(path), 'r');
    fs.fsyncSync(parent); fs.closeSync(parent);
  }
  append(event) {
    const record = { ...this.identity, seq: this.seq + 1, event };
    const line = JSON.stringify({ record, checksum: checksum(record) }) + '\n';
    fs.writeFileSync(this.fd, line);
    fs.fsyncSync(this.fd); // Deliberately simple reliable mode; no latency claim.
    this.seq = record.seq;
    return record;
  }
  close() { fs.closeSync(this.fd); }
}
export function readJournal(path) {
  const data = fs.readFileSync(path, 'utf8');
  const terminated = data.endsWith('\n');
  const lines = data.split('\n');
  lines.pop(); // An unterminated tail is isolated, never parsed as committed.
  let identity;
  const records = [];
  for (const line of lines) {
    const { record, checksum: hash } = JSON.parse(line);
    if (checksum(record) !== hash || record.seq !== records.length + 1) throw new Error('Invalid journal checksum/sequence');
    identity ??= { session: record.session, generation: record.generation };
    if (record.session !== identity.session || record.generation !== identity.generation) throw new Error('Mixed generation');
    records.push(record);
  }
  return { records, isolatedTail: !terminated };
}
