import json,os,sys,collections
g=json.load(open(os.environ['WSP_SCRATCH']+'/graph.json'))
E=g['edges']['prod'];ET=g['edges']['test'];M=g['mods']
A={}
def put(c,*ms):
    for m in ms: A[m]=c
put('base','util','fm','input','risk','disk','memory','choices','main')
put('store','model','store','resolve','message','lapse','artefact','kind_data','sharing','scratch','cmd_project','cmd_migrate','cmd_mandate')
put('wire','herdr','place','place_herdr','place_compound','place_super','tunnel','far','sync','launchd','detect_override')
put('top','daemon','fake')
put('trees','cmd_checkout','cmd_verify','treecopy','check_tree','build_output','throttle','warm','overlap','guard','greens','cold','leg_tree','affected')
put('engine','caption','stage','history','after','worklist','cycle','barrier','barrier_run','barrier_replay','verification','check','did','closing','ending','repair','split','starts','stand_down','untaken','waiting','legs','leg_seat','cmd_worklist','cmd_task','packet','packet_gen','attention','kind_attention','cmd_attend','cmd_message','hold','hold_close')
put('seats','agent_commands','cmd_spawn','cmd_agent','cmd_resume','wake','room','cmd_govern','cmd_machine','cmd_sandbox','cmd_watch','cmd_brief','cmd_install','cmd_stamp')
put('timeline','timeline','cmd_cost','cmd_burn','cmd_report','cmd_attempts','cmd_verifications')
put('surface','panel','detail','draw','kanban','live','story','arrange')
MOVED={'my_pane','local_backends','LOCAL_BACKEND_NAMES','log_line','governs','is_primary','seat_for','seat_query','seat_above_in','is_agent_id','new_agent_id','alive','list_root','reconcile','herdr_bin','processes','ancestors','row_of','parse_subject','command','read_source','fold','Rect','Args','build_stamp','stamp_word','BOARD_LABEL','FULL_LABEL','PANEL_LABEL','VIEW_LABEL','glyph','stty','WORKTREES','git','toplevel','git_ok','trunk','trunk_branch','trunk_tip','SEAT_ENV'}
order=['base','store','wire','trees','engine','seats','timeline','surface','top']
if len(sys.argv)>1 and sys.argv[1]=='order2': order=['base','store','wire','trees','seats','engine','timeline','surface']
rank={c:i for i,c in enumerate(order)}
def run(X,show=False):
    cnt=collections.defaultdict(lambda:collections.defaultdict(set));up=[]
    for a,bs in X.items():
        for b,s in bs.items():
            if A[a]==A[b]: continue
            s2=[x for x in s if x not in MOVED]
            if not s2: continue
            cnt[A[a]][A[b]]|={f'{b}::{x}' for x in s2}
            if rank[A[b]]>rank[A[a]]: up.append((a,b,sorted(s2)))
    return cnt,up
cnt,up=run(E)
print('prod up edges',len(up),'symbols',sum(len(s) for *_,s in up))
for c in order:
    ms=[m for m in M if A.get(m)==c]
    print(f'  {c:9} {len(ms):3} mods {sum(M[m]["prod"] for m in ms):6} prod {sum(M[m]["test"] for m in ms):6} test; up-out',sum(len(s) for a,b,s in up if A[a]==c))
if '-v' in sys.argv:
  for a,b,s in sorted(up,key=lambda x:(rank[A[x[0]]],x[0])): print(f'  {A[a]}:{a} -> {A[b]}:{b} {s[:6]}')
tc,tu=run(ET)
print('test-only up edges',len(tu))
