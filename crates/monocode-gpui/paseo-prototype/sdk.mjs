import {homedir} from 'node:os';
import {join,dirname} from 'node:path';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {randomUUID} from 'node:crypto';
import {DaemonClient} from 'paseo-source/packages/client/src/daemon-client.ts';
import {createWebSocketTransportFactory} from 'paseo-source/packages/client/src/daemon-client-websocket-transport.ts';
import {createHostChannelTransport} from 'paseo-source/packages/client/src/hosts/index.ts';
import {CLIENT_CAPS} from 'paseo-source/packages/protocol/src/client-capabilities.ts';
import {DEFAULT_CLIENT_CAPABILITIES} from 'paseo-source/packages/client/src/connection/index.ts';
import WebSocket from 'paseo-source/node_modules/ws/wrapper.mjs';
import {readLocalCredentialForTarget} from 'paseo-source/packages/server/src/server/local-credential.ts';
import {createHostClient} from 'paseo-source/packages/client/src/node/index.ts';
import {HttpsProxyAgent} from 'paseo-source/node_modules/https-proxy-agent/dist/index.js';

export const PASEO_CLIENT_CAPABILITIES = {
  ...Object.fromEntries(Object.keys(DEFAULT_CLIENT_CAPABILITIES).map(key=>[key,false])),
  [CLIENT_CAPS.ownedSubscriptions]:true,
  [CLIENT_CAPS.allProviders]:true,
  [CLIENT_CAPS.explicitEventSubscriptions]:true,
  [CLIENT_CAPS.selectiveAgentTimeline]:true,
  [CLIENT_CAPS.reasoningMergeEnum]:true,
  [CLIENT_CAPS.timelineNotifications]:true,
  [CLIENT_CAPS.timelineReplacementInvalidation]:true,
};
export const SILENT_LOGGER = {debug(){},info(){},warn(){},error(){},trace(){},child(){return this;}};

export function readConfig(){
 const path=process.env.PASEO_PROTOTYPE_CONFIG||join(homedir(),'Library/Application Support/Monocode Paseo Prototype/prototype.json');
 try{return JSON.parse(readFileSync(path,'utf8'));}catch(error){if(error.code==='ENOENT')return {};throw error;}
}
const config=readConfig();
const endpoint=config.endpoint||'127.0.0.1:6973';
const url=new URL('ws://'+endpoint+'/ws');
if(!['127.0.0.1','localhost','[::1]'].includes(url.hostname))throw Error('Prototype requires a loopback Paseo daemon');
export function paseoHome(){
  return config.paseoHome||process.env.PASEO_HOME||readFileSync(join(dirname(fileURLToPath(import.meta.url)),'paseo-home.txt'),'utf8').trim()||join(homedir(),'.paseo');
}
// The local daemon's persisted identity: lets the sidecar serve before (or without) the daemon's hello.
export function readLocalServerId(){try{return readFileSync(join(paseoHome(),'server-id'),'utf8').trim()||null;}catch{return null;}}
// Same rules as the daemon's webSocketProxyAgent (https_proxy/HTTPS_PROXY, then http_proxy; NO_PROXY plus loopback bypass);
// prototype.json `proxy` overrides the environment for wss/ws alike.
export function proxyAgentFor(address){
  const env=process.env,url=new URL(address),host=url.hostname.toLowerCase(),port=url.port||(url.protocol==='wss:'?'443':'80');
  const noProxy=env.no_proxy??env.NO_PROXY??'';
  const bypass=noProxy.split(/[,\s]+/).includes('*')?['*']:`${noProxy},localhost,127.0.0.1,[::1],::1`.toLowerCase().split(/[,\s]+/);
  if(bypass.some(entry=>{if(entry==='*')return true;const m=entry.match(/^(.+):(\d+)$/);if(m&&Number(m[2])!==Number(port))return false;
    const h=(m?m[1]:entry).replace(/^\*?\./,'');return h!==''&&(host===h||host.endsWith(`.${h}`));}))return undefined;
  const httpProxy=env.http_proxy||env.HTTP_PROXY||'';
  const proxy=typeof config.proxy==='string'&&config.proxy.trim()?config.proxy.trim():url.protocol==='wss:'?(env.https_proxy||env.HTTPS_PROXY||httpProxy):httpProxy;
  if(!proxy)return undefined;
  return new HttpsProxyAgent(proxy.includes('://')?proxy:`http://${proxy}`);
}
// Direct remote host link (relay offer / ws / ssh target) owned by this sidecar; reconnects with backoff on its own.
export function createDirectHostClient({connection}){
  return createHostClient(connection,{
    clientId:`monocode-paseo-direct-${randomUUID()}`,clientType:'cli',appVersion:'0.2.6',
    reconnect:{enabled:true,baseDelayMs:1000,maxDelayMs:30000},connectTimeoutMs:15000,
    capabilities:PASEO_CLIENT_CAPABILITIES,logger:SILENT_LOGGER,webSocketAgent:proxyAgentFor,
  });
}
export function createClient() {
return new DaemonClient({url:url.href,
  transportFactory:createWebSocketTransportFactory((url,options)=>new WebSocket(url,options?.protocols,{headers:options?.headers})),
  clientId:`monocode-paseo-prototype-${randomUUID()}`,clientType:'cli',appVersion:'0.2.6',
  localCredential:()=>{
    const home=paseoHome();
    // Same verified local port, including Desktop's wildcard listener. Never sent remotely.
    return readLocalCredentialForTarget(home,endpoint)||readLocalCredentialForTarget(home,'0.0.0.0:'+url.port)||undefined;
  },
  reconnect:{enabled:true,baseDelayMs:500,maxDelayMs:5000},connectTimeoutMs:10000,
  capabilities:PASEO_CLIENT_CAPABILITIES,
  logger:SILENT_LOGGER,
});
}

export function createRoutedClient({origin,serverId,scopeId,routeId}) {
  return new DaemonClient({
    url:'ws://routed-host/ws',
    clientId:scopeId,
    clientType:'cli',
    appVersion:'0.2.6',
    transportFactory:createHostChannelTransport(origin,serverId,scopeId,routeId),
    reconnect:{enabled:false,baseDelayMs:500,maxDelayMs:5000},
    connectTimeoutMs:15000,
    capabilities:PASEO_CLIENT_CAPABILITIES,
    logger:SILENT_LOGGER,
  });
}
