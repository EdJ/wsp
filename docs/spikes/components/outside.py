import json,os,glob,collections,re,time,statistics as st,sys
src=open(os.environ['WSP_SCRATCH']+'/comp3.py').read().split("MOVED=")[0]
ns={};exec(src.replace("g=json.load(open(os.environ['WSP_SCRATCH']+'/graph.json'))","g={'edges':{'prod':{},'test':{}},'mods':{}}"),ns);A=ns['A']
files=[f for f in glob.glob(os.path.expanduser('~/.claude/projects/*wsp--worktrees*/*.jsonl')) if time.time()-os.path.getmtime(f)<7*86400]
inside=0;outside=0;per=[];nses=0;ncomp=collections.Counter()
for f in files:
    title=None;tool={};edits=set();loads=[]
    for l in open(f,errors='replace'):
        try:d=json.loads(l)
        except: continue
        t=d.get('type')
        if t=='custom-title': title=d.get('customTitle')
        if t=='assistant':
            c=d.get('message',{}).get('content')
            if isinstance(c,list):
                for x in c:
                    if x.get('type')=='tool_use':
                        tool[x['id']]=(x['name'],x.get('input',{}))
                        if x['name'] in('Edit','Write'):
                            m=re.search(r'/src/(\w+)',x['input'].get('file_path',''))
                            if m: edits.add(m.group(1).replace('.rs',''))
                        if x['name']=='Bash':
                            for m in re.finditer(r'(?:git (?:add|commit)|sed -i|>>?)\s.*?src/(\w+)\.rs',x['input'].get('command','')): edits.add(m.group(1))
        elif t=='user':
            c=d.get('message',{}).get('content')
            if isinstance(c,list):
                for x in c:
                    if x.get('type')=='tool_result' and x.get('tool_use_id') in tool:
                        n,i=tool[x['tool_use_id']];cont=x.get('content')
                        s=sum(len(y.get('text','')) for y in cont if isinstance(y,dict)) if isinstance(cont,list) else len(cont or '')
                        fs=[]
                        if n=='Read': fs=re.findall(r'/src/(\w+)',i.get('file_path',''))
                        elif n=='Bash':
                            cmd=i.get('command','')
                            if re.search(r'\b(sed|cat|head|tail|grep|rg|awk|nl)\b',cmd) and not re.search(r'\bcargo (test|build|check)\b',cmd): fs=re.findall(r'src/(\w+)(?:\.rs)?',cmd)
                        fs=[x for x in fs if x in A]
                        for fl in set(fs): loads.append((fl,s/3.6/len(set(fs))))
    if not(title and re.match(r'wsp-\d+',title)) or not edits or not loads: continue
    ec={A[m] for m in edits if m in A}
    if not ec: continue
    nses+=1; ncomp[len(ec)]+=1
    i_=sum(v for fl,v in loads if A[fl] in ec); o_=sum(v for fl,v in loads if A[fl] not in ec)
    inside+=i_;outside+=o_; per.append(o_/(i_+o_))
print(nses,'sessions that edited src and read named files; components edited per session:',dict(sorted(ncomp.items())))
print('named-file tokens inside edited component(s): %d, outside: %d (%.0f%% outside)'%(inside,outside,100*outside/(inside+outside)))
print('per-session share outside: median %.0f%%'%(100*st.median(per)))
