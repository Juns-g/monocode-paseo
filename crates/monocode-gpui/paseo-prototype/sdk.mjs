import {homedir} from 'node:os';
import {join,dirname} from 'node:path';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {randomUUID} from 'node:crypto';
import {DaemonClient} from 'paseo-source/packages/client/src/daemon-client.ts';
import {createWebSocketTransportFactory} from 'paseo-source/packages/client/src/daemon-client-websocket-transport.ts';
import {CLIENT_CAPS} from 'paseo-source/packages/protocol/src/client-capabilities.ts';
import {DEFAULT_CLIENT_CAPABILITIES} from 'paseo-source/packages/client/src/connection/index.ts';
import WebSocket from 'paseo-source/node_modules/ws/wrapper.mjs';
import {readLocalCredentialForTarget} from 'paseo-source/packages/server/src/server/local-credential.ts';

export function readConfig(){
 const path=process.env.PASEO_PROTOTYPE_CONFIG||join(homedir(),'Library/Application Support/Monocode Paseo Prototype/prototype.json');
 try{return JSON.parse(readFileSync(path,'utf8'));}catch(error){if(error.code==='ENOENT')return {};throw error;}
}
const config=readConfig();
const endpoint=config.endpoint||'127.0.0.1:6973';
const url=new URL('ws://'+endpoint+'/ws');
if(!['127.0.0.1','localhost','[::1]'].includes(url.hostname))throw Error('Prototype requires a loopback Paseo daemon');
export function createClient() {
return new DaemonClient({url:url.href,
  transportFactory:createWebSocketTransportFactory((url,options)=>new WebSocket(url,options?.protocols,{headers:options?.headers})),
  clientId:`monocode-paseo-prototype-${randomUUID()}`,clientType:'cli',appVersion:'0.2.6',
  localCredential:()=>{
    const home=config.paseoHome||process.env.PASEO_HOME||readFileSync(join(dirname(fileURLToPath(import.meta.url)),'paseo-home.txt'),'utf8').trim()||join(homedir(),'.paseo');
    // Same verified local port, including Desktop's wildcard listener. Never sent remotely.
    return readLocalCredentialForTarget(home,endpoint)||readLocalCredentialForTarget(home,'0.0.0.0:'+url.port)||undefined;
  },
  reconnect:{enabled:true,baseDelayMs:500,maxDelayMs:5000},connectTimeoutMs:10000,
  capabilities:{...Object.fromEntries(Object.keys(DEFAULT_CLIENT_CAPABILITIES).map(key=>[key,false])),[CLIENT_CAPS.ownedSubscriptions]:true,[CLIENT_CAPS.allProviders]:true,[CLIENT_CAPS.explicitEventSubscriptions]:true,[CLIENT_CAPS.selectiveAgentTimeline]:true,[CLIENT_CAPS.reasoningMergeEnum]:true,[CLIENT_CAPS.timelineNotifications]:true,[CLIENT_CAPS.timelineReplacementInvalidation]:true},
  logger:{debug(){},info(){},warn(){},error(){},trace(){},child(){return this;}},
});
}
