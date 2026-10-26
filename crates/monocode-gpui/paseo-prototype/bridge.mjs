import {createInterface} from 'node:readline';
import {createClient} from './sdk.mjs';
import {Controller} from './controller.mjs';
import {projectTimelineRows} from 'paseo-source/packages/server/src/server/agent/timeline-projection.ts';
let queuedSnapshot;
const emit=value=>{
  if(value.event==='snapshot' && process.stdout.writableLength>1024*1024){queuedSnapshot=value;return;}
  if(value.event==='snapshot')value={...value,displayEntries:projectTimelineRows({rows:value.entries,mode:'projected'})};
  process.stdout.write(JSON.stringify(value)+'\n');
};
process.stdout.on('drain',()=>{if(queuedSnapshot){const value=queuedSnapshot;queuedSnapshot=null;emit(value);}});
const client=createClient();
const controller=new Controller(client,emit);
client.subscribeConnectionStatus(state=>{
  controller.connected=state.status==='connected';
  const info=client.getLastServerInfoMessage();
  emit({event:'connection',status:state.status,serverId:info?.serverId,version:info?.version});
  if(controller.connected)controller.invalidate();
});
client.subscribeRawMessages(message=>{
  if(message.type==='agent_status' && message.payload.agentId===controller.selected?.id) {
    controller.selected=message.payload.info;controller.snapshot();controller.invalidate();
  }
});
const lines=createInterface({input:process.stdin});
let queue=Promise.resolve();
lines.on('line',line=>{
  queue=queue.then(async()=>{
    let command;
    try {command=JSON.parse(line);const value=await controller.dispatch(command);emit({id:command.id,ok:true,value});}
    catch(error){emit({id:command?.id,ok:false,error:error instanceof Error?error.message:'Bridge operation failed'});}
  });
});
lines.on('close',()=>{controller.close();process.exit(0);});
process.on('SIGTERM',()=>{controller.close();process.exit(0);});
setInterval(()=>{if(controller.connected && controller.selected)controller.invalidate();},2500).unref();
client.connect().catch(()=>emit({event:'error',error:'Cannot authenticate/connect to local Paseo daemon at 127.0.0.1:6973. No credentials were changed.'}));
