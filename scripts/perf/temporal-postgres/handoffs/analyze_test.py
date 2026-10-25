import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('handoff_analyze', pathlib.Path(__file__).with_name('analyze.py'))
a = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(a)


class AnalyzeTest(unittest.TestCase):
    def test_nested_persistence_is_counted_once(self):
        self.assertEqual(a.union_ns([(1, 5), (2, 4), (4, 8), (10, 12)]), 9)
        with self.assertRaises(ValueError):
            a.union_ns([(5, 4)])

    def test_timestamp_preserves_nanoseconds(self):
        self.assertEqual(a.ns('2026-10-20T00:00:00.123456789Z') - a.ns('2026-10-20T00:00:00Z'), 123456789)

    def test_correlated_stages_and_nested_persistence(self):
        with tempfile.TemporaryDirectory() as tmp:
            arm = pathlib.Path(tmp)/'arm';step=arm/'sweep'/'rapid-2';step.mkdir(parents=True)
            (arm/'runtime.log').write_text('')
            (step/'results.json').write_text(json.dumps({'wallStart':'2026-10-20T00:00:00Z','wallEnd':'2026-10-20T00:00:20Z'}))
            (step/'fixture.json').write_text(json.dumps({'sessions':[{'id':'session_test'}]}))
            origin=a.ns('2026-10-20T00:00:00Z')
            attrs={'temporalWorkflowID':'u/session_test','temporalRunID':'r','rpc.grpc.status_code':0}
            spans=[]
            def span(id,trace,parent,service,name,start,end,request=None,response=None,kind=2):
                x={'span':id,'trace':trace,'parent':parent,'service':'io.temporal.'+service,'name':name,'kind':kind,'startNs':origin+int(start*1e9),'endNs':origin+int(end*1e9),'attributes':dict(attrs)}
                x['attributes']['rpc.request.payload']=request or {}
                x['attributes']['rpc.response.payload']=response or {}
                spans.append(x)
            span('add','a','','matching','AddWorkflowTask',2,2.1,{'scheduledEventId':'7','taskQueue':{'name':'q'}})
            span('poll','p','','frontend','PollWorkflowTaskQueue',3,7,{'taskQueue':{'name':'q'}},{'taskTokenHash':'token','scheduledTime':'2026-10-20T00:00:01Z'})
            span('record','p','poll','history','RecordWorkflowTaskStarted',4,6,{'scheduledEventId':'7'},{'scheduledTime':'2026-10-20T00:00:01Z'})
            span('rp','p','record','history','persistence.Update',4.5,5.5,kind=1)
            span('complete','c','','frontend','RespondWorkflowTaskCompleted',8,10,{'taskTokenHash':'token'})
            span('cp','c','complete','history','persistence.Update',8.5,9.5,kind=1)
            span('nested','c','cp','history','persistence.Nested',8.7,9.1,kind=1)
            arm.with_suffix('.spans.jsonl').write_text(''.join(json.dumps(s)+'\n' for s in spans))
            subprocess.run([sys.executable,str(pathlib.Path(__file__).with_name('analyze.py')),str(arm)],check=True,stdout=subprocess.DEVNULL)
            row=json.loads((arm/'handoff-rows.json').read_text())[0]
            self.assertEqual(row['stages']['scheduleToMatchingMs'],1000)
            self.assertEqual(row['stages']['matchingToRecordStartMs'],2000)
            self.assertEqual(row['stages']['recordPersistenceMs'],1000)
            self.assertEqual(row['stages']['completionPersistenceMs'],1000)
            self.assertFalse(row['pollStartedBeforeMatching'])
            self.assertEqual(row['pollsByStateAtAdd']['assigned'],0)


if __name__=='__main__':unittest.main()
