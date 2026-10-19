"""Attribute synthetic run stages from retained Temporal histories, without changing workflows."""
import base64
import collections
import datetime
import json
import pathlib
import statistics
import sys


def timestamp(value):
    return datetime.datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()


def distribution(values):
    if not values:
        return None
    values = sorted(values)
    return {'count': len(values), 'mean': statistics.mean(values),
            **{f'p{p}': values[min(len(values)-1, __import__('math').ceil(len(values)*p/100)-1)]
               for p in [50, 95, 99]}}


def analyze(events, start, end):
    completed = {str(e['activityTaskCompletedEventAttributes']['scheduledEventId']): e
                 for e in events if 'activityTaskCompletedEventAttributes' in e}
    started = {str(e['activityTaskStartedEventAttributes']['scheduledEventId']): e
               for e in events if 'activityTaskStartedEventAttributes' in e}
    by_id = {str(e['eventId']): e for e in events}
    workflow_tasks = []
    for event in events:
        attr = event.get('workflowTaskCompletedEventAttributes')
        if attr and str(attr['scheduledEventId']) in by_id and str(attr['startedEventId']) in by_id:
            workflow_tasks.append(tuple(timestamp(e['eventTime']) for e in
                [by_id[str(attr['scheduledEventId'])], by_id[str(attr['startedEventId'])], event]))
    runs, current = [], []
    incomplete = False
    signals = [e for e in events if 'workflowExecutionSignaledEventAttributes' in e]
    for event in events:
        attr = event.get('activityTaskScheduledEventAttributes')
        if not attr:
            continue
        name = attr['activityType']['name'].split('::')[-1]
        if name == 'prepare_session_toolset':
            current = []
            incomplete = False
        key = str(event['eventId'])
        if key not in completed or key not in started:
            incomplete = True
            continue
        payloads = attr.get('input', {}).get('payloads', [])
        request = json.loads(base64.b64decode(payloads[0]['data'])) if payloads else {}
        kinds = [x['event']['kind'] for x in request.get('events', [])]
        stage = '+'.join(kinds) if kinds else name
        a, b, c = map(timestamp, [event['eventTime'], started[key]['eventTime'], completed[key]['eventTime']])
        current.append({'activity': name, 'stage': stage, 'scheduled': a, 'started': b,
                        'completed': c, 'queueMs': (b-a)*1000, 'executionMs': (c-b)*1000,
                        'inputBytes': sum(len(base64.b64decode(p['data'])) for p in payloads),
                        'attempt': started[key]['activityTaskStartedEventAttributes'].get('attempt', 1)})
        if 'lightspeed.core.run.completed' not in kinds:
            continue
        if not incomplete and current and current[0]['activity'] == 'prepare_session_toolset' and start <= current[0]['scheduled'] and c <= end:
            gaps = [(right['scheduled']-left['completed'])*1000 for left, right in zip(current, current[1:])]
            between = [(a, b, c) for left, right in zip(current, current[1:])
                       for a, b, c in workflow_tasks if a >= left['completed'] and c <= right['scheduled']]
            wf_queue = sum((b-a)*1000 for a,b,c in between)
            wf_execution = sum((c-b)*1000 for a,b,c in between)
            prior = [s for s in signals if timestamp(s['eventTime']) <= current[0]['scheduled']]
            signal = timestamp(prior[-1]['eventTime']) if prior else None
            runs.append({'activities': current, 'cycleMs': (c-current[0]['scheduled'])*1000,
                         'activityQueueMs': sum(x['queueMs'] for x in current),
                         'activityExecutionMs': sum(x['executionMs'] for x in current),
                         'activityInputBytes': sum(x['inputBytes'] for x in current),
                         'betweenActivitiesMs': sum(gaps), 'workflowTasksBetween': len(between),
                         'workflowQueueBetweenMs': wf_queue, 'workflowExecutionBetweenMs': wf_execution,
                         'unattributedBetweenMs': sum(gaps)-wf_queue-wf_execution, 'overlappingGaps': sum(g < 0 for g in gaps),
                         'signalToPreparationMs': (current[0]['scheduled']-signal)*1000 if signal is not None else None})
        current = []
    return runs


def main():
    step = pathlib.Path(sys.argv[1])
    result = json.loads((step/'results.json').read_text())
    start, end = timestamp(result['wallStart']), timestamp(result['wallEnd'])
    histories = sorted((step/'histories').glob('*.json'))
    runs = [dict(history=path.name, **run) for path in histories
            for run in analyze(json.loads(path.read_text())['events'], start, end)]
    stages = collections.defaultdict(list)
    for run in runs:
        for stage in run['activities']:
            stages[stage['stage']].append(stage)
    output = {'histories': len(histories), 'completeMeasuredRuns': len(runs),
              'note': 'Only complete preparation-to-terminal cycles inside the measured window; between-activity time includes workflow dispatch, execution and persistence/RPC, not CPU alone.',
              'summary': {key: distribution([r[key] for r in runs if r[key] is not None]) for key in
                          ['cycleMs', 'activityQueueMs', 'activityExecutionMs', 'betweenActivitiesMs', 'workflowQueueBetweenMs', 'workflowExecutionBetweenMs',
                           'unattributedBetweenMs', 'signalToPreparationMs', 'activityInputBytes']},
              'activityCounts': dict(collections.Counter(x['activity'] for r in runs for x in r['activities'])),
              'stages': {name: {'count': len(xs), 'queueMs': distribution([x['queueMs'] for x in xs]),
                                'executionMs': distribution([x['executionMs'] for x in xs]),
                                'inputBytes': distribution([x['inputBytes'] for x in xs])} for name, xs in stages.items()},
              'runs': runs}
    (step/'workflow-analysis.json').write_text(json.dumps(output, indent=2))
    print(json.dumps({k:v for k,v in output.items() if k not in ['runs','stages']}, indent=2))


if __name__ == '__main__':
    main()
