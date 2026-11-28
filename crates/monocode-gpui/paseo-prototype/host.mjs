import {createHostServer} from '../../../host/server.ts';
import {createClient,createRoutedClient,createDirectHostClient,readConfig,readLocalServerId,paseoHome} from './sdk.mjs';
import {PaseoHost} from './host-adapter.mjs';
import {HostRegistry,savedHostsReader} from './hosts.mjs';
import {projectTimelineRows} from 'paseo-source/packages/server/src/server/agent/timeline-projection.ts';
import {randomBytes,timingSafeEqual} from 'node:crypto';
import {readFileSync,writeFileSync,mkdirSync} from 'node:fs';
import {join} from 'node:path';

const directory=process.argv[2];
if(!directory)throw Error('An isolated prototype directory is required');
mkdirSync(directory,{recursive:true,mode:0o700});
const tokenPath=join(directory,'paseo-host-token');
let token;
try{token=readFileSync(tokenPath,'utf8');}catch(e){if(e.code!=='ENOENT')throw e;token=randomBytes(32).toString('base64url');writeFileSync(tokenPath,token,{flag:'wx',mode:0o600});}
if(!/^[A-Za-z0-9_-]{43}$/.test(token))throw Error('Invalid isolated host token');
const client=createClient();
const config=readConfig();
const recentDays=config.directoryRecentDays; // See README: absent -> remote 7 days, local full scan.
// Known before the daemon's hello, so the sidecar can serve (stale local lists + direct remote hosts) while it is down.
const localServerId=config.serverId||readLocalServerId();
const OFFLINE_START_MS=5000;
const projectTimeline=rows=>projectTimelineRows({rows,mode:'projected'});
const registry=new HostRegistry({
  originClient:client,
  baseDirectory:directory,
  token,
  createAdapter:(routedClient,hostDir,options)=>new PaseoHost(routedClient,hostDir,projectTimeline,{...options,recentDays}),
  createRoutedClient,
  createDirectClient:createDirectHostClient,
  readSavedHosts:savedHostsReader(join(paseoHome(),'hosts.json')),
  transport:config.remoteHostTransport,
  localServerId,
  createServer:(store,adapter)=>createHostServer({store},[],undefined,adapter),
});
const host=new PaseoHost(client,directory,projectTimeline,{hostRegistry:registry,recentDays,persistDirectory:true,...(localServerId?{expectedServerId:localServerId}:{})});
registry.primaryHost=host;
host.allowLiveCheck=process.argv.includes('--live-create');
const store={get environmentId(){return host.serverId;},
  authenticated(value){return value.length===token.length && timingSafeEqual(Buffer.from(value),Buffer.from(token));},
  projects:()=>host.fileProjects(),
  project:id=>host.fileProject(id)};
// Auth, body limits, filesystem containment, Git and WorkspaceCommands stay original.
const server=createHostServer({store},[],undefined,host);
let first=true,started=false;
// Bind and announce once: after the first local directory, or offline (local daemon down) when the identity is known.
const start=()=>{
  if(started)return;started=true;
  server.once('error',()=>{console.error('Paseo host loopback unavailable');process.exit(1);});
  server.listen(0,'127.0.0.1',()=>{
    process.stdout.write(JSON.stringify({endpoint:`http://127.0.0.1:${server.address().port}`,token,serverId:host.serverId,
      version:client.getLastServerInfoMessage()?.version??null})+'\n');
    // Pre-warm model catalogs in the background after startup so opening the model picker is instant.
    host.prewarmCatalog().catch(()=>{});
  });
};
client.subscribeConnectionStatus(state=>{
  host.connected=state.status==='connected';host.invalidate();
  if(host.connected && first){
    first=false;
    const expected=config.serverId;if(expected&&expected!==client.getLastServerInfoMessage()?.serverId){console.error('Paseo identity mismatch');process.exit(1);}
    host.directory(true).then(start,()=>{if(started)return;if(host.serverId)start();else{console.error('Cannot discover Paseo projects');process.exit(1);}});
  }
});
client.subscribeRawMessages(message=>{if(message.type==='agent_status')host.applyAgentStatus(message);});
const stop=()=>{registry.close();host.close();server.close();process.exit(0);};
process.on('SIGTERM',stop);process.on('SIGINT',stop);
// Parent death releases client resources, never remote agents.
process.stdin.resume();process.stdin.on('end',stop);
// Local daemon down: serve offline (the client keeps reconnecting with backoff) instead of exiting.
setTimeout(()=>{if(first&&!started){if(host.serverId)start();else{console.error('Paseo connection timed out');process.exit(1);}}},host.serverId?OFFLINE_START_MS:20000).unref();
client.connect().catch(()=>{if(!host.serverId){console.error('Cannot connect to existing Paseo daemon');process.exit(1);}});
