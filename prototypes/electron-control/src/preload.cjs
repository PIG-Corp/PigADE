const { contextBridge, ipcRenderer } = require('electron');
contextBridge.exposeInMainWorld('pigade', {
  connect: () => ipcRenderer.send('host:connect'),
  command: command => ipcRenderer.send('host:command', command),
  onEvent: callback => ipcRenderer.on('host:event', (_event, value) => callback(value))
});
