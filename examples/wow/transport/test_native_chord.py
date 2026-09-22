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

class NativeTimingTests(unittest.TestCase):
 def test_ack_and_full_frame_budget_includes_roundtrips_and_holds(self):
  from protocol import Frame, keys
  from wayland import NATIVE_TIMEOUT_LIMIT
  for hold in (0, 1, 5, 20):
   for size in (0, 96):
    with self.subTest(hold=hold, payload=size):
     calls=[]
     def run(args, **kwargs):
      if args[:2]==['hyprctl','-j']:
       value=[] if args[2]=='binds' else {'address':'a','class':'b'}
       return SimpleNamespace(stdout=json.dumps(value).encode())
      self.assertNotEqual(args[-1], '--describe')
      count=len(args)-2
      # A 5ms compositor roundtrip previously timed out full frames even when
      # ACKs passed. No real process, clock delay, or desktop is involved here.
      elapsed=.060+(2*count+5)*.005+2*count*hold/1000
      self.assertGreater(kwargs['timeout'], elapsed)
      self.assertLess(kwargs['timeout'], NATIVE_TIMEOUT_LIMIT)
      calls.append((count, kwargs['timeout']))
      return SimpleNamespace(stdout=b'')
     desktop=Desktop('a','b',0,0,4,run=run,input_backend='native-chord',key_hold_ms=hold)
     desktop._native_verified=True
     packet=Frame(17,seq=1 if size else 0,payload=b'x'*size).encode()
     self.assertTrue(desktop.send_keys(keys(packet)))
     self.assertEqual(len(calls), 1)
     self.assertEqual(calls[0][0], 335 if size else 47)

 def test_overlong_native_batch_is_rejected_before_any_desktop_io(self):
  from protocol import Frame, keys
  for hold in (21, 50):
   with self.subTest(hold=hold):
    def forbidden(*args, **kwargs):
     self.fail('overlong batch reached desktop IO')
    desktop=Desktop('a','b',0,0,4,run=forbidden,input_backend='native-chord',key_hold_ms=hold)
    with self.assertRaisesRegex(ValueError, 'receiver frame deadline'):
     desktop.send_keys(keys(Frame(17,seq=1,payload=b'x'*96).encode()))
