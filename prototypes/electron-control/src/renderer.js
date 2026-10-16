/* Cell replica only. Deliberately no xterm/ANSI parser in the renderer. */
const panels = new Map();
let connected = false;
const terminalFont = '14px monospace';
const metrics = document.createElement('canvas').getContext('2d');
metrics.font = terminalFont;
const cellWidth = Math.ceil(metrics.measureText('M').width * devicePixelRatio) / devicePixelRatio;
const cellHeight = 19;
document.documentElement.style.setProperty('--cell-width', `${cellWidth}px`);
const connection = document.querySelector('#connection');
const command = data => window.pigade.command(data);
const send = (panel, data) => command({ session: panel.value.session, generation: panel.value.generation, ...data });
const ansi = ['#1d2025','#e06c75','#98c379','#e5c07b','#61afef','#c678dd','#56b6c2','#d7dae0','#5c6370','#e06c75','#98c379','#e5c07b','#61afef','#c678dd','#56b6c2','#ffffff'];
function color(mode, value, fallback) {
  if (mode === 0) return fallback;
  if (mode === 0x3000000) return `#${value.toString(16).padStart(6,'0')}`;
  if (value < 16) return ansi[value];
  if (value >= 232) { const n = 8 + (value - 232) * 10; return `rgb(${n},${n},${n})`; }
  const n = value - 16, cube = [0,95,135,175,215,255];
  return `rgb(${cube[Math.floor(n / 36)]},${cube[Math.floor(n / 6) % 6]},${cube[n % 6]})`;
}
function render(panel) {
  const value = panel.value;
  panel.title.textContent = `${value.session.slice(0,8)} · ${value.status === 'running' ? 'Live' : value.status} · ${value.frame.cols}×${value.frame.rows} · durable ${value.durableSeq}`;
  const fragment = document.createDocumentFragment();
  value.frame.lines.forEach((line,y) => {
    const row = document.createElement('div'); row.className = 'line';
    line.cells.forEach((cell,x) => {
      if (!cell.width) return;
      const span = document.createElement('span'); span.className = 'cell';
      span.textContent = cell.invisible ? ' ' : cell.chars || ' ';
      span.style.width = `${cell.width * cellWidth}px`;
      let fg = color(cell.fgMode, cell.fg, 'var(--text)'), bg = color(cell.bgMode, cell.bg, 'var(--terminal)');
      if (cell.inverse) [fg,bg] = [bg,fg];
      span.style.color = fg; span.style.backgroundColor = bg;
      if (cell.bold) span.style.fontWeight = 'bold';
      if (cell.italic) span.style.fontStyle = 'italic';
      if (cell.dim) span.style.opacity = '.65';
      if (cell.underline || cell.strike) span.style.textDecoration = `${cell.underline ? 'underline' : ''} ${cell.strike ? 'line-through' : ''}`;
      if (x === value.frame.cursorX && y === value.frame.cursorY) span.classList.add('cursor');
      row.append(span);
    });
    fragment.append(row);
  });
  panel.grid.replaceChildren(fragment);
  panel.input.disabled = !connected || value.status !== 'running';
}
function resize(panel) {
  if (!connected || panel.value.status !== 'running') return;
  const cols = Math.min(500, Math.max(2, Math.floor((panel.viewport.clientWidth - 16) / cellWidth)));
  const rows = Math.min(200, Math.max(2, Math.floor((panel.viewport.clientHeight - 16) / cellHeight)));
  if (cols !== panel.value.frame.cols || rows !== panel.value.frame.rows) send(panel, { kind:'resize', cols, rows });
}
function update(value) {
  let panel = panels.get(value.session);
  if (panel && panel.value.generation !== value.generation) return;
  if (!panel) {
    const element = document.createElement('section'); element.className = 'pane';
    const title = document.createElement('h2');
    const viewport = document.createElement('div'); viewport.className='viewport';
    const grid = document.createElement('pre'); grid.className='grid'; grid.setAttribute('aria-label','Текст терминала');
    const input = document.createElement('textarea'); input.className='input'; input.setAttribute('aria-label',`Ввод терминала ${value.session.slice(0,8)}`);
    viewport.append(grid,input); element.append(title,viewport); document.querySelector('#panes').append(element);
    panel = { element,title,viewport,grid,input,value }; panels.set(value.session,panel);
    viewport.addEventListener('click', () => { if (!getSelection().toString()) input.focus(); });
    input.addEventListener('input', event => { if (event.isComposing) return; if(input.value) send(panel,{kind:'input',bytes:input.value}); input.value=''; });
    input.addEventListener('compositionend', () => { if(input.value) send(panel,{kind:'input',bytes:input.value}); input.value=''; });
    input.addEventListener('keydown', event => {
      if (event.isComposing) return;
      const app = panel.value.frame.modes.applicationCursorKeysMode;
      const keys = { Enter:'\r', Backspace:'\x7f', Tab:'\t', Escape:'\x1b',
        ArrowUp:app?'\x1bOA':'\x1b[A', ArrowDown:app?'\x1bOB':'\x1b[B', ArrowRight:app?'\x1bOC':'\x1b[C', ArrowLeft:app?'\x1bOD':'\x1b[D', Delete:'\x1b[3~', Home:'\x1b[H',End:'\x1b[F' };
      let bytes = keys[event.key];
      if(event.ctrlKey && !event.shiftKey && /^[a-z]$/i.test(event.key)) bytes=String.fromCharCode(event.key.toUpperCase().charCodeAt(0)-64);
      if(bytes) { event.preventDefault(); send(panel,{kind:'input',bytes}); }
    });
    input.addEventListener('paste', event => {
      event.preventDefault(); const text = event.clipboardData.getData('text/plain');
      send(panel,{kind:'input',bytes:panel.value.frame.modes.bracketedPasteMode?`\x1b[200~${text}\x1b[201~`:text});
    });
    const observer = new ResizeObserver(() => { clearTimeout(panel.resizeTimer); panel.resizeTimer=setTimeout(()=>resize(panel),33); });
    observer.observe(viewport);
    send(panel,{kind:'subscribe'});
  }
  if (value.seq < panel.value.seq) return;
  panel.value=value; render(panel); resize(panel);
}
window.pigade.onEvent(event => {
  if (event.kind==='connected') { connected=true; connection.textContent='Host подключён'; command({kind:'list'}); }
  if (event.kind==='disconnected') { connected=false; connection.textContent='Host отключён · ввод заблокирован'; panels.forEach(render); }
  if (event.kind==='error') connection.textContent=event.error;
  if (event.kind==='list') event.sessions.forEach(update);
  if (event.kind==='frame'||event.kind==='started') update(event);
});
document.querySelector('#shell').onclick=()=>command({kind:'start',executable:'/bin/bash',argv:['--noprofile','--norc']});
document.querySelector('#python').onclick=()=>command({kind:'start',executable:'/usr/bin/python3',argv:['-q']});
document.querySelector('#reconnect').onclick=()=>window.pigade.connect();
document.querySelector('#theme').onclick=()=>document.documentElement.classList.toggle('light');
window.pigade.connect();
