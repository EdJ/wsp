import json,os,glob,collections,re,time,statistics
files=[f for f in glob.glob(os.path.expanduser('~/.claude/projects/*wsp--worktrees*/*.jsonl')) if time.time()-os.path.getmtime(f)<7*86400]
BIG={'cmd_agent','cmd_spawn','cmd_worklist','cycle','story','cmd_watch','agent_commands'}
cat=collections.Counter();catn=collections.Counter();perfile=collections.Counter()
tw=collections.Counter()  # token-requests by category
sessions=0;per=[]
for f in files:
    title=None;tool={};reqs=set();items=[]
    for l in open(f,errors='replace'):
        try:d=json.loads(l)
        except: continue
        t=d.get('type')
        if t=='custom-title': title=d.get('customTitle')
        if t=='assistant':
            rid=d.get('requestId') or d.get('message',{}).get('id')
            if rid: reqs.add(rid)
            c=d.get('message',{}).get('content')
            if isinstance(c,list):
                for x in c:
                    if x.get('type')=='tool_use': tool[x['id']]=(x['name'],x.get('input',{}))
        elif t=='user':
            c=d.get('message',{}).get('content')
            if isinstance(c,list):
                for x in c:
                    if x.get('type')=='tool_result' and x.get('tool_use_id') in tool:
                        n,i=tool[x['tool_use_id']]
                        cont=x.get('content')
                        s=sum(len(y.get('text','')) for y in cont if isinstance(y,dict)) if isinstance(cont,list) else len(cont or '')
                        k=n
                        files_=[]
                        if n=='Read':
                            p=i.get('file_path','')
                            if '/src/' in p and p.endswith('.rs'): k='src-read'; files_=[re.search(r'/src/([\w/]+)\.rs',p).group(1)]
                            else: k='other-read'
                        elif n=='Bash':
                            cmd=i.get('command','')
                            fs=re.findall(r'src/([\w/]+)\.rs',cmd)
                            if fs and re.search(r'\b(sed|cat|head|tail|grep|rg|awk|nl)\b',cmd) and not re.search(r'\bcargo (test|build|check)\b',cmd): k='bash-src'; files_=fs
                            else: k='bash-other'
                        elif n=='Grep': k='grep'
                        items.append((k,s,len(reqs),files_))
    if not (title and re.match(r'wsp-\d+',title) and reqs): continue
    sessions+=1; n=len(reqs); row=collections.Counter()
    for k,s,at,fs in items:
        cat[k]+=s;catn[k]+=1;tw[k]+=s/3.6*(n-at);row[k]+=s/3.6
        for fl in fs: perfile[fl]+=s/(3.6*max(1,len(fs)))
    per.append((title,n,row))
tot=sum(cat.values())
print(sessions,'sessions; tool-result bytes total',tot,'~tokens',int(tot/3.6))
for k,v in cat.most_common(): print(f'  {k:11} {v/3.6:11.0f} tokens in {catn[k]:5} results; token-requests {tw[k]:14.0f} ({tw[k]/sum(tw.values()):.0%})')
srcr=[ (t,n,r['src-read']+r['bash-src']+r['grep']) for t,n,r in per]
v=sorted(x[2] for x in srcr)
print('per session, code-finding tokens (src-read+bash-src+grep): median %d p90 %d mean %d'%(statistics.median(v),v[int(.9*len(v))-1],sum(v)/len(v)))
print('top src files by tokens loaded:')
bigsum=0;allsum=sum(perfile.values())
for m,vv in perfile.most_common(15): print(f'  {m:22} {int(vv):8}')
bigsum=sum(vv for m,vv in perfile.items() if m.split('/')[0] in BIG)
print('7 big files share of file-attributed tokens: %.0f%% of %d'%(100*bigsum/allsum,allsum))
