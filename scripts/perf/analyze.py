"""Summarize retained results; never treats missing observations as zero."""
import csv, datetime, glob, json, pathlib, re, sys
root=pathlib.Path(sys.argv[1])
observations=[json.loads(x) for x in open(root/'observations.jsonl')]
def ts(s): return datetime.datetime.fromisoformat(s.replace('Z','+00:00')).timestamp()
def metrics(text):
    return {line.rsplit(' ',1)[0]:float(line.rsplit(' ',1)[1]) for line in text.splitlines() if line and not line.startswith('#')}
def queries(path):
    return {r['queryid']:r for r in csv.DictReader(open(path))}
container_samples=[]
when=None
for line in open(root/'container-stats.jsonl'):
    if line.startswith('{'):
        value=json.loads(line);value['time']=when;container_samples.append(value)
    else: when=ts(line.strip())
rows=[]
for path in sorted(root.glob('sweep/*/results.json')):
    r=json.load(open(path)); start,end=ts(r['wallStart']),ts(r['wallEnd']); obs=[o for o in observations if start<=o['time']<=end]
    item={'name':path.parent.name,**r['summary'],'generator':r['generator'],'readers':r['readers']}
    lat=sorted(s['terminalMs']-s['dispatchMs'] for s in r['samples'] if 'terminalMs' in s and 'dispatchMs' in s)
    item['terminalFromDispatchMs']={f'p{p}':lat[min(len(lat)-1,__import__('math').ceil(len(lat)*p/100)-1)] if lat else None for p in [50,95,99]}
    origin=r['samples'][0]['scheduledMs']; stop=origin+r['config']['durationMs']
    item['outstandingAtArrivalStop']=sum(s.get('dispatchMs',float('inf'))<=stop and s.get('settledMs',s.get('terminalMs',float('inf')))>stop for s in r['samples'])
    item['containers']={}
    for name in ['lightspeed-postgres','lightspeed-temporal','lightspeed-minio']:
        cs=[c for c in container_samples if c['Name']==name and start<=c['time']<=end]
        if cs:item['containers'][name]={'meanCpuPercent':sum(float(c['CPUPerc'].strip('%')) for c in cs)/len(cs),'memorySamples': [cs[0]['MemUsage'],cs[-1]['MemUsage']],'blockIOSamples':[cs[0]['BlockIO'],cs[-1]['BlockIO']]}

    if len(obs)>1:
        a,b=obs[0],obs[-1]; dt=b['time']-a['time']
        item['processes']={pid:{'cpuCores':(b['processes'][pid]['cpu_ticks']-v['cpu_ticks'])/100/dt,'peakRssMiB':max(o['processes'].get(pid,{}).get('rss_bytes',0) for o in obs)/2**20} for pid,v in a['processes'].items() if pid in b['processes']}
        ma,mb=metrics(a.get('temporal','')),metrics(b.get('temporal',''))
        item['temporalScrapeFailures']=sum('temporal_error' in o for o in obs)
        item['temporalMeanMs']={}
        for prefix in ['temporal_workflow_task_schedule_to_start_latency','temporal_activity_schedule_to_start_latency','temporal_activity_execution_latency','temporal_workflow_task_execution_latency']:
            sums=sum(v-ma.get(k,0) for k,v in mb.items() if k.startswith(prefix+'_sum'))
            counts=sum(v-ma.get(k,0) for k,v in mb.items() if k.startswith(prefix+'_count'))
            item['temporalMeanMs'][prefix]=sums/counts if counts else None
        item['activityMeanMs']={}
        for k,v in mb.items():
            if k.startswith('temporal_activity_execution_latency_sum'):
                count_key=k.replace('_sum{','_count{')
                n=mb.get(count_key,0)-ma.get(count_key,0)
                item['activityMeanMs'][re.search(r'activity_type="([^"]+)"',k).group(1)]=(v-ma.get(k,0))/n if n else None
        item['temporalPeakSlots']={kind:max([float(line.rsplit(' ',1)[1]) for o in obs for line in o.get('temporal','').splitlines() if line.startswith('temporal_worker_task_slots_used{') and f'worker_type="{kind}"' in line], default=None) for kind in ['ActivityWorker','WorkflowWorker']}
    before,after=queries(path.parent/'pg-before.csv'),queries(path.parent/'pg-after.csv')
    q=[]
    for key,v in after.items():
        old=before.get(key,{})
        q.append({'query':v['query'],'calls':int(v['calls'])-int(old.get('calls',0)), 'executionMs':float(v['total_exec_time'])-float(old.get('total_exec_time',0))})
    item['pgCalls']=sum(v['calls'] for v in q);item['pgExecutionMs']=sum(v['executionMs'] for v in q)
    item['casQueryCalls']=sum(v['calls'] for v in q if 'cas_' in v['query']);item['casQueryMs']=sum(v['executionMs'] for v in q if 'cas_' in v['query'])
    item['topQueries']=sorted(q,key=lambda x:x['executionMs'],reverse=True)[:8]
    rows.append(item)
json.dump(rows,open(root/'analysis.json','w'),indent=2)
def fmt(v): return 'n/a' if v is None else f'{v:.0f}'
for r in rows:
    s=r
    print(f"| {s['name']} | {s['submitted']} / {s['scheduled']} | {s['completed']} | {s['completedDuringArrivalsPerSecond']:.2f} | {fmt(s['terminalMs']['p50'])} / {fmt(s['terminalMs']['p95'])} / {fmt(s['terminalMs']['p99'])} | {s['failed']} / {s['unfinished']} / {s['dropped']} | {s['drainMs']:.0f} | {s['pgCalls']} | {s['pgExecutionMs']:.0f} |")
