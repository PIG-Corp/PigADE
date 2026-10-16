// Invoked explicitly with Electron; excluded from Node's *.test test discovery.
const {app,BrowserWindow}=require('electron');
app.disableHardwareAcceleration(); // Deterministic screenshot smoke, not GPU/performance certification.
app.commandLine.appendSwitch('disable-features','Vulkan');
const {spawn}=require('node:child_process');
const fs=require('node:fs');
const os=require('node:os');
const path=require('node:path');
const {performance}=require('node:perf_hooks');
const root=path.resolve(__dirname,'..');
const directory=fs.mkdtempSync(path.join(os.tmpdir(),'pigade-electron-ui-'));
process.env.PIGADE_CONTROL_STATE=directory;
const child=spawn(process.env.PIGADE_NODE_EXECUTABLE||'node',[path.join(root,'src/host.mjs')],{env:process.env,stdio:['ignore','pipe','pipe']});
let hostError='';child.stderr.on('data',chunk=>hostError+=chunk.toString());
const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function until(predicate,label){for(let i=0;i<300;i++){if(await predicate())return;await sleep(25);}throw Error(`Timeout: ${label}; ${hostError}`);}
let win;
const watchdog=setTimeout(()=>{console.error('Electron smoke watchdog');child.kill('SIGTERM');app.exit(1);},25000);
async function window(){
  win=new BrowserWindow({width:1280,height:800,show:true,webPreferences:{preload:path.join(root,'src/preload.cjs'),contextIsolation:true,nodeIntegration:false,sandbox:true}});
  const net=require('node:net');let socket;
  const {ipcMain}=require('electron');
  ipcMain.removeAllListeners('host:connect');ipcMain.removeAllListeners('host:command');
  ipcMain.on('host:connect',()=>{
    socket=net.createConnection(path.join(directory,'host.sock'));socket.setEncoding('utf8');let pending='';
    socket.on('connect',()=>win.webContents.send('host:event',{kind:'connected'}));
    socket.on('data',bytes=>{pending+=bytes.toString();let i;while((i=pending.indexOf('\n'))!==-1){win.webContents.send('host:event',JSON.parse(pending.slice(0,i)));pending=pending.slice(i+1);}});
    socket.on('error',error=>win.webContents.send('host:event',{kind:'error',error:String(error)}));
  });
  ipcMain.on('host:command',(_event,command)=>socket.write(JSON.stringify({...command,protocol:1})+'\n'));
  win.on('closed',()=>socket?.destroy());
  win.webContents.on('console-message',(_event,level,message)=>console.log(`renderer: ${level}: ${message}`));
  await win.loadFile(path.join(root,'src/index.html'));
  await until(()=>win.webContents.executeJavaScript(`document.querySelector('#connection').textContent.includes('подключён')`),'UI host connection');
}
app.whenReady().then(async()=>{
  try{
    await until(()=>fs.existsSync(path.join(directory,'host.sock')),'host socket');
    const start=performance.now();await window();
    for(const id of ['shell','python','shell','python']) await win.webContents.executeJavaScript(`document.querySelector('#${id}').click()`);
    await until(()=>win.webContents.executeJavaScript(`document.querySelectorAll('.pane').length===4&&[...document.querySelectorAll('.pane h2')].every(e=>e.textContent.includes('Live'))`),'four live panels');
    await sleep(400);
    const identity=await win.webContents.executeJavaScript(`[...panels.values()].map(p=>({session:p.value.session,generation:p.value.generation,pid:p.value.pid}))`);
    fs.mkdirSync(path.join(root,'evidence'),{recursive:true});
    fs.writeFileSync(path.join(root,'evidence/ui-dark.png'),(await win.webContents.capturePage()).toPNG());
    await win.webContents.executeJavaScript(`document.querySelector('#theme').click()`);
    await sleep(50);fs.writeFileSync(path.join(root,'evidence/ui-light.png'),(await win.webContents.capturePage()).toPNG());
    win.destroy();await window();
    await until(()=>win.webContents.executeJavaScript(`document.querySelectorAll('.pane').length===4`),'four reattached panels');
    const reattached=await win.webContents.executeJavaScript(`[...panels.values()].map(p=>({session:p.value.session,generation:p.value.generation,pid:p.value.pid}))`);
    if(JSON.stringify(identity)!==JSON.stringify(reattached))throw Error('UI reopen changed process identity');
    const metrics=app.getAppMetrics().map(({pid,type,memory})=>({pid,type,memory}));
    const result={observedAt:new Date().toISOString(),electron:process.versions.electron,chrome:process.versions.chrome,node:process.versions.node,
      platform:process.platform,sessionType:process.env.XDG_SESSION_TYPE,display:process.env.DISPLAY,waylandDisplay:process.env.WAYLAND_DISPLAY,
      uiCreationAndFourLaunchMs:performance.now()-start,uiCloseReopenSameIdentities:true,panels:identity.length,
      metrics,caveat:'Software-rendered window smoke/screenshot on current session; no IME, accessibility, present latency or platform certification.'};
    fs.writeFileSync(path.join(root,'evidence/ui-smoke.json'),JSON.stringify(result,null,2)+'\n');
    console.log(JSON.stringify(result));win.destroy();
  }catch(error){console.error(error);process.exitCode=1;}
  finally{clearTimeout(watchdog);child.kill('SIGTERM');await new Promise(resolve=>child.once('exit',resolve));fs.rmSync(directory,{recursive:true,force:true});app.exit(process.exitCode||0);}
});
// The smoke intentionally destroys and recreates its only window.
app.on('window-all-closed',()=>{});
