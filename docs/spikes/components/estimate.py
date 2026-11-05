"""Rebuild and test share per commit under the layered cut, and the contract-crate estimate.

Run from the repo root after graph.py (which writes $WSP_SCRATCH/graph.json) and with assign.py beside it.
Reads `git log master --since=2026-10-22`; prints the mean share of prod lines rebuilt and of tests rerun.
"""
import os,re,subprocess,collections,sys,glob,statistics as st
src=open(os.path.join(os.path.dirname(__file__),'assign.py')).read().split("cnt,up=run(E)")[0]
ns={'__name__':'x'};sys.argv=['x'];exec(src,ns)
A,M,E,order,MOVED,rank=ns['A'],ns['M'],ns['E'],ns['order'],ns['MOVED'],ns['rank']
A['main']='top'
dep=collections.defaultdict(set)
for a,bs in E.items():
    for b,s in bs.items():
        if A[a]!=A[b] and rank[A[b]]<rank[A[a]] and [x for x in s if x not in MOVED]: dep[A[a]].add(A[b])
def clo(c,seen=None):
    seen=seen if seen is not None else set()
    for d in dep[c]:
        if d not in seen: seen.add(d);clo(d,seen)
    return seen
L=collections.Counter();tests=collections.Counter()
for m in M: L[A[m]]+=M[m]['prod']
for p in glob.glob('src/**/*.rs',recursive=True):
    m=re.match(r'src/(\w+)',p).group(1).replace('.rs','')
    tests[A[m]]+=len(re.findall(r'^\s*#\[test\]',open(p).read(),flags=re.M))
reb={c:{c}|{d for d in order if c in clo(d)} for c in order}
out=subprocess.run(['git','log','master','--since=2026-10-22','--no-merges','--pretty=format:@%h','--name-only','--','src'],capture_output=True,text=True).stdout
commits=[];cur=None
for line in out.split('\n'):
    if line.startswith('@'): cur=set();commits.append(cur)
    elif line.startswith('src/') and cur is not None: cur.add(re.match(r'src/(\w+)',line).group(1).replace('.rs',''))
tl=sum(L.values());tt=sum(tests.values());bs=[];ts=[];own=[];cc=[]
for c in commits:
    comps={A[m] for m in c}
    r=set().union(*[reb[x] for x in comps])
    bs.append(sum(L[x] for x in r)/tl);ts.append(sum(tests[x] for x in r)/tt);own.append(sum(tests[x] for x in comps)/tt)
    cc.append((sum(L[x] for x in comps)+L['top'])/tl)
print(len(commits),'commits')
print('stack: rebuild mean %.0f%%, tests rerun (dependents) mean %.0f%%, own components only %.0f%%'%(100*st.mean(bs),100*st.mean(ts),100*st.mean(own)))
m=st.mean(cc);print('contract crates, edit inside an implementation: rebuild mean %.0f%%'%(100*m))
for p in (0.2,0.35,0.5): print(' %d%% of commits change a contract: %.0f%%'%(p*100,100*((1-p)*m+p)))
