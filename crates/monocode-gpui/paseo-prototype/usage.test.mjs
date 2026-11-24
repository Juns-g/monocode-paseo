import {test} from 'node:test';
import assert from 'node:assert/strict';
import {normalizeUsageReports,fetchPaseoUsage,formatReportError,ALLOWED_USAGE_SOURCES} from './usage.mjs';

const T0=Date.parse('2026-05-01T00:00:00Z');
const win=(extra={})=>({id:'five_hour',label:'Session',shortLabel:'5h',summary:true,usedPct:28,resetsAt:'2026-05-01T03:00:00Z',durationSeconds:18000,...extra});
const entry=(sourceId,id,extra={})=>({id,sourceId,sourceLabel:sourceId[0].toUpperCase()+sourceId.slice(1),account:{},fetchedAt:'2026-05-01T00:00:00Z',
  report:{status:'available',windows:[win()]},...extra});

test('only codex and antigravity survive, ordered codex first then by id',()=>{
  const out=normalizeUsageReports([entry('grok','g'),entry('antigravity','b'),entry('claude','c'),entry('codex','z'),entry('opencode','o'),entry('codex','a'),entry('devin','d'),{id:'x',sourceId:'pi'},null],T0);
  assert.deepEqual(ALLOWED_USAGE_SOURCES,['codex','antigravity']);
  assert.deepEqual(out.sources.map(s=>s.id),['a','z','b']);
  assert.equal(out.fetchedAt,T0);
});
test('percent derivation, null buckets and timestamp normalization',()=>{
  const [s]=normalizeUsageReports([entry('codex','c',{account:{label:'work'},report:{status:'available',planLabel:'pro',observedAt:'2026-05-01T00:30:00Z',windows:[
    win(),win({id:'wk',usedPct:undefined,remainingPct:90,runsOutAt:'2026-05-01T02:00:00Z'}),win({id:'off',usedPct:null,remainingPct:null,resetsAt:null}),win({id:'over',usedPct:140})],
    balances:[{id:'credits',label:'Credits',remaining:12.5,unit:'credits'}]}})]).sources;
  assert.equal(s.accountLabel,'work');assert.equal(s.planLabel,'pro');assert.equal(s.retained,false);assert.equal(s.error,null);
  assert.equal(s.observedAt,T0+30*60_000);assert.equal(s.fetchedAt,T0);
  const [a,b,c,d]=s.windows;
  assert.equal(a.remainingPct,72);assert.equal(a.resetsAt,T0+3*3600_000);assert.equal(a.runsOutAt,null);assert.equal(a.summary,true);
  assert.equal(b.usedPct,10);assert.equal(b.runsOutAt,T0+2*3600_000);
  assert.equal(c.usedPct,null);assert.equal(c.remainingPct,null);assert.equal(c.resetsAt,null);
  assert.equal(d.remainingPct,0);
  assert.deepEqual(s.balances[0],{id:'credits',label:'Credits',used:null,remaining:12.5,limit:null,unit:'credits',resetsAt:null,tone:null});
});
test('lastSuccessful keeps the last-known windows with a retained flag and typed error',()=>{
  const last={fetchedAt:'2026-04-30T23:00:00Z',report:{status:'available',planLabel:'plus',windows:[win({usedPct:50})]}};
  const cases=[
    [{status:'unavailable',problem:{kind:'expired',expiresAt:'2026-05-01T00:00:00Z'}},'Sign-in expired'],
    [{status:'unavailable',problem:{kind:'rejected',status:401}},'Request rejected (HTTP 401)'],
    [{status:'unavailable',problem:{kind:'no_quota',detail:'No plan'}},'No plan'],
    [{status:'error',error:'boom'},'boom'],
  ];
  for(const [report,error] of cases){
    const [s]=normalizeUsageReports([entry('antigravity','a',{report,lastSuccessful:last})]).sources;
    assert.equal(s.retained,true);assert.equal(s.status,report.status);assert.equal(s.error,error);
    assert.equal(s.planLabel,'plus');assert.equal(s.windows[0].remainingPct,50);assert.equal(s.observedAt,Date.parse(last.fetchedAt));
  }
  const [bare]=normalizeUsageReports([entry('codex','c',{report:{status:'error',error:'x'}})]).sources;
  assert.equal(bare.retained,false);assert.deepEqual(bare.windows,[]);assert.equal(bare.error,'x');assert.equal(bare.observedAt,null);
  assert.equal(formatReportError({status:'available'}),null);
});
test('fetchPaseoUsage forwards forceRefresh and filters streamed plus final reports',async()=>{
  const calls=[];
  const client={async listUsageReports(options,onReport){calls.push(options);onReport(entry('grok','g'));onReport(entry('codex','c'));return {requestId:'r',reports:[entry('antigravity','a'),entry('grok','g')]};}};
  const a=await fetchPaseoUsage(client,{});
  const b=await fetchPaseoUsage(client,{forceRefresh:true});
  assert.deepEqual(calls,[{forceRefresh:false},{forceRefresh:true}]);
  assert.deepEqual(a.sources.map(s=>s.sourceId),['codex','antigravity']);assert.equal(b.sources.length,2);
  assert.deepEqual((await fetchPaseoUsage(client,{forceRefresh:'yes'}).then(()=>calls.at(-1))),{forceRefresh:false});
});
test('a slow source times out into the already streamed reports; nothing streamed rejects',async()=>{
  const slow={listUsageReports:(o,onReport)=>{onReport(entry('codex','c'));return new Promise(()=>{});}};
  const out=await fetchPaseoUsage(slow,{},30);
  assert.deepEqual(out.sources.map(s=>s.id),['c']);
  const dead={listUsageReports:()=>new Promise(()=>{})};
  await assert.rejects(fetchPaseoUsage(dead,{},30),/timed out/);
});
test('unsupported hosts and daemon errors surface cleanly',async()=>{
  const client={listUsageReports:async()=>{throw Error('Update the host to see usage.');}};
  await assert.rejects(fetchPaseoUsage(client,{}),/Update the host/);
  const sync={listUsageReports(){throw Error('sync throw');}};
  await assert.rejects(fetchPaseoUsage(sync,{}),/sync throw/);
});
