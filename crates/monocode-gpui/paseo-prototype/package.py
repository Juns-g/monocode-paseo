#!/usr/bin/env python3
"""Package an already optimized native binary as an isolated Paseo prototype."""
import json, plistlib, shutil, subprocess, sys, os
from pathlib import Path
root=Path(__file__).resolve().parents[3]
out=Path(sys.argv[1]).resolve()
app=out/'Monocode Paseo Prototype.app'
if app.exists():
    raise SystemExit('Refusing to overwrite candidate; choose a fresh output directory')
resources=app/'Contents/Resources';binary=app/'Contents/MacOS/monocode-gpui'
resources.mkdir(parents=True);binary.parent.mkdir()
subprocess.run(['node',str(Path(__file__).with_name('bundle.mjs')),str(resources)],check=True,cwd=root)
target=Path(os.environ.get('CARGO_TARGET_DIR') or root/'crates/monocode-gpui/target')
shutil.copy2(target/'release/monocode-gpui',binary)
shutil.copy2(root/'src-tauri/icons/icon.icns',resources/'monocode.icns')
for source,name in [(root/'LICENSE','Monocode-LICENSE'),(root/'NOTICE','Monocode-NOTICE'),((Path(os.environ['PASEO_SOURCE'])/'LICENSE'),'Paseo-LICENSE'),((Path(os.environ['PASEO_SOURCE'])/'node_modules/ws/LICENSE'),'ws-LICENSE'),((Path(os.environ['PASEO_SOURCE'])/'node_modules/zod/LICENSE'),'zod-LICENSE')]:
    if source.exists():shutil.copy2(source,resources/name)
metadata={'CFBundleExecutable':'monocode-gpui','CFBundleIdentifier':'dev.usemono.paseo-prototype',
 'CFBundleName':'Monocode Paseo Prototype','CFBundleDisplayName':'Monocode Paseo Prototype',
 'CFBundlePackageType':'APPL','CFBundleShortVersionString':'0.2.10','CFBundleVersion':'28',
 'MonocodeUpstreamBase':'main','MonocodeUpstreamCommit':'00c693077d7e74ae72092e4194f5cd2384585467',
 'CFBundleIconFile':'monocode.icns','NSHighResolutionCapable':True,'NSPrincipalClass':'NSApplication',
 'PaseoPrototype':True,'MonocodeDataMode':'paseo-prototype'}
# Main checks a stable literal marker before any Store or ordinary data-mode path.
plist=plistlib.dumps(metadata).decode().replace('<key>PaseoPrototype</key>\n\t<true/>','<key>PaseoPrototype</key><true/>')
(app/'Contents/Info.plist').write_text(plist)
subprocess.run(['codesign','--force','--sign','-',str(app)],check=True)
subprocess.run(['codesign','--verify','--deep','--strict',str(app)],check=True)
print(app)
