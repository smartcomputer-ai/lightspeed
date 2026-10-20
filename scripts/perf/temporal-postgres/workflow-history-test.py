"""Timing attribution tests: boundaries, incomplete histories and overlap visibility."""
import base64
import importlib.util
import json
import pathlib
import unittest

spec = importlib.util.spec_from_file_location('history', pathlib.Path(__file__).with_name('workflow-history.py'))
history = importlib.util.module_from_spec(spec)
spec.loader.exec_module(history)


def activity(index, name, a, b, c, terminal=False):
    request = {'events': [{'event': {'kind': 'lightspeed.core.run.completed'}}]} if terminal else {}
    payload = base64.b64encode(json.dumps(request).encode()).decode()
    return [
        {'eventId': str(index), 'eventTime': f'2026-01-01T00:00:{a:06.3f}Z',
         'activityTaskScheduledEventAttributes': {'activityType': {'name': name}, 'input': {'payloads': [{'data': payload}]}}},
        {'eventId': str(index+1), 'eventTime': f'2026-01-01T00:00:{b:06.3f}Z',
         'activityTaskStartedEventAttributes': {'scheduledEventId': str(index), 'attempt': 1}},
        {'eventId': str(index+2), 'eventTime': f'2026-01-01T00:00:{c:06.3f}Z',
         'activityTaskCompletedEventAttributes': {'scheduledEventId': str(index)}}]


class TimingTests(unittest.TestCase):
    def setUp(self):
        self.events = activity(1, 'prepare_session_toolset', 1, 2, 3) + activity(4, 'append_events', 4, 5, 6, True)
        self.origin = history.timestamp('2026-01-01T00:00:00Z')

    def test_queue_execution_and_transition_partition_cycle(self):
        run, = history.analyze(self.events, self.origin, self.origin+10)
        self.assertEqual(run['activityQueueMs'], 2000)
        self.assertEqual(run['activityExecutionMs'], 2000)
        self.assertEqual(run['betweenActivitiesMs'], 1000)
        self.assertEqual(run['cycleMs'], 5000)
        self.assertIsNone(run['signalToPreparationMs'])

    def test_workflow_task_references_partition_transition_gap(self):
        events = self.events + [
            {'eventId': '8', 'eventTime': '2026-01-01T00:00:03Z'},
            {'eventId': '9', 'eventTime': '2026-01-01T00:00:03.250Z'},
            {'eventId': '10', 'eventTime': '2026-01-01T00:00:04Z',
             'workflowTaskCompletedEventAttributes': {'scheduledEventId': '8', 'startedEventId': '9'}}]
        run, = history.analyze(events, self.origin, self.origin+10)
        self.assertEqual(run['workflowTasksBetween'], 1)
        self.assertEqual(run['workflowQueueBetweenMs'], 250)
        self.assertEqual(run['workflowExecutionBetweenMs'], 750)
        self.assertEqual(run['unattributedBetweenMs'], 0)

    def test_excludes_warmup_and_partial_boundary_cycles(self):
        self.assertEqual(history.analyze(self.events, self.origin+2, self.origin+10), [])
        self.assertEqual(history.analyze(self.events, self.origin, self.origin+5), [])

    def test_incomplete_history_is_not_reported_as_zero_cost(self):
        self.assertEqual(history.analyze(self.events[0:2]+self.events[3:], self.origin, self.origin+10), [])

    def test_overlap_is_exposed_instead_of_claiming_serial_attribution(self):
        events = activity(1, 'prepare_session_toolset', 1, 2, 4) + activity(4, 'append_events', 3, 5, 6, True)
        run, = history.analyze(events, self.origin, self.origin+10)
        self.assertEqual(run['overlappingGaps'], 1)


if __name__ == '__main__':
    unittest.main()
