import hashlib
import unittest
from collector import routing, normalize
from opentelemetry.proto.trace.v1.trace_pb2 import Span


class CollectorTest(unittest.TestCase):
    def test_join_identity_is_retained_without_sensitive_bodies(self):
        token = 'synthetic-token'
        out = routing({'taskToken': token, 'workflowExecution': {'workflowId': 'w', 'runId': 'r'},
                       'scheduledEventId': '7', 'input': {'data': 'omit'},
                       'history': {'events': ['omit']}, 'identity': 'worker',
                       'pollRequest': {'taskQueue': {'name': 'queue', 'kind': 'normal'}}})
        self.assertEqual(out['taskTokenHash'], hashlib.sha256(token.encode()).hexdigest())
        self.assertEqual(out['workflowExecution'], {'workflowId': 'w', 'runId': 'r'})
        self.assertEqual(out['scheduledEventId'], '7')
        self.assertNotIn('history', out)
        self.assertNotIn('input', out)
        self.assertEqual(out['pollRequest']['taskQueue']['name'], 'queue')

    def test_only_shard_id_survives_persistence_payload(self):
        span = Span(name='persistence')
        span.attributes.add(key='persistence.request.payload').value.string_value = '{"ShardID": 3, "Data": "omit"}'
        out = normalize(span, 'history')
        self.assertEqual(out['attributes'], {'persistence.shardId': 3})

    def test_payload_parse_failure_is_visible_and_headers_are_dropped(self):
        span = Span(name='rpc', start_time_unix_nano=10, end_time_unix_nano=20)
        span.attributes.add(key='rpc.request.payload').value.string_value = 'broken'
        span.attributes.add(key='rpc.request.headers.authorization').value.string_value = 'omit'
        span.attributes.add(key='persistence.request.payload').value.string_value = 'omit'
        out = normalize(span, 'service')
        self.assertTrue(out['attributes']['rpc.request.payload.parseError'])
        self.assertEqual(len(out['attributes']), 1)
        self.assertEqual(out['endNs'] - out['startNs'], 10)


if __name__ == '__main__':
    unittest.main()
