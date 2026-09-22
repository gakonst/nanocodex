import json,os,tempfile,unittest
from pathlib import Path
from publish import envelope,publish,encode
from test_project import fixture

class Publisher(unittest.TestCase):
    def test_envelope_and_private_atomic_file(self):
        value=envelope(fixture(),'Example app','Example window',1234001)
        self.assertEqual(value['capturedAt'],1234000)
        self.assertEqual(value['schemaVersion'],1)
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'snapshot.json';publish(value,path)
            self.assertEqual(json.loads(path.read_bytes()),value)
            self.assertEqual(os.stat(path).st_mode&0o777,0o600)
            self.assertEqual(len(list(Path(d).iterdir())),1)
    def test_rejects_stale_future_and_bad_identity(self):
        for now in (1300000,1230000):
            with self.assertRaises(ValueError):envelope(fixture(),'App','Window',now)
        with self.assertRaises(ValueError):envelope(fixture(),'','Window',1234001)
    def test_truncation_keeps_budget_and_timestamp(self):
        f=fixture();f['frames']=[f['frames'][0]]*250
        value=envelope(f,'App','Window',1234001)
        self.assertLessEqual(len(encode(value['data'])),8000)
        self.assertGreater(value['data']['omitted_elements'],0)
        self.assertTrue(value['data']['partial'])
        self.assertEqual(value['capturedAt'],1234000)

if __name__=='__main__':unittest.main()
