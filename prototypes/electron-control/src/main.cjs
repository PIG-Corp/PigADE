const { app, BrowserWindow, ipcMain } = require('electron');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const state = process.env.PIGADE_CONTROL_STATE || path.join(process.env.XDG_STATE_HOME || path.join(os.homedir(), '.local/state'), 'pigade-electron-control');
let socket;
app.whenReady().then(() => {
  const win = new BrowserWindow({ width: 1280, height: 800, minWidth: 800, minHeight: 600,
    backgroundColor: '#17191D', webPreferences: { preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true, nodeIntegration: false, sandbox: true } });
  win.loadFile(path.join(__dirname, 'index.html'));
  win.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  win.webContents.on('will-navigate', event => event.preventDefault());
  ipcMain.on('host:connect', () => {
    if (socket) socket.destroy();
    socket = net.createConnection(path.join(state, 'host.sock'));
    socket.setEncoding('utf8');
    let pending = '';
    socket.on('connect', () => win.webContents.send('host:event', { kind: 'connected' }));
    socket.on('data', bytes => {
      pending += bytes.toString('utf8');
      if (Buffer.byteLength(pending) > 4 * 1024 * 1024) { socket.destroy(); return; }
      let index;
      while ((index = pending.indexOf('\n')) !== -1) {
        const line = pending.slice(0, index); pending = pending.slice(index + 1);
        try { win.webContents.send('host:event', JSON.parse(line)); } catch { socket.destroy(); }
      }
    });
    socket.on('error', error => win.webContents.send('host:event', { kind: 'error', error: error.message }));
    socket.on('close', () => { if (!win.isDestroyed()) win.webContents.send('host:event', { kind: 'disconnected' }); });
  });
  ipcMain.on('host:command', (_event, command) => {
    if (socket && !socket.destroyed && JSON.stringify(command).length < 1024 * 1024) socket.write(JSON.stringify({ ...command, protocol: 1 }) + '\n');
  });
  win.on('closed', () => { socket?.destroy(); socket = undefined; });
});
app.on('window-all-closed', () => app.quit());
