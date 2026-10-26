import {test} from 'node:test';
import assert from 'node:assert/strict';
import {Controller,joinEntries,canArchiveWorkspace} from './controller.mjs';
function fixture() {
 let agent={id:'a',status:'idle',activeTurn:null,pendingPermissions:[]};let sends=0,cancels=0;
 let pages=[];const events=[];
 const client={getLastServerInfoMessage:()=>({serverId:'s'}),fetchAgent:async()=>({agent}),
  fetchAgentTimeline:async()=>pages.shift()??({agent,epoch:'e',entries:[],startCursor:null,hasOlder:false}),
  sendAgentMessage:async()=>{sends++;},cancelAgent:async()=>{cancels++;},
  subscribeAgentTimeline:()=>Object.assign(()=>{}, {ready:Promise.resolve()}),close(){}};
 const c=new Controller(client,v=>events.push(v));c.connected=true;c.selected=agent;c.selectedServer='s';
 return {c,client,events,pages,setAgent:v=>{agent=v;c.selected=v;},sends:()=>sends,cancels:()=>cancels};
}
test('exact identity, busy, and duplicate-send boundaries use production dispatch',async()=>{
 const f=fixture();const command={op:'send',serverId:'s',agentId:'a',text:'hello',messageId:'m',acknowledgeRace:true};
 await assert.rejects(f.c.dispatch({...command,serverId:'other'}));
 await assert.rejects(f.c.dispatch({...command,agentId:'b'}));
 f.setAgent({id:'a',status:'running',activeTurn:{turnId:'t'},pendingPermissions:[]});
 await assert.rejects(f.c.dispatch(command));assert.equal(f.sends(),0);
 f.setAgent({id:'a',status:'idle',activeTurn:null,pendingPermissions:[]});
 await f.c.dispatch(command);await assert.rejects(f.c.dispatch(command));assert.equal(f.sends(),1);f.c.close();
});
test('unknown send is never replayed and cancel is turn-fenced, never archival',async()=>{
 const f=fixture();f.client.sendAgentMessage=async()=>{throw Error('lost reply');};
 await assert.rejects(f.c.dispatch({op:'send',serverId:'s',agentId:'a',text:'hello',messageId:'unknown',acknowledgeRace:true}),/outcome unknown/);
 await assert.rejects(f.c.dispatch({op:'send',serverId:'s',agentId:'a',text:'hello',messageId:'unknown',acknowledgeRace:true}),/already used/);
 f.setAgent({id:'a',status:'running',activeTurn:{turnId:'t'},pendingPermissions:[]});
 await assert.rejects(f.c.dispatch({op:'cancel',serverId:'s',agentId:'a',turnId:'old'}));assert.equal(f.cancels(),0);
 await f.c.dispatch({op:'cancel',serverId:'s',agentId:'a',turnId:'t'});assert.equal(f.cancels(),1);
 await assert.rejects(f.c.dispatch({op:'archive',serverId:'s',agentId:'a'}));f.c.close();
});
test('canonical tail replaces same seq; epoch reset removes old pagination',async()=>{
 const f=fixture();const entry=(seq,text)=>({seqStart:seq,seqEnd:seq,item:{type:'assistant_message',text}});
 assert.equal(joinEntries([entry(1,'old')],[entry(1,'new')])[0].item.text,'new');
 f.c.epoch='e';f.c.older=[entry(0,'older')];
 f.pages.push({agent:f.c.selected,epoch:'next',entries:[entry(2,'new epoch')],startCursor:{epoch:'next',seq:2},hasOlder:true});
 await f.c.refresh();assert.equal(f.c.older.length,0);assert.equal(f.c.epoch,'next');assert.equal(f.events.at(-1).entries.length,1);f.c.close();
});
test('expired or unsupported permission never auto approves',async()=>{
 const f=fixture();await assert.rejects(f.c.dispatch({op:'permission',serverId:'s',agentId:'a',requestId:'r',actionId:'allow'}));f.c.close();
});
test('offered permission forwards exact agent, request, action and behavior',async()=>{
 const f=fixture();const replies=[];
 f.setAgent({id:'a',status:'awaiting_permission',pendingPermissions:[{id:'request-a',kind:'tool',actions:[{id:'deny-once',behavior:'deny'}]}]});
 f.client.respondToPermissionAndWait=async(...args)=>replies.push(args);
 await f.c.dispatch({op:'permission',serverId:'s',agentId:'a',requestId:'request-a',actionId:'deny-once'});
 assert.deepEqual(replies,[['a','request-a',{behavior:'deny',selectedActionId:'deny-once'}]]);
 f.c.close();assert.equal(f.cancels(),0);
});
test('older pagination and live latest remain distinct; replacement removes stale older window',async()=>{
 const f=fixture();const entry=n=>({seqStart:n,seqEnd:n,item:{type:'assistant_message',text:String(n)}});
 f.c.epoch='e';f.c.tail=[entry(100)];f.c.startCursor={epoch:'e',seq:100};
 f.pages.push({agent:f.c.selected,epoch:'e',entries:[entry(50)],startCursor:{epoch:'e',seq:50},hasOlder:true});
 await f.c.dispatch({op:'older',serverId:'s',agentId:'a'});
 f.pages.push({agent:f.c.selected,epoch:'e',entries:[entry(101)],startCursor:{epoch:'e',seq:101},hasOlder:true});
 await f.c.refresh();assert.deepEqual(f.events.at(-1).entries.map(e=>e.seqStart),[50,101]);assert.equal(f.events.at(-1).gap,true);
 f.pages.push({agent:f.c.selected,epoch:'e2',entries:[entry(1)],startCursor:{epoch:'e2',seq:1},hasOlder:false});
 await f.c.refresh();assert.deepEqual(f.events.at(-1).entries.map(e=>e.seqStart),[1]);f.c.close();
});
test('viewing clears only exact selected non-permission attention',async()=>{
 const f=fixture(),cleared=[];f.client.clearAgentAttention=async id=>cleared.push(id);
 const command={op:'clearAttention',serverId:'s',agentId:'a'};
 f.setAgent({id:'a',status:'idle',requiresAttention:true,attentionReason:'finished',pendingPermissions:[]});
 await assert.rejects(f.c.dispatch({...command,serverId:'other'}));
 await assert.rejects(f.c.dispatch({...command,agentId:'b'}));
 f.setAgent({...f.c.selected,attentionReason:'permission'});
 await assert.rejects(f.c.dispatch(command),/Permission attention/);
 f.setAgent({...f.c.selected,attentionReason:'finished',pendingPermissions:[{id:'p'}]});
 await assert.rejects(f.c.dispatch(command),/Permission attention/);assert.deepEqual(cleared,[]);
 f.setAgent({...f.c.selected,pendingPermissions:[]});
 await f.c.dispatch(command);assert.deepEqual(cleared,['a']);
 f.setAgent({...f.c.selected,requiresAttention:false});
 await f.c.dispatch(command);assert.deepEqual(cleared,['a']);
 f.client.fetchAgent=async()=>({agent:{id:'wrong',requiresAttention:true}});
 await assert.rejects(f.c.dispatch(command),/Exact agent ID/);assert.equal(cleared.length,1);
 f.client.fetchAgent=async()=>{f.c.selected={id:'b'};return {agent:{id:'a',requiresAttention:true}};};
 await assert.rejects(f.c.dispatch(command),/Wrong selected agent/);assert.equal(cleared.length,1);
 f.c.close();assert.equal(f.sends(),0);assert.equal(f.cancels(),0);
});
function workspaceFixture() {
 const f=fixture(),writes=[];
 let workspace={id:'w',workspaceKind:'directory',name:'Synthetic',title:null,pinnedAt:null,archivingAt:null,gitRuntime:null};
 f.setAgent({id:'a',workspaceId:'w',status:'idle',pendingPermissions:[]});
 f.client.fetchWorkspaces=async()=>({entries:[workspace],pageInfo:{nextCursor:null}});
 f.client.setWorkspaceTitle=async(...args)=>writes.push(['title',...args]);
 f.client.setWorkspacePinned=async(...args)=>writes.push(['pin',...args]);
 f.client.archiveWorkspace=async id=>{writes.push(['archive',id]);return {workspaceId:id,archivedAt:'2026-10-25T00:00:00Z',error:null};};
 return {...f,writes,setWorkspace:w=>{workspace=w;},workspace:()=>workspace};
}
for(const [op,extra,expected] of [
 ['renameWorkspace',{title:' New title '},['title','w','New title']],
 ['pinWorkspace',{pinned:true},['pin','w',true]],
 ['archiveWorkspace',{},['archive','w']],
]) test(`${op} uses exact card workspace, refuses wrong identity and preserves remote ownership`,async()=>{
 const f=workspaceFixture();const c={op,serverId:'s',agentId:'a',workspaceId:'w',...extra};
 await assert.rejects(f.c.dispatch({...c,serverId:'wrong'}));
 await assert.rejects(f.c.dispatch({...c,agentId:'wrong'}));
 await assert.rejects(f.c.dispatch({...c,workspaceId:'wrong'}));assert.deepEqual(f.writes,[]);
 if(op==='archiveWorkspace') {
  const safe=f.workspace();
  for(const risky of [{...safe,workspaceKind:'worktree'},{...safe,gitRuntime:{isPaseoOwnedWorktree:true}},{...safe,gitRuntime:{isDirty:true}},{...safe,gitRuntime:{aheadOfOrigin:1}},{...safe,diffStat:{additions:1,deletions:0}}]) {
   f.setWorkspace(risky);await assert.rejects(f.c.dispatch(c),/请在 Paseo 中归档/);
  }
  assert.deepEqual(f.writes,[]);f.setWorkspace(safe);
 }
 if(op==='renameWorkspace')await assert.rejects(f.c.dispatch({...c,title:' '}),/cancelled/);
 // Context actions on another card do not open/steer the visible conversation.
 f.c.selected={id:'other',workspaceId:'other-workspace'};
 const result=await f.c.dispatch(c);
 assert.deepEqual(f.writes,[expected]);assert.equal(result.workspaceId,'w');assert.equal(result.serverId,'s');
 assert.equal(f.c.selected.id,'other');assert.equal(f.sends(),0);assert.equal(f.cancels(),0);f.c.close();
});
test('directory joins authoritative workspace titles/pins across descriptor pages; archive detaches only its subscription',async()=>{
 const f=workspaceFixture();let calls=0;
 f.client.fetchAgentHistory=async()=>({entries:[{agent:{id:'a',workspaceId:'w'},project:{workspaceName:'old'}}],pageInfo:{nextCursor:'agent-next'}});
 f.client.fetchWorkspaces=async options=>{calls++;return options.page.cursor
  ?{entries:[{...f.workspace(),name:'Renamed',pinnedAt:'2026-10-25T00:00:00Z'}],pageInfo:{nextCursor:null}}
  :{entries:[],pageInfo:{nextCursor:'descriptor-next'}};};
 const page=await f.c.dispatch({op:'list',serverId:'s'});
 assert.equal(calls,2);assert.equal(page.entries[0].project.workspaceName,'Renamed');
 assert.equal(page.entries[0].workspace.pinnedAt,'2026-10-25T00:00:00Z');assert.equal(page.entries[0].workspace.canArchive,true);
 assert.equal(page.pageInfo.nextCursor,'agent-next');
 let detached=0;f.c.subscription=()=>{detached++;};
 await f.c.dispatch({op:'archiveWorkspace',serverId:'s',agentId:'a',workspaceId:'w'});
 assert.equal(detached,1);assert.equal(f.c.selected,null);assert.equal(f.cancels(),0);f.c.close();
 assert.equal(canArchiveWorkspace({workspaceKind:'worktree',gitRuntime:{isDirty:false,aheadOfOrigin:0}}),false);
});
