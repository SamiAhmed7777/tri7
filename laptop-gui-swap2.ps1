$ErrorActionPreference = 'Stop'
Set-Location C:\Coins\TRI-LAPTOP
schtasks /end /tn tri7gui-laptop 2>$null | Out-Null
Start-Sleep -Seconds 3
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
$gh = (Get-FileHash C:\Users\Sami\tri7_wallet_gui.new.exe -Algorithm SHA256).Hash
"uploaded: $gh"
Remove-Item .\tri7_wallet_gui.exe -Force -ErrorAction SilentlyContinue
Move-Item C:\Users\Sami\tri7_wallet_gui.new.exe .\tri7_wallet_gui.exe
$gh2 = (Get-FileHash .\tri7_wallet_gui.exe -Algorithm SHA256).Hash
"in-place: $gh2"
if ($gh -ne $gh2) { throw 'hash mismatch' }
Start-ScheduledTask -TaskName tri7gui-laptop
Start-Sleep -Seconds 15
$p = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($p) { "gui RUNNING pid=" + $p[0].Id } else { "gui NOT RUNNING (task state: " + (Get-ScheduledTask -TaskName tri7gui-laptop).State + ")" }
Get-Content C:\Coins\TRI-LAPTOP\node\state\node.log -Tail 3 -ErrorAction SilentlyContinue
"SWAP_DONE"
