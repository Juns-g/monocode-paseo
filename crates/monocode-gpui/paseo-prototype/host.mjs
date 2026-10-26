import {createHostServer} from '../../../host/server.ts';
import {createClient,readConfig} from './sdk.mjs';
import {PaseoHost} from './host-adapter.mjs';
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
const host=new PaseoHost(client,directory,rows=>projectTimelineRows({rows,mode:'projected'}));
host.allowLiveCheck=process.argv.includes('--live-create');
const store={get environmentId(){return host.serverId;},
  authenticated(value){return value.length===token.length && timingSafeEqual(Buffer.from(value),Buffer.from(token));},
  projects:()=>host.fileProjects(),
  project:id=>host.fileProject(id)};
// Auth, body limits, filesystem containment, Git and WorkspaceCommands stay original.
const server=createHostServer({store},[],undefined,host);
let first=true;
client.subscribeConnectionStatus(state=>{
  host.connected=state.status==='connected';host.invalidate();
  if(host.connected && first){
    first=false;
    const expected=readConfig().serverId;if(expected&&expected!==client.getLastServerInfoMessage()?.serverId){console.error('Paseo identity mismatch');process.exit(1);}
    host.directory(true).then(()=>{
      server.once('error',()=>{console.error('Paseo host loopback unavailable');process.exit(1);});
      server.listen(0,'127.0.0.1',()=>{
        process.stdout.write(JSON.stringify({endpoint:`http://127.0.0.1:${server.address().port}`,token,serverId:host.serverId,
          version:client.getLastServerInfoMessage()?.version})+'\n');
      });
    }).catch(()=>{console.error('Cannot discover Paseo projects');process.exit(1);});
  }
});
client.subscribeRawMessages(message=>{if(message.type==='agent_status')host.invalidate();});
const stop=()=>{host.close();server.close();process.exit(0);};
process.on('SIGTERM',stop);process.on('SIGINT',stop);
// Parent death releases client resources, never remote agents.
process.stdin.resume();process.stdin.on('end',stop);
setTimeout(()=>{if(first){console.error('Paseo connection timed out');process.exit(1);}},20000).unref();
client.connect().catch(()=>{console.error('Cannot connect to existing Paseo daemon');process.exit(1);});
