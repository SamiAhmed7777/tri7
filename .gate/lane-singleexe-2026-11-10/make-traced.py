#!/usr/bin/env python3
"""Mechanically derive wrapper-traced.bat from wrapper-final.bat.
Defaults: src=wrapper-final.bat, dst=wrapper-traced.bat.
Adds ONLY breadcrumb echo lines (no other edits); verifies the result's
non-breadcrumb lines equal the source's (the only difference is the
instrumentation). Prints both sha256 hashes. Exits nonzero on any mismatch."""
import hashlib, sys
args = sys.argv[1:]
src_path = args[0] if len(args) > 0 else 'wrapper-final.bat'
dst_path = args[1] if len(args) > 1 else 'wrapper-traced.bat'
assert 'wrapper-spaces' not in src_path, 'source must be wrapper-final.bat, not the spaces variant' 
src = open(src_path, newline='').read()
lines = src.split('\r\n')
out = []
for l in lines:
    if l.startswith('break >'):
        out.append('echo [%time%] IDENTITY-TRUNCATED >> C:\\updtest\\debug.log')
    if 'Get-ScheduledTask' in l and 'powershell' in l:
        out.append('echo [%time%] DISCOVERY+TASK-STOP >> C:\\updtest\\debug.log')
    if 'taskkill' in l:
        out.append('echo [%time%] KILL >> C:\\updtest\\debug.log')
    if 'fake-setup.exe' in l and '/S' in l:
        out.append('echo [%time%] INSTALLER-START >> C:\\updtest\\debug.log')
    if 'install-failed.marker' in l and 'echo' in l:
        out.append('echo [%time%] FAILURE-MARKER-WRITTEN >> C:\\updtest\\debug.log')
    if 'relaunch.cmd' in l and 'start' in l:
        out.append('echo [%time%] RELAUNCH-START >> C:\\updtest\\debug.log')
    if 'Start-ScheduledTask' in l:
        out.append('echo [%time%] TASK-RESTORE-START >> C:\\updtest\\debug.log')
    if l.startswith('del "%~dp0update-tasks.txt"'):
        out.append('echo [%time%] IDENTITY-RETAINED-AS-RECOVERY-STATE >> C:\\updtest\\debug.log')
    if l.startswith('del "%~f0"'):
        out.append('echo [%time%] SELF-DEL >> C:\\updtest\\debug.log')
    out.append(l)
    if 'fake-setup.exe' in l and '/S' in l:
        out.append('echo [%time%] INSTALLER-DONE rc=%errorlevel% >> C:\\updtest\\debug.log')
    if 'Start-ScheduledTask' in l:
        out.append('echo [%time%] TASK-RESTORE-DONE >> C:\\updtest\\debug.log')
traced = '\r\n'.join(out)
open(dst_path, 'w', newline='').write(traced)
src_nb = [l for l in src.split('\r\n') if l.strip()]
dst_nb = [l for l in traced.split('\r\n') if l.strip() and not l.startswith('echo [%time%]')]
ok = src_nb == dst_nb
print('source sha256:', hashlib.sha256(src.encode()).hexdigest())
print('traced sha256:', hashlib.sha256(traced.encode()).hexdigest())
print('non-breadcrumb lines identical:', ok)
sys.exit(0 if ok else 1)
