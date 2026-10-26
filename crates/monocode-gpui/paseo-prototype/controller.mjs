// One selected daemon-owned agent. No local session store, provider launch or retry outbox.
export const PAGE = 100;
export function isBusy(agent) { return !agent || !['idle','closed','error'].includes(agent.status) || agent.providerUnavailable || !!agent.activeTurn || !!agent.pendingPermissions?.length; }
export function canArchiveWorkspace(w) {
  const git=w?.gitRuntime,diff=w?.diffStat;
  return !!w && ['directory','local_checkout','checkout'].includes(w.workspaceKind)
    && !w.worktreeSlug && !git?.isPaseoOwnedWorktree && !w.archivingAt
    && git?.isDirty!==true && !(git?.isDirty==null && ((diff?.additions??0)>0 || (diff?.deletions??0)>0))
    && (git?.aheadOfOrigin??0)<=0;
}
export function boundedEntries(entries) {
  return entries.map(entry=>{
    const item=entry.item;const raw=JSON.stringify(item);
    if(raw.length<=32000)return entry;
    const text=raw.slice(0,32000)+'\n[原型截断；完整内容请在 Paseo 查看]';
    return {...entry,prototypeTruncated:true,item:item.type==='tool_call'
      ?{type:item.type,name:item.name,status:item.status,detail:{type:'plain_text',text}}
      :{type:item.type,text:item.text?item.text.slice(0,32000)+'\n[原型截断；完整内容请在 Paseo 查看]':text,message:item.message?.slice(0,32000),pluginId:item.pluginId}};
  });
}
export function joinEntries(older, tail) {
  const entries = new Map();
  for (const entry of [...older, ...tail]) entries.set(entry.seqStart, entry);
  return [...entries.values()].sort((a,b) => a.seqStart-b.seqStart);
}
export class Controller {
  constructor(client, emit) {
    this.client=client; this.emit=emit; this.generation=0; this.tail=[]; this.older=[]; this.seen=new Set();
    this.connected=false; this.selected=null; this.loading=false; this.dirty=false;
  }
  identity(serverId) {
    if (!this.connected || serverId !== this.client.getLastServerInfoMessage()?.serverId) throw Error('Daemon identity changed or disconnected; reopen the agent.');
  }
  target(command) {
    this.identity(command.serverId);
    if (!this.selected || command.agentId !== this.selected.id) throw Error('Wrong selected agent');
  }
  async workspaces(ids) {
    const found=new Map();if(!ids.size)return found;let cursor;
    do {
      const page=await this.client.fetchWorkspaces({page:{limit:200,...(cursor?{cursor}:{})}});
      for(const w of page.entries)if(ids.has(w.id))found.set(w.id,w);
      cursor=page.pageInfo?.nextCursor;
    } while(cursor && found.size<ids.size);
    return found;
  }
  async workspaceTarget(c) {
    this.identity(c.serverId);
    if(!c.agentId || !c.workspaceId)throw Error('Exact agent and workspace IDs required');
    const found=await this.client.fetchAgent(c.agentId);this.identity(c.serverId);
    if(found?.agent.id!==c.agentId || found.agent.workspaceId!==c.workspaceId)throw Error('Agent/workspace identity mismatch');
    const w=(await this.workspaces(new Set([c.workspaceId]))).get(c.workspaceId);
    this.identity(c.serverId);
    if(!w || w.id!==c.workspaceId)throw Error('Workspace unavailable');
    return w;
  }
  snapshot() {
    if (!this.selected) return;
    this.emit({event:'snapshot',serverId:this.client.getLastServerInfoMessage()?.serverId,
      agentId:this.selected.id,agent:this.selected,epoch:this.epoch,entries:joinEntries(this.older,this.tail),
      hasOlder:this.hasOlder,startCursor:this.startCursor,olderWindow:!!this.older.length,
      gap:!!this.older.length && this.older.at(-1).seqEnd+1 < (this.tail[0]?.seqStart??0)});
  }
  invalidate() {
    this.dirty=true;
    if (!this.timer) this.timer=setTimeout(() => {this.timer=null; void this.refresh().catch(() => this.emit({event:'error',error:'History refresh failed; reconnect or refresh.'}));},160);
  }
  async refresh() {
    if (!this.connected || !this.selected) return;
    if(this.selectedServer!==this.client.getLastServerInfoMessage()?.serverId){this.subscription?.();this.selected=null;throw Error('Daemon identity changed; select the agent again.');}
    if (this.loading) {this.dirty=true;return;}
    this.loading=true; this.dirty=false;
    const generation=this.generation,id=this.selected.id;
    try {
      const page=await this.client.fetchAgentTimeline(id,{projection:'canonical',direction:'tail',limit:PAGE});
      if (generation!==this.generation) return;
      if (page.agent?.id!==id) throw Error('Timeline identity mismatch');
      if (this.epoch!==page.epoch || page.reset || page.staleCursor) this.older=[];
      this.epoch=page.epoch;this.tail=boundedEntries(page.entries);this.selected=page.agent;
      if (!this.older.length) {this.startCursor=page.startCursor;this.hasOlder=page.hasOlder;}
      this.snapshot();
    } finally {this.loading=false;if(this.dirty)this.invalidate();}
  }
  async dispatch(c) {
    if(c.op==='list') {
      this.identity(c.serverId);
      const result=await this.client.fetchAgentHistory({page:{limit:100,...(c.cursor?{cursor:c.cursor}:{})},...(c.query?{search:c.query}:{}),filter:{includeArchived:false}});
      const workspaces=await this.workspaces(new Set(result.entries.map(e=>e.agent.workspaceId).filter(Boolean)));
      this.identity(c.serverId);
      return {serverId:c.serverId,entries:result.entries.map(e=>{
        const w=workspaces.get(e.agent.workspaceId);
        return {...e,workspace:w?{id:w.id,name:w.name,title:w.title,pinnedAt:w.pinnedAt,canArchive:canArchiveWorkspace(w)}:null,
          project:{...e.project,...(w?{workspaceName:w.name}:{})}};
      }),pageInfo:result.pageInfo};
    }
    if(c.op==='projects') {this.identity(c.serverId);return this.client.listProjects();}
    if(['renameWorkspace','pinWorkspace','archiveWorkspace'].includes(c.op)) {
      const w=await this.workspaceTarget(c);
      if(c.op==='renameWorkspace') {
        if(typeof c.title!=='string' || !c.title.trim())throw Error('Empty rename cancelled');
        await this.client.setWorkspaceTitle(w.id,c.title.trim());
      } else if(c.op==='pinWorkspace') {
        if(typeof c.pinned!=='boolean')throw Error('Explicit pin state required');
        await this.client.setWorkspacePinned(w.id,c.pinned);
      } else {
        if(!canArchiveWorkspace(w))throw Error('请在 Paseo 中归档');
        const result=await this.client.archiveWorkspace(w.id);
        if(result.error)throw Error(result.error);
        if(result.workspaceId!==w.id || !result.archivedAt)throw Error('Workspace archive not confirmed');
        if(this.selected?.workspaceId===w.id) {
          this.subscription?.();this.subscription=null;this.generation++;this.selected=null;this.tail=[];this.older=[];
        }
      }
      return {serverId:c.serverId,workspaceId:w.id};
    }
    if(c.op==='open') {
      this.identity(c.serverId);
      const found=await this.client.fetchAgent(c.agentId);
      this.identity(c.serverId);
      if(found?.agent.id!==c.agentId) throw Error('Exact agent ID required');
      this.subscription?.();this.generation++;this.selectedServer=c.serverId;this.selected=found.agent;this.tail=[];this.older=[];this.epoch=null;
      this.subscription=this.client.subscribeAgentTimeline(c.agentId,() => this.invalidate());
      await this.subscription.ready;await this.refresh();
      return {agentId:c.agentId};
    }
    this.target(c);
    if(c.op==='clearAttention') {
      const found=await this.client.fetchAgent(c.agentId);this.target(c);
      if(found?.agent.id!==c.agentId)throw Error('Exact agent ID required');
      if(found.agent.attentionReason==='permission' || found.agent.pendingPermissions?.length)throw Error('Permission attention cannot be cleared by viewing.');
      if(found.agent.requiresAttention) {
        await this.client.clearAgentAttention(c.agentId);
        this.invalidate();
      }
      return {};
    }
    if(c.op==='snapshot') {await this.refresh();return {agentId:this.selected.id,busy:isBusy(this.selected),status:this.selected.status,epoch:this.epoch,entries:joinEntries(this.older,this.tail).length};}
    if(c.op==='latest') {this.older=[];await this.refresh();return {};}
    if(c.op==='older') {
      const generation=this.generation;
      const page=await this.client.fetchAgentTimeline(c.agentId,{projection:'canonical',direction:'before',cursor:this.startCursor,limit:PAGE});
      if(generation!==this.generation)return {};
      if(page.agent?.id!==c.agentId)throw Error('Timeline identity mismatch');
      if(page.epoch!==this.epoch || page.reset || page.staleCursor) {this.older=[];await this.refresh();throw Error('History epoch changed; returned to latest.');}
      this.older=joinEntries(boundedEntries(page.entries),this.older).slice(0,PAGE*3);this.startCursor=page.startCursor;this.hasOlder=page.hasOlder;this.snapshot();return {};
    }
    if(c.op==='send') {
      if(!c.messageId || this.seen.has(c.messageId))throw Error('Send attempt already used; inspect history before explicitly trying a new message.');
      const found=await this.client.fetchAgent(c.agentId);this.target(c);
      if(isBusy(found?.agent))throw Error('Agent is busy or awaiting permission; draft retained.');
      if(!c.acknowledgeRace)throw Error('Paseo has no atomic idle-only send; explicit concurrent-send risk acknowledgement required.');
      if(!c.text?.trim() || c.text.length>65536)throw Error('Empty or oversized message');
      this.seen.add(c.messageId);if(this.seen.size>1000)this.seen.delete(this.seen.values().next().value);
      try {await this.client.sendAgentMessage(c.agentId,c.text,{messageId:c.messageId,activeTurnBehavior:'interrupt'});}
      catch {throw Error('Send outcome unknown. No automatic retry. Inspect canonical history in either client before a new send.');}
      this.invalidate();return {accepted:true,messageId:c.messageId};
    }
    if(c.op==='cancel') {
      const found=await this.client.fetchAgent(c.agentId);this.target(c);
      if(!c.turnId || found?.agent.activeTurn?.turnId!==c.turnId)throw Error('Turn changed; cancellation refused.');
      await this.client.cancelAgent(c.agentId);this.invalidate();return {};
    }
    if(c.op==='permission') {
      const found=await this.client.fetchAgent(c.agentId);this.target(c);
      const request=found?.agent.pendingPermissions.find(p=>p.id===c.requestId);
      const action=request?.actions?.find(a=>a.id===c.actionId);
      if(!action || request.kind==='question')throw Error('Unsupported or expired permission. Resolve it in Paseo.');
      await this.client.respondToPermissionAndWait(c.agentId,c.requestId,{behavior:action.behavior,selectedActionId:action.id});
      this.invalidate();return {};
    }
    throw Error('Unsupported operation');
  }
  close() {clearTimeout(this.timer);this.subscription?.();this.client.close();}
}
