"""Local OTLP receiver retaining timings/IDs, never model or persistence bodies.

Requires grpcio and opentelemetry-proto. Bind only a local Docker bridge address.
Server debug tracing is synthetic-test-only; even discarded payloads cost CPU.
"""
import argparse
import concurrent.futures
import hashlib
import ipaddress
import json
import pathlib
import resource
import re
import signal
import threading
import time

import grpc
from opentelemetry.proto.collector.trace.v1 import trace_service_pb2 as pb
from opentelemetry.proto.collector.trace.v1 import trace_service_pb2_grpc as rpc

FIELDS = {
    'workflowid', 'runid', 'scheduledid', 'scheduledeventid', 'startedeventid',
    'activityid', 'pollerid', 'requestid', 'identity', 'name', 'kind', 'taskid',
    'attempt', 'normalname', 'forwardedsource', 'taskqueue', 'taskqueuetype',
    'scheduletostarttimeout', 'scheduledtime', 'startedtime', 'stickyexecutionenabled',
}
CONTAINERS = {
    'execution', 'workflowexecution', 'pollrequest', 'taskqueue', 'request',
    'response', 'workflowtask', 'activitytask', 'taskqueuemetadata', 'stickyattributes',
}


def routing(value):
    if not isinstance(value, dict):
        return {}
    result = {}
    for key, val in value.items():
        low = key.lower().replace('_', '')
        if low == 'tasktoken' and isinstance(val, str):
            result['taskTokenHash'] = hashlib.sha256(val.encode()).hexdigest()
        elif low in CONTAINERS and isinstance(val, dict):
            result[key] = routing(val)
        elif low in FIELDS and isinstance(val, (str, bool, int, float)):
            result[key] = val
    return result


def attr_value(value):
    field = value.WhichOneof('value')
    if field in ('string_value', 'bool_value', 'int_value', 'double_value'):
        return getattr(value, field)
    return None


def normalize(span, service):
    attrs = {}
    for a in span.attributes:
        val = attr_value(a.value)
        if a.key in ('rpc.request.payload', 'rpc.response.payload'):
            try:
                attrs[a.key] = routing(json.loads(val))
            except (ValueError, TypeError):
                attrs[a.key + '.parseError'] = True
        elif a.key == 'persistence.request.payload' and isinstance(val, str):
            match = re.search(r'"ShardID"\s*:\s*(\d+)', val)
            if match:
                attrs['persistence.shardId'] = int(match[1])
        elif a.key.startswith(('rpc.request.headers.', 'rpc.response.headers.', 'persistence.request.', 'persistence.response.')):
            continue
        elif val is not None:
            attrs[a.key] = val
    return {'trace': span.trace_id.hex(), 'span': span.span_id.hex(),
            'parent': span.parent_span_id.hex(), 'service': service, 'name': span.name,
            'kind': span.kind, 'startNs': span.start_time_unix_nano,
            'endNs': span.end_time_unix_nano, 'status': span.status.code,
            'attributes': attrs}


class Collector(rpc.TraceServiceServicer):
    def __init__(self, output):
        self.output = output
        self.lock = threading.Lock()
        self.spans = self.batches = self.wire_bytes = 0

    def Export(self, request, context):
        rows = []
        for rs in request.resource_spans:
            service = next((attr_value(a.value) for a in rs.resource.attributes if a.key == 'service.name'), '')
            for ss in rs.scope_spans:
                rows.extend(normalize(s, service) for s in ss.spans)
        data = ''.join(json.dumps(row, separators=(',', ':')) + '\n' for row in rows)
        with self.lock:
            self.output.write(data)
            self.output.flush()
            self.spans += len(rows)
            self.batches += 1
            self.wire_bytes += request.ByteSize()
        return pb.ExportTraceServiceResponse()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('output', type=pathlib.Path)
    p.add_argument('--bind', default='172.17.0.1:14317')
    args = p.parse_args()
    address = ipaddress.ip_address(args.bind.rsplit(':', 1)[0])
    if not address.is_private or address.is_unspecified:
        p.error('bind must be a specific private or loopback address')
    stopped = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_: stopped.set())
    signal.signal(signal.SIGINT, lambda *_: stopped.set())
    started = time.time()
    with args.output.open('x') as f:
        receiver = Collector(f)
        server = grpc.server(concurrent.futures.ThreadPoolExecutor(max_workers=4),
                             options=[('grpc.max_receive_message_length', 128 * 1024 * 1024)])
        rpc.add_TraceServiceServicer_to_server(receiver, server)
        if not server.add_insecure_port(args.bind):
            raise RuntimeError('could not bind collector')
        server.start()
        print('collector ready', flush=True)
        stopped.wait()
        server.stop(5).wait()
    usage = resource.getrusage(resource.RUSAGE_SELF)
    args.output.with_suffix('.stats.json').write_text(json.dumps({
        'spans': receiver.spans, 'batches': receiver.batches,
        'receivedProtobufBytes': receiver.wire_bytes, 'elapsedSeconds': time.time() - started,
        'userCpuSeconds': usage.ru_utime, 'systemCpuSeconds': usage.ru_stime,
        'maxRssKiB': usage.ru_maxrss,
    }, indent=2))


if __name__ == '__main__':
    main()
