"""Correlate native Temporal spans by task token, workflow/run and event IDs.

Intervals are non-overlapping handoff stages. Persistence is a nested union,
not an extra duration to add. Unmatched/ambiguous tasks remain explicit.
"""
import argparse
import bisect
import collections
import datetime
import json
import math
import pathlib
import re


def ns(value):
    head, _, frac = value.rstrip('Z').partition('.')
    seconds = int(datetime.datetime.fromisoformat(head).replace(tzinfo=datetime.timezone.utc).timestamp())
    return seconds * 10**9 + int((frac + '000000000')[:9])


def union_ns(intervals):
    total = 0
    end = None
    for a, b in sorted(intervals):
        if b < a:
            raise ValueError('negative interval')
        total += b - max(a, end if end is not None else a) if end is None or b > end else 0
        end = max(end if end is not None else b, b)
    return total


def stats(values):
    values = sorted(values)
    if not values:
        return {'n': 0}
    return {'n': len(values), 'mean': sum(values) / len(values),
            **{f'p{p}': values[max(0, math.ceil(len(values)*p/100)-1)] for p in (50,95,99)},
            'max': values[-1]}


def payload(s, direction):
    return s['attributes'].get('rpc.' + direction + '.payload', {})


def method(s):
    return s['name'].rsplit('/', 1)[-1]


def task_key(s):
    a = s['attributes'];p = payload(s, 'request')
    return (a.get('temporalWorkflowID'), a.get('temporalRunID'), str(p.get('scheduledEventId', '')))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('arm', type=pathlib.Path)
    args = parser.parse_args();arm=args.arm
    spans=[json.loads(l) for l in arm.with_suffix('.spans.jsonl').open()]
    index={s['span']:s for s in spans}
    if len(index)!=len(spans):raise ValueError('duplicate span IDs')
    traces=collections.defaultdict(list);children=collections.defaultdict(list)
    add=collections.defaultdict(list);completed=collections.defaultdict(list);polls=[]
    for s in spans:
        traces[s['trace']].append(s);children[s['parent']].append(s)
        if s['kind']!=2:continue
        m=method(s)
        if m in ('AddWorkflowTask','AddActivityTask'):add[task_key(s)].append(s)
        if s['service'].endswith('.frontend'):
            if m in ('PollWorkflowTaskQueue','PollActivityTaskQueue'):polls.append(s)
            if m in ('RespondWorkflowTaskCompleted','RespondActivityTaskCompleted','RespondQueryTaskCompleted'):
                token=payload(s,'request').get('taskTokenHash')
                if token:completed[token].append(s)
    def persistence_intervals(s):
        found=[];todo=list(children[s['span']])
        while todo:
            t=todo.pop();todo.extend(children[t['span']])
            if t['name'].startswith('persistence.'):
                found.append((max(s['startNs'],t['startNs']),min(s['endNs'],t['endNs'])))
        return [(a,b) for a,b in found if b>=a]
    def persistence(s):
        return union_ns(persistence_intervals(s))/1e6
    # Completed poll spans let us reconstruct outstanding RPCs backwards in time,
    # including empty/cancelled polls at shutdown. This is server-visible availability,
    # not an assumption based on unused execution slots.
    occupancy=collections.defaultdict(lambda: [[],[]])
    for s in polls:
        req=payload(s,'request');queue=req.get('taskQueue',{}).get('name')
        occupancy[(method(s),queue)][0].append(s['startNs'])
        occupancy[(method(s),queue)][1].append(s['endNs'])
    for starts,ends in occupancy.values():starts.sort();ends.sort()
    availability=collections.defaultdict(lambda: [[],[]])
    for poll in polls:
        q=payload(poll,'request').get('taskQueue',{}).get('name')
        rec=[s for s in traces[poll['trace']] if s['kind']==2 and method(s) in ('RecordWorkflowTaskStarted','RecordActivityTaskStarted')]
        if len(rec)==1:
            pieces=[('awaitingAssignment',poll['startNs'],rec[0]['startNs']),('assigned',rec[0]['startNs'],poll['endNs'])]
        else:pieces=[('unknown',poll['startNs'],poll['endNs'])]
        for label,a,b in pieces:
            availability[(method(poll),q,label)][0].append(a)
            availability[(method(poll),q,label)][1].append(b)
    for starts,ends in availability.values():starts.sort();ends.sort()
    def available(m,q,label,t):
        starts,ends=availability[(m,q,label)]
        return bisect.bisect_right(starts,t)-bisect.bisect_right(ends,t)
    def outstanding(m,q,t):
        starts,ends=occupancy[(m,q)]
        return bisect.bisect_right(starts,t)-bisect.bisect_right(ends,t)
    activations=collections.defaultdict(list)
    for line in (arm/'runtime.log').open():
        try:x=json.loads(line)
        except ValueError:continue
        fields=x.get('fields',{})
        if fields.get('message')=='Sending activation to lang':
            match=re.search(r'run_id: ([^,]+),',fields.get('activation',''))
            if match:activations[match[1]].append(ns(x['timestamp']))
    for values in activations.values():values.sort()
    summaries=[];all_rows=[]
    for file in sorted((arm/'sweep').glob('*/results.json')):
        result=json.load(file.open());lo,hi=ns(result['wallStart']),ns(result['wallEnd'])
        fixture=json.load((file.parent/'fixture.json').open());sessions={x['id'] for x in fixture['sessions']}
        rows=[];missing=collections.Counter()
        for poll in polls:
            if not lo<=poll['endNs']<=hi:continue
            response=payload(poll,'response');token=response.get('taskTokenHash')
            if not token:continue
            wid=poll['attributes'].get('temporalWorkflowID','')
            if wid.rsplit('/',1)[-1] not in sessions:continue
            kind='workflow' if method(poll)=='PollWorkflowTaskQueue' else 'activity'
            records=[s for s in traces[poll['trace']] if s['kind']==2
                     and method(s)==('RecordWorkflowTaskStarted' if kind=='workflow' else 'RecordActivityTaskStarted')
                     and s['attributes'].get('temporalWorkflowID')==wid
                     and s['attributes'].get('rpc.grpc.status_code')==0]
            completes=[s for s in completed[token] if s['attributes'].get('rpc.grpc.status_code')==0]
            if not records and len(completes)==1 and method(completes[0])=='RespondQueryTaskCompleted':
                missing['direct_query_polls_excluded']+=1;continue
            if len(records)!=1 or len(completes)!=1:
                missing[f'records_{len(records)}_completions_{len(completes)}']+=1;continue
            record=records[0];complete=completes[0];key=task_key(record)
            scheduled=payload(record,'response').get('scheduledTime',response.get('scheduledTime'))
            if not scheduled:missing['no_scheduled_timestamp']+=1;continue
            scheduled=ns(scheduled)
            candidates=[s for s in add[key] if scheduled<=s['startNs']<=record['startNs']]
            if not candidates:missing['no_add_rpc']+=1;continue
            first=min(candidates,key=lambda s:s['startNs'])
            points=[scheduled,first['startNs'],record['startNs'],record['endNs'],poll['endNs'],complete['startNs'],complete['endNs']]
            if any(b<a for a,b in zip(points,points[1:])):
                missing['non_monotonic_boundary']+=1;continue
            queue=payload(poll,'request').get('taskQueue',{}).get('name')
            stages=dict(zip(['scheduleToMatchingMs','matchingToRecordStartMs','recordStartRpcMs',
                             'recordEndToPollReturnMs','pollReturnToCompletionRequestMs','completionRpcMs'],
                            [(b-a)/1e6 for a,b in zip(points,points[1:])]))
            activation=[t for t in activations[key[1]] if poll['endNs']<=t<=complete['startNs']]
            if kind=='workflow' and len(activation)==1:
                stages['pollReturnToActivationMs']=(activation[0]-poll['endNs'])/1e6
                stages['activationToCompletionRequestMs']=(complete['startNs']-activation[0])/1e6
            stages['addToChosenPollArrivalMs']=max(0,poll['startNs']-first['startNs'])/1e6
            stages['chosenPollOrAddToRecordMs']=(record['startNs']-max(poll['startNs'],first['startNs']))/1e6
            starts,ends=occupancy[(method(poll),queue)]
            next_start=bisect.bisect_left(starts,poll['endNs'])
            if next_start<len(starts):
                stages['nextPollArrivalAfterReturnMs']=(starts[next_start]-poll['endNs'])/1e6
            for label,span in [('record',record),('completion',complete)]:
                intervals=persistence_intervals(span)
                if intervals:
                    stages[label+'BeforeFirstPersistenceMs']=(min(a for a,b in intervals)-span['startNs'])/1e6
                    stages[label+'AfterLastPersistenceMs']=(span['endNs']-max(b for a,b in intervals))/1e6
            record_p=persistence(record);complete_p=persistence(complete)
            stages.update(recordPersistenceMs=record_p,completionPersistenceMs=complete_p)
            row={'scenario':file.parent.name,'kind':kind,'workflowId':key[0],'runId':key[1],
                 'scheduledEventId':key[2],'tokenHash':token,'pollSpan':poll['span'],
                 'addSpan':first['span'],'recordSpan':record['span'],'completeSpan':complete['span'],
                 'startNs':scheduled,'endNs':complete['endNs'],
                 'pollStartedBeforeMatching':poll['startNs']<=first['startNs'],
                 'sameQueuePollsAtAdd':outstanding(method(poll),queue,first['startNs']),
                 'pollLeadAtAddMs':(first['startNs']-poll['startNs'])/1e6,
                 'pollsByStateAtAdd':{label:available(method(poll),queue,label,first['startNs']) for label in ('awaitingAssignment','assigned','unknown')},
                 'matchingQueue':payload(first,'request').get('taskQueue'), 'workerQueue':queue,
                 'stages':stages}
            rows.append(row)
        summary={'scenario':file.parent.name,'unmatched':dict(missing),'kinds':{}}
        for kind in ('workflow','activity'):
            selected=[r for r in rows if r['kind']==kind]
            keys={k for row in selected for k in row['stages']}
            summary['kinds'][kind]={'tasks':len(selected),'stages':{k:stats([r['stages'][k] for r in selected if k in r['stages']]) for k in sorted(keys)},
                'pollAlreadyOpenFraction':sum(r['pollStartedBeforeMatching'] for r in selected)/len(selected) if selected else None,
                'pollsAtAdd':stats([r['sameQueuePollsAtAdd'] for r in selected]),
                'pollLeadAtAddMs':stats([r['pollLeadAtAddMs'] for r in selected]),
                'pollsByStateAtAdd':{label:stats([r['pollsByStateAtAdd'][label] for r in selected]) for label in ('awaitingAssignment','assigned','unknown')}}
        summaries.append(summary);all_rows.extend(rows)
    (arm/'handoff-rows.json').write_text(json.dumps(all_rows,indent=2))
    (arm/'handoff-summary.json').write_text(json.dumps({'spans':len(spans),'parseErrors':sum(any(k.endswith('.parseError') for k in s['attributes']) for s in spans),'steps':summaries},indent=2))
    print(json.dumps(summaries,indent=2))


if __name__=='__main__':main()
