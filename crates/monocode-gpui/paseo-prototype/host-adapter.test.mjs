import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,readFileSync,readdirSync,writeFileSync,mkdirSync,symlinkSync,realpathSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {execFileSync} from 'node:child_process';
import {listHostFiles} from '../../../host/workspace.ts';
import {resolveHostWorktreeAsync} from '../../../host/git-worktrees.ts';
import {PaseoHost,Attempts,projectBlocks,SERVER,harness,summary} from './host-adapter.mjs';
function fixture(){
  const directory=mkdtempSync(join(tmpdir(),'paseo-adapter-test-'));
  const a={id:'a',provider:'example-hermes-storage',cwd:'/fixture',workspaceId:'w',status:'idle',model:'opaque:exact-model',updatedAt:'2026-10-25T00:00:00Z',pendingPermissions:[],activeTurn:null};
  const e={agent:a,project:{projectKey:'p',projectName:'Synthetic',workspaceName:'Short',checkout:{cwd:'/fixture',currentBranch:'main'}}};
  const events={sends:0,cancels:0,writes:[]};let changed;
  const client={getLastServerInfoMessage:()=>({serverId:SERVER,version:'synthetic'}),
    fetchAgentHistory:async()=>({entries:[e],pageInfo:{nextCursor:null}}),
    fetchWorkspaces:async()=>({entries:[{id:'w',name:'Short',workspaceKind:'directory',pinnedAt:null}],pageInfo:{}}),
    fetchAgent:async id=>({agent:{...a,id}}),
    fetchAgentTimeline:async()=>({agent:a,epoch:'e',hasOlder:false,entries:[
      {seqStart:1,seqEnd:1,timestamp:'2026-10-25T00:00:00Z',item:{type:'user_message',text:'Synthetic question'}},
      {seqStart:2,seqEnd:2,timestamp:'2026-10-25T00:00:03Z',item:{type:'assistant_message',text:'Synthetic reply'}}]}),
    subscribeAgentTimeline:(_,callback)=>{changed=callback;return Object.assign(()=>{},{ready:Promise.resolve()});},
    sendAgentMessage:async()=>{events.sends++;},cancelAgent:async()=>{events.cancels++;},
    respondToPermissionAndWait:async(...args)=>{events.writes.push(args);},close(){}};
  const host=new PaseoHost(client,directory);host.connected=true;
  return {host,client,a,e,events,directory,changed:()=>changed?.(),close(){host.close();rmSync(directory,{recursive:true,force:true});}};
}
test('all directory pages, cwd identities, aliases, pending-only input, pins and archive state',async()=>{
  const f=fixture();let pages=0;
  f.client.fetchAgentHistory=async()=>({entries:[{...f.e,agent:{...f.a,id:String(++pages),archivedAt:pages===2?'archived':null}}],pageInfo:{nextCursor:pages===1?'next':null}});
  await f.host.directory();assert.equal(f.host.entries.size,2);assert.equal(f.host.projects.size,1);
  const result=(await f.host.request('sessions.list',{projectId:'paseo:/fixture'})).result;
  assert.equal(result[0].harness,'hermes');assert.equal(result[0].model,'hermes:opaque:exact-model');
  assert.equal(result[0].needsInput,false);assert.equal(result[1].archived,true);assert.equal(harness('example-hermes-travel'),'hermes');f.close();
});
test('workspace root keeps its display title while child sessions retain their task titles',async()=>{
  const f=fixture();try{
    const agents=[
      {...f.a,id:'root',title:'Root prompt'},
      {...f.a,id:'research',title:'Research delivery flow',labels:{'paseo.parent-agent-id':'root'}},
      {...f.a,id:'implement',title:'Implement delivery flow',labels:{'paseo.parent-agent-id':'root'}},
    ];
    f.client.fetchAgentHistory=async()=>({entries:agents.map(agent=>({...f.e,agent})),pageInfo:{}});
    f.client.fetchWorkspaces=async()=>({entries:[{id:'w',name:'Short',title:'Plan delivery automation',workspaceKind:'directory'}],pageInfo:{}});
    f.client.fetchAgent=async id=>({agent:agents.find(a=>a.id===id)});
    f.client.fetchAgentTimeline=async id=>({agent:agents.find(a=>a.id===id),epoch:'e',hasOlder:false,entries:[]});
    const rows=(await f.host.request('sessions.list',{projectId:'paseo:/fixture'})).result;
    assert.deepEqual(rows.map(({id,title})=>({id,title})),[
      {id:'root',title:'Plan delivery automation'},
      {id:'research',title:'Research delivery flow'},
      {id:'implement',title:'Implement delivery flow'},
    ]);
    const snapshot=(await f.host.request('sessions.sync',{sessionId:'research'})).result;
    assert.equal(snapshot.value.session.title,'Research delivery flow');
    assert.equal(snapshot.value.session.id,'research');
    assert.equal(f.events.sends,0);assert.deepEqual(f.events.writes,[]);
  }finally{f.close();}
});
test('child titles fall back without dropping sessions that legitimately have equal titles',async()=>{
  const f=fixture();try{
    for(const [title,workspace,want] of [
      ['',{title:'Workspace label'},'Workspace label'],
      ['Task',undefined,'Task'],
      ['',undefined,'Short'],
    ]){
      const entry={...f.e,agent:{...f.a,title,labels:{'paseo.parent-agent-id':'root'}},workspace};
      assert.equal(summary(entry,'paseo:/fixture').title,want);
    }
    f.client.fetchAgentHistory=async()=>({entries:['one','two'].map(id=>({...f.e,agent:{...f.a,id,title:'Same task',labels:{'paseo.parent-agent-id':'root'}}})),pageInfo:{}});
    const rows=(await f.host.request('sessions.list',{projectId:'paseo:/fixture'})).result;
    assert.deepEqual(rows.map(({id,title})=>({id,title})),[{id:'one',title:'Same task'},{id:'two',title:'Same task'}]);
  }finally{f.close();}
});
test('workspace pin belongs to one root and parent identities survive without flattening children',async()=>{
  const f=fixture();try{
    const agents=[
      {...f.a,id:'child',title:'Research',createdAt:2000,labels:{'paseo.parent-agent-id':'a','paseo.parent-server-id':SERVER}},
      {...f.a,id:'peer',title:'Another conversation',createdAt:3000},
      {...f.a,id:'a',title:'Original prompt',createdAt:1000},
    ];
    f.client.fetchAgentHistory=async()=>({entries:agents.map(agent=>({...f.e,agent})),pageInfo:{}});
    f.client.fetchWorkspaces=async()=>({entries:[{id:'w',name:'Short',title:'Workspace',pinnedAt:'2026-10-26',workspaceKind:'directory'}],pageInfo:{}});
    f.client.fetchAgent=async id=>({agent:agents.find(a=>a.id===id)});
    f.client.setWorkspacePinned=async(...args)=>f.events.writes.push(args);
    const rows=(await f.host.request('sessions.list',{projectId:'paseo:/fixture'})).result;
    assert.deepEqual(rows.filter(r=>r.pinned).map(r=>r.id),['a']);
    assert.equal(rows.find(r=>r.id==='child').paseoParentAgentId,'a');
    assert.equal(rows.find(r=>r.id==='child').paseoWorkspaceId,'w');
    assert.equal(rows.find(r=>r.id==='a').paseoParentAgentId,null);
    assert.equal(rows.find(r=>r.id==='peer').title,'Another conversation');
    await assert.rejects(f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'child',pinned:true}),/置顶只适用于工作区的主会话/);
    await assert.rejects(f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'peer',pinned:true}),/置顶只适用于工作区的主会话/);
    assert.deepEqual(f.events.writes,[]);
    await f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',pinned:false});
    assert.deepEqual(f.events.writes,[['w',false]]);
    f.client.fetchAgentHistory=async()=>({entries:[...agents].reverse().map(agent=>({...f.e,agent})),pageInfo:{}});
    await f.host.directory(true);
    const reordered=(await f.host.request('sessions.list',{projectId:'paseo:/fixture'})).result;
    assert.deepEqual(reordered.filter(r=>r.pinned).map(r=>r.id),['a']);
    assert.equal(reordered.length,3);
  }finally{f.close();}
});
test('same-workspace siblings and detached subagents nest under workspace anchor while distinct workspaces and foreign parents stay top-level',async()=>{
  const f=fixture();
  try{
    const parentUuid='f63a33fc-958c-4433-a14a-0a79582c133a';
    f.client.fetchAgentHistory=async()=>({entries:[
      {...f.e,agent:{...f.a,id:'detached-ws',title:'Detached subagent',createdAt:1000,labels:{'paseo.parent-server-id':SERVER}}},
      {...f.e,agent:{...f.a,id:'root',title:'Root',createdAt:2000,labels:{}}},
      {...f.e,agent:{...f.a,id:'sibling',title:parentUuid,runtimeInfo:{extra:{title:'Optimize Conversation Document and Webpage'}},createdAt:3000,labels:{}}},
      {...f.e,agent:{...f.a,id:'detached-labeled',title:'Labeled subagent',createdAt:4000,labels:{'paseo.parent-server-id':SERVER,parent_agent:'sibling'}}},
      {...f.e,agent:{...f.a,id:'foreign',title:'Foreign subagent',createdAt:5000,labels:{'paseo.parent-server-id':'other-server',parent_agent:'sibling'}}},
      {...f.e,agent:{...f.a,id:'other-ws',workspaceId:'w2',title:'Independent workspace',createdAt:6000,labels:{}}},
    ],pageInfo:{}});
    await f.host.directory();
    const rows=(await f.host.request('sessions.list',{projectId:'paseo:/fixture'})).result;
    assert.equal(rows.find(r=>r.id==='root').paseoWorkspaceAnchor,true);
    assert.equal(rows.find(r=>r.id==='root').paseoParentAgentId,null);
    assert.equal(rows.find(r=>r.id==='sibling').paseoParentAgentId,'root');
    assert.equal(rows.find(r=>r.id==='sibling').title,'Optimize Conversation Document and Webpage');
    assert.equal(rows.find(r=>r.id==='detached-ws').paseoParentAgentId,'root');
    assert.equal(rows.find(r=>r.id==='detached-labeled').paseoParentAgentId,'sibling');
    assert.equal(rows.find(r=>r.id==='foreign').paseoParentAgentId,null);
    assert.equal(rows.find(r=>r.id==='other-ws').paseoParentAgentId,null);
  }finally{f.close();}
});
test('actual native Blocks retain all prose, tool state, stable ids, user timestamps and completed duration',()=>{
  const page={epoch:'e',agent:{status:'idle'},entries:[
    {seqStart:1,timestamp:1000,item:{type:'user_message',text:'question'}},
    {seqStart:2,timestamp:2000,item:{type:'reasoning',text:'thinking'}},
    {seqStart:3,timestamp:3000,item:{type:'tool_call',callId:'c',name:'read',status:'failed',detail:{type:'read',filePath:'a.rs',content:'x'}}},
    {seqStart:4,timestamp:4000,item:{type:'assistant_message',text:'report'}},
    {seqStart:5,timestamp:5000,item:{type:'assistant_message',text:'follow-up'}}]};
  const blocks=projectBlocks(page);assert.deepEqual(blocks.map(b=>b.role),['user','reasoning','tool','assistant','assistant']);
  assert.equal(blocks[0].startedAt,1000);assert.equal(blocks[0].durationMs,4000);assert.equal(blocks[2].tool.status,'failed');
  assert.equal(blocks[2].tool.nativeName,'read');assert.deepEqual(JSON.parse(blocks[2].tool.nativeInput),{filePath:'a.rs'});
  assert.deepEqual(blocks.map(b=>b.id),projectBlocks(page).map(b=>b.id));
});
test('snapshot, unchanged and delta all traverse production adapter; identity mismatch refuses',async()=>{
  const f=fixture();await f.host.directory();const first=(await f.host.request('sessions.sync',{sessionId:'a'})).result;
  assert.equal(first.kind,'snapshot');const revision=first.value.revision;
  const repeated=(await f.host.request('sessions.sync',{sessionId:'a',revision})).result;assert.equal(repeated.kind,'unchanged');
  f.a.updatedAt='2026-10-25T00:00:04Z';f.changed();
  const next=(await f.host.request('sessions.sync',{sessionId:'a',revision})).result;assert.equal(next.kind,'delta');assert.equal(next.base,revision);
  f.client.fetchAgent=async()=>({agent:{...f.a,id:'wrong'}});
  await assert.rejects(f.host.history('a'),/identity/);f.close();
});
test('send busy/identity, exact cancel and permission mapping; unknown sends never replay across restart',async()=>{
  const f=fixture();await f.host.directory();
  await assert.rejects(f.host.command({type:'send',sessionId:'wrong',commandId:'wrong',text:'x'}));
  f.a.activeTurn={turnId:'t'};
  await assert.rejects(f.host.command({type:'send',sessionId:'a',commandId:'busy',text:'x'}),/busy/);
  await assert.rejects(f.host.command({type:'cancel',sessionId:'a',commandId:'bad-cancel',runId:'wrong'}),/Turn/);
  assert.equal(f.events.cancels,0);
  await f.host.command({type:'cancel',sessionId:'a',commandId:'cancel',runId:'t'});assert.equal(f.events.cancels,1);
  f.a.pendingPermissions=[{id:'opaque-request',kind:'tool',name:'read',actions:[{id:'only-deny',behavior:'deny'}]}];
  await f.host.history('a');const number=f.host.number('a','opaque-request');
  await f.host.command({type:'approve',sessionId:'a',commandId:'approve',runId:'t',requestId:number,decision:'deny'});
  assert.deepEqual(f.events.writes,[['a','opaque-request',{behavior:'deny',selectedActionId:'only-deny'}]]);
  f.a.activeTurn=null;f.a.pendingPermissions=[];f.client.sendAgentMessage=async()=>{f.events.sends++;throw Error('lost ack');};
  const command={type:'send',sessionId:'a',commandId:'lost',text:'synthetic'};
  await assert.rejects(f.host.command(command),/unknown/);
  const restored=new Attempts(f.directory);await assert.rejects(restored.run(command,()=>{f.events.sends++;}),/No replay/);
  assert.equal(f.events.sends,1);f.close();
});
test('journal returns accepted receipt once, refuses changed payload, persists pending before side effect',async()=>{
  const f=fixture();const command={commandId:'once',type:'synthetic'};let calls=0;
  const action=()=>{calls++;assert.equal(JSON.parse(readFileSync(join(f.directory,'paseo-command-attempts.json'))).once.state,'pending');return {commandId:'once',sessionId:'a'};};
  const receipt=await f.host.attempts.run(command,action);
  assert.deepEqual(await new Attempts(f.directory).run(command,action),receipt);assert.equal(calls,1);
  await assert.rejects(f.host.attempts.run({...command,type:'other'},action),/different content/);f.close();
});
test('native questions round-trip exact requests; permission number mapping survives host restart',async()=>{
  const f=fixture();await f.host.directory();
  f.a.activeTurn={turnId:'t'};
  f.a.pendingPermissions=[{id:'q-opaque',kind:'question',name:'Question',input:{questions:[
    {question:'Synthetic question',header:'Choice',options:[{label:'One'},{label:'Two'}],multiSelect:false}]}}];
  const h=await f.host.history('a');const requestId=h.snapshot.session.pendingQuestion.requestId;
  const restored=new PaseoHost(f.client,f.directory);restored.connected=true;
  assert.equal(restored.number('a','q-opaque'),requestId);assert.notEqual(restored.number('b','q-opaque'),requestId);
  await f.host.command({type:'answer',commandId:'answer',sessionId:'a',runId:'t',requestId,reply:{kind:'answered',answers:{0:['1']}}});
  assert.deepEqual(f.events.writes,[['a','q-opaque',{behavior:'allow',updatedInput:{...f.a.pendingPermissions[0].input,answers:{Choice:'Two'}}}]]);
  await assert.rejects(f.host.command({type:'answer',commandId:'bad-answer',sessionId:'a',runId:'t',requestId,reply:{kind:'answered',answers:{0:['unknown']}}}),/Unknown question/);f.close();
});
test('rename/pin use exact workspace guards; archive never reaches the workspace; delete refuses',async()=>{
  const f=fixture();await f.host.directory();
  f.client.setWorkspaceTitle=async(...a)=>f.events.writes.push(['title',...a]);
  f.client.setWorkspacePinned=async(...a)=>f.events.writes.push(['pin',...a]);
  f.client.updateAgent=async(...a)=>f.events.writes.push(['agent',...a]);
  await f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',title:'new'});
  await f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',pinned:true});
  assert.deepEqual(f.events.writes,[['title','w','new'],['pin','w',true]]);
  await assert.rejects(f.host.request('sessions.update',{projectId:'wrong',sessionId:'a',pinned:true}),/Wrong project/);
  await assert.rejects(f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',title:'x',pinned:true}),/One supported/);
  f.client.fetchWorkspaces=async()=>({entries:[{id:'w',workspaceKind:'worktree',worktreeSlug:'owned'}],pageInfo:{}});
  f.client.archiveWorkspace=async()=>{throw Error('workspace archive must not be used');};
  f.client.archiveAgent=async id=>{f.events.writes.push(['archive',id]);return {archivedAt:'synthetic'};};
  await f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',archived:true});
  assert.deepEqual(f.events.writes.slice(2),[['archive','a']]);
  await assert.rejects(f.host.request('sessions.delete',{sessionId:'a'}),/删除/);f.close();
});
// Mutable Paseo world: root anchor 'a', its child, and a non-anchor sibling 'peer' in one dirty worktree workspace.
function world(){
  const f=fixture(),calls=[];
  const agents=new Map([
    ['a',{...f.a,id:'a',title:'Original prompt',createdAt:1000}],
    ['child',{...f.a,id:'child',title:'Research',createdAt:2000,labels:{'paseo.parent-agent-id':'a'}}],
    ['peer',{...f.a,id:'peer',title:'Another conversation',createdAt:3000}]]);
  const workspace={id:'w',name:'Short',title:'Workspace',workspaceKind:'worktree',worktreeSlug:'owned',gitRuntime:{isDirty:true,isPaseoOwnedWorktree:true},pinnedAt:'2026-10-26'};
  f.client.fetchAgentHistory=async()=>({entries:[...agents.values()].map(agent=>({...f.e,agent:{...agent}})),pageInfo:{}});
  f.client.fetchWorkspaces=async()=>({entries:[{...workspace}],pageInfo:{}});
  f.client.fetchAgent=async id=>({agent:{...agents.get(id)}});
  f.client.fetchAgentTimeline=async id=>({agent:{...agents.get(id)},epoch:'e',hasOlder:false,entries:[]});
  f.client.archiveWorkspace=async()=>{throw Error('workspace archive must not be used');};
  f.client.setWorkspaceTitle=async(...a)=>{calls.push(['setWorkspaceTitle',...a]);workspace.title=a[1];};
  f.client.setWorkspacePinned=async(...a)=>calls.push(['setWorkspacePinned',...a]);
  f.client.updateAgent=async(id,patch)=>{calls.push(['updateAgent',id,patch]);agents.get(id).title=patch.name;};
  f.client.archiveAgent=async id=>{calls.push(['archiveAgent',id]);agents.get(id).archivedAt='2026-10-29T00:00:00Z';return {archivedAt:agents.get(id).archivedAt};};
  f.client.refreshAgent=async id=>{calls.push(['refreshAgent',id]);agents.get(id).archivedAt=null;return {};};
  const update=patch=>f.host.request('sessions.update',{projectId:'paseo:/fixture',...patch});
  const list=async()=>(await f.host.request('sessions.list',{projectId:'paseo:/fixture'})).result;
  return {...f,agents,calls,workspace,update,list};
}
test('sub-conversation and sibling rename write the agent title; the anchor renames only the workspace',async()=>{
  const w=world();try{
    await w.host.directory();await w.host.request('sessions.sync',{sessionId:'child'});
    const before=(await w.host.request('sessions.sync',{sessionId:'child'})).result;
    const result=(await w.update({sessionId:'child',title:'  Renamed child  '})).result;
    assert.equal(result.title,'Renamed child');
    await w.update({sessionId:'peer',title:'Renamed peer'});
    await w.update({sessionId:'a',title:'Renamed root'});
    assert.deepEqual(w.calls,[['updateAgent','child',{name:'Renamed child'}],['updateAgent','peer',{name:'Renamed peer'}],['setWorkspaceTitle','w','Renamed root']]);
    await w.host.directory(true);
    const rows=await w.list();
    assert.deepEqual(rows.map(({id,title})=>({id,title})).sort((x,y)=>x.id.localeCompare(y.id)),[{id:'a',title:'Renamed root'},{id:'child',title:'Renamed child'},{id:'peer',title:'Renamed peer'}]);
    const after=(await w.host.request('sessions.sync',{sessionId:'child',revision:before.value.revision})).result;
    assert.notEqual(after.kind,'unchanged');
    assert.equal((after.kind==='snapshot'?after.value:after.value).session.title,'Renamed child');
  }finally{w.close();}
});
test('a child with a null title is renamed through the agent so its title becomes visible',async()=>{
  const w=world();try{
    w.agents.get('child').title=null;await w.host.directory();
    await w.update({sessionId:'child',title:'Now visible'});
    assert.deepEqual(w.calls,[['updateAgent','child',{name:'Now visible'}]]);
    assert.equal((await w.list()).find(r=>r.id==='child').title,'Now visible');
  }finally{w.close();}
});
test('empty or whitespace rename is rejected before any write',async()=>{
  const w=world();try{
    await w.host.directory();
    for(const id of ['a','child','peer'])for(const title of ['','   ','\n\t'])await assert.rejects(w.update({sessionId:id,title}),/Empty rename/);
    assert.deepEqual(w.calls,[]);
  }finally{w.close();}
});
test('only the anchor can pin and only the anchor reports paseoWorkspaceAnchor',async()=>{
  const w=world();try{
    await w.host.directory();
    for(const id of ['child','peer'])await assert.rejects(w.update({sessionId:id,pinned:true}),/置顶只适用于工作区的主会话；子会话或同一工作区的其他会话不能单独置顶。/);
    assert.deepEqual(w.calls,[]);
    await w.update({sessionId:'a',pinned:false});
    assert.deepEqual(w.calls,[['setWorkspacePinned','w',false]]);
    assert.deepEqual((await w.list()).filter(r=>r.paseoWorkspaceAnchor).map(r=>r.id),['a']);
  }finally{w.close();}
});
test('archive closes exactly one agent, never the workspace, even for a dirty worktree or a running agent',async()=>{
  for(const id of ['a','child','peer']){
    const w=world();try{
      await w.host.directory();
      if(id==='peer')w.agents.get(id).activeTurn={turnId:'t'};
      const result=(await w.update({sessionId:id,archived:true})).result;
      assert.deepEqual(w.calls,[['archiveAgent',id]]);
      assert.equal(result.archived,true);assert.equal(result.pinned,false);
      const row=(await w.list()).find(r=>r.id===id);
      assert.equal(row.archived,true);assert.equal(row.pinned,false);
      assert.equal(w.events.cancels,0);
    }finally{w.close();}
  }
});
test('archive of an archived agent and unarchive of a live agent make no client mutation',async()=>{
  const w=world();try{
    w.agents.get('child').archivedAt='2026-10-28T00:00:00Z';w.agents.get('peer').activeTurn={turnId:'t'};
    await w.host.directory();
    assert.equal((await w.update({sessionId:'child',archived:true})).result.archived,true);
    assert.equal((await w.update({sessionId:'a',archived:false})).result.archived,false);
    const running=(await w.update({sessionId:'peer',archived:false})).result;
    assert.equal(running.archived,false);assert.equal(running.status,'running');
    assert.deepEqual(w.calls,[]);
  }finally{w.close();}
});
test('unarchive of an archived agent refreshes exactly that agent',async()=>{
  const w=world();try{
    w.agents.get('peer').archivedAt='2026-10-28T00:00:00Z';await w.host.directory();
    assert.equal((await w.list()).find(r=>r.id==='peer').archived,true);
    const result=(await w.update({sessionId:'peer',archived:false})).result;
    assert.deepEqual(w.calls,[['refreshAgent','peer']]);
    assert.equal(result.archived,false);
    assert.equal((await w.list()).find(r=>r.id==='peer').archived,false);
  }finally{w.close();}
});
test('archive without a confirmed archivedAt rejects',async()=>{
  const w=world();try{
    await w.host.directory();w.client.archiveAgent=async()=>({});
    await assert.rejects(w.update({sessionId:'child',archived:true}),/Archive not confirmed/);
  }finally{w.close();}
});
test('a mutation never answers from a directory fetch that started before it',async()=>{
  const w=world();try{
    await w.host.directory();
    const history=w.client.fetchAgentHistory;let release;
    // The in-flight fetch reads pre-archive rows, then waits.
    w.client.fetchAgentHistory=async(...a)=>{const page=await history(...a);w.client.fetchAgentHistory=history;await new Promise(r=>{release=r;});return page;};
    const stale=w.host.directory(true);
    await new Promise(r=>setImmediate(r));
    const archived=w.update({sessionId:'child',archived:true});
    await new Promise(r=>setImmediate(r));release();await stale;
    assert.equal((await archived).result.archived,true);
    assert.equal((await w.list()).find(r=>r.id==='child').archived,true);
    assert.deepEqual(w.calls,[['archiveAgent','child']]);
  }finally{w.close();}
});
test('archive updates in-memory directory without triggering a full paginated history rescan',async()=>{
  const w=world();try{
    await w.host.directory();
    let historyCalls=0;
    const history=w.client.fetchAgentHistory;
    w.client.fetchAgentHistory=async(...a)=>{historyCalls++;return history(...a);};
    const result=(await w.update({sessionId:'a',archived:true})).result;
    assert.equal(result.archived,true);
    assert.equal(result.pinned,false);
    assert.equal(historyCalls,0,'archive must not rescan full directory');
    const rows=await w.list();
    assert.equal(historyCalls,0,'subsequent list uses patched directory without rescanning');
    assert.equal(rows.find(r=>r.id==='a').archived,true);
    assert.equal(rows.find(r=>r.id==='child').archived,true,'paseo.parent-agent-id child cascades');
    assert.equal(rows.find(r=>r.id==='peer').archived,false,'detached sibling stays live');
  }finally{w.close();}
});
test('applyAgentStatus patches known agent in place without wiping directory cache and invalidates unknown agent',async()=>{
  const w=world();try{
    await w.host.directory();
    let historyCalls=0;
    const history=w.client.fetchAgentHistory;
    w.client.fetchAgentHistory=async(...a)=>{historyCalls++;return history(...a);};
    const updated={...w.agents.get('peer'),status:'running',activeTurn:{turnId:'t1'}};
    w.host.applyAgentStatus({type:'agent_status',payload:{agentId:'peer',status:'running',info:updated}});
    const rows=await w.list();
    assert.equal(historyCalls,0,'known agent_status must not force a full directory rescan');
    assert.equal(rows.find(r=>r.id==='peer').status,'running');
    w.host.applyAgentStatus({type:'agent_status',payload:{agentId:'brand-new',status:'idle',info:{id:'brand-new',workspaceId:'w-new'}}});
    assert.equal(w.host.directoryAt,0,'unknown agent_status invalidates directory for discovery');
  }finally{w.close();}
});
test('16 parent-redacted snapshot sequences project without lost prose or duplicate block IDs',t=>{
  const directory=process.env.PASEO_R8_SHAPES;
  if(!directory){t.skip('Set PASEO_R8_SHAPES to the parent-redacted corpus');return;}
  const files=readdirSync(directory).filter(p=>p.endsWith('.json'));assert.equal(files.length,16);let count=0;
  const output=process.env.PASEO_R8_BLOCKS;
  if(output)mkdirSync(output,{recursive:true});
  for(const file of files){
    const projected=[];
    for(const page of JSON.parse(readFileSync(join(directory,file),'utf8'))){
      const blocks=projectBlocks(page);count++;
      assert.equal(new Set(blocks.map(b=>b.id)).size,blocks.length);
      const prose=(page.displayEntries??page.entries).filter(e=>['user_message','assistant_message'].includes(e.item.type));
      assert.deepEqual(blocks.filter(b=>['user','assistant'].includes(b.role)).map(b=>b.text),prose.map(e=>e.item.text));
      projected.push({session:{blocks},status:page.agent.status});
    }
    if(output)writeFileSync(join(output,file),JSON.stringify(projected));
  }
  assert.equal(count,37);
});

test('filesystem handlers canonicalize the recorded root alias without changing project identity',async()=>{
  const f=fixture();
  try {
    const actual=join(f.directory,'actual'),recorded=join(f.directory,'RecordedCase');
    mkdirSync(actual);writeFileSync(join(actual,'fixture.txt'),'synthetic');symlinkSync(actual,recorded,'dir');
    f.a.cwd=recorded;f.e.project.checkout.cwd=recorded;await f.host.directory();
    const project=(await f.host.request('projects.list',{})).result[0];
    assert.equal(project.cwd,recorded);
    for(const path of [recorded,'.']) {
      const params={projectId:project.id,path,cwd:recorded};
      assert.equal(await f.host.request('files.list',params),null);
      const root=f.host.fileProject(project.id).cwd;
      const files=await listHostFiles(root,params.path);
      assert.equal(files.some(file=>file.name==='fixture.txt'),true);
      assert.equal(root,realpathSync.native(actual));
      assert.equal(await resolveHostWorktreeAsync(root,params.cwd),root);
    }
    const outside=join(f.directory,'outside');mkdirSync(outside);
    const escape={projectId:project.id,path:outside};await f.host.request('files.list',escape);
    await assert.rejects(listHostFiles(f.host.fileProject(project.id).cwd,escape.path),/outside the workspace/);
    const missing={id:'missing',cwd:join(f.directory,'deleted'),name:'Deleted synthetic checkout'};
    f.host.projects.set(missing.id,missing);
    await assert.rejects(f.host.request('files.list',{projectId:missing.id,path:missing.cwd}),/已不存在/);
    assert.equal(f.host.fileProjects().length,1);
    assert.equal(f.host.projects.get(project.id).cwd,recorded);
    assert.equal(f.host.entries.get('a').agent.cwd,recorded);
  } finally { f.close(); }
});

test('new chat creates an independent directory workspace with official naming context and never replays an ambiguous create',async()=>{
  const f=fixture();try{
    f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>({models:[{provider:'hermes',id:'m',label:'M'}]});
    f.client.listProviderModes=async()=>({modes:[{id:'dont_ask'}]});f.client.listProviderFeatures=async()=>({features:[]});
    let workspaceCalls=0,agentCalls=0,options,agentOptions,effectiveNamingContext;
    f.client.createAgent=async o=>{agentCalls++;agentOptions=o;return {...f.a,id:o.agentId,workspaceId:o.workspaceId,cwd:o.config.cwd};};
    f.client.createWorkspace=async o=>{
      workspaceCalls++;options=o;
      // session.ts replaces firstAgentContext whenever an initial agent is supplied.
      effectiveNamingContext=o.agent?{prompt:o.agent.initialPrompt,attachments:o.agent.attachments}:o.firstAgentContext;
      return {workspace:{id:'new-workspace',name:'New workspace',workspaceKind:'directory'},
        ...(o.agent?{agent:{...f.a,id:o.agent.agentId,workspaceId:'new-workspace'}}:{}),error:null};
    };
    const create={type:'create',commandId:'independent-once',projectId:'paseo:'+f.directory,harness:'hermes',model:'hermes:m',firstAgentContext:{prompt:'Review the project tasks',attachments:[]}};
    const receipt=await f.host.command(create);
    assert.equal(workspaceCalls,1);assert.equal(agentCalls,1);
    assert.deepEqual(options.source,{kind:'directory',path:f.directory});
    assert.deepEqual(effectiveNamingContext,{prompt:'Review the project tasks',attachments:[]});
    assert.equal(options.agent,undefined,'the daemon must not overwrite the naming context with an empty initialPrompt');
    assert.equal(options.callerAgentId,undefined);assert.equal(agentOptions.callerAgentId,undefined);
    assert.equal(agentOptions.initialPrompt,undefined,'the separately journaled send owns the only prompt delivery');
    assert.equal(agentOptions.config.modeId,'dont_ask');assert.equal(agentOptions.config.cwd,f.directory);
    assert.equal(agentOptions.workspaceId,'new-workspace');
    assert.equal(options.idempotencyKey,create.commandId);assert.equal(agentOptions.idempotencyKey,create.commandId+':agent');
    assert.equal(receipt.sessionId,agentOptions.agentId);
    assert.equal(f.host.entries.get(receipt.sessionId).agent.workspaceId,'new-workspace');
    assert.deepEqual(await f.host.command(create),receipt);assert.equal(workspaceCalls,1);assert.equal(agentCalls,1);
    f.client.createWorkspace=async()=>{workspaceCalls++;throw Error('unknown workspace create ack');};
    const lost={...create,commandId:'independent-lost'};
    await assert.rejects(f.host.command(lost),/unknown workspace create ack/);
    const restored=new PaseoHost(f.client,f.directory);restored.connected=true;
    await assert.rejects(restored.command(lost),/No replay/);assert.equal(workspaceCalls,2);
    f.client.createWorkspace=async()=>{workspaceCalls++;return {workspace:{id:'workspace-before-agent-ack',name:'New workspace'},error:null};};
    f.client.createAgent=async()=>{agentCalls++;throw Error('unknown agent create ack');};
    const lostAgent={...create,commandId:'independent-agent-lost'};
    await assert.rejects(f.host.command(lostAgent),/unknown agent create ack/);
    const restoredAgent=new PaseoHost(f.client,f.directory);restoredAgent.connected=true;
    await assert.rejects(restoredAgent.command(lostAgent),/No replay/);
    assert.equal(workspaceCalls,3);assert.equal(agentCalls,2);
    assert.equal(f.events.sends,0);
  }finally{f.close();}
});

test('workspace choices are explicit project-scoped identities and invalid selections never fall back to the latest workspace',async()=>{
  const f=fixture();try{
    f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
    const other=join(f.directory,'other');mkdirSync(other);
    f.client.fetchAgentHistory=async()=>({entries:[f.e,{...f.e,agent:{...f.a,id:'foreign',workspaceId:'foreign-w',cwd:other},project:{...f.e.project,projectKey:'foreign',checkout:{cwd:other}}}],pageInfo:{}});
    f.client.fetchWorkspaces=async()=>({entries:[{id:'w',title:'Existing workspace',name:'Existing workspace',workspaceKind:'directory'},{id:'foreign-w',title:'Other workspace',workspaceKind:'directory'}],pageInfo:{}});
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>({models:[{provider:'hermes',id:'m',label:'M'}]});
    f.client.listProviderModes=async()=>({modes:[{id:'dont_ask'}]});f.client.listProviderFeatures=async()=>({features:[]});
    let creates=0;
    f.client.createAgent=async o=>{creates++;assert.equal(o.workspaceId,'w');assert.equal(o.callerAgentId,undefined);return {...f.a,id:o.agentId};};
    const choices=(await f.host.request('paseo.workspaces',{projectId:'paseo:'+f.directory})).result;
    assert.deepEqual(choices,[{id:'w',title:'Existing workspace',cwd:f.directory}]);
    const c={type:'create',commandId:'explicit-existing',projectId:'paseo:'+f.directory,workspaceId:'w',harness:'hermes',model:'hermes:m'};
    await f.host.command(c);assert.equal(creates,1);
    for(const workspaceId of ['foreign-w','missing','']){
      await assert.rejects(f.host.command({...c,workspaceId,commandId:'refuse-'+workspaceId}),/workspace|工作区/);
    }
    assert.equal(creates,1);
  }finally{f.close();}
});

test('workspace menu lists only the most recent workspaces, newest first',async()=>{
  const f=fixture();try{
    f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
    const entries=Array.from({length:25},(_,i)=>({...f.e,agent:{...f.a,id:'a'+i,workspaceId:'w'+i,updatedAt:new Date(1000+i*1000).toISOString()}}));
    f.client.fetchAgentHistory=async()=>({entries,pageInfo:{}});
    f.client.fetchWorkspaces=async()=>({entries:entries.map((_,i)=>({id:'w'+i,title:'W'+i,workspaceKind:'directory'})),pageInfo:{}});
    const choices=(await f.host.request('paseo.workspaces',{projectId:'paseo:'+f.directory})).result;
    assert.equal(choices.length,20);
    assert.deepEqual(choices.slice(0,2).map(w=>w.id),['w24','w23']);
    assert.equal(choices.at(-1).id,'w5');
  }finally{f.close();}
});

test('generated workspace title refreshes an already opened conversation without new agent messages',async()=>{
  const f=fixture();try{
    let title='Provisional prompt title';
    f.client.fetchWorkspaces=async()=>({entries:[{id:'w',title,name:title,workspaceKind:'directory'}],pageInfo:{}});
    assert.equal((await f.host.request('sessions.sync',{sessionId:'a'})).result.value.session.title,title);
    title='Generated concise workspace title';
    await f.host.directory(true);
    assert.equal((await f.host.request('sessions.sync',{sessionId:'a'})).result.value.session.title,title);
    assert.equal(f.events.sends,0);
  }finally{f.close();}
});

test('asynchronous free-text questions answer the exact request without ordinary send or dismissal',async()=>{
  const f=fixture();try{
    f.a.status='running';f.a.activeTurn={turnId:'async-turn'};
    f.a.pendingPermissions=[{id:'permission-async',kind:'question',name:'request_user_input_async',input:{questions:[{id:'0',header:'Question 1',question:'Which document should be used?',options:[],isOther:true}]}}];
    let answer;
    f.client.respondToPermissionAndWait=async(...args)=>{answer=args;f.a.pendingPermissions=[];};
    const first=(await f.host.request('sessions.sync',{sessionId:'a'})).result.value;
    assert.equal(first.session.pendingQuestion.questions[0].allowCustom,true);
    await f.host.command({type:'answer',commandId:'async-answer',sessionId:'a',runId:'async-turn',requestId:first.session.pendingQuestion.requestId,reply:{kind:'answered',answers:[[]],custom:['Use the linked task document']}});
    assert.equal(answer[0],'a');assert.equal(answer[1],'permission-async');
    assert.equal(answer[2].behavior,'allow');
    assert.deepEqual(answer[2].updatedInput.answers,{'Question 1':'Use the linked task document'});
    assert.equal((await f.host.request('sessions.sync',{sessionId:'a'})).result.value.session.pendingQuestion,undefined);
    assert.equal(f.events.sends,0);assert.equal(f.events.cancels,0);
  }finally{f.close();}
});

 test('native create uses exact workspace/full mode and durable idempotent receipt, unknown ack never creates twice',async()=>{
  const f=fixture();
  try {
    f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>({models:[{provider:'hermes',id:'model',label:'Model',thinkingOptions:[{id:'high',label:'High'}]}]});
    f.client.listProviderModes=async()=>({modes:[{id:'confirm'},{id:'dont_ask'}]});
    f.client.listProviderFeatures=async()=>({features:[]});
    let creates=0,options;
    f.client.createAgent=async o=>{creates++;options=o;return {...f.a,id:o.agentId,provider:'hermes'};};
    await f.host.directory();
    const c={type:'create',commandId:'create-once',projectId:'paseo:'+f.directory,workspaceId:'w',harness:'hermes',model:'hermes:model',runtimeMode:'supervised',modelSettings:{effort:'high',fast:'false'}};
    const first=await f.host.command(c);
    assert.equal(options.workspaceId,'w');assert.equal(options.config.modeId,'dont_ask');assert.equal(options.config.cwd,f.directory);
    assert.equal(options.config.thinkingOptionId,'high');assert.equal(options.initialPrompt,undefined);
    assert.equal(options.idempotencyKey,c.commandId);assert.equal(first.sessionId,options.agentId);
    assert.deepEqual(await f.host.command(c),first);assert.equal(creates,1);
    f.client.createAgent=async()=>{creates++;throw Error('unknown create ack');};
    const lost={...c,commandId:'create-lost'};await assert.rejects(f.host.command(lost),/unknown create ack/);
    const restored=new PaseoHost(f.client,f.directory);restored.connected=true;
    await assert.rejects(restored.command(lost),/No replay/);assert.equal(creates,2);
  }finally{f.close();}
 });

test('configure maps exact model/thinking/features for exact alias, keeps mode, rejects busy/unknown values before writes',async()=>{
  const f=fixture();
  try {
    await f.host.directory();
    f.client.listProviderModels=async provider=>({models:[{provider,id:'new:exact',label:'Exact',thinkingOptions:[{id:'high',label:'High'}]}]});
    f.client.listProviderFeatures=async()=>({features:[{type:'toggle',id:'fast',label:'Fast',value:false},{type:'select',id:'service_tier',label:'Tier',options:[{id:'default',label:'Normal'}]}]});
    f.client.setAgentModel=async(...a)=>f.events.writes.push(['model',...a]);
    f.client.setAgentThinkingOption=async(...a)=>f.events.writes.push(['thinking',...a]);
    f.client.setAgentFeature=async(...a)=>f.events.writes.push(['feature',...a]);
    const c={type:'configure',sessionId:'a',commandId:'cfg',model:'hermes:new:exact',runtimeMode:'full-access',modelSettings:{effort:'high','paseo.fast':'true','paseo.service_tier':'default',fast:'false'}};
    await f.host.command(c);
    assert.deepEqual(f.events.writes,[['model','a','new:exact'],['thinking','a','high'],['feature','a','fast',true],['feature','a','service_tier','default']]);
    assert.equal(f.a.currentModeId,undefined); // Never calls setAgentMode on existing agents.
    f.events.writes=[];f.a.status='running';await assert.rejects(f.host.command({...c,commandId:'busy-config'}),/busy/);assert.equal(f.events.writes.length,0);
    f.a.status='idle';await assert.rejects(f.host.command({...c,commandId:'unknown-model',model:'hermes:unknown'}),/Exact model/);
    await assert.rejects(f.host.command({...c,commandId:'unknown-effort',modelSettings:{effort:'invented'}}),/思考选项/);
    assert.equal(f.events.writes.length,0);
    f.client.getLastServerInfoMessage=()=>({serverId:SERVER,features:{agentConfigApply:true}});
    f.client.applyAgentConfig=async(...a)=>f.events.writes.push(['bundle',...a]);
    await f.host.command({...c,commandId:'atomic-bundle'});
    assert.deepEqual(f.events.writes,[['bundle','a',{modelId:'new:exact',thinkingOptionId:'high',featureValues:{fast:true,service_tier:'default'}}]]);
  }finally{f.close();}
});

test('new agents select full-access modes from each daemon provider catalog, including custom aliases',async()=>{
  for(const [provider,id] of [['hermes','dont_ask'],['example-hermes-travel','dont_ask'],['claude-code','bypassPermissions'],['codex','full-access'],['omp','full']]) {
    const f=fixture();try{
      f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
      f.client.listAvailableProviders=async()=>({providers:[{provider,available:true}]});
      f.client.listProviderModels=async()=>({models:[{provider,id:'m',label:'M'}]});
      f.client.listProviderFeatures=async()=>({features:[]});f.client.listProviderModes=async()=>({modes:[{id:'confirm'},{id}]});
      let calls=0;f.client.createAgent=async o=>{calls++;assert.equal(o.config.modeId,id);assert.equal(o.config.provider,provider);return {...f.a,id:o.agentId,provider};};
      await f.host.directory();await f.host.command({type:'create',commandId:'new-'+provider,projectId:'paseo:'+f.directory,workspaceId:'w',harness:harness(provider),model:harness(provider)+':m',modelSettings:{}});
      assert.equal(calls,1);
      f.client.listProviderModes=async()=>({modes:[{id:'confirm'}]});
      await assert.rejects(f.host.command({type:'create',commandId:'no-full',projectId:'paseo:'+f.directory,workspaceId:'w',harness:harness(provider),model:harness(provider)+':m'}),/全权限/);
      assert.equal(calls,1);
    }finally{f.close();}
  }
});

test('default projects exclude missing cwd and archived-only entries; all restores them and recent project is first',async()=>{
  const f=fixture();try{
    const old=join(f.directory,'old'),recent=join(f.directory,'recent'),archived=join(f.directory,'archived'),missing=join(f.directory,'missing');
    for(const cwd of [old,recent,archived])mkdirSync(cwd);
    f.client.fetchAgentHistory=async()=>({entries:[[old,1000,false],[recent,4000,false],[archived,9000,true],[missing,10000,false]].map(([cwd,updatedAt,done])=>({...f.e,agent:{...f.a,id:cwd,cwd,updatedAt,archivedAt:done?'archived':null},project:{...f.e.project,checkout:{cwd}}})),pageInfo:{}});
    const visible=(await f.host.request('projects.list',{})).result;
    assert.deepEqual(visible.map(p=>p.cwd),[recent,old]);
    const all=(await f.host.request('projects.list',{includeAll:true})).result;
    assert.equal(all.length,4);assert.equal(all.filter(p=>!p.exists).length,1);
    assert.equal(f.host.entries.size,4);
  }finally{f.close();}
});

test('models and icons come from official daemon metadata and preserve routed ids',async()=>{
  const f=fixture();try{
    await f.host.directory();let calls=0;
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>{calls++;return {models:[{provider:'hermes',id:'route:exact',label:'Friendly',thinkingOptions:[{id:'high',label:'High',isDefault:true}],defaultThinkingOptionId:'high'}]};};
    f.client.listProviderFeatures=async()=>({features:[{id:'switch',type:'toggle',label:'Switch',value:false}]});
    f.client.getProjectIcon=async id=>({projectId:id,icon:{data:'eA==',mimeType:'image/png'}});
    const response=(await f.host.request('models.list',{projectId:'paseo:/fixture'})).result;
    assert.equal(response.models.hermes[0].id,'hermes:route:exact');assert.equal(response.models.hermes[0].name,'Friendly');
    assert.deepEqual(response.models.hermes[0].settings.map(s=>s.id),['effort','paseo.switch']);
    await f.host.request('models.list',{projectId:'paseo:/fixture'});assert.equal(calls,1);
    assert.equal((await f.host.request('paseo.projectIcons',{})).result['paseo:/fixture'].mimeType,'image/png');
    assert.equal((await f.host.request('projects.list',{includeAll:true,includeIcons:true})).result[0].icon.mimeType,'image/png');
  }finally{f.close();}
});

test('opt-in smoke cleanup can archive only its created agent, never a workspace',async()=>{
  const f=fixture();try{
    await f.host.directory();let archived=0;f.client.archiveAgent=async id=>{assert.equal(id,'a');archived++;return {archivedAt:'synthetic'};};
    f.host.attempts.values.created={state:'accepted',receipt:{sessionId:'a',paseoCreated:true}};
    const request={commandId:'created',sessionId:'a'};
    await assert.rejects(f.host.request('paseo.checkCleanup',request),/restricted/);
    f.host.allowLiveCheck=true;
    await assert.rejects(f.host.request('paseo.checkCleanup',{...request,sessionId:'wrong'}),/restricted/);
    await f.host.request('paseo.checkCleanup',request);assert.equal(archived,1);
  }finally{f.close();}
});

test('unchanged configure keeps an existing off-catalog route usable without discovery or mutation',async()=>{
  const f=fixture();try{
    await f.host.directory();
    await f.host.command({type:'configure',commandId:'same-offlist',sessionId:'a',model:'hermes:opaque:exact-model',runtimeMode:'full-access',modelSettings:{fast:'false'}});
    assert.equal(f.events.writes.length,0);
  }finally{f.close();}
});

test('create accepts the canonical own checkout while retaining recorded cwd for SDK/group identity',async()=>{
  const f=fixture();try{
    const actual=join(f.directory,'root'),recorded=join(f.directory,'RootCase');mkdirSync(actual);symlinkSync(actual,recorded,'dir');
    f.a.cwd=recorded;f.e.project.checkout.cwd=recorded;
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>({models:[{provider:'hermes',id:'m',label:'M'}]});
    f.client.listProviderFeatures=async()=>({features:[]});f.client.listProviderModes=async()=>({modes:[{id:'dont_ask'}]});
    f.client.createAgent=async o=>{assert.equal(o.config.cwd,recorded);return {...f.a,id:o.agentId,cwd:actual};};
    await f.host.directory();
    await f.host.command({type:'create',commandId:'canonical-own-checkout',projectId:'paseo:'+recorded,workspaceId:'w',harness:'hermes',model:'hermes:m',worktreeCwd:actual});
  }finally{f.close();}
});

test('conditional directory returns no rows for an unchanged revision and changes on actual summary updates',async()=>{
  const f=fixture();try{
    f.a.persistence={sessionId:'native-fixture'};
    await f.host.directory();
    const p={projectId:'paseo:/fixture',withRevision:true};
    const first=(await f.host.request('sessions.list',p)).result;
    assert.equal(first.values.length,1);assert.equal(typeof first.revision,'string');
    assert.equal((await f.host.history('a')).snapshot.session.providerSessionId,'native-fixture');
    const unchanged=(await f.host.request('sessions.list',{...p,revision:first.revision})).result;
    assert.deepEqual(unchanged,{revision:first.revision,unchanged:true});
    f.a.updatedAt='2026-10-26T00:00:00Z';f.a.activeTurn={id:'turn'};f.a.pendingPermissions=[{id:'approval'}];f.host.invalidate();
    const next=(await f.host.request('sessions.list',{...p,revision:first.revision})).result;
    assert.notEqual(next.revision,first.revision);assert.equal(next.values.length,1);
    assert.equal(next.structuralRevision,first.structuralRevision,'active status must not change structural revision');
    f.a.model='different-model';f.host.invalidate();
    const configured=(await f.host.request('sessions.list',p)).result;
    assert.notEqual(configured.structuralRevision,next.structuralRevision,'model changes are structural even before a transcript is opened');
    assert.equal(Array.isArray((await f.host.request('sessions.list',{projectId:p.projectId})).result),true);
  }finally{f.close();}
});

test('Helpers navigation unions child cwds while create stays in root workspace and preserves agent authority',async()=>{
  const f=fixture();try{
    const root=join(f.directory,'Helpers'),child=join(root,'bot-example-storage');mkdirSync(child,{recursive:true});
    const agents=[{...f.a,id:'root',provider:'hermes',cwd:root,workspaceId:'root-w',updatedAt:1000},{...f.a,id:'child',cwd:child,workspaceId:'child-w',updatedAt:2000}];
    const entries=agents.map(agent=>({agent,project:{...f.e.project,projectName:'Helpers',checkout:{cwd:agent.cwd,currentBranch:null}}}));
    f.client.fetchAgentHistory=async()=>({entries,pageInfo:{}});
    f.client.fetchWorkspaces=async()=>({entries:agents.map(a=>({id:a.workspaceId,workspaceKind:'directory',name:a.id})),pageInfo:{}});
    f.client.fetchAgent=async id=>({agent:agents.find(a=>a.id===id)});
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>({models:[{provider:'hermes',id:'m',label:'M'}]});
    f.client.listProviderModes=async()=>({modes:[{id:'dont_ask'}]});f.client.listProviderFeatures=async()=>({features:[]});
    let options;f.client.createAgent=async o=>{options=o;return {...agents[0],id:o.agentId};};
    await f.host.directory();assert.equal(f.host.projects.size,1);
    const p=[...f.host.projects.values()][0];assert.equal(p.cwd,root);
    const rows=(await f.host.request('sessions.list',{projectId:p.id})).result;
    assert.deepEqual(rows.map(r=>r.id),['root','child']);assert.equal(rows[1].cwd,child);
    assert.equal((await f.host.target('child')).agent.workspaceId,'child-w');
    await f.host.command({type:'create',commandId:'root-create',projectId:p.id,workspaceId:'root-w',harness:'hermes',model:'hermes:m'});
    assert.equal(options.workspaceId,'root-w');assert.equal(options.config.cwd,root);assert.equal(options.config.modeId,'dont_ask');assert.equal(options.initialPrompt,undefined);
  }finally{f.close();}
});
test('canonical cwd aliases share one stable navigation identity without rewriting daemon cwds',async()=>{
  const f=fixture();try{
    const actual=join(f.directory,'actual'),alias=join(f.directory,'Alias');mkdirSync(actual);symlinkSync(actual,alias,'dir');
    const entries=[actual,alias].map((cwd,i)=>({agent:{...f.a,id:'alias-'+i,cwd},project:{...f.e.project,projectName:'Workspace',checkout:{cwd}}}));
    f.client.fetchAgentHistory=async()=>({entries,pageInfo:{}});await f.host.directory();
    const first=[...f.host.projects.values()][0];assert.equal(f.host.projects.size,1);
    f.client.fetchAgentHistory=async()=>({entries:[...entries].reverse(),pageInfo:{}});await f.host.directory(true);
    assert.equal([...f.host.projects.values()][0].id,first.id);
    assert.equal((await f.host.request('sessions.list',{projectId:first.id})).result.length,2);
    assert.equal(f.host.entries.get('alias-0').agent.cwd,actual);assert.equal(f.host.entries.get('alias-1').agent.cwd,alias);
  }finally{f.close();}
});

test('linked Git worktrees share repository navigation without mixing unrelated same-name projects or execution cwds',async()=>{
  const f=fixture();try{
    const root=join(realpathSync(f.directory),'repo'),branch=join(f.directory,'feature'),other=join(f.directory,'other'),hooks=join(f.directory,'hooks');
    mkdirSync(hooks);mkdirSync(other);
    const git=(...args)=>execFileSync('git',['-c',`core.hooksPath=${hooks}`,...args],{cwd:f.directory,stdio:'pipe'});
    git('init','-q',root);
    git('-C',root,'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-q','--allow-empty','-m','fixture');
    git('-C',root,'worktree','add','--detach',branch,'HEAD');
    const entries=[root,branch,other].map((cwd,i)=>({
      agent:{...f.a,id:['root','child','other'][i],workspaceId:`w-${i}`,cwd,labels:i===1?{'paseo.parent-agent-id':'root'}:{}},
      project:{...f.e.project,projectName:'Same label',checkout:{cwd,currentBranch:i===1?'feature':'main'}},
    }));
    f.client.fetchAgentHistory=async()=>({entries,pageInfo:{}});
    f.client.fetchWorkspaces=async()=>({entries:entries.map(e=>({id:e.agent.workspaceId,name:e.agent.id,workspaceKind:'directory'})),pageInfo:{}});
    f.client.fetchAgent=async id=>({agent:entries.find(e=>e.agent.id===id).agent});
    await f.host.directory();
    const projects=(await f.host.request('projects.list',{})).result;
    assert.deepEqual(projects.map(p=>p.cwd).sort(),[other,root].sort());
    const rows=(await f.host.request('sessions.list',{projectId:`paseo:${root}`})).result;
    assert.deepEqual(rows.map(r=>[r.id,r.cwd,r.paseoParentAgentId]),[['root',root,null],['child',branch,'root']]);
    assert.equal((await f.host.target('child')).agent.workspaceId,'w-1');
    f.client.fetchAgentHistory=async()=>({entries:[...entries].reverse(),pageInfo:{}});
    await f.host.directory(true);
    assert.deepEqual((await f.host.request('sessions.list',{projectId:`paseo:${root}`})).result.map(r=>r.id).sort(),['child','root']);
    f.client.fetchAgentHistory=async()=>({entries:[entries[1]],pageInfo:{}});
    await f.host.directory(true);
    assert.deepEqual((await f.host.request('projects.list',{})).result.map(p=>p.cwd),[root]);
    assert.equal((await f.host.request('sessions.list',{projectId:`paseo:${root}`})).result[0].cwd,branch);
    assert.equal(f.events.sends,0);
  }finally{f.close();}
});

test('runtime server identity is learned once and cannot change after attachment',async()=>{
 const f=fixture();let serverId='synthetic-runtime';f.client.getLastServerInfoMessage=()=>({serverId});
 const host=new PaseoHost(f.client,f.directory);host.connected=true;
 await host.directory();assert.equal(host.serverId,serverId);
 serverId='wrong-runtime';await assert.rejects(host.request('projects.list',{}),/identity/);
 host.close();f.close();
});
test('Paseo bucket changes are volatile and entered time stays stable within a bucket',async()=>{
 const f=fixture();await f.host.directory();
 const query=()=>f.host.request('sessions.list',{projectId:'paseo:/fixture',withRevision:true});
 const before=(await query()).result;const entered=f.host.entries.get('a').paseoStatusEnteredAt;
 f.a.updatedAt='2026-10-25T00:01:00Z';await f.host.directory(true);
 assert.equal(f.host.entries.get('a').paseoStatusEnteredAt,entered);
 f.a.status='running';f.a.activeTurn={turnId:'synthetic'};await f.host.directory(true);
 const running=(await query()).result;assert.equal(f.host.entries.get('a').paseoBucket,'running');
 assert.equal(before.structuralRevision,running.structuralRevision);
 f.a.status='error';f.a.requiresAttention=true;f.a.attentionReason='error';await f.host.directory(true);assert.equal(f.host.entries.get('a').paseoBucket,'failed');
 f.a.requiresAttention=false;await f.host.directory(true);assert.equal(f.host.entries.get('a').paseoBucket,'done');
 f.a.pendingPermissions=[{id:'permission'}];await f.host.directory(true);assert.equal(f.host.entries.get('a').paseoBucket,'needs_input');
 f.close();
});

test('opt-in real daemon creates one isolated idle agent and archives only that agent',
  {skip:process.env.PASEO_LIVE_CREATE_CHECK!=='1',timeout:90000},async()=>{
  const {createClient,readConfig}=await import('./sdk.mjs');
  const expected=readConfig().serverId;
  assert.ok(expected,'Live checks require an explicitly pinned local daemon');
  const directory=mkdtempSync(join(process.env.PASEO_LIVE_CHECK_ROOT||tmpdir(),'paseo-idle-check-'));
  const cwd=join(directory,'workspace');mkdirSync(cwd);
  const client=createClient(),host=new PaseoHost(client,directory);
  let createdId,createdWorkspace;
  try{
    await client.connect();host.connected=true;host.identity();
    assert.equal(host.serverId,expected);
    await host.directory(true);
    const before=new Set([...host.entries.values()].map(e=>e.agent.workspaceId));
    const providers=await host.providers();assert.ok(providers.includes('claude'));
    const models=await host.catalog('claude',cwd);
    const model=models.find(m=>/haiku/i.test(m.id))||models[0];assert.ok(model);
    // Only navigation metadata is bootstrapped; all SDK creation calls are real.
    const projectId='paseo:'+cwd;
    host.projects.set(projectId,{id:projectId,name:'Isolated create check',cwd,exists:true,active:true});
    host.directoryAt=Date.now();
    const receipt=await host.command({type:'create',commandId:'live-idle-'+Date.now(),
      projectId,harness:'claude',model:'claude:'+model.id,runtimeMode:'full-access'});
    createdId=receipt.sessionId;
    const found=await client.fetchAgent(createdId);
    assert.equal(found.agent.id,createdId);
    assert.equal(found.agent.cwd,cwd);
    createdWorkspace=found.agent.workspaceId;
    assert.ok(!before.has(createdWorkspace),'Must not attach to any existing workspace');
    assert.ok(!found.agent.labels?.['paseo.parent-agent-id']);
    assert.ok(!found.agent.activeTurn);
    assert.equal(found.agent.pendingPermissions?.length??0,0);
    await host.directory(true);
    assert.equal([...host.entries.values()].filter(e=>e.agent.workspaceId===createdWorkspace).length,1);
    await host.request('sessions.update',{projectId,sessionId:createdId,archived:true});
    const archived=await client.fetchAgent(createdId);
    assert.ok(archived.agent.archivedAt);
    console.log(JSON.stringify({liveCreateCheck:{serverId:expected,agentId:createdId,
      workspaceId:createdWorkspace,separateWorkspace:true,parent:null,promptSent:false,archived:true,directory}}));
  }finally{
    // Evidence is retained; never delete directories or clean up an unproven ID.
    host.close();
  }
});

test('steer uses exact observed turn and one stable message identity; safe deferral can queue-send later',async()=>{
 const f=fixture(),sent=[];await f.host.directory();
 f.client.sendAgentMessage=async(id,text,options)=>sent.push({id,text,...options});
 const c={type:'send',sessionId:'a',commandId:'steer-once',followUpId:'steer-once',text:'Synthetic steer',activeTurnBehavior:'steer',runId:'t'};
 f.a.activeTurn={turnId:'t'};f.a.status='running';
 await assert.rejects(f.host.command({...c,commandId:'wrong-turn',runId:'other'}),/Turn changed/);
 await assert.rejects(f.host.command({...c,commandId:'normal-busy',activeTurnBehavior:undefined,runId:undefined}),/busy/);
 f.a.pendingPermissions=[{id:'p'}];
 assert.equal((await f.host.command(c)).deferred,true);assert.equal(sent.length,0);
 assert.equal(new Attempts(f.directory).values[c.commandId],undefined);
 f.a.pendingPermissions=[];
 await f.host.command(c);await f.host.command(c);
 assert.deepEqual(sent,[{id:'a',text:'Synthetic steer',messageId:'steer-once',activeTurnBehavior:'steer'}]);
 await assert.rejects(f.host.command({...c,runId:'changed'}),/different content/);
 const ended={...c,commandId:'ended-once',followUpId:'ended-once'};f.a.activeTurn=null;f.a.status='idle';
 assert.equal((await f.host.command(ended)).deferred,true);
 await f.host.command({...ended,activeTurnBehavior:undefined,runId:undefined});
 assert.equal(sent.length,2);assert.equal(sent[1].messageId,'ended-once');assert.equal(sent[1].activeTurnBehavior,'interrupt');f.close();
});
test('unknown steer never replays after adapter restart',async()=>{
 const f=fixture();await f.host.directory();f.a.activeTurn={turnId:'t'};f.a.status='running';
 let sends=0;f.client.sendAgentMessage=async()=>{sends++;throw Error('lost reply');};
 const c={type:'send',sessionId:'a',commandId:'unknown-steer',text:'Synthetic',activeTurnBehavior:'steer',runId:'t'};
 await assert.rejects(f.host.command(c),/unknown/);
 const again=new PaseoHost(f.client,f.directory);again.connected=true;await again.directory();
 await assert.rejects(again.command(c),/No replay/);assert.equal(sends,1);again.close();f.close();
});

test('view attention is identity-checked and idempotent without durable command journal writes',async()=>{
  const f=fixture();try{
    await f.host.directory();f.a.requiresAttention=true;f.a.attentionReason='finished';
    let clears=0;f.client.clearAgentAttention=async()=>{clears++;f.a.requiresAttention=false;};
    let saves=0;const save=f.host.attempts.save.bind(f.host.attempts);
    f.host.attempts.save=()=>{saves++;save();};
    const started=performance.now();
    for(let i=0;i<220;i++)await f.host.command({type:'view',commandId:'view-'+i,sessionId:'a'});
    assert.equal(clears,1);assert.equal(saves,0);assert.equal(Object.keys(f.host.attempts.values).length,0);
    await assert.rejects(f.host.command({type:'view',commandId:'wrong',sessionId:'missing'}),/Unknown Paseo agent/);
    f.a.requiresAttention=true;f.a.attentionReason='permission';
    await f.host.command({type:'view',commandId:'permission',sessionId:'a'});assert.equal(clears,1);
    console.log(JSON.stringify({viewOperations:220,journalWrites:saves,journalEntries:Object.keys(f.host.attempts.values).length,elapsedMs:performance.now()-started}));
  }finally{f.close();}
});

const bucketOf=async(host,id='a')=>(await host.request('sessions.list',{projectId:'paseo:/fixture'})).result.find(v=>v.id===id).paseoBucket;
const seenFile=f=>join(f.directory,'paseo-seen-attention.json');
const readSeen=f=>{try{return JSON.parse(readFileSync(seenFile(f),'utf8'));}catch(e){if(e.code==='ENOENT')return null;throw e;}};

test('view with a clear that never answers returns within the bound and lists done without journal writes',async()=>{
  const f=fixture();try{
    await f.host.directory();f.a.requiresAttention=true;f.a.attentionReason='finished';f.host.clearTimeoutMs=50;
    await f.host.directory(true);assert.equal(await bucketOf(f.host),'attention');
    const before=(await f.host.request('sessions.list',{projectId:'paseo:/fixture',withRevision:true})).result;
    let clears=0;f.client.clearAgentAttention=()=>{clears++;return new Promise(()=>{});};
    let saves=0;const save=f.host.attempts.save.bind(f.host.attempts);f.host.attempts.save=()=>{saves++;save();};
    const started=performance.now();
    const receipt=await f.host.command({type:'view',commandId:'hang-1',sessionId:'a'});
    assert.ok(performance.now()-started<2000);assert.equal(receipt.sessionId,'a');
    await f.host.command({type:'view',commandId:'hang-2',sessionId:'a'});assert.equal(clears,1);
    assert.equal(await bucketOf(f.host),'done');assert.equal(saves,0);assert.equal(Object.keys(f.host.attempts.values).length,0);
    const after=(await f.host.request('sessions.list',{projectId:'paseo:/fixture',withRevision:true})).result;
    assert.notEqual(after.revision,before.revision);assert.equal(after.structuralRevision,before.structuralRevision);
    assert.deepEqual(readSeen(f),[['a',Date.parse(f.a.updatedAt)]]);
  }finally{f.close();}
});
test('a rejecting clear is non-fatal and leaves no unhandled rejection',async()=>{
  const f=fixture();try{
    await f.host.directory();f.a.requiresAttention=true;f.a.attentionReason='finished';
    const unhandled=[];const listener=error=>unhandled.push(error);process.on('unhandledRejection',listener);
    let reject;f.host.clearTimeoutMs=20;f.client.clearAgentAttention=()=>new Promise((_,r)=>{reject=r;});
    await f.host.command({type:'view',commandId:'late-reject',sessionId:'a'});
    reject(Error('late failure'));await new Promise(r=>setTimeout(r,20));process.off('unhandledRejection',listener);
    assert.deepEqual(unhandled,[]);assert.equal(await bucketOf(f.host),'done');
  }finally{f.close();}
});
test('seen overlay survives restart, reappears on a newer finish and is pruned when attention ends',async()=>{
  const f=fixture();try{
    await f.host.directory();f.a.requiresAttention=true;f.a.attentionReason='finished';f.host.clearTimeoutMs=20;
    f.client.clearAgentAttention=()=>new Promise(()=>{});
    await f.host.command({type:'view',commandId:'seen',sessionId:'a'});
    const restarted=new PaseoHost(f.client,f.directory);restarted.connected=true;
    assert.equal(await bucketOf(restarted),'done');
    f.a.updatedAt='2026-10-26T00:00:00Z';
    await restarted.directory(true);assert.equal(await bucketOf(restarted),'attention');
    assert.equal(readSeen(f).length,1);
    restarted.close();
    // Polling with no change never rewrites the file; only dropping an entry does.
    const again=new PaseoHost(f.client,f.directory);again.connected=true;
    let writes=0;const save=again.saveSeen.bind(again);again.saveSeen=()=>{writes++;save();};
    f.a.updatedAt='2026-10-25T00:00:00Z';
    for(let i=0;i<3;i++){await again.directory(true);assert.equal(await bucketOf(again),'done');}
    assert.equal(writes,0);assert.equal(readSeen(f).length,1);
    f.a.requiresAttention=false;
    for(let i=0;i<3;i++){await again.directory(true);assert.equal(await bucketOf(again),'done');}
    assert.equal(writes,1);assert.deepEqual(readSeen(f),[]);
    // Running then finishing again with a newer stamp is a fresh episode.
    f.a.requiresAttention=true;f.a.updatedAt='2026-10-27T00:00:00Z';
    await again.directory(true);assert.equal(await bucketOf(again),'attention');
    again.close();
  }finally{f.close();}
});
test('corrupt seen file is tolerated and permission attention records no overlay',async()=>{
  const f=fixture();try{
    writeFileSync(seenFile(f),'{not json');
    const host=new PaseoHost(f.client,f.directory);host.connected=true;await host.directory();
    f.a.requiresAttention=true;f.a.attentionReason='permission';
    let clears=0;f.client.clearAgentAttention=async()=>{clears++;};
    await host.command({type:'view',commandId:'perm',sessionId:'a'});
    assert.equal(clears,0);assert.equal(host.seen.size,0);assert.equal(readFileSync(seenFile(f),'utf8'),'{not json');
    await host.directory(true);assert.equal(await bucketOf(host),'needs_input');host.close();
  }finally{f.close();}
});
// Fake daemon timeline of `count` entries (seq 1..count, user/assistant alternating, 1 s apart) with real tail/before paging.
function pagedTimeline(f,count,calls=[]){
  const all=Array.from({length:count},(_,i)=>({seqStart:i+1,seqEnd:i+1,timestamp:1000*(i+1),item:{type:i%2?'assistant_message':'user_message',text:`m${i+1}`}}));
  f.client.fetchAgentTimeline=async(id,o)=>{
    calls.push(o);
    const end=o.direction==='before'?all.findIndex(e=>e.seqStart>=o.cursor.seq):all.length,rows=all.slice(Math.max(0,end-o.limit),end);
    return {agent:{...f.a,id},epoch:'e',hasOlder:rows.length>0&&rows[0].seqStart>1,entries:rows,startCursor:rows[0]&&{epoch:'e',seq:rows[0].seqStart}};
  };
  return all;
}
test('historyBefore pages older blocks with window-identical ids and a next cursor until the last page',async()=>{
  const f=fixture();try{
    const calls=[];pagedTimeline(f,250,calls);await f.host.directory();
    const sync=(await f.host.request('sessions.sync',{sessionId:'a'})).result.value,hw=sync.session.historyWindow;
    assert.equal(sync.session.blocks.length,250);assert.deepEqual(hw,{epoch:'e',before:null});
    const first=(await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:201,limit:100})).result;
    assert.equal(first.epoch,'e');assert.equal(first.blocks.length,100);assert.deepEqual(first.blocks.map(b=>b.id),Array.from({length:100},(_,i)=>`e:${101+i}`));
    assert.equal(first.before,101);
    const window=new Map(sync.session.blocks.map(b=>[b.id,b]));
    // Same ids and content as inside the window; only the trailing prompt of a page loses its duration (its turn continues past the boundary).
    first.blocks.forEach((b,i)=>assert.deepEqual(i===98?{...window.get(b.id),durationMs:undefined}:window.get(b.id),i===98?{...b,durationMs:undefined}:b));
    assert.equal(first.blocks[98].role,'user');assert.equal(first.blocks[98].durationMs,undefined);assert.equal(window.get(first.blocks[98].id).durationMs,1000);
    const second=(await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:first.before})).result;
    assert.deepEqual(second.blocks.map(b=>b.id),Array.from({length:100},(_,i)=>`e:${1+i}`));assert.equal(second.before,null);
    const last=calls.at(-1);assert.deepEqual([last.direction,last.cursor,last.limit],['before',{epoch:'e',seq:101},100]);
    const clamped=await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:201,limit:1000});assert.equal(clamped.result.blocks.length,100);
    assert.equal(calls.at(-1).limit,100);
    await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:201,limit:0});assert.equal(calls.at(-1).limit,1);
  }finally{f.close();}
});
test('historyBefore rejects wrong identity, epoch, reset, stale cursor and invalid params without touching state',async()=>{
  const f=fixture();try{
    pagedTimeline(f,10);await f.host.directory();const good=f.client.fetchAgentTimeline;
    const ask=(extra={})=>f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:5,...extra});
    f.client.fetchAgentTimeline=async(...args)=>({...await good(...args),agent:{...f.a,id:'other'}});await assert.rejects(ask(),/history changed/);
    f.client.fetchAgentTimeline=async(...args)=>({...await good(...args),epoch:'f'});await assert.rejects(ask(),/history changed/);
    f.client.fetchAgentTimeline=async(...args)=>({...await good(...args),reset:true});await assert.rejects(ask(),/history changed/);
    f.client.fetchAgentTimeline=async(...args)=>({...await good(...args),staleCursor:true});await assert.rejects(ask(),/history changed/);
    f.client.fetchAgentTimeline=good;
    for(const bad of [{before:-1},{before:1.5},{before:'5'},{before:Number.MAX_SAFE_INTEGER+1},{before:undefined},{epoch:''},{epoch:7},{epoch:undefined}])await assert.rejects(ask(bad),/Invalid history/);
    await assert.rejects(ask({sessionId:'missing'}),/Unknown/);
    assert.equal(f.host.histories.size,0);assert.equal((await ask()).result.blocks.length,4);assert.equal(f.host.histories.size,0);
  }finally{f.close();}
});
test('historyBefore pages carry no permission blocks and no duration for a trailing prompt',async()=>{
  const f=fixture();try{
    f.a.pendingPermissions=[{id:'perm',kind:'tool',name:'bash',title:'Run',description:'d'}];
    const e=(seq,type,at)=>({seqStart:seq,seqEnd:seq,timestamp:at,item:{type,text:`m${seq}`}});
    f.client.fetchAgentTimeline=async id=>({agent:{...f.a,id},epoch:'e',hasOlder:true,entries:[e(10,'user_message',1000),e(11,'assistant_message',2000),e(12,'user_message',3000),e(13,'assistant_message',9000),e(14,'user_message',9500)]});
    await f.host.directory();
    const {blocks,before}=(await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:20})).result;
    assert.equal(before,10);assert.equal(blocks.length,5);assert.ok(!blocks.some(b=>b.id.startsWith('permission:')||b.approval));
    assert.equal(blocks[0].durationMs,1000);assert.equal(blocks[2].durationMs,6000);assert.equal(blocks[4].durationMs,undefined);
    // The same agent state is idle in a window, where the trailing prompt does get its duration.
    f.a.pendingPermissions=[];assert.equal(projectBlocks({epoch:'e',agent:{status:'idle'}},[e(14,'user_message',9500),e(15,'assistant_message',9900)]).at(0).durationMs,400);
    const empty=pagedTimeline(f,0);assert.equal(empty.length,0);
    assert.deepEqual((await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:1})).result,{epoch:'e',blocks:[],before:null});
  }finally{f.close();}
});
test('the sync window stays bounded, reports its cursor, has no history notice and advertises paging',async()=>{
  const f=fixture();try{
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    const all=pagedTimeline(f,2500);await f.host.directory();
    const value=(await f.host.request('sessions.sync',{sessionId:'a'})).result.value;
    assert.ok(value.session.blocks.length<=1000);assert.ok(value.session.blocks.length>=100);
    assert.ok(!value.session.blocks.some(b=>b.id==='paseo-history-window'));
    const h=f.host.histories.get('a');assert.ok(h.entries.length<=1000);
    assert.deepEqual(value.session.historyWindow,{epoch:'e',before:h.entries[0].seqStart});assert.ok(value.session.historyWindow.before>1);
    assert.equal(value.session.blocks[0].id,`e:${h.entries[0].seqStart}`);assert.equal(all.at(-1).seqStart,2500);
    const caps=(await f.host.request('environment.describe',{})).result.capabilities;assert.ok(caps.includes('paseo.history-pages'));
    const page=(await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:value.session.historyWindow.before})).result;
    assert.equal(page.blocks.at(-1).id,`e:${value.session.historyWindow.before-1}`);assert.equal(h.entries[0].seqStart,value.session.historyWindow.before);
  }finally{f.close();}
});
test('paseo.usage is advertised and relays only Codex and Antigravity reports through the daemon client',async()=>{
  const f=fixture();try{
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    const caps=(await f.host.request('environment.describe',{})).result.capabilities;assert.ok(caps.includes('paseo.usage'));
    const seen=[];
    const report=(sourceId,id)=>({id,sourceId,sourceLabel:sourceId,account:{},fetchedAt:'2026-10-25T00:00:00Z',
      report:{status:'available',windows:[{id:'w',label:'Session',usedPct:30,resetsAt:'2026-10-25T05:00:00Z'}]}});
    f.client.listUsageReports=async(options,onReport)=>{seen.push(options);onReport(report('grok','g'));return {requestId:'r',reports:[report('antigravity','a'),report('codex','c'),report('grok','g')]};};
    const out=(await f.host.request('paseo.usage',{forceRefresh:true})).result;
    assert.deepEqual(seen,[{forceRefresh:true}]);
    assert.deepEqual(out.sources.map(s=>s.sourceId),['codex','antigravity']);
    assert.equal(out.sources[0].windows[0].remainingPct,70);
    await f.host.request('paseo.usage',{});assert.deepEqual(seen.at(-1),{forceRefresh:false});
  }finally{f.close();}
});

// F1: image attachments
{
const ATT='33333333-3333-4333-8333-333333333333';
const PNG=Buffer.from([0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,1,2,3,4,5]);
const upload=(host,id=ATT,bytes=PNG)=>host.request('attachments.upload',{id,offset:0,size:bytes.length,data:bytes.toString('base64')});
const imageRef=(over={})=>({id:ATT,name:'shot.png',mimeType:'image/png',kind:'image',size:PNG.length,...over});
test('environment.describe advertises attachment upload and read',async()=>{
  const f=fixture();f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
  const {result}=await f.host.request('environment.describe',{});
  assert.ok(result.capabilities.includes('attachments.upload'));assert.ok(result.capabilities.includes('attachments.read'));f.close();
});
test('uploaded image reaches the daemon once, retries return the receipt, and the transcript keeps a readable attachment',async()=>{
  const f=fixture(),sent=[];await f.host.directory();
  f.client.sendAgentMessage=async(id,text,options)=>{f.events.sends++;sent.push({id,text,...options});};
  f.client.fetchAgentTimeline=async()=>({agent:f.a,epoch:'e',hasOlder:false,entries:[
    {seqStart:1,seqEnd:1,timestamp:'2026-10-25T00:00:00Z',item:{type:'user_message',text:'',clientMessageId:'img-1'}},
    {seqStart:2,seqEnd:2,timestamp:'2026-10-25T00:00:01Z',item:{type:'user_message',text:'plain',clientMessageId:'other'}}]});
  assert.deepEqual((await upload(f.host)).result,{offset:PNG.length});
  const command={type:'send',sessionId:'a',commandId:'img-1',text:'',attachments:[imageRef()]};
  const receipt=await f.host.command(command);
  assert.deepEqual(sent,[{id:'a',text:'',messageId:'img-1',activeTurnBehavior:'interrupt',images:[{data:PNG.toString('base64'),mimeType:'image/png'}]}]);
  assert.deepEqual(await f.host.command(command),receipt);assert.equal(f.events.sends,1);
  const blocks=(await f.host.history('a')).snapshot.session.blocks;
  assert.deepEqual(blocks[0].attachments,[imageRef()]);assert.equal(blocks[1].attachments,undefined);
  const chunk=(await f.host.request('attachments.read',{sessionId:'a',id:ATT,offset:0})).result;
  assert.deepEqual({...chunk,data:Buffer.from(chunk.data,'base64')},{data:PNG,offset:PNG.length,size:PNG.length});
  await assert.rejects(f.host.request('attachments.read',{sessionId:'b',id:ATT,offset:0}),/not found/);
  // Older history pages also preserve bound image attachments for the same session.
  f.client.fetchAgentTimeline=async()=>({agent:f.a,epoch:'e',hasOlder:false,entries:[
    {seqStart:1,seqEnd:1,timestamp:'2026-10-25T00:00:00Z',item:{type:'user_message',text:'',clientMessageId:'img-1'}}]});
  const older=(await f.host.request('sessions.historyBefore',{sessionId:'a',epoch:'e',before:2,limit:10})).result;
  assert.deepEqual(older.blocks[0].attachments,[imageRef()]);
  // The binding survives an adapter restart.
  const again=new PaseoHost(f.client,f.directory);
  assert.deepEqual(again.attachments.forMessage('a','img-1'),[imageRef()]);again.close();
  f.close();
});
test('non-image, incomplete, unsupported and steer attachments never reach the daemon',async()=>{
  const f=fixture();await f.host.directory();await upload(f.host);
  const send=over=>f.host.command({type:'send',sessionId:'a',commandId:`c-${Math.random()}`,text:'x',...over});
  await assert.rejects(send({attachments:[imageRef({kind:'file',mimeType:'application/pdf'})]}),/仅支持图片/);
  await assert.rejects(send({attachments:[imageRef({mimeType:'image/svg+xml'})]}),/仅支持图片/);
  await assert.rejects(send({attachments:[imageRef({size:PNG.length+1})]}),/incomplete/);
  await assert.rejects(send({attachments:[imageRef()],intent:'plan'}),/计划或草稿/);
  await assert.rejects(send({attachments:[imageRef()],draftBlockId:'d'}),/计划或草稿/);
  await assert.rejects(send({attachments:[imageRef()],planBlockId:'p'}),/计划或草稿/);
  f.a.activeTurn={turnId:'t'};f.a.status='running';
  await assert.rejects(send({attachments:[imageRef()],activeTurnBehavior:'steer',runId:'t'}),/仅支持纯文本/);
  assert.equal(f.events.sends,0);f.close();
});
}

// F6: Paseo worktrees discovery, creation and reuse
test('paseo.worktrees is advertised, merges checkout worktrees/branches/workspaces and handles non-git directories gracefully',async()=>{
  const f=fixture();try{
    f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    const caps=(await f.host.request('environment.describe',{})).result.capabilities;
    assert.ok(caps.includes('paseo.worktrees'));
    const wtDir=join(f.directory,'wt-feature');mkdirSync(wtDir);
    f.client.listCheckoutWorktrees=async()=>[
      {path:f.directory,branchName:'main',head:'aaa',createdAt:'2026-10-25T00:00:00Z',containsCwd:true},
      {path:wtDir,branchName:'feat/wt',head:'bbb',createdAt:'2026-10-26T00:00:00Z',containsCwd:false},
    ];
    f.client.getPaseoWorktreeList=async()=>({worktrees:[{worktreePath:wtDir}]});
    f.client.getBranchSuggestions=async()=>({branches:['main','feat/wt','release']});
    const info=(await f.host.request('paseo.worktrees',{projectId:'paseo:'+f.directory})).result;
    assert.equal(info.isGitRepo,true);assert.equal(info.currentBranch,'main');
    assert.deepEqual(info.branches,['main','feat/wt','release']);
    assert.equal(info.worktrees.length,2);
    assert.equal(info.worktrees[0].workspaceId,'w');assert.equal(info.worktrees[0].containsCwd,true);
    assert.equal(info.worktrees[1].isPaseoOwned,true);assert.equal(info.worktrees[1].workspaceId,null);
    f.client.listCheckoutWorktrees=async()=>{throw Error('not a git repo');};
    f.client.getPaseoWorktreeList=async()=>({worktrees:[],error:'not a git repo'});
    f.client.getBranchSuggestions=async()=>({branches:[],error:'not a git repo'});
    const plain=(await f.host.request('paseo.worktrees',{projectId:'paseo:'+f.directory})).result;
    assert.equal(plain.isGitRepo,false);assert.deepEqual(plain.worktrees,[]);assert.deepEqual(plain.branches,[]);
  }finally{f.close();}
});

test('create with worktree delegates to Paseo createWorkspace(kind: worktree), binds workspaceDirectory to agent cwd, and preserves Attempts no-replay',async()=>{
  const f=fixture();try{
    f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
    const wtDir=join(f.directory,'paseo-wt-feat');mkdirSync(wtDir);
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>({models:[{provider:'hermes',id:'m',label:'M'}]});
    f.client.listProviderModes=async()=>({modes:[{id:'dont_ask'}]});f.client.listProviderFeatures=async()=>({features:[]});
    let workspaceCalls=0,agentCalls=0,wsOpts,agOpts;
    f.client.createWorkspace=async o=>{
      workspaceCalls++;wsOpts=o;
      return {workspace:{id:'ws-wt-1',name:'feat/wt',workspaceKind:'worktree',worktreeSlug:'feat-wt',workspaceDirectory:wtDir},error:null};
    };
    f.client.createAgent=async o=>{
      agentCalls++;agOpts=o;
      return {...f.a,id:o.agentId,workspaceId:o.workspaceId,cwd:o.config.cwd};
    };
    const create={type:'create',commandId:'wt-create-1',projectId:'paseo:'+f.directory,harness:'hermes',model:'hermes:m',
      worktree:{action:'branch-off',branchName:'feat/wt',baseBranch:'main'},
      firstAgentContext:{prompt:'Build worktree flow',attachments:[]}};
    const receipt=await f.host.command(create);
    assert.equal(workspaceCalls,1);assert.equal(agentCalls,1);
    assert.deepEqual(wsOpts.source,{kind:'worktree',cwd:f.directory,action:'branch-off',branchName:'feat/wt',baseBranch:'main',refName:'main'});
    assert.deepEqual(wsOpts.firstAgentContext,{prompt:'Build worktree flow',attachments:[]});
    assert.equal(wsOpts.idempotencyKey,'wt-create-1');
    assert.equal(agOpts.workspaceId,'ws-wt-1');assert.equal(agOpts.config.cwd,wtDir);
    assert.equal(agOpts.idempotencyKey,'wt-create-1:agent');
    assert.equal(f.host.entries.get(receipt.sessionId).agent.cwd,wtDir);
    assert.deepEqual(await f.host.command(create),receipt);
    assert.equal(workspaceCalls,1);assert.equal(agentCalls,1);
    f.client.createWorkspace=async()=>{workspaceCalls++;throw Error('lost worktree create ack');};
    const lost={...create,commandId:'wt-create-lost'};
    await assert.rejects(f.host.command(lost),/lost worktree create ack/);
    const restored=new PaseoHost(f.client,f.directory);restored.connected=true;
    await assert.rejects(restored.command(lost),/No replay/);
    assert.equal(workspaceCalls,2);
  }finally{f.close();}
});

test('create rejects invalid branch names, conflicting workspaceId+worktree, or unverified worktreeCwd with zero mutations and accepts verified worktreeCwd',async()=>{
  const f=fixture();try{
    f.a.cwd=f.directory;f.e.project.checkout.cwd=f.directory;
    const verifiedWt=join(f.directory,'existing-wt');mkdirSync(verifiedWt);
    const unverifiedDir=join(f.directory,'not-a-wt');mkdirSync(unverifiedDir);
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    f.client.listProviderModels=async()=>({models:[{provider:'hermes',id:'m',label:'M'}]});
    f.client.listProviderModes=async()=>({modes:[{id:'dont_ask'}]});f.client.listProviderFeatures=async()=>({features:[]});
    f.client.listCheckoutWorktrees=async()=>[
      {path:f.directory,branchName:'main',containsCwd:true},
      {path:verifiedWt,branchName:'feat/existing',containsCwd:false},
    ];
    let mutations=0,wsOpts,agOpts;
    f.client.createWorkspace=async o=>{mutations++;wsOpts=o;return {workspace:{id:'ws-existing-wt',name:'Existing wt',workspaceKind:'directory',workspaceDirectory:o.source.path},error:null};};
    f.client.createAgent=async o=>{mutations++;agOpts=o;return {...f.a,id:o.agentId,workspaceId:o.workspaceId,cwd:o.config.cwd};};
    const base={type:'create',projectId:'paseo:'+f.directory,harness:'hermes',model:'hermes:m'};
    await assert.rejects(f.host.command({...base,commandId:'bad-1',worktree:{action:'branch-off',branchName:'../bad'}}),/分支名称格式无效/);
    await assert.rejects(f.host.command({...base,commandId:'bad-2',worktree:{action:'branch-off',branchName:'foo bar'}}),/分支名称格式无效/);
    await assert.rejects(f.host.command({...base,commandId:'bad-3',workspaceId:'w',worktree:{action:'branch-off'}}),/不能同时指定/);
    await assert.rejects(f.host.command({...base,commandId:'bad-4',worktreeCwd:verifiedWt,worktree:{action:'branch-off'}}),/不能同时指定/);
    await assert.rejects(f.host.command({...base,commandId:'bad-5',worktreeCwd:unverifiedDir}),/不属于该项目的 Git worktree/);
    assert.equal(mutations,0);
    const receipt=await f.host.command({...base,commandId:'good-existing-wt',worktreeCwd:verifiedWt});
    assert.equal(mutations,2);
    assert.deepEqual(wsOpts.source,{kind:'directory',path:verifiedWt});
    assert.equal(agOpts.config.cwd,verifiedWt);
    assert.equal(f.host.entries.get(receipt.sessionId).agent.cwd,verifiedWt);
  }finally{f.close();}
});

// F5: Paseo native rewind
{
test('projectBlocks emits paseoMessageId preferring providerMessageId and preserves >32KB user message IDs',()=>{
  const huge='u'.repeat(35000);
  const blocks=projectBlocks({epoch:'e',agent:{status:'idle'},entries:[
    {seqStart:1,seqEnd:1,timestamp:1000,providerMessageId:'prov-1',item:{type:'user_message',text:'first',messageId:'client-1',clientMessageId:'client-1'}},
    {seqStart:2,seqEnd:2,timestamp:2000,item:{type:'assistant_message',text:'reply-1',messageId:'asst-1'}},
    {seqStart:3,seqEnd:3,timestamp:3000,item:{type:'user_message',text:huge,messageId:'msg-huge',clientMessageId:'msg-huge'}},
    {seqStart:4,seqEnd:4,timestamp:4000,item:{type:'user_message',text:'legacy-no-id'}}]});
  assert.equal(blocks[0].paseoMessageId,'prov-1');
  assert.equal(blocks[0].paseoTruncated,undefined);
  assert.equal(blocks[1].paseoMessageId,undefined);
  assert.equal(blocks[2].paseoMessageId,'msg-huge');
  assert.equal(blocks[2].paseoTruncated,true);
  assert.equal(blocks[3].paseoMessageId,undefined);
});

test('environment.describe advertises paseo.rewind and sessions.sync exposes paseoRewindModes with feature gating',async()=>{
  const f=fixture();try{
    f.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
    const caps=(await f.host.request('environment.describe',{})).result.capabilities;
    assert.ok(caps.includes('paseo.rewind'));
    f.a.capabilities={supportsRewindConversation:true,supportsRewindFiles:false,supportsRewindBoth:true};
    const first=(await f.host.request('sessions.sync',{sessionId:'a'})).result.value;
    assert.deepEqual(first.session.paseoRewindModes,['conversation','both']);
    f.client.getLastServerInfoMessage=()=>({serverId:SERVER,version:'synthetic',features:{rewind:false}});
    f.host.invalidate();
    const disabled=(await f.host.request('sessions.sync',{sessionId:'a'})).result.value;
    assert.deepEqual(disabled.session.paseoRewindModes,[]);
  }finally{f.close();}
});

test('paseo.rewind refuses busy, awaiting-permission, archived, unsupported-mode and invalid calls with zero daemon mutations',async()=>{
  const f=fixture(),rewinds=[];try{
    f.client.rewindAgent=async(...args)=>{rewinds.push(args);return {ok:true};};
    f.a.capabilities={supportsRewindConversation:true,supportsRewindFiles:false,supportsRewindBoth:false};
    await f.host.directory();
    // Busy: activeTurn
    f.a.activeTurn={turnId:'t1'};
    await assert.rejects(f.host.request('paseo.rewind',{commandId:'rw-busy-turn',sessionId:'a',messageId:'m1',mode:'conversation'}),/正在运行|空闲/);
    // Busy: running status
    f.a.activeTurn=null;f.a.status='running';
    await assert.rejects(f.host.request('paseo.rewind',{commandId:'rw-busy-status',sessionId:'a',messageId:'m1',mode:'conversation'}),/正在运行|空闲/);
    // Busy: pending permission
    f.a.status='idle';f.a.pendingPermissions=[{id:'p1',kind:'tool'}];
    await assert.rejects(f.host.request('paseo.rewind',{commandId:'rw-busy-perm',sessionId:'a',messageId:'m1',mode:'conversation'}),/正在运行|等待审批/);
    f.a.pendingPermissions=[];
    // Archived
    f.a.archivedAt='2026-10-25T00:00:00Z';
    await assert.rejects(f.host.request('paseo.rewind',{commandId:'rw-archived',sessionId:'a',messageId:'m1',mode:'conversation'}),/已归档/);
    f.a.archivedAt=null;
    // Unsupported mode
    await assert.rejects(f.host.request('paseo.rewind',{commandId:'rw-bad-mode',sessionId:'a',messageId:'m1',mode:'both'}),/不支持此回退模式/);
    // Missing params
    await assert.rejects(f.host.request('paseo.rewind',{commandId:'',sessionId:'a',messageId:'m1',mode:'conversation'}),/Missing command/);
    await assert.rejects(f.host.request('paseo.rewind',{commandId:'rw-no-msg',sessionId:'a',messageId:'',mode:'conversation'}),/messageId/);
    assert.equal(rewinds.length,0);
  }finally{f.close();}
});

test('paseo.rewind succeeds when idle, forces fresh snapshot on next sessions.sync, and enforces Attempts idempotency',async()=>{
  const f=fixture(),rewinds=[];try{
    let epoch='e1';
    let entries=[
      {seqStart:1,seqEnd:1,timestamp:1000,providerMessageId:'pm-1',item:{type:'user_message',text:'Keep me'}},
      {seqStart:2,seqEnd:2,timestamp:2000,item:{type:'assistant_message',text:'Reply 1'}},
      {seqStart:3,seqEnd:3,timestamp:3000,providerMessageId:'pm-2',item:{type:'user_message',text:'Rewind me'}},
      {seqStart:4,seqEnd:4,timestamp:4000,item:{type:'assistant_message',text:'Reply 2'}}];
    f.a.capabilities={supportsRewindConversation:true,supportsRewindFiles:true,supportsRewindBoth:true};
    f.client.fetchAgentTimeline=async()=>({agent:f.a,epoch,hasOlder:false,entries});
    f.client.rewindAgent=async(agentId,messageId,mode)=>{
      rewinds.push([agentId,messageId,mode]);
      if(mode!=='files'){
        epoch='e2';
        entries=entries.slice(0,2);
      }
      return {ok:true};
    };
    await f.host.directory();
    const initial=(await f.host.request('sessions.sync',{sessionId:'a'})).result;
    assert.equal(initial.kind,'snapshot');
    assert.equal(initial.value.session.blocks.length,4);
    assert.equal(initial.value.session.blocks[2].paseoMessageId,'pm-2');

    const cmd={commandId:'rw-ok-1',sessionId:'a',messageId:'pm-2',mode:'both'};
    const receipt=(await f.host.request('paseo.rewind',cmd)).result;
    assert.equal(receipt.ok,true);
    assert.equal(receipt.commandId,'rw-ok-1');
    assert.deepEqual(rewinds,[['a','pm-2','both']]);

    // Duplicate commandId returns cached receipt without calling rewindAgent twice.
    const dup=(await f.host.request('paseo.rewind',cmd)).result;
    assert.deepEqual(dup,receipt);
    assert.equal(rewinds.length,1);

    // Next sessions.sync returns full 'snapshot' (never 'delta' against pre-rewind revision!).
    const after=(await f.host.request('sessions.sync',{sessionId:'a',revision:initial.value.revision})).result;
    assert.equal(after.kind,'snapshot');
    assert.equal(after.value.session.blocks.length,2);
    assert.equal(after.value.session.historyWindow.epoch,'e2');

    // Unknown/failed rewind attempt is never replayed across restart.
    f.client.rewindAgent=async()=>{rewinds.push(['lost']);throw Error('network lost');};
    const lostCmd={commandId:'rw-lost',sessionId:'a',messageId:'pm-1',mode:'conversation'};
    await assert.rejects(f.host.request('paseo.rewind',lostCmd),/network lost/);
    const restarted=new PaseoHost(f.client,f.directory);restarted.connected=true;
    await assert.rejects(restarted.request('paseo.rewind',lostCmd),/No replay/);
    assert.equal(rewinds.length,2);
    restarted.close();
  }finally{f.close();}
});
}

// F7: multi-host isolation & routed loopback servers
import {LOCAL_FS_CAPABILITIES,BASE_CAPABILITIES} from './host-adapter.mjs';
import {HostRegistry} from './hosts.mjs';
import {PASEO_CLIENT_CAPABILITIES,createRoutedClient} from './sdk.mjs';
import {createHostServer} from '../../../host/server.ts';
{
const REMOTE_CWD='/home/bianrenping.juns/work/code/ai_scope'; // Non-existent Linux path on macOS.
const TOKEN='B'.repeat(43);

function remoteFixture(serverId='srv-devbox-boe') {
  const directory=mkdtempSync(join(tmpdir(),'paseo-remote-host-test-'));
  const a={id:'remote-agent-1',provider:'codex',cwd:REMOTE_CWD,workspaceId:'w-remote',status:'idle',model:'gpt-5.4',updatedAt:'2026-11-24T00:00:00Z',pendingPermissions:[],activeTurn:null};
  const e={agent:a,project:{projectKey:'pk-ai-scope',projectName:'ai_scope',workspaceName:'Remote WS',checkout:{cwd:REMOTE_CWD,currentBranch:'main'}}};
  const client={
    getLastServerInfoMessage:()=>({serverId,version:'0.17.4'}),
    fetchAgentHistory:async()=>({entries:[e],pageInfo:{nextCursor:null}}),
    fetchWorkspaces:async()=>({entries:[{id:'w-remote',name:'Remote WS',title:'Remote WS',workspaceKind:'directory'}],pageInfo:{}}),
    fetchAgent:async id=>({agent:{...a,id}}),
    fetchAgentTimeline:async()=>({agent:a,epoch:'e1',hasOlder:false,entries:[
      {seqStart:1,seqEnd:1,timestamp:'2026-11-24T00:00:00Z',item:{type:'user_message',text:'Hello devbox'}},
      {seqStart:2,seqEnd:2,timestamp:'2026-11-24T00:00:02Z',item:{type:'assistant_message',text:'Hello from devbox'}}]}),
    subscribeAgentTimeline:()=>Object.assign(()=>{},{ready:Promise.resolve()}),
    listAvailableProviders:async()=>({providers:[{provider:'codex',available:true}]}),
    listProviderModels:async()=>({models:[{provider:'codex',id:'gpt-5.4',label:'GPT-5.4'}]}),
    listProviderModes:async()=>({modes:[{id:'full-access'}]}),
    listProviderFeatures:async()=>({features:[]}),
    getProjectIcon:async id=>({projectId:id,icon:null}),
    close(){},
  };
  const host=new PaseoHost(client,directory,rows=>rows,{isRemoteHost:true,expectedServerId:serverId});
  host.connected=true;
  return {host,client,a,e,directory,close(){host.close();rmSync(directory,{recursive:true,force:true});}};
}

test('remote PaseoHost omits local FS capabilities, skips Mac stat/realpath, creates remotely, and never returns null to host/server.ts',async()=>{
  assert.equal(typeof PASEO_CLIENT_CAPABILITIES,'object');
  assert.equal(typeof createRoutedClient,'function');
  const rf=remoteFixture('srv-devbox-boe');
  const store={
    get environmentId(){return rf.host.serverId;},
    authenticated:v=>v===TOKEN,
    projects:()=>rf.host.fileProjects(),
    project:id=>rf.host.fileProject(id),
  };
  const server=createHostServer({store},[],undefined,rf.host);
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const endpoint=`http://127.0.0.1:${server.address().port}`;
  const rpc=async(method,params={})=>{
    const res=await fetch(`${endpoint}/rpc`,{
      method:'POST',
      headers:{Authorization:`Bearer ${TOKEN}`,'Content-Type':'application/json'},
      body:JSON.stringify({version:1,environmentId:'srv-devbox-boe',method,params}),
    });
    return {status:res.status,body:await res.json()};
  };
  try{
    const desc=(await rpc('environment.describe')).body.result;
    assert.equal(desc.environmentId,'srv-devbox-boe');
    assert.deepEqual(desc.capabilities,BASE_CAPABILITIES);
    for(const cap of [...LOCAL_FS_CAPABILITIES,'paseo.hosts']){
      assert.equal(desc.capabilities.includes(cap),false,`remote host must not advertise ${cap}`);
    }

    // Remote Linux paths work without touching Mac fs.
    const visible=(await rpc('projects.list',{})).body.result;
    assert.equal(visible.length,1);
    assert.equal(visible[0].cwd,REMOTE_CWD);
    assert.equal(visible[0].exists,true);
    assert.deepEqual(rf.host.fileProjects(),[]);
    assert.throws(()=>rf.host.fileProject(`paseo:${REMOTE_CWD}`),/not mounted/);

    // Every local FS / Git / workspace method explicitly errors and NEVER falls back to host/server.ts.
    for(const method of ['workspace.run','files.read','files.list','files.index','files.search','files.searchContent','git.diff','git.branches','git.worktrees','git.index','git.fileDiff','projects.browse']){
      const out=await rpc(method,{projectId:`paseo:${REMOTE_CWD}`,command:'list_dir',path:REMOTE_CWD});
      assert.equal(out.status,400,`expected 400 rejection for ${method}`);
      assert.match(out.body.error,/远程宿主机暂不支持/);
    }

    // Nested host routing from a remote host is refused.
    assert.equal((await rpc('paseo.hosts.list')).body.result.supported,false);
    assert.equal((await rpc('paseo.hosts.connect',{serverId:'srv-devbox-ai'})).status,400);

    // Remote create uses POSIX path comparison (never canonicalPath on Mac).
    let createdWorkspace=null,createdAgent=null;
    rf.client.createWorkspace=async o=>{createdWorkspace=o;return {workspace:{id:'w-new-remote',name:'New Remote',workspaceDirectory:REMOTE_CWD},error:null};};
    rf.client.createAgent=async o=>{createdAgent=o;return {...rf.a,id:o.agentId,workspaceId:'w-new-remote',cwd:`${REMOTE_CWD}/`};};
    const created=(await rpc('commands.dispatch',{
      type:'create',
      commandId:'remote-create-1',
      projectId:`paseo:${REMOTE_CWD}`,
      harness:'codex',
      model:'codex:gpt-5.4',
      worktreeCwd:`${REMOTE_CWD}/`,
    })).body.result;
    assert.equal(created.paseoCreated,true);
    assert.equal(createdWorkspace.source.path,REMOTE_CWD);
    assert.equal(createdAgent.config.cwd,REMOTE_CWD);
  }finally{
    server.close();
    server.closeAllConnections?.();
    rf.close();
  }
});

test('primary PaseoHost dispatches paseo.hosts.list and paseo.hosts.connect via HostRegistry',async()=>{
  const lf=fixture();
  const rf=remoteFixture('srv-devbox-ai');
  lf.client.getLastServerInfoMessage=()=>({serverId:SERVER,version:'0.17.4',features:{connectedHosts:true}});
  lf.client.listHosts=async()=>({
    self:{serverId:SERVER,label:'Local Mac'},
    hosts:[{registrationId:'r-ai',serverId:'srv-devbox-ai',label:'devbox-ai',enabled:true,status:'online',targetDescription:'ssh://devbox-ai',lastOnlineAt:null,error:null}],
  });
  lf.client.openHostRoute=async()=>'route-ai-1';
  lf.client.listAvailableProviders=async()=>({providers:[{provider:'hermes',available:true}]});
  const registry=new HostRegistry({
    originClient:lf.client,
    baseDirectory:lf.directory,
    token:TOKEN,
    createRoutedClient:()=>({...rf.client,connect:async()=>{},setReconnectEnabled(){},subscribeConnectionStatus:()=>()=>{},subscribeRawMessages:()=>()=>{}}),
    createAdapter:(c,dir,opts)=>new PaseoHost(c,dir,rows=>rows,opts),
    createServer:(store,adapter)=>createHostServer({store},[],undefined,adapter),
  });
  lf.host.hostRegistry=registry;
  try{
    const localCaps=(await lf.host.request('environment.describe',{})).result.capabilities;
    assert.ok(localCaps.includes('paseo.hosts'));
    const listed=(await lf.host.request('paseo.hosts.list',{})).result;
    assert.equal(listed.supported,true);
    assert.equal(listed.hosts[0].label,'devbox-ai');
    const connected=(await lf.host.request('paseo.hosts.connect',{serverId:'srv-devbox-ai'})).result;
    assert.equal(connected.serverId,'srv-devbox-ai');
    assert.equal(connected.status,'online');
    assert.match(connected.endpoint,/^http:\/\/127\.0\.0\.1:\d+$/);
    const remoteProjectsRes=await fetch(`${connected.endpoint}/rpc`,{
      method:'POST',
      headers:{Authorization:`Bearer ${TOKEN}`,'Content-Type':'application/json'},
      body:JSON.stringify({version:1,environmentId:'srv-devbox-ai',method:'projects.list',params:{}}),
    });
    const remoteProjects=(await remoteProjectsRes.json()).result;
    assert.equal(remoteProjects.length,1);
    assert.equal(remoteProjects[0].cwd,REMOTE_CWD);
  }finally{
    registry.close();
    rf.close();
    lf.close();
  }
});
}
