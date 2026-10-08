import pathlib,json,re,collections,statistics
root=pathlib.Path(__file__).resolve().parent;runs=[]
for p in sorted(list(root.glob('cpu-*.txt'))+list(root.glob('nvme-*.txt'))+list(root.glob('confirm-*.txt'))):
 s=p.read_text()
 assert 'completed,scratch_retained=' in s,p
 assert s.count('fresh_process_exact_rows_indexes_and_operations=verified')==2,p
 def fields(name):
  line=next(x for x in s.splitlines() if x.startswith(name+','));out={}
  for item in line.split(',')[1:]:
   if '=' not in item:continue
   k,v=item.split('=',1)
   try:v=float(v) if '.' in v else int(v)
   except ValueError:pass
   out[k]=v
  return out
 r={'file':p.name,'workload':fields('workload')}
 for name in ['point','area','inventory','durable_save','durable_save_rate','frame_work_60hz','frame_overlap','checkpoint','reclaim']:r[name]=fields(name)
 r['rss_kib']={phase:int(re.search(r'memory,phase='+phase+r',VmRSS: (\d+)',s).group(1)) for phase in ['seeded','saved','after_maintenance']}
 r['sync_groups']=[{'requests':int(a),'groups':int(b)} for a,b in re.findall(r'^sync_groups,requests=(\d+),groups=(\d+)$',s,re.M)]
 assert sum(x['requests']*x['groups'] for x in r['sync_groups'])==r['workload']['saves'],p
 r['save_max_ms']=r['durable_save']['max_us']/1000
 runs.append(r)
assert len(runs)==30
(root/'summary.json').write_text(json.dumps({'runs':runs},indent=2)+'\n')
groups=collections.defaultdict(list)
for r in runs:groups[(r['file'].split('-')[0],r['workload']['mode'],'before' if '-before-' in r['file'] else 'after')].append(r)
for label,rs in sorted(groups.items()):
 print(label,'RSS saved',[r['rss_kib']['saved'] for r in rs],'rate',[r['durable_save_rate']['saves_per_s'] for r in rs],'savep99us',[r['durable_save']['p99_us'] for r in rs],'maxms',[r['save_max_ms'] for r in rs],'readp50us',[[r[n]['p50_us'] for r in rs] for n in ['point','area','inventory']])
