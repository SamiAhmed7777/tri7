
$ErrorActionPreference = 'Continue'
schtasks /end /tn tri7d2 2>$null | Out-Null
schtasks /end /tn tri7gui-run 2>$null | Out-Null
Start-Sleep 3
Get-Process follow_tip -ErrorAction SilentlyContinue | Stop-Process -Force
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 2
Start-ScheduledTask -TaskName tri7d2
Start-Sleep -Seconds 12
Start-ScheduledTask -TaskName tri7gui-run
Start-Sleep -Seconds 12
$ft = Get-Process follow_tip -ErrorAction SilentlyContinue
if ($ft) { "daemon RUNNING pid=" + $ft[0].Id + " start=" + $ft[0].StartTime } else { "daemon NOT RUNNING" }
$g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($g) { "gui RUNNING pid=" + $g[0].Id } else { "gui NOT RUNNING" }
