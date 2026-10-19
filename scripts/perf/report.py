"""Render compact, pasteable tables from analyze.py output and retained samples."""
import json, pathlib, sys
root=pathlib.Path(sys.argv[1]); rows=json.load(open(root/'analysis.json'))
def key(r):
    name=r['name']; group=next(i for i,p in enumerate(['rapid-','active-readers-','idle-readers-','history-']) if name.startswith(p))
    return group,int(next(part for part in name.split('-') if part.isdigit())),name
rows.sort(key=key)
def f(v): return 'n/a' if v is None else f'{v:.0f}'
def q(v):return '/'.join(f(v.get('p'+str(p))) for p in [50,95,99])
print('All main steps use 30s arrivals. Rates are per second; latencies/drain are milliseconds. F/U/D = failed/unfinished/dropped. Completed rates show during-arrivals / including-drain. Read rows count HTTP reads, not agent runs.\n')
print('| Step | Offered/s | Submitted/s | Completed/s (arrival/drain) | Scheduled/submitted/completed | F/U/D | Outstanding at stop | Drain ms |\n|---|---:|---:|---:|---:|---:|---:|---:|')
for r in rows:
 print(f"| {r['name']} | {r['offeredPerSecond']:.0f} | {r['submittedPerSecond']:.2f} | {r['completedDuringArrivalsPerSecond']:.2f}/{r['completedIncludingDrainPerSecond']:.2f} | {r['scheduled']}/{r['submitted']}/{r['completed']} | {r['failed']}/{r['unfinished']}/{r['dropped']} | {r['outstandingAtArrivalStop']} | {r['drainMs']:.0f} |")
print('\nEvery submitted run in these tables was admitted. Read admission is not a run admission. Latencies below describe terminal results, excluding transport failures and dropped work. First visible means first run-associated event observed after admission; it may be the acceptance event, not first output.\n')
print('| Step | Admission p50/p95/p99 | First visible p50/p95/p99 | Terminal p50/p95/p99 | Dispatch lag p99 |\n|---|---:|---:|---:|---:|')
for r in rows: print(f"| {r['name']} | {q(r['admissionMs'])} | {q(r['firstProgressMs'])} | {q(r['terminalMs'])} | {r['dispatchLagMs']['p99']:.2f} |")
print('\nCPU is average cores (1.0 = one core). Runtime RSS is sampled peak; PG/Temporal RSS is the final container sample. Sampling is approximate at window boundaries.\n')
print('| Step | Runtime cores / peak MiB | PG cores / RSS | Temporal cores / RSS | Generator CPU seconds / RSS MiB / loop p99 ms |\n|---|---:|---:|---:|---:|')
for r in rows:
 processes=r.get('processes', {})
 proc=next(iter(processes.values()), None)
 runtime=f"{proc['cpuCores']:.3f}/{proc['peakRssMiB']:.1f}" if proc else 'n/a'
 def container(name):
  c=r['containers'].get(name)
  return f"{c['meanCpuPercent']/100:.3f}/{c['memorySamples'][-1].split(' / ')[0]}" if c else 'n/a'
 temporal=container('lightspeed-perf-temporal') if 'lightspeed-perf-temporal' in r['containers'] else container('lightspeed-temporal')
 g=r['generator']
 print(f"| {r['name']} | {runtime} | {container('lightspeed-postgres')} | {temporal} | {(g['cpu']['user']+g['cpu']['system'])/1e6:.2f}/{g['rss']/2**20:.1f}/{g['eventLoopP99Ms']:.1f} |")
print('\nSQL times are query execution sums, not full request or transaction latency. Temporal times are SDK mean milliseconds; idle steps have no execution observations. CAS column is the subset of SQL calls/time involving CAS tables, not all CAS I/O.\n')
print('| Step | PG calls / execution ms | CAS SQL calls / ms | Workflow queue ms | Activity execution ms | LLM activity ms |\n|---|---:|---:|---:|---:|---:|')
for r in rows:
 t=r.get('temporalMeanMs',{});llm=r.get('activityMeanMs',{}).get('WorkflowActivities::llm_generate')
 print(f"| {r['name']} | {r['pgCalls']}/{r['pgExecutionMs']:.0f} | {r['casQueryCalls']}/{r['casQueryMs']:.0f} | {f(t.get('temporal_workflow_task_schedule_to_start_latency'))} | {f(t.get('temporal_activity_execution_latency'))} | {f(llm)} |")
print('\nSimulator headroom (10s arrivals, max concurrency 256). All requests completed; zero errors, drops or unfinished work. Where present, large input is 540,672 bytes plus an identifying prefix, with 8KiB output.\n')
print('| Case | Offered/s | Completed/s incl drain | p50/p95/p99 ms | Generator CPU seconds | Handler mean ms | Simulator cores / peak MiB |\n|---|---:|---:|---:|---:|---:|---:|')
for name in ['simulator-100','simulator-500','simulator-1000','simulator-large-100']:
 if not (root/(name+'.json')).exists(): continue
 r=json.load(open(root/(name+'.json')));capacity=json.load(open(root/'simulator-resources.json'))[name];s=r['summary'];n=r['after']['requests']-r['before']['requests'];mean=(r['after']['handler_microseconds_total']-r['before']['handler_microseconds_total'])/n/1000
 print(f"| {name} | {s['offeredPerSecond']} | {s['completedIncludingDrainPerSecond']:.2f} | {q(s['terminalMs'])} | {(r['cpu']['user']+r['cpu']['system'])/1e6:.2f} | {mean:.2f} | {capacity['simulatorCpuCores']:.3f}/{capacity['peakRssMiB']:.1f} |")
print('\nAdditional reads of the prepared mature fixture, one page of at most 512 events per request (no model calls):\n')
print('| Rate | Scheduled/completed | F/U/D | p50/p95/p99 ms | Generator CPU seconds |\n|---:|---:|---:|---:|---:|')
for rate in [10,100]:
 if not (root/f'history-read-{rate}.json').exists(): continue
 r=json.load(open(root/f'history-read-{rate}.json'));s=r['summary'];print(f"| {rate} | {s['scheduled']}/{s['completed']} | {s['failed']}/{s['unfinished']}/{s['dropped']} | {q(s['terminalMs'])} | {(r['cpu']['user']+r['cpu']['system'])/1e6:.2f} |")
