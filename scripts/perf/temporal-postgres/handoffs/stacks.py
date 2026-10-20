"""Count sampled handoff stacks; these are observations, not CPU percentages."""
import collections
import json
import pathlib
import re
import sys

root=pathlib.Path(sys.argv[1]);records=[];examples={}
for file in sorted(root.glob('*.txt')):
    if '.start.' in file.name or '.end.' in file.name:
        continue
    counts=collections.Counter()
    for block in file.read_text().split('\n\n'):
        if not any(v in block for v in ('.RecordWorkflowTaskStarted(','.RecordActivityTaskStarted(',
                                        '.RespondWorkflowTaskCompleted(','.RespondActivityTaskCompleted(')):
            continue
        if '.ioSemaphoreAcquire(' in block and 'PrioritySemaphoreImpl).Acquire' in block:
            category='history_shard_io_semaphore'
        elif 'PrioritySemaphoreImpl).Acquire' in block:
            category='other_priority_semaphore'
        elif 'ClientStream).waitOnHeader' in block:
            category='grpc_client_awaiting_response'
        elif any(v in block for v in ('pgconn.','pgx.','pgxpool.')) and any(v in block for v in ('runtime_pollWait','syscall.read','syscall.write')):
            category='postgres_network_or_syscall'
        else:
            category='other'
        counts[category]+=1;examples.setdefault(category,block)
    records.append({'file':file.name,'start':file.with_suffix('.start.txt').read_text().strip(),
                    'end':file.with_suffix('.end.txt').read_text().strip(),'counts':dict(counts)})
print(json.dumps({'samples':records,'total':dict(sum((collections.Counter(r['counts']) for r in records),collections.Counter())),
                  'representativeStacks':examples},indent=2))
