import unittest,json
from types import SimpleNamespace
from wayland import Desktop
class NativeChordTests(unittest.TestCase):
 def test_batch_identity_and_binding_guard(self):
  calls=[]; binds=[]
  def run(args,**kwargs):
   calls.append(args)
   if args[:2]==['hyprctl','-j']:
    value=binds if args[2]=='binds' else {'address':'a','class':'b'}
    return SimpleNamespace(stdout=json.dumps(value).encode())
   if args[-1]=='--describe':
    return SimpleNamespace(stdout=json.dumps({'schema':1,'modifiers':['CTRL','SHIFT'],'keys':['F'+str(n) for n in (1,2,3,5,6,7,8,9,10,11)]}).encode())
   return SimpleNamespace(stdout=b'')
  d=Desktop('a','b',0,0,4,run=run,input_backend='native-chord')
  self.assertTrue(d.send_keys(['F21','F13','F20','F22']))
  emitted=[c for c in calls if c[0].endswith('carrier-keys') and c[-1]!='--describe']
  self.assertEqual(len(emitted),1)
  self.assertEqual(emitted[0][1:],['0','C9','C1','C8','C10'])
  binds.append({'modmask':5,'key':'F5'})
  self.assertFalse(d.send_keys(['F21']))
  self.assertEqual(len([c for c in calls if c[0].endswith('carrier-keys') and c[-1]!='--describe']),1)
 def test_invalid_modes(self):
  for options in ({'key_hold_ms':-1},{'key_hold_ms':51},{'key_encoding':'binary'}):
   with self.assertRaises(ValueError):Desktop('a','b',0,0,4,input_backend='native-chord',**options)
if __name__=='__main__':unittest.main()

class NativeValidationTests(unittest.TestCase):
 def test_native_binary_rejects_noncarrier_before_desktop(self):
  import subprocess,pathlib
  binary=pathlib.Path(__file__).parent/'native/carrier-keys'
  if not binary.exists(): self.skipTest('native helper not built')
  for args in (['0','A'],['-1','C1'],['0','C11'],['51','C1'],['0','C1','F13']):
   self.assertEqual(subprocess.run([str(binary.resolve()),*args],env={}).returncode,2)

class NativeDescriptorTests(unittest.TestCase):
 def test_built_binary_excludes_alt_and_f4(self):
  import subprocess,pathlib
  binary=pathlib.Path(__file__).parent/'native/carrier-keys'
  if not binary.exists():self.skipTest('native helper not built')
  spec=json.loads(subprocess.check_output([str(binary.resolve()),'--describe'],env={}))
  self.assertEqual(spec['modifiers'],['CTRL','SHIFT'])
  self.assertNotIn('F4',spec['keys'])
  self.assertEqual(len(spec['keys']),10)
  self.assertEqual(subprocess.run([str(binary.resolve()),'0','F13'],env={}).returncode,2)
