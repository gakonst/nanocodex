"""Instrument only the archived build; production views are unchanged."""
import pathlib, sys
root, fixture = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
p = root / 'macos/Nanocodex/NanocodexApp.swift'
s = p.read_text()
start = s.index('            model.runtime.requestOverride =')
end = s.index('            return', start)
s = s[:start] + '            EvidenceFixture.start(model)\n' + s[end:]
s = s.replace('NSApp.activate(ignoringOtherApps: true)', '// Do not activate or type into the desktop.')
p.write_text(s + '\n' + fixture.read_text())
p = root / 'macos/Nanocodex.xcodeproj/project.pbxproj'
s = p.read_text().replace('buildPhases = (CE1000000000000000000004, ', 'buildPhases = (').replace(', F2E18FFC17E2595362B5468A); buildRules', '); buildRules')
s = s.replace('xyz.paradigm.nanocodex.macos"', 'xyz.nanocodex.evidence.pr396"')
p.write_text(s)
