import json
from pathlib import Path
import tempfile
import unittest
from run import grade, decode, validate, trial, report, ci, command
from hillclimb import decide

class Tests(unittest.TestCase):
    def test_cases(self):
        validate([dict(case_id='test',prompt='write x',tags=['smoke'],hard_reason='negative control',split='train',checks=[dict(kind='file_absent',path='x')])])
    def test_grader(self):
        with tempfile.TemporaryDirectory() as t:
            root=Path(t);(root/'a').write_text('yes\n')
            c={'checks':[{'kind':'file_equals','path':'a','expected':'yes\n'},{'kind':'file_absent','path':'no'}]}
            self.assertTrue(all(v['pass'] for v in grade(c,root,'')))
            (root/'a').write_text('yes');self.assertFalse(grade(c,root,'')[0]['pass'])
            (root/'a').unlink();(root/'a').symlink_to('/etc/passwd');self.assertFalse(grade(c,root,'')[0]['pass'])
    def test_streams(self):
        self.assertFalse(decode('junk')['completed'])
        self.assertTrue(decode('{"type":"turn.completed","usage":{"input_tokens":1}}')['completed'])
        self.assertEqual(decode('{"type":"assistant.message","payload":{"text":"OK"}}\n{"type":"run.completed","payload":{"status":"completed","usage":{"input_tokens":2}}}')['answer'],'OK')
        self.assertEqual(decode('{"type":"run.failed"}')['error'],'provider_error')
    def test_mock_and_report(self):
        with tempfile.TemporaryDirectory() as t:
            out=Path(t);(out/'transcripts').mkdir()
            case={'case_id':'x','split':'train','checks':[{'kind':'file_equals','path':'a','expected':'ok'}]}
            cfg={'agent':'stock_codex','model':'m','effort':'low'}
            rows=[trial(case,cfg,i,out,True,1) for i in range(2)]
            self.assertEqual(rows[0]['score'],1);self.assertNotEqual(rows[0]['transcript_path'],rows[1]['transcript_path'])
            self.assertTrue(report(rows,out)['warnings'])
            with self.assertRaises(ValueError):decide(rows,rows)
    def test_noise(self):
        self.assertEqual(ci([0]*10),[0,0])
        a=[];b=[]
        for i in range(12):
            r=dict(case_id=str(i),repeat=0,split='train' if i<6 else 'test',score=0,error=None,mock=False,grader_verdicts=[[],[]]);a.append(r);b.append(dict(r,score=1))
        self.assertTrue(decide(a,b)['keep']);self.assertFalse(decide(a,a)['keep'])
        with self.assertRaises(ValueError):decide(a,b[:-1])
    def test_effort(self):
        self.assertIn('model_reasoning_effort="high"',command({'agent':'stock_codex','model':'m','effort':'high'},Path('/tmp'),'p'))
    def test_error_zero(self):
        with tempfile.TemporaryDirectory() as t:
            out=Path(t);(out/'transcripts').mkdir()
            c={'case_id':'x','split':'train','prompt':'p','checks':[{'kind':'file_absent','path':'no'}]}
            r=trial(c,{'agent':'custom','model':'m','effort':'low','argv':['/usr/bin/false']},0,out,False,1)
            self.assertEqual(r['score'],0);self.assertEqual(r['error'],'exit_1')
if __name__=='__main__':unittest.main()

class RegressionTests(unittest.TestCase):
    def test_native_option_order(self):
        cmd=command({'agent':'nanocodex','model':'m','effort':'low'},Path('/tmp'),'p')
        self.assertLess(cmd.index('run'),cmd.index('--cwd'))
    def test_tree_addition(self):
        with tempfile.TemporaryDirectory() as t:
            root=Path(t);(root/'a').write_text('x')
            case={'checks':[dict(kind='tree_equals',expected={'a':'x'})]}
            self.assertTrue(grade(case,root,'')[0]['pass'])
            (root/'surprise').write_text('oops')
            self.assertFalse(grade(case,root,'')[0]['pass'])
    def test_timeout(self):
        import sys
        with tempfile.TemporaryDirectory() as t:
            out=Path(t);(out/'transcripts').mkdir()
            case=dict(case_id='timeout',prompt='p',split='train',checks=[dict(kind='file_absent',path='x')])
            cfg=dict(agent='custom',model='m',effort='low',argv=[sys.executable,'-c','import time;time.sleep(10)'])
            r=trial(case,cfg,0,out,False,.1)
            self.assertEqual(r['error'],'timeout');self.assertEqual(r['score'],0)
