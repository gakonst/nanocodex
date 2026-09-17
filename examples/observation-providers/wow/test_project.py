import unittest
from project import project


def fixture():
    return {'schema':'nanocodex.observe.v1','sequence':1,
            'provenance':{'finished':{'server_seconds':1234}},
            'frames':[{'name':'Quest','role':'Button','labels':['Ready for turn-in'],
                       'rect':{'left':4,'bottom':5,'width':6,'height':7,'effective_scale':0.8}},
                      {'name':'Input','role':'EditBox','labels':['must not export'],'rect':{}}],
            'recent_speech':[{'source':'bsspeak','text':'The North Sea'}],
            'scan':{'truncated':True}}


class Projection(unittest.TestCase):
    def test_labels_coordinates_and_redaction(self):
        timestamp, context = project(fixture())
        self.assertEqual(timestamp,1234000)
        self.assertEqual(context['elements'][0]['labels'],['Ready for turn-in'])
        self.assertEqual(context['elements'][0]['rect']['effective_scale'],0.8)
        self.assertEqual(context['elements'][1]['labels'],[])
        self.assertTrue(context['partial'])
        self.assertEqual(context['recent_speech'][0]['text'],'The North Sea')

    def test_rejects_unrelated_clipboard_and_nonfinite_timestamps(self):
        for value in ({'schema':'other'}, {}, None):
            with self.assertRaises(ValueError): project(value)
        for timestamp in (True, float('nan'), float('inf'), '1234'):
            value=fixture();value['provenance']['finished']['server_seconds']=timestamp
            with self.assertRaises(ValueError): project(value)

    def test_partial_discloses_clipping_and_history(self):
        value=fixture();value['scan']['truncated']=False
        value['frames'][0]['labels']=['界'*512]*9
        self.assertTrue(project(value)[1]['partial'])
        value=fixture();value['scan']['truncated']=False
        value['speech_history_may_be_truncated']=True
        self.assertTrue(project(value)[1]['partial'])

    def test_utf8_text_budget(self):
        value=fixture();value['frames'][0]['labels']=['界'*512]
        _,context=project(value)
        self.assertLessEqual(len(context['elements'][0]['labels'][0].encode()),512)

    def test_limits_and_filters_unknown_fields(self):
        value=fixture();value['frames'][0]['labels']=['x'*1000]*12
        value['frames'][0]['command']='do not forward'
        _,context=project(value)
        self.assertEqual(len(context['elements'][0]['labels']),8)
        self.assertEqual(len(context['elements'][0]['labels'][0]),512)
        self.assertNotIn('command',context['elements'][0])
        value['frames']*=126
        with self.assertRaises(ValueError): project(value)


if __name__=='__main__': unittest.main()
