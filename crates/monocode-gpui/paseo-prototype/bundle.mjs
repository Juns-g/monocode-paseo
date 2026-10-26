import {createRequire} from 'node:module';
import {existsSync,realpathSync,writeFileSync,mkdirSync} from 'node:fs';
import {resolve,dirname,join} from 'node:path';
import {fileURLToPath} from 'node:url';
const here=dirname(fileURLToPath(import.meta.url));
const paseo=process.env.PASEO_SOURCE;if(!paseo)throw Error('Set PASEO_SOURCE to a local Paseo source checkout with its dependencies installed');
const require=createRequire(join(paseo,'package.json'));
const esbuild=require('esbuild');
const output=resolve(process.argv[2]);mkdirSync(output,{recursive:true});
const plugins=[{name:'native-only-catalog-imports',setup(build){
 build.onResolve({filter:/^paseo-source\//},args=>({path:join(paseo,args.path.slice('paseo-source/'.length))}));
 build.onResolve({filter:/^@tauri-apps\//},args=>({path:args.path,namespace:'native-only'}));
 build.onResolve({filter:/^react$/},args=>({path:args.path,namespace:'native-only'}));
 build.onLoad({filter:/.*/,namespace:'native-only'},()=>({contents:'const unavailable=()=>{throw Error("Provider settings are managed by Paseo");}; export const invoke=unavailable,listen=unavailable,open=unavailable,useEffect=unavailable,useState=unavailable;'}));
}},{name:'current-paseo-source',setup(build){
build.onResolve({filter:/^@getpaseo\/protocol\/validation\/ws-outbound$/},()=>({path:join(here,'validate.mjs')}));
  build.onResolve({filter:/daemon-client-relay-e2ee-transport/},()=>({path:join(here,'no-relay.mjs')}));
  build.onResolve({filter:/^@getpaseo\/(protocol|relay)\//},args=>{
   const match=args.path.match(/^@getpaseo\/(protocol|relay)\/(.+)$/);
   const base=join(paseo,'packages',match[1],'src',match[2]);
   for(const path of [base+'.ts',join(base,'index.ts')])if(existsSync(path))return {path};
  });
}}];
for(const [entry,name] of (process.env.PASEO_ADAPTER_TEST ? [['host-adapter.test.mjs','adapter.test.mjs']] : [['bridge.mjs','paseo-bridge.mjs'],['host.mjs','paseo-host.mjs']])) {
 await esbuild.build({entryPoints:[join(here,entry)],outfile:join(output,name),bundle:true,platform:'node',format:'esm',external:['bufferutil','utf-8-validate'],banner:{js:'import { createRequire } from "node:module"; const require = createRequire(import.meta.url);'},plugins});
}
writeFileSync(join(output,'node-path.txt'),realpathSync(process.execPath)+'\n');
writeFileSync(join(output,'paseo-home.txt'),'');
