#!/usr/bin/env python3
"""Build an owned audio source only; never launches it or plays sound."""
import datetime, hashlib, json, math, pathlib, plistlib, struct, subprocess, wave
root=pathlib.Path(__file__).resolve().parents[2]
folder=root/'artifacts'/('native-audio-'+datetime.datetime.now(datetime.UTC).strftime('%Y%m%dT%H%M%S%fZ'))
app=folder/'Skyre Synthetic Audio.app'
(app/'Contents/MacOS').mkdir(parents=True)
(app/'Contents/Resources').mkdir()
(app/'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleExecutable':'Tone','CFBundleIdentifier':'org.skyre.rebuild.audiofixture.20260906','CFBundleName':'Skyre Synthetic Audio','CFBundlePackageType':'APPL'}))
source=pathlib.Path(__file__).with_name('Tone.swift')
subprocess.run(['xcrun','swiftc','-swift-version','5',str(source),'-o',str(app/'Contents/MacOS/Tone')],check=True)
with wave.open(str(app/'Contents/Resources/synthetic-440hz.wav'),'wb') as output:
 output.setnchannels(1); output.setsampwidth(2); output.setframerate(24000)
 output.writeframes(b''.join(struct.pack('<h',round(32767*0.1*math.sin(2*math.pi*440*i/24000))) for i in range(48000)))
subprocess.run(['codesign','--force','--sign','-',str(app)],check=True)
(folder/'Tone.swift').write_bytes(source.read_bytes())
(folder/'manifest.json').write_text(json.dumps({'bundle':str(app),'scope':'Owned generated tone source; no playback or capture occurred during build','files':{str(p.relative_to(folder)):hashlib.sha256(p.read_bytes()).hexdigest() for p in folder.rglob('*') if p.is_file()}},indent=2)+'\n')
print(app)
