#!/usr/bin/env python3
"""Exercise the packaged native --paseo-probe and identical production bridge.
Default output is metadata only. Writes require an explicitly named validation agent.
"""
import argparse, json, subprocess, selectors, time
from pathlib import Path
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--app',type=Path,required=True)
parser.add_argument('--agent',help='Exact parent-created validation agent ID')
parser.add_argument('--server',help='Refuse a different server identity')
parser.add_argument('--text',help='Send one prompt to the named validation agent')
parser.add_argument('--allow-write',action='store_true')
parser.add_argument('--seconds',type=float,default=12)
parser.add_argument('--cancel-turn',help='Explicit turn ID from the same validation agent')
args=parser.parse_args()
if (args.text or args.cancel_turn) and (not args.allow_write or not args.agent):parser.error('Writes require --allow-write and --agent (use a parent-created validation agent only)')
exe=args.app/'Contents/MacOS/monocode-gpui'
p=subprocess.Popen([str(exe),'--paseo-probe'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,bufsize=1)
selector=selectors.DefaultSelector();selector.register(p.stdout,selectors.EVENT_READ)
serial=0;server=None;opened=False;wrote=False;got_list=False;got_snapshot=False;failure=False

def send(op,**fields):
 global serial
 serial+=1
 p.stdin.write(json.dumps(dict(id=serial,op=op,serverId=server,agentId=args.agent,**fields))+'\n');p.stdin.flush()
 return serial

try:
 end=time.monotonic()+args.seconds
 while time.monotonic()<end:
  if not selector.select(.5):continue
  line=p.stdout.readline()
  if not line:break
  value=json.loads(line)
  if value.get('event')=='connection':
   print(json.dumps(value,ensure_ascii=False),flush=True)
   if value.get('status')=='connected' and not server:
    server=value['serverId']
    if args.server and args.server!=server:raise RuntimeError('Server identity mismatch')
    send('list')
    if args.agent:send('open')
  elif value.get('event')=='snapshot':
   got_snapshot=True;agent=value.get('agent',{})
   print(json.dumps({'snapshot':{'serverId':value['serverId'],'agentId':value['agentId'],'status':agent.get('status'),
    'turnId':(agent.get('activeTurn') or {}).get('turnId'),'epoch':value.get('epoch'),
    'seqs':[[e['seqStart'],e['seqEnd']] for e in value.get('entries',[])],
    'nativeRowCount':value.get('nativeRowCount'),'permissions':[r['id'] for r in agent.get('pendingPermissions',[])]}},ensure_ascii=False),flush=True)
   if args.text and not wrote:
    import uuid
    wrote=True;send('send',text=args.text,messageId=str(uuid.uuid4()),acknowledgeRace=True)
   elif args.cancel_turn and not wrote:wrote=True;send('cancel',turnId=args.cancel_turn)
  elif value.get('id'):
   if value.get('value',{}).get('entries') is not None:
    got_list=True
    print(json.dumps({'directory':{'count':len(value['value']['entries']),'hasMore':value['value']['pageInfo']['hasMore']}}),flush=True)
   else:
    print(json.dumps(value,ensure_ascii=False),flush=True)
    if value.get('ok') is False:failure=True
  elif value.get('event')=='error':print(json.dumps(value),flush=True);failure=True
finally:
 p.stdin.close()
 try:p.wait(timeout=3)
 except subprocess.TimeoutExpired:p.terminate();p.wait(timeout=3)
 if p.stderr:
  error=p.stderr.read()
  if error:print(error[-1000:])
if not server or not got_list or (args.agent and not got_snapshot):raise SystemExit(2)
if failure:raise SystemExit(1)
