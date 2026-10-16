import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { performance } from 'node:perf_hooks';
import { Session, restore, startHost } from '../src/host.mjs';
import { snapshot } from '../src/terminal.mjs';
import { readJournal } from '../src/journal.mjs';
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate, label, timeout = 6000) {
  const start = performance.now();
  while (!predicate()) { if(performance.now()-start>timeout) throw new Error(`Timeout: ${label}`); await sleep(10); }
}
function text(session) {
  return session.getFrame().frame.lines.map(line => line.cells.map(cell => cell.chars).join('')).join('\n');
}
test('real shell PTY + 1000 ordered resizes + replay retain complete observable model', async () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'pigade-control-'));
  const session = new Session(directory, '/bin/bash', ['--noprofile','--norc'],80,24);
  try {
    await until(() => text(session).includes('bash-'), 'shell prompt');
    const start=performance.now();
    for(let i=0;i<1000;i++) await session.resize(80+i%3,24+i%2);
    session.input("printf '\\033[31mPTY_OK Привет 中 é\\033[0m\\n'\r");
    await until(() => text(session).split('\n').some(line=>line.startsWith('PTY_OK Привет')), 'PTY echo');
    await session.chain;
    const live=snapshot(session.model.terminal);
    const filename=path.join(directory,`${session.session}.jsonl`);
    const replay=await restore(filename);
    assert.deepEqual(snapshot(replay.model.terminal),live);
    assert.equal(replay.records.filter(r=>r.event.kind==='resize').length,999); // initial 80x24 is a no-op
    assert.equal(replay.isolatedTail,false);
    assert.ok(replay.records.some(r=>r.event.kind==='output'));
    fs.mkdirSync('evidence',{recursive:true});
    fs.writeFileSync('evidence/pty-resize.json',JSON.stringify({observedAt:new Date().toISOString(),
      resizeRequests:1000,appliedResizes:999,fullReplayEquivalent:true,elapsedMs:performance.now()-start,
      totalJournalBytes:fs.statSync(filename).size,node:process.version,
      caveat:'Host/fsync/parse smoke timing, not UI resize latency. Output is a shell fixture, not a certified AI CLI.'},null,2)+'\n');
    replay.model.terminal.dispose();
  } finally { await session.close(); fs.rmSync(directory,{recursive:true,force:true}); }
});
test('second real CLI: Python interactive PTY', async () => {
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'pigade-python-'));
  const session=new Session(directory,'/usr/bin/python3',['-q']);
  try {
    await until(()=>text(session).includes('>>>'),'Python prompt');
    session.input("print('PYTHON_PTY_OK')\r");
    await until(()=>text(session).split('\n').some(line=>line.trim()==='PYTHON_PTY_OK'),'Python result');
    await session.chain;
    const replay=await restore(path.join(directory,`${session.session}.jsonl`));
    assert.deepEqual(snapshot(replay.model.terminal),snapshot(session.model.terminal));
    replay.model.terminal.dispose();
  } finally { await session.close(); fs.rmSync(directory,{recursive:true,force:true}); }
});
test('journal tail isolation, mutation and mixed generations rejected',async()=>{
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'pigade-journal-'));
  const session=new Session(directory,'/bin/sh',['-c','printf JOURNAL_OK']);
  await until(()=>session.status==='exited','short shell exit');
  const filename=path.join(directory,`${session.session}.jsonl`);
  await session.close();
  try {
    const original=fs.readFileSync(filename,'utf8');
    fs.appendFileSync(filename,'{"torn":');
    const recovered=await restore(filename);
    assert.equal(recovered.isolatedTail,true); recovered.model.terminal.dispose();
    fs.writeFileSync(filename,original.replace('JOURNAL','CHANGED'));
    // Mutation of the record bytes (not merely an absent human-readable token).
    const lines=original.trimEnd().split('\n');
    const row=JSON.parse(lines[0]);row.record.generation='corrupted';lines[0]=JSON.stringify(row);
    fs.writeFileSync(filename,lines.join('\n')+'\n');
    assert.throws(()=>readJournal(filename),/checksum/);
  } finally { fs.rmSync(directory,{recursive:true,force:true}); }
});
function client(socketPath) {
  const socket=net.createConnection(socketPath), messages=[];
  socket.setEncoding('utf8');
  let pending='';
  socket.on('data',bytes=>{pending+=bytes.toString();let i;while((i=pending.indexOf('\n'))!==-1){messages.push(JSON.parse(pending.slice(0,i)));pending=pending.slice(i+1);}});
  return { socket,messages,send:value=>socket.write(JSON.stringify({protocol:1,...value})+'\n') };
}
test('four live PTYs survive renderer-client close/reconnect with same identities',async()=>{
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'pigade-reattach-'));
  const host=await startHost(directory);
  let connection=client(host.socketPath);
  try {
    await once(connection.socket,'connect');
    for(let i=0;i<4;i++) connection.send({kind:'start',executable:'/bin/bash',argv:['--noprofile','--norc']});
    await until(()=>connection.messages.filter(m=>m.kind==='started').length===4,'four panels');
    const identities=connection.messages.filter(m=>m.kind==='started').map(({session,generation,pid})=>({session,generation,pid}));
    for(let cycle=0;cycle<10;cycle++) {
      connection.socket.destroy();
      connection=client(host.socketPath); await once(connection.socket,'connect');
      connection.send({kind:'list'});
      await until(()=>connection.messages.some(m=>m.kind==='list'),'manifest after reconnect');
      const actual=connection.messages.find(m=>m.kind==='list').sessions.map(({session,generation,pid})=>({session,generation,pid}));
      assert.deepEqual(actual,identities);
    }
    const identity=identities[0];
    connection.send({kind:'input',...identity,generation:'stale',bytes:'BAD\r'});
    await until(()=>connection.messages.some(m=>m.kind==='error'),'stale generation rejected');
    assert.match(connection.messages.find(m=>m.kind==='error').error,/stale generation/);
  } finally { connection.socket.destroy(); await host.close(); fs.rmSync(directory,{recursive:true,force:true}); }
});
test('separate host process survives a client process kill; SIGKILL host leaves replayable journal',async()=>{
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'pigade-process-'));
  const child=spawn(process.execPath,['src/host.mjs'],{env:{...process.env,PIGADE_CONTROL_STATE:directory},stdio:['ignore','pipe','pipe']});
  let connection;
  try {
    await until(()=>fs.existsSync(path.join(directory,'host.sock')),'host socket');
    connection=client(path.join(directory,'host.sock'));await once(connection.socket,'connect');
    connection.send({kind:'start',executable:'/bin/sh',argv:['-c','printf HOST_RECOVERY_OK; sleep 30']});
    await until(()=>connection.messages.some(m=>m.kind==='started'),'started');
    const started=connection.messages.find(m=>m.kind==='started');
    connection.send({kind:'subscribe',session:started.session,generation:started.generation});
    await until(()=>connection.messages.some(m=>m.kind==='frame'&&m.frame.lines.some(l=>l.cells.map(c=>c.chars).join('').includes('HOST_RECOVERY_OK'))),'durable sentinel');
    const ui=spawn(process.execPath,['-e',`const net=require('node:net');net.createConnection(process.argv[1]);setInterval(()=>{},1000)`,path.join(directory,'host.sock')],{stdio:'ignore'});
    await once(ui,'spawn');ui.kill('SIGKILL');await once(ui,'exit');
    assert.equal(child.exitCode,null);
    connection.socket.destroy();
    child.kill('SIGKILL');await once(child,'exit');
    const replay=await restore(path.join(directory,`${started.session}.jsonl`));
    assert.ok(replay.records.some(r=>r.event.kind==='output'));
    assert.ok(snapshot(replay.model.terminal).normal.lines.some(l=>l.cells.map(c=>c.chars).join('').includes('HOST_RECOVERY_OK')));
    replay.model.terminal.dispose();
    // No recovery spawn is attempted: this prototype cannot resolve launch outcome.
    try { process.kill(started.pid,'SIGTERM'); } catch {}
  } finally { connection?.socket.destroy();if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await once(child,'exit');}fs.rmSync(directory,{recursive:true,force:true}); }
});
