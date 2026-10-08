import subprocess,pathlib,json,datetime,hashlib,time,os
out=pathlib.Path('/tmp/skrin-performance-evidence')
def cmd(args): return subprocess.check_output(args,text=True).strip()
def stamp(): return datetime.datetime.now(datetime.timezone.utc).isoformat()
metadata={'start_utc':stamp(),'source_commit':cmd(['git','rev-parse','HEAD']),'baseline_commit':'fd4c486','compiler':cmd(['rustc','+1.89.0','-Vv']),'uname':cmd(['uname','-a']),'cpu':cmd(['lscpu']),'filesystem':cmd(['findmnt','-T','/home/mikpe/.cache','-o','SOURCE,FSTYPE,OPTIONS','-n']),'devices':cmd(['lsblk','-o','NAME,MODEL,SIZE,TYPE,MOUNTPOINTS']),'governor':pathlib.Path('/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor').read_text().strip(),'binary_sha256':{v:hashlib.sha256(pathlib.Path(f'/tmp/skrin-game-world-{v}').read_bytes()).hexdigest() for v in ['before','after']},'source_sha256':{str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in [pathlib.Path('crates/skrin/benches/game_world.rs'),pathlib.Path('crates/skrin/benches/support/game_sqlite.rs'),pathlib.Path('crates/skrin/examples/support/world.rs')]},'runs':[],'caveats':'Shared workstation; other repository Actions runner executing Java compilation/test workloads. No control of CPU clocks/device caches/background filesystem load. No local compilation/validation during measurements. tmpfs results exclude physical durability. All runs retained; no outlier rejection.'}
configs=[('native',1),('snapshot',1),('group_snapshot',1),('sqlite',1),('sqlite_bulk',1),('group_snapshot',8),('sqlite_bulk',8),('sqlite_batch',8)]
for repeat in range(1,4):
    ordered=configs[(repeat-1)*3:]+configs[:(repeat-1)*3]
    for mode,window in ordered:
        name=f'nvme-{repeat}-{mode}-w{window}.txt'
        args=['/tmp/skrin-game-world-after','/home/mikpe/.cache',mode,'100000','128','64',str(window)]
        info={'output':name,'argv':args,'start_utc':stamp(),'processes_start':cmd(['ps','-eo','pid,comm,pcpu','--sort=-pcpu']).splitlines()[:12]}
        with (out/name).open('w') as f:
            result=subprocess.run(args,stdout=f,stderr=subprocess.STDOUT)
        info['end_utc']=stamp();info['exit_code']=result.returncode
        metadata['runs'].append(info)
        (out/'metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
        print(name,'exit',result.returncode,flush=True)
        if result.returncode: raise SystemExit(result.returncode)
metadata['end_utc']=stamp()
(out/'metadata.json').write_text(json.dumps(metadata,indent=2)+'\n')
