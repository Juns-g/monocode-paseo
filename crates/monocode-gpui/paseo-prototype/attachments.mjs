// Image attachments for the Paseo adapter. Paseo's daemon takes inline base64 images on send;
// this store implements the Host chunk-upload protocol (host/attachments.ts) in front of it and
// remembers which uploads belong to which message so the transcript keeps its thumbnails.
import {closeSync,existsSync,mkdirSync,openSync,readFileSync,readSync,renameSync,statSync,writeFileSync,writeSync} from 'node:fs';
import {join} from 'node:path';

export const MAX_ATTACHMENT_BYTES=20*1024*1024;
export const MAX_ATTACHMENTS_PER_SEND=20;
export const MAX_CHUNK_BYTES=512*1024;
const MAX_MESSAGES_PER_SESSION=200;
const UUID_RE=/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export const ALLOWED_IMAGE_MIMES=new Set(['image/png','image/jpeg','image/webp','image/gif']);
const plain=v=>v!==null&&typeof v==='object'&&!Array.isArray(v);

export class AttachmentStore {
  constructor(directory) {
    this.dir=join(directory,'paseo-attachments');
    this.indexPath=join(directory,'paseo-attachments-index.json');
    this.index={};
    try{
      const value=JSON.parse(readFileSync(this.indexPath,'utf8'));
      if(plain(value))this.index=value;
    }catch(e){if(e.code!=='ENOENT'&&!(e instanceof SyntaxError))throw e;}
  }
  pathFor(id) {
    if(typeof id!=='string'||!UUID_RE.test(id))throw Error('Invalid attachment ID');
    return join(this.dir,id);
  }
  upload(input) {
    if(!plain(input))throw Error('Invalid attachment chunk');
    const path=this.pathFor(input.id),{offset,size,data:encoded}=input;
    if(!Number.isSafeInteger(offset)||offset<0||!Number.isSafeInteger(size)||size<0||size>MAX_ATTACHMENT_BYTES
      ||typeof encoded!=='string'||encoded.length>Math.ceil(MAX_CHUNK_BYTES/3)*4
      ||(encoded.length>0&&(!/^[A-Za-z0-9+/]*={0,2}$/.test(encoded)||encoded.length%4)))throw Error('Invalid attachment chunk');
    const bytes=Buffer.from(encoded,'base64');
    if(bytes.length>MAX_CHUNK_BYTES||offset+bytes.length>size)throw Error('Invalid attachment chunk size');
    mkdirSync(this.dir,{recursive:true,mode:0o700});
    const fd=openSync(path,offset===0?'a+':'r+',0o600);
    try{
      const length=statSync(path).size;
      if(length===offset)writeSync(fd,bytes,0,bytes.length,offset);
      else if(length>=offset+bytes.length){
        const existing=Buffer.alloc(bytes.length);
        readSync(fd,existing,0,existing.length,offset);
        if(!existing.equals(bytes))throw Error('Attachment retry does not match uploaded bytes');
      }else throw Error('Attachment chunks are out of order');
      return {offset:offset+bytes.length};
    }finally{closeSync(fd);}
  }
  // Validates every reference and returns normalized refs plus the daemon's {data,mimeType} images.
  prepareSend(value) {
    if(value===undefined||value===null||(Array.isArray(value)&&value.length===0))return {refs:[],images:[]};
    if(!Array.isArray(value)||value.length>MAX_ATTACHMENTS_PER_SEND)throw Error('Invalid attachments');
    const refs=[],images=[];let total=0;
    for(const entry of value){
      if(!plain(entry)||typeof entry.id!=='string'||!UUID_RE.test(entry.id)||typeof entry.name!=='string'||!entry.name.trim()||entry.name.length>255
        ||!Number.isSafeInteger(entry.size)||entry.size<1||entry.size>MAX_ATTACHMENT_BYTES)throw Error('Invalid attachment');
      if(entry.kind!=='image'||!ALLOWED_IMAGE_MIMES.has(entry.mimeType))throw Error('此适配层目前仅支持图片附件（PNG、JPEG、WebP、GIF）；其他文件请在 Paseo 发送。');
      total+=entry.size;
      if(total>MAX_ATTACHMENT_BYTES)throw Error('图片附件总大小超过 20 MB 限制。');
      const path=this.pathFor(entry.id);
      let actual=-1;try{actual=statSync(path).size;}catch(e){if(e.code!=='ENOENT')throw e;}
      if(actual!==entry.size)throw Error(`Attachment ${entry.name} is incomplete`);
      refs.push({id:entry.id,name:entry.name,mimeType:entry.mimeType,kind:'image',size:entry.size});
      images.push({data:readFileSync(path).toString('base64'),mimeType:entry.mimeType});
    }
    return {refs,images};
  }
  bindToMessage(sessionId,messageId,refs) {
    if(!refs?.length)return;
    const messages=this.index[sessionId]??={};
    delete messages[messageId];messages[messageId]=refs; // re-insert keeps insertion order == recency
    const keys=Object.keys(messages);
    for(const key of keys.slice(0,Math.max(0,keys.length-MAX_MESSAGES_PER_SESSION)))delete messages[key];
    mkdirSync(join(this.indexPath,'..'),{recursive:true});
    const temp=`${this.indexPath}.tmp`;
    writeFileSync(temp,JSON.stringify(this.index),{mode:0o600});renameSync(temp,this.indexPath);
  }
  forMessage(sessionId,messageId) {
    return this.index[sessionId]?.[messageId]??null;
  }
  // Serves only images bound to the requested session; client paths are never used.
  read(input) {
    if(!plain(input))throw Error('Image attachment not found');
    const attachment=Object.values(this.index[String(input.sessionId??'')]??{}).flat().find(file=>file.id===input.id&&file.kind==='image');
    if(!attachment)throw Error('Image attachment not found');
    const {offset}=input;
    if(!Number.isSafeInteger(offset)||offset<0||offset>attachment.size)throw Error('Invalid attachment offset');
    const path=this.pathFor(attachment.id);
    if(!existsSync(path)||statSync(path).size!==attachment.size)throw Error('Attachment is incomplete');
    // Non-final chunks are divisible by three so the client can join base64 pieces.
    const bytes=Buffer.alloc(Math.min(3*Math.floor(MAX_CHUNK_BYTES/3),attachment.size-offset));
    const fd=openSync(path,'r');
    try{
      const read=readSync(fd,bytes,0,bytes.length,offset);
      return {data:bytes.subarray(0,read).toString('base64'),offset:offset+read,size:attachment.size};
    }finally{closeSync(fd);}
  }
}
