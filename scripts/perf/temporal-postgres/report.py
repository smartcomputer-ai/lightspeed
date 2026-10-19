"""Additional tables for a PostgreSQL Temporal report; optional SQLite comparison."""
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
rows = {r['name']: r for r in json.load(open(root / 'analysis.json'))}
server = json.load(open(root / 'temporal-analysis.json'))
def cell(value):
    return 'n/a' if value is None else f'{value:.1f}'
print('| Step | Temporal PG cores / final RSS | History calls / SQL ms | Visibility calls / SQL ms | Activity queue ms | UpdateWorkflow persistence ms |')
print('|---|---:|---:|---:|---:|---:|')
for evidence in server:
    row = rows[evidence['name']]
    db = row['containers'].get('lightspeed-perf-temporal-postgres')
    resources = f"{db['meanCpuPercent']/100:.3f}/{db['memorySamples'][-1].split(' / ')[0]}" if db else 'n/a'
    history, visibility = evidence['databases']['temporal'], evidence['databases']['temporal_visibility']
    queue = row.get('temporalMeanMs', {}).get('temporal_activity_schedule_to_start_latency')
    update = evidence['serverMeansMs'].get('persistence_latency_sum{namespace="default",operation="UpdateWorkflowExecution",service_name="history"}', {}).get('meanMs')
    print(f"| {row['name']} | {resources} | {history['calls']}/{history['executionMs']:.0f} | {visibility['calls']}/{visibility['executionMs']:.0f} | {cell(queue)} | {cell(update)} |")
if len(sys.argv) > 2:
    baseline = {r['name']: r for r in json.load(open(pathlib.Path(sys.argv[2]) / 'analysis.json'))}
    print('\nTopology comparison: completion rates include drain; terminal latencies describe successful requests. Version/configuration differences prevent persistence-only attribution.\n')
    print('| Step | SQLite completed/s | PG completed/s | SQLite terminal p50 / p99 ms | PG terminal p50 / p99 ms |')
    print('|---|---:|---:|---:|---:|')
    for name in ['rapid-2', 'rapid-10', 'rapid-40', 'active-readers-40', 'history-40', 'idle-readers-512', 'idle-readers-2048']:
        old, new = baseline[name], rows[name]
        print(f"| {name} | {old['completedIncludingDrainPerSecond']:.2f} | {new['completedIncludingDrainPerSecond']:.2f} | {old['terminalMs']['p50']:.0f}/{old['terminalMs']['p99']:.0f} | {new['terminalMs']['p50']:.0f}/{new['terminalMs']['p99']:.0f} |")
