// Paseo owns execution/history. This module translates the existing Host contract.
import {deriveAgentStateBucket} from 'paseo-source/packages/protocol/src/agent-state-bucket.ts';
import {createHash} from 'node:crypto';
import {readFileSync,writeFileSync,renameSync,mkdirSync,realpathSync,statSync} from 'node:fs';
import {join,isAbsolute,resolve,dirname,basename} from 'node:path';
import {Controller,boundedEntries,joinEntries} from './controller.mjs';
import {parseQuestionFormQuestions} from 'paseo-source/packages/app/src/components/question-form-card-core.ts';
import {fullAccessMode,nativeModel,nativeSettings,configuration} from './configuration.mjs';
import {SyncTransfers} from '../../../host/sync-transfer.ts';

export const SERVER='fixture-server'; // Synthetic test identity; runtime learns and pins the handshake.
const WORKSPACE_CHOICE_LIMIT=20; // Menu shows recent workspaces; creation still validates the full list.
export const CAPABILITIES=['sessions','approvals','questions','paseo.preserve-prose','paseo.configure','paseo.project-icons','paseo.workspaces','sessions.no-delete','sessions.list-revision',
  'files.read','files.list','files.index','files.search','files.searchContent','diff','git.branches','git.worktrees',
  'git.index','git.fileDiff','workspace.run'];
const stamp=value=>{const n=typeof value==='number'?value:Date.parse(value);return Number.isFinite(n)?n:0;};
// Only the filesystem boundary canonicalizes paths; recorded Paseo identity stays intact.
function canonicalPath(path) {
  try{return realpathSync.native(path);}
  catch(error){
    if(['ENOENT','ENOTDIR'].includes(error.code))throw Object.assign(Error('目录或文件已不存在，请在 Paseo 中确认项目路径。'),{code:error.code});
    throw error;
  }
}
function navigationPath(path) {
  try{return realpathSync.native(path);}
  catch(error){if(['ENOENT','ENOTDIR'].includes(error.code))return path;throw error;}
}
function linkedRepositoryRoot(cwd) {
  try{
    const marker=readFileSync(join(cwd,'.git'),'utf8').trim().match(/^gitdir:\s*(.+)$/);
    if(!marker)return null;
    const git=resolve(cwd,marker[1]),common=resolve(git,readFileSync(join(git,'commondir'),'utf8').trim());
    return basename(common)==='.git'?navigationPath(dirname(common)):null;
  }catch(error){if(['ENOENT','ENOTDIR','EISDIR'].includes(error.code))return null;throw error;}
}
export function harness(provider='') {
  if(provider==='hermes'||provider.includes('-hermes-'))return 'hermes';
  return provider==='claude-code'?'claude':provider;
}
const mode=agent=>['dont_ask','bypassPermissions','full-access','full'].includes(agent.currentModeId)?'full-access':'supervised';
export function summary(entry,projectId) {
  const a=entry.agent,provider=harness(a.provider),cwd=a.cwd||entry.project.checkout.cwd;
  return {id:a.id,projectId,title:((a.labels?.['paseo.parent-agent-id']||(entry.paseoWorkspaceAnchorId&&entry.paseoWorkspaceAnchorId!==a.id))&&a.title)||entry.workspace?.title||entry.project.workspaceName||a.title||'Paseo agent',harness:provider,cwd,
    model:`${provider}:${a.model||''}`,runtimeMode:mode(a),createdAt:stamp(a.createdAt),updatedAt:stamp(a.updatedAt),
    status:a.activeTurn||a.status==='running'||a.pendingPermissions?.length?'running':'idle',
    needsInput:!!a.pendingPermissions?.length,pinned:!!entry.workspace?.pinnedAt&&!a.archivedAt&&entry.paseoWorkspaceAnchorId===a.id,archived:!!a.archivedAt,
    paseoWorkspaceId:a.workspaceId??null,paseoParentAgentId:entry.paseoParentAgentId??null,
    paseoBucket:entry.paseoBucket,paseoStatusEnteredAt:entry.paseoStatusEnteredAt,
    branch:entry.project.checkout.currentBranch,providerSessionId:a.persistence?.sessionId};
}
export function projectBlocks(page,entries=page.displayEntries??page.entries,requestNumber=()=>null) {
  const blocks=[];let prompt,last=0;
  for(const entry of boundedEntries(entries)) {
    const item=entry.item,role={user_message:'user',assistant_message:'assistant',reasoning:'reasoning',tool_call:'tool',error:'error'}[item.type];
    const at=stamp(entry.timestamp);
    if(!role){if(at)last=at;continue;}
    if(role==='user'){
      if(prompt && last>=prompt.startedAt)prompt.durationMs=last-prompt.startedAt;
      prompt=null;
    }
    const block={id:`${page.epoch}:${entry.seqStart}`,role,text:item.text??item.message??'',sentAt:at};
    if(role==='user'){
      block.startedAt=at;block.providerTurnId=entry.turnId??undefined;prompt=block;
    }
    if(role==='tool') {
      const d=item.detail??{};
      const output=d.output??d.content??d.unifiedDiff??d.result??d.text??d.log??item.error??'';
      const input={};for(const key of ['filePath','path','command','cwd','offset','limit','query','url','description'])if(d[key]!=null)input[key]=d[key];
      block.tool={callId:item.callId,nativeName:item.name,title:item.metadata?.title,kind:item.metadata?.kind,
        status:item.status,nativeInput:JSON.stringify(input),nativePresentation:item.metadata,
        detail:(typeof output==='string'?output:JSON.stringify(output)).slice(0,32000)};
    }
    blocks.push(block);if(at)last=at;
  }
  if(prompt && !page.agent?.activeTurn && page.agent?.status!=='running' && last>=prompt.startedAt)prompt.durationMs=last-prompt.startedAt;
  for(const request of page.agent?.pendingPermissions??[]) {
    if(request.kind==='question')continue;
    blocks.push({id:`permission:${request.id}`,role:'tool',text:request.title||request.name,
      tool:{title:request.title||request.name,status:'running',detail:request.description||''},
      approval:{requestId:requestNumber(request.id)}});
  }
  if(page.hasOlder)blocks.unshift({id:'paseo-history-window',role:'system',text:'较早历史请在 Paseo 查看；此窗口最多显示最近 1000 条记录。'});
  return blocks;
}

// The native durable outbox may retry after a lost HTTP response or restart.
// Journal BEFORE any remote mutation; unknown/pending outcomes never execute again.
export class Attempts {
  constructor(directory) {
    this.path=join(directory,'paseo-command-attempts.json');mkdirSync(directory,{recursive:true,mode:0o700});
    try{this.values=JSON.parse(readFileSync(this.path,'utf8'));}catch(e){if(e.code!=='ENOENT')throw e;this.values={};}
    this.active=new Map();
  }
  save(){const temp=this.path+'.tmp';writeFileSync(temp,JSON.stringify(this.values),{mode:0o600});renameSync(temp,this.path);}
  async run(command,action) {
    if(!command.commandId||typeof command.commandId!=='string')throw Error('Missing command identity');
    const key=command.commandId,hash=createHash('sha256').update(JSON.stringify(command)).digest('hex');
    const prior=this.values[key];
    if(prior){
      if(prior.hash!==hash)throw Error('Command identity reused for different content');
      if(this.active.has(key))return this.active.get(key);
      if(prior.state==='accepted')return prior.receipt;
      throw Error('Previous command outcome unknown or rejected. No replay. Check canonical history in Paseo before an explicit new send.');
    }
    this.values[key]={hash,state:'pending'};this.save();
    const work=Promise.resolve().then(async()=>{
      try{const receipt=await action();this.values[key]={hash,state:'accepted',receipt};this.save();return receipt;}
      catch(error){this.values[key]={hash,state:'unknown'};this.save();throw error;}
      finally{this.active.delete(key);}
    });
    this.active.set(key,work);return work;
  }
}

export class PaseoHost {
  constructor(client,directory,projectTimeline=rows=>rows) {
    this.client=client;this.serverId=client.getLastServerInfoMessage()?.serverId??null;this.projectTimeline=projectTimeline;this.attempts=new Attempts(directory);
    this.statuses=new Map();this.entries=new Map();this.projects=new Map();this.histories=new Map();this.numbers=new Map();
    this.numberPath=join(directory,'paseo-permission-ids.json');
    try{this.numbers=new Map(JSON.parse(readFileSync(this.numberPath,'utf8')));}catch(e){if(e.code!=='ENOENT')throw e;}
    this.transfers=new SyncTransfers();
    this.directoryAt=0;this.connected=false;this.revision=Date.now();this.catalogs=new Map();this.icons=new Map();
  }
  fileProjects(){return [...this.projects.keys()].flatMap(id=>{
    try{return [this.fileProject(id)];}catch(error){if(['ENOENT','ENOTDIR'].includes(error.code))return [];throw error;}
  });}
  fileProject(id){const p=this.projects.get(id);if(!p)throw Error('Unknown Paseo project');return {...p,cwd:canonicalPath(p.cwd)};}
  prepareFiles(params){
    if(params.projectId!=null)this.fileProject(params.projectId);
    for(const key of ['path','cwd'])if(typeof params[key]==='string' && isAbsolute(params[key]))params[key]=canonicalPath(params[key]);
  }
  identity(){const observed=this.client.getLastServerInfoMessage()?.serverId;if(!this.connected||!observed||(this.serverId&&observed!==this.serverId))throw Error('Paseo disconnected or daemon identity changed');this.serverId??=observed;}
  control(agent){const c=new Controller(this.client,()=>{});c.connected=this.connected;c.selected=agent;c.selectedServer=this.serverId;c.invalidate=()=>{};return c;}
  number(agent,id){const key=JSON.stringify([agent,id]);if(!this.numbers.has(key)){this.numbers.set(key,this.numbers.size+1);const temp=this.numberPath+'.tmp';writeFileSync(temp,JSON.stringify([...this.numbers]),{mode:0o600});renameSync(temp,this.numberPath);}return this.numbers.get(key);}
  invalidate(){for(const h of this.histories.values()){h.dirty=true;h.generation=(h.generation??0)+1;}this.directoryAt=0;}
  async directory(force=false){
    this.identity();if(!force && Date.now()-this.directoryAt<2500)return;
    if(this.directoryWork)return this.directoryWork;
    this.directoryWork=(async()=>{
      const entries=new Map(),projects=new Map();let cursor;const seen=new Set();
      do{
        const page=await this.client.fetchAgentHistory({page:{limit:100,...(cursor?{cursor}:{})},filter:{includeArchived:true}});
        for(const entry of page.entries){
          const cwd=entry.project?.checkout?.cwd;if(!cwd)continue;
          // Native remote project identities are cwd-qualified; merge same-cwd placements.
          const id=`paseo:${cwd}`;projects.set(id,{id,cwd,name:entry.project.projectName});entries.set(entry.agent.id,{...entry,projectId:id});
        }
        cursor=page.pageInfo?.nextCursor;
        if(cursor && seen.has(JSON.stringify(cursor)))throw Error('Repeated directory cursor');
        seen.add(JSON.stringify(cursor));
      }while(cursor);
      const ids=new Set([...entries.values()].map(e=>e.agent.workspaceId).filter(Boolean));
      const workspaces=await this.control(null).workspaces(ids);this.identity();
      for(const entry of entries.values())entry.workspace=workspaces.get(entry.agent.workspaceId);
      // Workspace-owned controls appear once; preserve real agent identities underneath.
      const anchors=new Map();
      const ordered=[...entries.values()].sort((a,b)=>Number(!!a.agent.archivedAt)-Number(!!b.agent.archivedAt)
        ||Number(!!a.agent.labels?.['paseo.parent-agent-id'])-Number(!!b.agent.labels?.['paseo.parent-agent-id'])
        ||stamp(a.agent.createdAt)-stamp(b.agent.createdAt)||a.agent.id.localeCompare(b.agent.id));
      for(const e of ordered){
        const key=e.agent.workspaceId??e.agent.id;
        if(!anchors.has(key))anchors.set(key,e.agent.id);
      }
      for(const e of entries.values()){
        e.paseoWorkspaceAnchorId=anchors.get(e.agent.workspaceId??e.agent.id);
        const labels=e.agent.labels??{};
        e.paseoParentAgentId=labels['paseo.parent-server-id']&&labels['paseo.parent-server-id']!==this.serverId
          ?null:labels['paseo.parent-agent-id']||null;
      }
      // Linked Git checkouts share navigation, never their execution cwd or agent identity.
      const canonicalProjects=new Map(),aliases=new Map(),repositoryRoots=new Map();
      for(const p of projects.values())repositoryRoots.set(p.id,linkedRepositoryRoot(p.cwd));
      const placements=[...projects.values()].sort((a,b)=>Number(!!repositoryRoots.get(a.id))-Number(!!repositoryRoots.get(b.id))||a.cwd.localeCompare(b.cwd));
      for(const p of placements){
        const root=repositoryRoots.get(p.id),path=navigationPath(root??p.cwd),prior=canonicalProjects.get(path);
        if(prior){aliases.set(p.id,prior.id);projects.delete(p.id);}
        else{
          const canonical=root?{id:`paseo:${root}`,cwd:root,name:p.name}:p;
          canonicalProjects.set(path,canonical);
          if(canonical.id!==p.id){aliases.set(p.id,canonical.id);projects.delete(p.id);projects.set(canonical.id,canonical);}
        }
      }
      for(const entry of entries.values())entry.projectId=aliases.get(entry.projectId)??entry.projectId;
      // Bots workspaces share one navigation project; execution remains agent-owned.
      const roots=[...projects.values()].sort((a,b)=>a.cwd.length-b.cwd.length);
      for(const root of roots){
        const base=navigationPath(root.cwd).replace(/\/$/,'');
        const key=[...entries.values()].find(e=>e.projectId===root.id)?.project.projectKey;
        if(!key)continue;
        for(const entry of entries.values()){
          if(entry.project.projectKey===key && navigationPath(entry.project.checkout.cwd).startsWith(base+'/'))entry.projectId=root.id;
        }
      }
      const liveProjects=new Set([...entries.values()].map(e=>e.projectId));
      for(const id of projects.keys())if(!liveProjects.has(id))projects.delete(id);
      for(const e of entries.values()){
        const bucket=deriveAgentStateBucket({...e.agent,status:e.agent.status==='error'?'error':e.agent.activeTurn?'running':e.agent.status,pendingPermissionCount:e.agent.pendingPermissions?.length??0});
        const old=this.statuses.get(e.agent.id);const entered=old?.bucket===bucket?old.entered:stamp(e.agent.updatedAt)||Date.now();
        this.statuses.set(e.agent.id,{bucket,entered});e.paseoBucket=bucket;e.paseoStatusEnteredAt=entered;
      }
      this.entries=entries;this.projects=projects;this.directoryAt=Date.now();
    })();
    try{await this.directoryWork;}finally{this.directoryWork=null;}
  }
  async target(id){
    this.identity();if(!this.entries.has(id))await this.directory(true);
    const entry=this.entries.get(id);if(!entry)throw Error('Unknown Paseo agent');
    const found=await this.client.fetchAgent(id);this.identity();
    if(found?.agent.id!==id||found.agent.workspaceId!==entry.agent.workspaceId)throw Error('Exact agent/workspace identity changed');
    entry.agent=found.agent;return entry;
  }
  async history(id){
    const entry=await this.target(id);let h=this.histories.get(id);
    if(!h){
      while(this.histories.size>=16){const [key,old]=this.histories.entries().next().value;old.subscription?.();this.histories.delete(key);}
      h={dirty:true,entries:[],generation:0};this.histories.set(id,h);
      h.subscription=this.client.subscribeAgentTimeline(id,()=>{h.dirty=true;h.generation++;});await h.subscription.ready;
    }
    this.histories.delete(id);this.histories.set(id,h);
    const agentChanged=JSON.stringify(h.agent)!==JSON.stringify(entry.agent)||h.snapshot?.session.title!==summary(entry,entry.projectId).title;
    if(h.dirty||!h.page){
      const generation=h.generation;
      // Re-fetch explicitly after reconnect: SDK subscription restoration misses gaps.
      let page=await this.client.fetchAgentTimeline(id,{projection:'canonical',direction:'tail',limit:100});
      if(page.agent?.id!==id)throw Error('Timeline agent identity mismatch');
      let rows=page.entries,cursor=page.startCursor,hasOlder=page.hasOlder;
      if(h.page?.epoch===page.epoch && h.entries.length && !page.reset && !page.staleCursor && (h.entries.at(-1)?.seqEnd??0)+1>=(rows[0]?.seqStart??0)){
        rows=joinEntries(h.entries,rows).slice(-1000);hasOlder=h.page.hasOlder||rows.length===1000;
      }else{
        while(hasOlder && rows.length<1000){
          const older=await this.client.fetchAgentTimeline(id,{projection:'canonical',direction:'before',cursor,limit:100});
          if(older.agent?.id!==id||older.epoch!==page.epoch||older.reset||older.staleCursor)throw Error('History changed while paging; retry');
          rows=joinEntries(older.entries,rows);cursor=older.startCursor;hasOlder=older.hasOlder;
        }
        // Do not lose changes that arrived while older pages were loading.
        const tail=await this.client.fetchAgentTimeline(id,{projection:'canonical',direction:'tail',limit:100});
        if(tail.agent?.id!==id||tail.epoch!==page.epoch)throw Error('History epoch changed; retry');
        rows=joinEntries(rows,tail.entries).slice(-1000);page=tail;
      }
      h.entries=rows;h.page={...page,hasOlder};h.dirty=h.generation!==generation;
    }
    if(agentChanged||!h.snapshot||h.changedRows!==h.entries){
      const s=summary(entry,entry.projectId),blocks=projectBlocks({...h.page,agent:entry.agent},this.projectTimeline(h.entries),request=>this.number(id,request));
      const session={id,harness:s.harness,providerSessionId:s.providerSessionId,model:s.model,modelSettings:nativeSettings(entry.agent),runtimeMode:s.runtimeMode,title:s.title,cwd:s.cwd,
        busy:s.status==='running',blocks,paseoModeId:entry.agent.currentModeId};
      const question=entry.agent.pendingPermissions?.find(p=>p.kind==='question');
      // Arbitrary provider question forms need exact schemas. Keep them explicit.
      if(question && !parseQuestionFormQuestions(question.input))blocks.push({id:`question:${question.id}`,role:'error',text:'此 Paseo 提问暂不支持原生表单，请在 Paseo 中回答。'});
      const form=question && parseQuestionFormQuestions(question.input);
      if(form)session.pendingQuestion={requestId:this.number(id,question.id),title:question.title||question.name,
        questions:form.map((q,i)=>({id:String(i),prompt:q.question,multiSelect:q.multiSelect,
          allowCustom:q.allowOther||q.options.length===0,options:q.options.map((o,n)=>({id:String(n),label:o.label,description:o.description}))}))};
      const content={...s,session,runId:entry.agent.activeTurn?.turnId};
      const signature=JSON.stringify(content);
      if(signature!==h.signature) {
        if(h.snapshot)h.previous=h.snapshot;
        h.snapshot={...content,revision:++this.revision};h.signature=signature;
      }h.agent=structuredClone(entry.agent);h.changedRows=h.entries;
    }
    return h;
  }
  workspaceChoices(projectId){
    if(!this.projects.has(projectId))throw Error('Unknown Paseo project');
    const choices=new Map();
    for(const entry of this.entries.values()){
      const w=entry.workspace;
      if(entry.projectId!==projectId||!w||w.archivedAt||w.archivingAt||w.internal)continue;
      const prior=choices.get(w.id),at=stamp(entry.agent.updatedAt);
      if(!prior)choices.set(w.id,{id:w.id,title:w.title||w.name||w.id,cwd:w.workspaceDirectory||entry.project.checkout.cwd,at});
      else prior.at=Math.max(prior.at,at);
    }
    // Paseo usually gives each conversation its own workspace: newest first.
    return [...choices.values()].sort((a,b)=>b.at-a.at||a.id.localeCompare(b.id)).map(({at,...w})=>w);
  }
  projectList(includeAll=false){
    return [...this.projects.values()].map(p=>{
      const rows=[...this.entries.values()].filter(e=>e.projectId===p.id);
      let exists=false;try{exists=statSync(p.cwd).isDirectory();}catch(error){if(!['ENOENT','ENOTDIR'].includes(error.code))throw error;}
      return {...p,exists,active:rows.some(e=>!e.agent.archivedAt),updatedAt:Math.max(0,...rows.map(e=>stamp(e.agent.updatedAt)))};
    }).filter(p=>includeAll||(p.exists&&p.active)).sort((a,b)=>b.updatedAt-a.updatedAt||a.id.localeCompare(b.id));
  }
  async providers(){
    if(!this.providerCache || Date.now()-this.providerCache.at>60000){
      const value=await this.client.listAvailableProviders();this.identity();
      if(value.error)throw Error(value.error);
      this.providerCache={at:Date.now(),rows:value.providers.filter(p=>p.available).map(p=>p.provider)};
    }
    return this.providerCache.rows;
  }
  async catalog(provider,cwd){
    const key=JSON.stringify([provider,cwd]),prior=this.catalogs.get(key);
    if(prior && Date.now()-prior.at<60000)return prior.work;
    const work=(async()=>{
      const value=await this.client.listProviderModels(provider,{cwd});this.identity();
      if(value.error)throw Error(value.error);
      if(!value.models?.length)throw Error('Paseo returned an empty model catalog');
      return value.models;
    })();
    this.catalogs.set(key,{at:Date.now(),work});
    try{return await work;}catch(error){this.catalogs.delete(key);throw error;}
  }
  async features(provider,cwd,model){
    const key=JSON.stringify(['features',provider,cwd,model]),prior=this.catalogs.get(key);
    if(prior && Date.now()-prior.at<60000)return prior.work;
    const work=(async()=>{const value=await this.client.listProviderFeatures({provider,cwd,model});this.identity();if(value.error)throw Error(value.error);return value.features??[];})();
    this.catalogs.set(key,{at:Date.now(),work});
    try{return await work;}catch(error){this.catalogs.delete(key);throw error;}
  }
  async config(provider,cwd,c){
    const prefix=harness(provider)+':';
    if(typeof c.model!=='string'||!c.model.startsWith(prefix))throw Error('不能更换现有 Paseo agent 的 provider；请新建会话。');
    const id=c.model.slice(prefix.length),models=await this.catalog(provider,cwd),model=models.find(m=>m.id===id && m.isSelectable!==false);
    if(!model)throw Error('Exact model not advertised by this Paseo provider');
    return configuration(model,await this.features(provider,cwd,id),c.modelSettings??{});
  }
  async create(c){
    return this.attempts.run(c,async()=>{
      await this.directory();const project=this.projects.get(c.projectId);
      if(!project)throw Error('Unknown Paseo project');
      this.fileProject(project.id);
      const selected=c.workspaceId==null?null:this.workspaceChoices(project.id).find(w=>w.id===c.workspaceId);
      if(c.workspaceId!=null&&!selected)throw Error('所选工作区不存在或不属于当前项目；不会自动选择其他工作区。');
      const cwd=selected?.cwd??project.cwd;
      if(c.worktreeCwd&&canonicalPath(c.worktreeCwd)!==canonicalPath(cwd))throw Error('工作目录与所选 Paseo 工作区不一致；不会创建 worktree。');
      const context=c.firstAgentContext;
      if(context!=null&&(typeof context.prompt!=='string'||context.prompt.length>65536||(context.attachments?.length??0)>0))throw Error('Invalid workspace naming context');
      const available=await this.providers();
      const choices=available.filter(p=>harness(p)===c.harness);
      const provider=choices.includes(c.harness)?c.harness:choices.length===1?choices[0]:null;
      if(!provider)throw Error('Paseo provider unavailable or ambiguous');
      const config=await this.config(provider,cwd,c);
      const modes=await this.client.listProviderModes(provider,{cwd});this.identity();
      if(modes.error)throw Error(modes.error);
      config.modeId=fullAccessMode(modes.modes??[]);config.provider=provider;config.cwd=cwd;
      if(config.thinkingOptionId===null)delete config.thinkingOptionId;
      const hash=createHash('sha256').update(this.serverId+':'+c.commandId).digest('hex');
      const agentId=`${hash.slice(0,8)}-${hash.slice(8,12)}-4${hash.slice(13,16)}-8${hash.slice(17,20)}-${hash.slice(20,32)}`;
      this.identity();
      let agent,workspace;
      if(selected){
        agent=await this.client.createAgent({agentId,workspaceId:selected.id,config,idempotencyKey:c.commandId});
        workspace=[...this.entries.values()].find(e=>e.projectId===project.id&&e.workspace?.id===selected.id)?.workspace;
      }else{
        const priorWorkspaces=new Set([...this.entries.values()].map(e=>e.agent.workspaceId));
        // An inline agent makes the daemon replace firstAgentContext with its
        // initialPrompt. Keep naming separate from the exactly-once user send.
        const created=await this.client.createWorkspace({source:{kind:'directory',path:cwd},
          ...(context?.prompt.trim()?{firstAgentContext:{prompt:context.prompt,attachments:[]}}:{}),idempotencyKey:c.commandId});
        if(created.error||!created.workspace||created.agent)throw Error(created.error||'Workspace create acknowledgement is incomplete or unexpected; do not replay');
        workspace=created.workspace;
        if(priorWorkspaces.has(workspace.id))throw Error('New workspace acknowledgement reused an existing workspace; do not replay');
        this.identity();
        agent=await this.client.createAgent({agentId,workspaceId:workspace.id,config,idempotencyKey:c.commandId+':agent'});
      }
      this.identity();
      if(agent.id!==agentId||!workspace?.id||agent.workspaceId!==workspace.id||canonicalPath(agent.cwd)!==canonicalPath(cwd)||agent.labels?.['paseo.parent-agent-id'])throw Error('Create acknowledgement identity mismatch; do not replay');
      const template=[...this.entries.values()].find(e=>e.projectId===project.id&&(!selected||e.agent.workspaceId===selected.id));
      this.entries.set(agent.id,{...template,project:{...template?.project,workspaceName:workspace.title||workspace.name,checkout:{...template?.project.checkout,cwd}},
        projectId:project.id,agent,workspace,paseoWorkspaceAnchorId:selected?template?.paseoWorkspaceAnchorId:agent.id,paseoParentAgentId:null});
      this.invalidate();
      return {commandId:c.commandId,sessionId:agent.id,revision:++this.revision,paseoCreated:true};
    });
  }
  async command(c){
    this.identity();if(c.type==='create')return this.create(c);
    const entry=await this.target(c.sessionId),agent=entry.agent,control=this.control(agent);
    return this.attempts.run(c,async()=>{
      const base={serverId:this.serverId,agentId:agent.id};
      if(c.type==='configure'){
        const requested={...(c.modelSettings??{})},current=nativeSettings(agent);
        for(const key of ['fast','thinking'])if(requested[key]==='false' && !Object.hasOwn(current,key))delete requested[key];
        const unchanged=c.model===summary(entry,entry.projectId).model && JSON.stringify(Object.entries(requested).sort())===JSON.stringify(Object.entries(current).sort());
        // Existing off-catalog models remain usable; no mutation or substituted ID.
        if(unchanged)return {commandId:c.commandId,sessionId:agent.id,revision:this.revision};
        if(agent.activeTurn||agent.status==='running'||agent.pendingPermissions?.length)throw Error('Paseo agent is busy; wait before configuring');
        const config=await this.config(agent.provider,agent.cwd,c);
        const fresh=await this.target(agent.id);
        if(fresh.agent.activeTurn||fresh.agent.status==='running'||fresh.agent.pendingPermissions?.length)throw Error('Paseo agent became busy; configuration not applied');
        this.identity();
        if(this.client.getLastServerInfoMessage()?.features?.agentConfigApply){
          // Exact bundle request; modeId intentionally absent for existing agents.
          const {model,...rest}=config;
          await this.client.applyAgentConfig(agent.id,{modelId:model,...rest});
        }else{
          // Older daemons expose only individual setters. Failure is not replayed.
          if(agent.model!==config.model)await this.client.setAgentModel(agent.id,config.model);
          this.identity();await this.client.setAgentThinkingOption(agent.id,config.thinkingOptionId);
          for(const [id,value] of Object.entries(config.featureValues??{})){this.identity();await this.client.setAgentFeature(agent.id,id,value);}
        }
      }else if(c.type==='view'){
        if(agent.requiresAttention && agent.attentionReason!=='permission' && !agent.pendingPermissions?.length)await control.dispatch({...base,op:'clearAttention'});
      }else if(c.type==='send'){
        if(c.attachments?.length||c.intent||c.draftBlockId||c.planBlockId)throw Error('此适配层暂不支持附件、计划或草稿；请在 Paseo 发送。');
        await control.dispatch({...base,op:'send',messageId:c.commandId,text:c.text,acknowledgeRace:true});
      }else if(c.type==='cancel'){
        await control.dispatch({...base,op:'cancel',turnId:c.runId});
      }else if(c.type==='approve'){
        if((c.runId??null)!==(agent.activeTurn?.turnId??null))throw Error('Permission turn changed');
        const request=agent.pendingPermissions.find(p=>this.number(agent.id,p.id)===c.requestId);
        const actions=request?.actions?.filter(a=>a.behavior===c.decision)??[];
        if(actions.length!==1)throw Error('此审批有多个操作或已过期，请在 Paseo 处理。');
        await control.dispatch({...base,op:'permission',requestId:request.id,actionId:actions[0].id});
      }else if(c.type==='answer'){
        if((c.runId??null)!==(agent.activeTurn?.turnId??null))throw Error('Question turn changed');
        const request=agent.pendingPermissions.find(p=>p.kind==='question'&&this.number(agent.id,p.id)===c.requestId);
        const form=request && parseQuestionFormQuestions(request.input);
        if(!form)throw Error('Unsupported or expired question; answer in Paseo');
        let response;
        if(c.reply?.kind==='skipped')response={behavior:'deny'};
        else if(c.reply?.kind==='answered'){
          const answers={};
          for(const [i,q] of form.entries()){
            const selected=c.reply.answers?.[i]??[],other=c.reply.custom?.[i];
            if(!Array.isArray(selected)||(!q.multiSelect&&selected.length>1))throw Error('Invalid question selections');
            const labels=selected.map(id=>{const o=q.options.find((_,n)=>String(n)===id);if(!o)throw Error('Unknown question option');return o.label;});
            if(other!=null){if(typeof other!=='string'||!(q.allowOther||q.options.length===0))throw Error('Unexpected custom answer');labels.push(other.trim());}
            if(!labels.length&&!q.allowEmpty)throw Error('Question requires an answer');
            answers[q.header]=labels.join(', ');
          }
          response={behavior:'allow',updatedInput:{...request.input,answers}};
        }else throw Error('Invalid question reply');
        this.identity();await this.client.respondToPermissionAndWait(agent.id,request.id,response);
      }else throw Error('此操作暂不支持，请在 Paseo 完成。');
      this.invalidate();return {commandId:c.commandId,sessionId:agent.id,revision:this.revision};
    });
  }
  async request(method,p){
    this.identity();
    if(method==='paseo.workspaces'){
      await this.directory();
      return {result:this.workspaceChoices(p.projectId).slice(0,WORKSPACE_CHOICE_LIMIT)};
    }
    if(method==='environment.describe')return {result:{protocolVersion:1,environmentId:this.serverId,name:`Paseo ${this.client.getLastServerInfoMessage()?.version??''}`,
      platform:process.platform,providers:[...new Set((await this.providers()).map(harness))],capabilities:CAPABILITIES}};
    if(['projects.list','sessions.list','projects.open'].includes(method)){
      await this.directory();
      if(method==='projects.list'){
        const projects=this.projectList(p.includeAll===true);
        if(p.includeIcons){const icons=(await this.request('paseo.projectIcons',{})).result;return {result:projects.map(project=>({...project,icon:icons[project.id]??null}))};}
        return {result:projects};
      }
      if(method==='projects.open'){const project=[...this.projects.values()].find(v=>v.cwd===p.cwd);if(!project)throw Error('只能打开现有 Paseo 项目');return {result:project};}
      if(!this.projects.has(p.projectId))throw Error('Unknown project');
      const values=[...this.entries.values()].filter(e=>e.projectId===p.projectId).map(e=>summary(e,p.projectId));
      if(p.withRevision===true){
        const revision=createHash('sha256').update(JSON.stringify([...values].sort((a,b)=>a.id.localeCompare(b.id)))).digest('hex');
        const structural=[...values].sort((a,b)=>a.id.localeCompare(b.id)).map(value=>{
          const stable={...value};for(const key of ['updatedAt','status','needsInput','paseoBucket','paseoStatusEnteredAt'])delete stable[key];return stable;
        });
        const structuralRevision=createHash('sha256').update(JSON.stringify(structural)).digest('hex');
        return {result:p.revision===revision?{revision,unchanged:true}:{revision,structuralRevision,values}};
      }
      return {result:values};
    }
    if(method==='models.list'){
      await this.directory();const cwd=this.projects.get(p.projectId)?.cwd??p.cwd;
      if(!cwd||![...this.projects.values()].some(v=>v.cwd===cwd))throw Error('Unknown model catalog cwd');
      const models={},errors={};
      const providers=await this.providers();
      await Promise.all(providers.map(async provider=>{
        const key=harness(provider);
        // Aliases remain attachable; the native new-session harness chooses its builtin first.
        if(provider!==key && providers.includes(key))return;
        try{const catalog=await this.catalog(provider,cwd);models[key]=await Promise.all(catalog.map(async m=>({...nativeModel(m,await this.features(provider,cwd,m.id)),id:`${key}:${m.id}`})));}
        catch(error){errors[key]=error.message;}
      }));return {result:{models,errors}};
    }
    if(method==='paseo.projectIcons'){
      await this.directory();const icons={};
      await Promise.all([...this.projects.values()].map(async p=>{
        const entry=[...this.entries.values()].find(e=>e.projectId===p.id),id=entry?.project?.projectKey;
        if(!id)return;
        if(!this.icons.has(id)){
          try{const value=await this.client.getProjectIcon(id);if(value.projectId===id&&!value.error)this.icons.set(id,value.icon??null);}catch{this.icons.set(id,null);}
        }
        icons[p.id]=this.icons.get(id)??null;
      }));return {result:icons};
    }
    if(method==='sessions.sync'){
      const h=await this.history(p.sessionId),value=h.snapshot;
      let sync={kind:'snapshot',value};
      if(p.revision===value.revision)sync={kind:'unchanged',revision:value.revision};
      else if(h.previous && h.previous.revision===p.revision){
        const previous=new Map(h.previous.session.blocks.map(b=>[b.id,JSON.stringify(b)]));
        const {blocks,...session}=value.session;
        sync={kind:'delta',base:p.revision,value:{...value,session},blockIds:blocks.map(b=>b.id),blocks:blocks.filter(b=>previous.get(b.id)!==JSON.stringify(b))};
      }
      return {result:this.transfers.respond(p.sessionId,sync)};
    }
    if(method==='sessions.syncChunk')return {result:this.transfers.chunk(p.sessionId,p.transfer,p.offset)};
    if(method==='commands.dispatch')return {result:await this.command(p)};
    if(method==='sessions.update'){
      const e=await this.target(p.sessionId);if(e.projectId!==p.projectId)throw Error('Wrong project');
      if(e.paseoWorkspaceAnchorId!==e.agent.id)throw Error('工作区的改名、置顶或归档请在主任务上操作。');
      const c=this.control(e.agent),base={serverId:this.serverId,agentId:e.agent.id,workspaceId:e.agent.workspaceId};
      // Exactly one field per operation avoids partial multi-field mutations.
      if(Object.keys(p).filter(k=>!['sessionId','projectId'].includes(k)).length!==1)throw Error('One supported workspace change required');
      if(p.title!==undefined)await c.dispatch({...base,op:'renameWorkspace',title:p.title});
      else if(p.pinned!==undefined)await c.dispatch({...base,op:'pinWorkspace',pinned:p.pinned});
      else if(p.archived===true)await c.dispatch({...base,op:'archiveWorkspace'});
      else throw Error('请在 Paseo 中修改此设置');
      await this.directory(true);return {result:{...summary(this.entries.get(e.agent.id)??e,e.projectId),archived:p.archived??!!e.agent.archivedAt}};
    }
    if(method==='paseo.checkCleanup'){
      const attempt=this.attempts.values[p.commandId];
      const receipt=attempt?.state==='accepted'?attempt.receipt:null;
      if(!this.allowLiveCheck || !receipt?.paseoCreated || receipt.sessionId!==p.sessionId)throw Error('Cleanup is restricted to this opt-in check create receipt');
      const entry=await this.target(p.sessionId);this.identity();
      if(entry.agent.activeTurn||entry.agent.status==='running'||entry.agent.pendingPermissions?.length)throw Error('Cleanup refuses busy agent');
      // Only this freshly-created agent, NEVER its reused workspace or siblings.
      const result=await this.client.archiveAgent(entry.agent.id);this.invalidate();return {result};
    }
    if(method==='sessions.delete')throw Error('删除远程会话不受支持；请在 Paseo 处理。');
    if(method==='workspace.run'){
      const reads=['list_dir','list_project_files','read_text_file','read_binary_file','read_file_preview','stat_files','git_diff_index','git_diff_files','git_diff_stats','git_file_diff','git_locate_files','git_head_message','git_pr_status','git_history','git_commit_files','git_commit_file_diff','git_staged_context','git_range_context','git_branches','git_worktrees','search_project'];
      if(!reads.includes(p.command))throw Error('此版本的项目文件和 Git 操作为只读。');
      if(p.args && typeof p.args==='object'){this.prepareFiles(p.args);if(p.args.options && typeof p.args.options==='object')this.prepareFiles(p.args.options);}
      return null;
    }
    if(['files.read','files.list','files.index','files.search','files.searchContent','git.diff','git.branches','git.worktrees','git.index','git.fileDiff'].includes(method)){this.prepareFiles(p);return null;}
    throw Error('此操作暂不支持，请在 Paseo 完成。');
  }
  close(){for(const h of this.histories.values())h.subscription?.();this.client.close();}
}
