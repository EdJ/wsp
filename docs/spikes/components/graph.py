import re,os,sys,json,collections
root='src'
mods={}  # module name -> list of files
for dp,dn,fn in os.walk(root):
    for f in fn:
        if not f.endswith('.rs'): continue
        p=os.path.join(dp,f); rel=os.path.relpath(p,root)
        parts=rel[:-3].split('/')
        if parts[-1]=='mod': parts=parts[:-1]
        top=parts[0]
        mods.setdefault(top,[]).append(p)
top=set(mods)
def strip(s):
    # remove block comments, line comments, string literals (rough)
    s=re.sub(r'/\*.*?\*/','',s,flags=re.S)
    out=[]
    for line in s.split('\n'):
        # drop // comments not in strings (rough)
        i=0;instr=False;res=''
        while i<len(line):
            c=line[i]
            if instr:
                if c=='\\': i+=2; continue
                if c=='"': instr=False
                i+=1; continue
            if c=='"': instr=True; i+=1; continue
            if c=='/' and line[i:i+2]=='//': break
            res+=c;i+=1
        out.append(res)
    return '\n'.join(out)
def split_test(s):
    m=re.search(r'^#\[cfg\(test\)\]\s*\n\s*(pub(\(crate\))? )?mod \w+ \{',s,flags=re.M)
    if m: return s[:m.start()],s[m.start():]
    return s,''
def refs(code,self_mod):
    out=collections.defaultdict(set)
    # use crate::{...} groups
    def expand(prefix,body):
        # body: comma-separated items possibly with nested braces
        items=[];depth=0;cur=''
        for ch in body:
            if ch=='{':depth+=1
            if ch=='}':depth-=1
            if ch==',' and depth==0: items.append(cur.strip());cur=''
            else: cur+=ch
        if cur.strip(): items.append(cur.strip())
        for it in items:
            m=re.match(r'^(.*?)\{(.*)\}$',it,flags=re.S)
            if m: expand(prefix+m.group(1),m.group(2))
            else: yield_ref(prefix+it)
    def yield_ref(path):
        path=path.strip()
        path=re.sub(r'\s+as\s+\w+$','',path)
        segs=[x for x in path.split('::') if x]
        if not segs: return
        if segs[0] in top or segs[0]=='main':
            m=segs[0]; sym=segs[1] if len(segs)>1 else '*'
            out[m].add(sym)
        else:
            out['main'].add(segs[0])
    for m in re.finditer(r'use\s+crate::\{(.*?)\};',code,flags=re.S):
        expand('',m.group(1))
    for m in re.finditer(r'use\s+crate::([\w:]+(?:::\*)?)(?:\s+as\s+\w+)?;',code):
        yield_ref(m.group(1))
    for m in re.finditer(r'use\s+crate::(\w+)::\{(.*?)\};',code,flags=re.S):
        mod=m.group(1)
        expand(mod+'::',m.group(2))
    # inline paths crate::x::y
    for m in re.finditer(r'(?<![\w:])crate::(\w+)(?:::(\w+))?',code):
        a,b=m.group(1),m.group(2)
        if a in top: out[a].add(b or '*')
        else: out['main'].add(a)
    return out
edges={'prod':collections.defaultdict(lambda:collections.defaultdict(set)),'test':collections.defaultdict(lambda:collections.defaultdict(set))}
lines={}
tl={}
for mod,files in mods.items():
    for p in files:
        raw=open(p).read()
        prod,test=split_test(raw)
        lines[mod]=lines.get(mod,0)+prod.count('\n')
        tl[mod]=tl.get(mod,0)+test.count('\n')
        for kind,txt in (('prod',prod),('test',test)):
            r=refs(strip(txt),mod)
            for b,syms in r.items():
                if b==mod: continue
                edges[kind][mod][b]|=syms
# main.rs symbols (defined in main) shows as 'main'
json.dump({'mods':{m:{'prod':lines[m],'test':tl[m]} for m in mods},
 'edges':{k:{a:{b:sorted(s) for b,s in v.items()} for a,v in e.items()} for k,e in edges.items()}},open(os.environ['WSP_SCRATCH']+'/graph.json','w'))
print(len(mods),'modules', sum(lines.values()),'prod', sum(tl.values()),'test')
