import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,writeFileSync,readFileSync,truncateSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {AttachmentStore,MAX_ATTACHMENT_BYTES,MAX_CHUNK_BYTES} from './attachments.mjs';

const ID='11111111-1111-4111-8111-111111111111',ID2='22222222-2222-4222-8222-222222222222';
const tmp=()=>mkdtempSync(join(tmpdir(),'paseo-attach-'));
const b64=b=>Buffer.from(b).toString('base64');
const ref=(id,over={})=>({id,name:'shot.png',mimeType:'image/png',kind:'image',size:4,...over});
function stored(dir,id,bytes){const s=new AttachmentStore(dir);s.upload({id,offset:0,size:bytes.length,data:b64(bytes)});return s;}

test('chunked upload appends, accepts exact retries and refuses tampered, out-of-order or malformed chunks',()=>{
  const dir=tmp(),s=new AttachmentStore(dir),bytes=Buffer.from('abcdefgh');
  assert.deepEqual(s.upload({id:ID,offset:0,size:8,data:b64(bytes.subarray(0,4))}),{offset:4});
  assert.deepEqual(s.upload({id:ID,offset:0,size:8,data:b64(bytes.subarray(0,4))}),{offset:4});
  assert.throws(()=>s.upload({id:ID,offset:0,size:8,data:b64('zzzz')}),/does not match/);
  assert.throws(()=>s.upload({id:ID,offset:6,size:8,data:b64('gh')}),/out of order/);
  assert.deepEqual(s.upload({id:ID,offset:4,size:8,data:b64(bytes.subarray(4))}),{offset:8});
  assert.equal(readFileSync(s.pathFor(ID)).toString(),'abcdefgh');
  assert.throws(()=>s.upload({id:'../etc/passwd',offset:0,size:1,data:b64('a')}),/Invalid attachment ID/);
  assert.throws(()=>s.upload({id:ID2,offset:0,size:MAX_ATTACHMENT_BYTES+1,data:''}),/Invalid attachment chunk/);
  assert.throws(()=>s.upload({id:ID2,offset:0,size:4,data:'not base64!'}),/Invalid attachment chunk/);
  assert.throws(()=>s.upload({id:ID2,offset:0,size:1,data:b64('ab')}),/chunk size/);
  assert.throws(()=>s.upload({id:ID2,offset:-1,size:4,data:''}),/Invalid attachment chunk/);
  rmSync(dir,{recursive:true,force:true});
});

test('prepareSend returns raw base64 images for every supported type and refuses everything else',()=>{
  const dir=tmp(),png=Buffer.from([1,2,3,4]),s=stored(dir,ID,png);
  assert.deepEqual(s.prepareSend(undefined),{refs:[],images:[]});
  assert.deepEqual(s.prepareSend([]),{refs:[],images:[]});
  for(const mimeType of ['image/png','image/jpeg','image/webp','image/gif']){
    const out=s.prepareSend([ref(ID,{mimeType})]);
    assert.deepEqual(out,{refs:[ref(ID,{mimeType})],images:[{data:b64(png),mimeType}]});
    assert.ok(!out.images[0].data.startsWith('data:'));
  }
  assert.throws(()=>s.prepareSend([ref(ID,{kind:'file'})]),/仅支持图片/);
  assert.throws(()=>s.prepareSend([ref(ID,{kind:'audio',mimeType:'audio/wav'})]),/仅支持图片/);
  assert.throws(()=>s.prepareSend([ref(ID,{mimeType:'application/pdf'})]),/仅支持图片/);
  assert.throws(()=>s.prepareSend([ref(ID,{mimeType:'image/svg+xml'})]),/仅支持图片/);
  assert.throws(()=>s.prepareSend([ref(ID,{size:0})]),/Invalid attachment/);
  assert.throws(()=>s.prepareSend([ref(ID,{size:5})]),/incomplete/);
  assert.throws(()=>s.prepareSend([ref(ID2)]),/incomplete/);
  assert.throws(()=>s.prepareSend([ref(ID,{name:' '})]),/Invalid attachment/);
  assert.throws(()=>s.prepareSend('x'),/Invalid attachments/);
  assert.throws(()=>s.prepareSend(Array(21).fill(ref(ID))),/Invalid attachments/);
  rmSync(dir,{recursive:true,force:true});
});

test('prepareSend enforces the combined size cap',()=>{
  const dir=tmp(),s=new AttachmentStore(dir),half=MAX_ATTACHMENT_BYTES/2+1;
  // Sparse files keep the test cheap while stat().size reports the declared size.
  for(const id of [ID,ID2]){s.upload({id,offset:0,size:0,data:''});truncateSync(s.pathFor(id),half);}
  assert.throws(()=>s.prepareSend([ref(ID,{size:half}),ref(ID2,{size:half})]),/20 MB/);
  assert.equal(s.prepareSend([ref(ID,{size:half})]).images.length,1);
  rmSync(dir,{recursive:true,force:true});
});

test('bound attachments survive restart, are read in 3-byte aligned chunks and never cross sessions',()=>{
  const dir=tmp(),bytes=Buffer.alloc(MAX_CHUNK_BYTES*2+7,7),s=new AttachmentStore(dir);
  for(let at=0;at<bytes.length;at+=MAX_CHUNK_BYTES)s.upload({id:ID,offset:at,size:bytes.length,data:b64(bytes.subarray(at,at+MAX_CHUNK_BYTES))});
  const r=ref(ID,{size:bytes.length});s.bindToMessage('s1','m1',[r]);
  const again=new AttachmentStore(dir);
  assert.deepEqual(again.forMessage('s1','m1'),[r]);assert.equal(again.forMessage('s1','nope'),null);assert.equal(again.forMessage('s2','m1'),null);
  const parts=[];let offset=0;
  while(offset<bytes.length){
    const chunk=again.read({sessionId:'s1',id:ID,offset});
    assert.equal(chunk.size,bytes.length);
    if(chunk.offset<bytes.length)assert.equal(Buffer.from(chunk.data,'base64').length%3,0);
    parts.push(Buffer.from(chunk.data,'base64'));offset=chunk.offset;
  }
  assert.ok(Buffer.concat(parts).equals(bytes));
  assert.throws(()=>again.read({sessionId:'s2',id:ID,offset:0}),/not found/);
  assert.throws(()=>again.read({sessionId:'s1',id:ID2,offset:0}),/not found/);
  assert.throws(()=>again.read({sessionId:'s1',id:ID,offset:bytes.length+1}),/offset/);
  assert.throws(()=>again.read({sessionId:'s1',id:ID,offset:'0'}),/offset/);
  rmSync(dir,{recursive:true,force:true});
});

test('binding keeps only the most recent messages per session and tolerates a corrupt index',()=>{
  const dir=tmp(),s=new AttachmentStore(dir);
  for(let i=0;i<205;i++)s.bindToMessage('s',`m${i}`,[ref(ID)]);
  assert.equal(s.forMessage('s','m0'),null);assert.ok(s.forMessage('s','m204'));assert.equal(Object.keys(s.index.s).length,200);
  writeFileSync(s.indexPath,'{broken');
  assert.deepEqual(new AttachmentStore(dir).index,{});
  rmSync(dir,{recursive:true,force:true});
});
