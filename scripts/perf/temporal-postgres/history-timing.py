"""Compute server history timing for a single retained workflow and load window."""
import datetime
import json
import statistics
import sys

def ts(s):
    return datetime.datetime.fromisoformat(s.replace('Z', '+00:00')).timestamp()
result = json.load(open(sys.argv[2]))
start, end = ts(result['wallStart']), ts(result['wallEnd'])
events = json.load(open(sys.argv[1]))['events']
by_id = {e['eventId']: e for e in events}
values = {}
for e in events:
    if not start <= ts(e['eventTime']) <= end:
        continue
    for kind in ['workflowTask', 'activityTask']:
        for suffix, ref, label in [('Started', 'scheduledEventId', 'queueMs'), ('Completed', 'startedEventId', 'executionMs')]:
            attr = e.get(kind + suffix + 'EventAttributes')
            if attr and attr.get(ref) in by_id:
                elapsed = (ts(e['eventTime']) - ts(by_id[attr[ref]]['eventTime'])) * 1000
                values.setdefault(kind + '.' + label, []).append(elapsed)
summary = {}
for key, v in values.items():
    v.sort()
    summary[key] = {'count': len(v), 'mean': statistics.mean(v), 'p50': v[(len(v)-1)//2], 'p95': v[min(len(v)-1, __import__('math').ceil(len(v)*.95)-1)], 'max': max(v)}
print(json.dumps(summary, indent=2))
