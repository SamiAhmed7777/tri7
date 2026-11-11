@echo off
rem tri7 auto-update: wait for GUI exit (max 60s), settle the daemon (max 15s), silent-install, relaunch
set /a n=0
rem truncate the identity file ONCE, before any wait loop (r4
rem placement regression: inside the loop it wiped the capture every
rem iteration)
break > "%~dp0update-tasks.txt"
:wait
tasklist /FI "IMAGENAME eq tri7_wallet_gui.exe" 2>nul | find /I "tri7_wallet_gui.exe" >nul
if not errorlevel 1 (set /a n+=1 & if %n% LSS 60 (timeout /t 1 /nobreak >nul & goto wait))
set /a n=0
:waitd
tasklist /FI "IMAGENAME eq follow_tip.exe" 2>nul | find /I "follow_tip.exe" >nul
if not errorlevel 1 (set /a n+=1 & if %n% LSS 15 (timeout /t 1 /nobreak >nul & goto waitd))
rem daemon may be task-managed and never exit: stop its OWNING task(s)
rem (any task whose query mentions follow_tip), then force-kill the
rem process and RE-CHECK - a watchdog respawn inside NSIS's replace
rem window would leave the old daemon exe locked. Fleet watchdogs run
rem hourly; kill+install completes in seconds. The task/watchdog
rem restarts the daemon on the new binary afterwards.
rem stop scheduled tasks that LAUNCH the daemon: actions mentioning
rem follow_tip OR the fleet's ft_run launcher (Tasks module exposes
rem actions cleanly - no CSV parsing, no localized headers). Residual:
rem a task whose launcher REFERENCES follow_tip only inside the .bat
rem body is caught by the taskkill below; fleet watchdogs tick hourly
rem while this script completes in seconds.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Get-ScheduledTask | Where-Object { (@($_.Actions | ForEach-Object { $_.Execute + ' ' + $_.Arguments }) -join ' ') -match 'follow_tip|ft_run' } | ForEach-Object { ($_.TaskPath + $_.TaskName) | Out-File "%~dp0update-tasks.txt" -Append; Stop-ScheduledTask -TaskPath $_.TaskPath -TaskName $_.TaskName }" >nul 2>&1
taskkill /F /IM follow_tip.exe >nul 2>&1
timeout /t 2 /nobreak >nul
taskkill /F /IM follow_tip.exe >nul 2>&1
"C:\upd test\fake-setup.exe" /S
set /a rc=%errorlevel%
rem failed install must NEVER read as a successful install-and-restart:
rem leave a marker the relaunched GUI surfaces, then relaunch anyway
rem (the previous binary is still in place after a failed NSIS run).
if not %rc%==0 > "%~dp0install-failed.marker" echo update-install-failed rc=%rc%
start "" "C:\upd test\relaunch.cmd"
rem restore the daemon tasks we stopped (the operator's staking
rem resumes on the NEW binary without any manual step)
if exist "%~dp0update-tasks.txt" powershell -NoProfile -ExecutionPolicy Bypass -Command "Get-Content '%~dp0update-tasks.txt' | ForEach-Object { $id = $_; $i = $id.LastIndexOf('\'); Start-ScheduledTask -TaskPath $id.Substring(0, $i + 1) -TaskName $id.Substring($i + 1) }" >nul 2>&1
rem update-tasks.txt is RETAINED on disk: if the restoration start
rem was lost (duplicate-instance port-bind cascade, task disabled),
rem it is the actionable recovery state - the identities to
rem Start-ScheduledTask. The next wrapper run truncates it.
del "%~f0"
