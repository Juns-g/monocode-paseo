// Read-only Codex + Antigravity quota, relayed from the Paseo daemon (`usage.list_reports`).
// Credentials, tokens and provider CLIs stay inside the daemon; this module never touches them.
export const ALLOWED_USAGE_SOURCES=['codex','antigravity'];
// Stays under host/server.ts's 20 s request timeout and the desktop client's 30 s socket timeout.
export const USAGE_FETCH_TIMEOUT_MS=19_000;

const ms=value=>{
  if(typeof value==='number')return Number.isFinite(value)?Math.trunc(value):null;
  if(typeof value!=='string'||!value)return null;
  const n=Date.parse(value);return Number.isFinite(n)?n:null;
};
const num=value=>typeof value==='number'&&Number.isFinite(value)?value:null;
const clamp=value=>Math.min(100,Math.max(0,value));
const TONES=new Set(['default','ok','warning','danger']);

export function formatReportError(report){
  if(report?.status==='error')return String(report.error||'Usage request failed');
  if(report?.status==='unavailable'){
    const p=report.problem??{};
    if(p.kind==='expired')return 'Sign-in expired';
    if(p.kind==='rejected')return `Request rejected${Number.isFinite(p.status)?` (HTTP ${p.status})`:''}`;
    if(p.kind==='no_quota')return String(p.detail||'No quota reported');
    return 'Usage unavailable';
  }
  return null;
}

function normalizeWindow(w){
  let used=num(w?.usedPct),remaining=num(w?.remainingPct);
  if(used===null&&remaining!==null)used=clamp(100-remaining);
  else if(remaining===null&&used!==null)remaining=clamp(100-used);
  return {
    id:String(w?.id??''),label:String(w?.label??''),
    shortLabel:typeof w?.shortLabel==='string'?w.shortLabel:null,
    summary:w?.summary===true,
    usedPct:used,remainingPct:remaining,
    resetsAt:ms(w?.resetsAt),
    durationSeconds:num(w?.durationSeconds),
    runsOutAt:ms(w?.runsOutAt),
    shortfallPct:num(w?.shortfallPct),
    tone:TONES.has(w?.tone)?w.tone:null,
  };
}
function normalizeBalance(b){
  return {
    id:String(b?.id??''),label:String(b?.label??''),
    used:num(b?.used),remaining:num(b?.remaining),limit:num(b?.limit),
    unit:['usd','credits','requests','tokens'].includes(b?.unit)?b.unit:'credits',
    resetsAt:ms(b?.resetsAt),tone:TONES.has(b?.tone)?b.tone:null,
  };
}

function normalizeEntry(entry){
  const fetchedAt=ms(entry.fetchedAt);
  const current=entry.report??{};
  const last=entry.lastSuccessful;
  const retained=current.status!=='available'&&last?.report?.status==='available';
  const shown=current.status==='available'?current:retained?last.report:null;
  const error=current.status==='available'?null:formatReportError(current);
  return {
    id:String(entry.id),
    sourceId:entry.sourceId,
    sourceLabel:String(entry.sourceLabel||entry.sourceId),
    accountLabel:typeof entry.account?.label==='string'&&entry.account.label?entry.account.label:null,
    planLabel:typeof shown?.planLabel==='string'?shown.planLabel:null,
    status:['available','unavailable','error'].includes(current.status)?current.status:'error',
    retained,
    error,
    fetchedAt,
    observedAt:shown?(retained
      ?ms(shown.observedAt)??ms(last.fetchedAt)
      :ms(shown.observedAt)??ms(entry.successfulAt)??fetchedAt):null,
    windows:Array.isArray(shown?.windows)?shown.windows.map(normalizeWindow):[],
    balances:Array.isArray(shown?.balances)?shown.balances.map(normalizeBalance):[],
  };
}

export function normalizeUsageReports(reports,now=Date.now()){
  const sources=[...(reports??[])]
    .filter(entry=>entry&&ALLOWED_USAGE_SOURCES.includes(entry.sourceId)&&typeof entry.id==='string')
    .map(normalizeEntry)
    .sort((a,b)=>ALLOWED_USAGE_SOURCES.indexOf(a.sourceId)-ALLOWED_USAGE_SOURCES.indexOf(b.sourceId)||a.id.localeCompare(b.id));
  return {fetchedAt:now,sources};
}

export async function fetchPaseoUsage(client,params={},timeoutMs=USAGE_FETCH_TIMEOUT_MS){
  const collected=new Map();
  const options={forceRefresh:params?.forceRefresh===true};
  const keep=report=>{if(report&&ALLOWED_USAGE_SOURCES.includes(report.sourceId)&&typeof report.id==='string')collected.set(report.id,report);};
  let timer;
  const timeout=new Promise(resolve=>{timer=setTimeout(()=>resolve({timedOut:true}),timeoutMs);});
  const call=Promise.resolve().then(()=>client.listUsageReports(options,keep));
  // A late rejection after the race is settled must not become an unhandled rejection.
  call.catch(()=>{});
  try{
    const outcome=await Promise.race([call.then(payload=>({payload})),timeout]);
    if(outcome.timedOut){
      if(collected.size===0)throw Error('Paseo usage request timed out');
    }else{
      for(const report of outcome.payload?.reports??[])keep(report);
    }
    return normalizeUsageReports([...collected.values()]);
  }finally{clearTimeout(timer);}
}
