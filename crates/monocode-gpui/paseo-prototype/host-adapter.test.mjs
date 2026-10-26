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
    await assert.rejects(f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'child',pinned:true}),/主任务/);
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
test('rename/pin/archive use exact workspace guards; risky archive and delete refuse',async()=>{
  const f=fixture();await f.host.directory();
  f.client.setWorkspaceTitle=async(...a)=>f.events.writes.push(['title',...a]);
  f.client.setWorkspacePinned=async(...a)=>f.events.writes.push(['pin',...a]);
  await f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',title:'new'});
  await f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',pinned:true});
  assert.deepEqual(f.events.writes,[['title','w','new'],['pin','w',true]]);
  await assert.rejects(f.host.request('sessions.update',{projectId:'wrong',sessionId:'a',pinned:true}),/Wrong project/);
  f.client.fetchWorkspaces=async()=>({entries:[{id:'w',workspaceKind:'worktree',worktreeSlug:'owned'}],pageInfo:{}});
  await assert.rejects(f.host.request('sessions.update',{projectId:'paseo:/fixture',sessionId:'a',archived:true}),/Paseo/);
  await assert.rejects(f.host.request('sessions.delete',{sessionId:'a'}),/删除/);f.close();
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
 f.a.status='error';await f.host.directory(true);assert.equal(f.host.entries.get('a').paseoBucket,'failed');
 f.a.pendingPermissions=[{id:'permission'}];await f.host.directory(true);assert.equal(f.host.entries.get('a').paseoBucket,'needs_input');
 f.close();
});

test('opt-in real daemon creates one isolated idle agent and archives only its own workspace',
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
