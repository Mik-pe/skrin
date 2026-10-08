import pathlib,re,json,statistics
root=pathlib.Path(__file__).resolve().parent
def fields(line):
    result={}
    for x in line.split(',')[1:]:
        if '=' not in x: continue
        key,value=x.split('=',1)
        try: value=float(value) if '.' in value else int(value)
        except ValueError: pass
        result[key]=value
    return result
def parse(path):
    data=path.read_text()
    assert 'completed,scratch_retained=' in data, f'incomplete/failed run: {path}'
    assert data.count('fresh_process_exact_rows_indexes_and_operations=verified')==2,path
    result={'file':path.name}
    for name in ['workload','point','area','inventory','durable_save','durable_save_rate','frame_work_60hz','frame_start_lateness','frame_overlap','checkpoint','reclaim']:
        line=next(l for l in data.splitlines() if l.startswith(name+','))
        result[name]=fields(line)
    result['sync_groups']=[fields(l) for l in data.splitlines() if l.startswith('sync_groups,')]
    assert sum(x['requests']*x['groups'] for x in result['sync_groups'])==result['workload']['saves'],path
    result['saved_rss_kib']=int(re.search(r'memory,phase=saved,VmRSS: (\d+) kB',data).group(1))
    result['disk']=[fields(l) for l in data.splitlines() if l.startswith('disk,')]
    result['fresh_process_open']=[fields(l) for l in data.splitlines() if l.startswith('fresh_process_open,')]
    return result
files=sorted(root.glob('*.txt'))
runs=[x for f in files if (x:=parse(f))]
(root/'summary.json').write_text(json.dumps({'runs':runs},indent=2)+'\n')
for prefix in ['cpu-paired','nvme']:
    groups={}
    for r in runs:
        if not r['file'].startswith(prefix): continue
        w=r['workload']; label=(('before' if '-before-' in r['file'] else 'after')+' ' if prefix=='cpu-paired' else '')+w['mode']+' w'+str(w['window'])
        groups.setdefault(label,[]).append(r)
    print(prefix)
    for label,values in groups.items():
        if len(values)!=3: continue
        rates=[v['durable_save_rate']['saves_per_s'] for v in values]
        p99=[v['durable_save']['p99_us']/1000 for v in values]
        reads=[statistics.median(v[n]['p50_us'] for v in values) for n in ['point','area','inventory']]
        frames=[v['frame_work_60hz']['p99_us']/1000 for v in values]
        rss=[v['saved_rss_kib'] for v in values]
        print(label,'saves/s',rates,'median',statistics.median(rates),'p99ms',p99,'read p50us',reads,'framesp99ms',frames,'rssKiB',rss)
