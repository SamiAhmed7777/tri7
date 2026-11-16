$ErrorActionPreference = 'Stop'
Set-Location C:\Users\hello\tri7
schtasks /end /tn tri7gui-run 2>$null | Out-Null
Start-Sleep -Seconds 3
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
$gh = (Get-FileHash .\tri7_wallet_gui.new.exe -Algorithm SHA256).Hash
"uploaded: $gh"
Remove-Item .\tri7_wallet_gui.exe -Force -ErrorAction SilentlyContinue
Move-Item .\tri7_wallet_gui.new.exe .\tri7_wallet_gui.exe
$gh2 = (Get-FileHash .\tri7_wallet_gui.exe -Algorithm SHA256).Hash
"in-place: $gh2"
if ($gh -ne $gh2) { throw 'hash mismatch' }
Start-ScheduledTask -TaskName tri7gui-run
Start-Sleep -Seconds 15
$p = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
$ft = Get-Process follow_tip -ErrorAction SilentlyContinue
if ($p) { "gui RUNNING pid=" + $p[0].Id } else { "gui NOT RUNNING (state: " + (Get-ScheduledTask -TaskName tri7gui-run).State + ")" }
if ($ft) { "daemon still RUNNING pid=" + $ft[0].Id } else { "DAEMON DOWN!" }
"SWAP_DONE"
