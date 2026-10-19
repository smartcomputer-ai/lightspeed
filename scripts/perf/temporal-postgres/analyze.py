"""Summarize server-side evidence without treating absent counters as zero."""
import csv
import datetime
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
def timestamp(value):
    return datetime.datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()

steps = []
for path in sorted(root.glob('sweep/*/results.json')):
    result = json.loads(path.read_text())
    steps.append({'name': path.parent.name, 'start': timestamp(result['wallStart']),
                  'end': timestamp(result['wallEnd']), 'path': path.parent, 'observations': []})
prefixes = ('persistence_latency_', 'visibility_persistence_latency_',
            'service_latency_', 'task_latency_', 'task_schedule_to_start_latency_',
            'syncmatch_latency_', 'asyncmatch_latency_', 'poll_success',
            'approximate_backlog_', 'workflow_task_query_latency_', 'persistence_sql_',
            'memory_', 'num_goroutines')
with (root / 'observations.jsonl').open() as source:
    for line in source:
        if not line.endswith('\n'):
            raise ValueError('Stop the observer before analysis; last sample is incomplete')
        observation = json.loads(line)
        relevant = [step for step in steps if step['start'] <= observation['time'] <= step['end']]
        if not relevant:
            continue
        metrics = {}
        for metric in observation.get('temporal_server', '').splitlines():
            if metric.startswith(prefixes) and '_bucket{' not in metric:
                key, value = metric.rsplit(' ', 1)
                metrics[key] = float(value)
        for step in relevant:
            step['observations'].append({'time': observation['time'], 'metrics': metrics})

output = []
for step in steps:
    item = {'name': step['name'], 'databases': {}}
    for db in ['temporal', 'temporal_visibility']:
        def read(phase):
            return {r['queryid']: r for r in csv.DictReader((step['path'] / f'{db}-{phase}.csv').open())}
        before, after = read('before'), read('after')
        queries = []
        for key, row in after.items():
            old = before.get(key, {})
            queries.append({'query': row['query'], 'calls': int(row['calls']) - int(old.get('calls', 0)),
                            'executionMs': float(row['total_exec_time']) - float(old.get('total_exec_time', 0)),
                            'sharedReads': int(row['shared_blks_read']) - int(old.get('shared_blks_read', 0))})
        item['databases'][db] = {'calls': sum(q['calls'] for q in queries),
                                 'executionMs': sum(q['executionMs'] for q in queries),
                                 'sharedReads': sum(q['sharedReads'] for q in queries),
                                 'topQueries': sorted(queries, key=lambda q: q['executionMs'], reverse=True)[:10]}
    observations = step['observations']
    item['serverSamples'] = len(observations)
    item['serverMeansMs'] = {}
    if len(observations) >= 2:
        first, last = observations[0], observations[-1]
        item['coverageSeconds'] = last['time'] - first['time']
        a, b = first['metrics'], last['metrics']
        for key, value in b.items():
            if '_sum{' not in key:
                continue
            count = key.replace('_sum{', '_count{')
            n = b.get(count, 0) - a.get(count, 0)
            if n > 0 and value >= a.get(key, 0):
                item['serverMeansMs'][key] = {'count': n, 'meanMs': 1000 * (value - a.get(key, 0)) / n}
        item['backlogPeakBySeries'] = {key: max(o['metrics'].get(key, 0) for o in observations) for key in b if key.startswith('approximate_backlog_')}
        item['sqlPoolPeakBySeries'] = {key: max(o['metrics'].get(key, 0) for o in observations) for key in b if key.startswith('persistence_sql_')}
    pg_samples = []
    for line in (root / 'pg-observations.jsonl').open():
        observation = json.loads(line)
        if step['start'] <= observation['time'] <= step['end']:
            pg_samples.append(observation)
    item['pgSamples'] = len(pg_samples)
    item['pgObservedWaits'] = {}
    for sample in pg_samples:
        for db, data in sample['databases'].items():
            for wait in data.get('waits') or []:
                key = f"{db}: {wait['state']}/{wait['wait_event_type']}/{wait['wait_event']}"
                item['pgObservedWaits'][key] = item['pgObservedWaits'].get(key, 0) + wait['count']
    if len(pg_samples) >= 2:
        item['pgDatabaseDeltas'] = {}
        for db, data in pg_samples[-1]['databases'].items():
            old = pg_samples[0]['databases'].get(db, {}).get('database')
            new = data.get('database')
            if old and new:
                item['pgDatabaseDeltas'][db] = {key: new[key] - old[key] for key in ['xact_commit', 'xact_rollback', 'blks_read', 'blks_hit', 'temp_bytes', 'deadlocks', 'blk_read_time', 'blk_write_time']}
    output.append(item)
(root / 'temporal-analysis.json').write_text(json.dumps(output, indent=2))
print('| Step | History calls / SQL ms | Visibility calls / SQL ms | Server samples |')
print('|---|---:|---:|---:|')
for item in output:
    h, v = item['databases']['temporal'], item['databases']['temporal_visibility']
    print(f"| {item['name']} | {h['calls']} / {h['executionMs']:.0f} | {v['calls']} / {v['executionMs']:.0f} | {item['serverSamples']} |")
