import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import server

class BridgeStatusTests(unittest.TestCase):
    def test_public_status_omits_private_fields_and_detects_stale_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'auto-bridge').mkdir();path=root/'auto-bridge/status.json'
            path.write_text(json.dumps({'state':'connected','credentials':'PRIVATE','prompt':'PRIVATE','stats':{'captures':2,'input_batches':3,'token':'PRIVATE'}}))
            with patch.object(server,'ROOT',root):
                result=server.bridge_status()
                self.assertEqual(result['stats'],{'captures':2,'input_batches':3})
                self.assertNotIn('PRIVATE',json.dumps(result));self.assertTrue(result['fresh'])
                os.utime(path,(1,1));self.assertFalse(server.bridge_status()['fresh'])
    def test_missing_and_invalid_evidence_never_claim_connection(self):
        with tempfile.TemporaryDirectory() as tmp,patch.object(server,'ROOT',Path(tmp)):
            self.assertFalse(server.bridge_status()['available'])
            (Path(tmp)/'auto-bridge').mkdir();(Path(tmp)/'auto-bridge/status.json').write_text('[]')
            self.assertFalse(server.bridge_status()['available'])
