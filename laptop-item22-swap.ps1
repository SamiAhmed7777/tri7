$ErrorActionPreference = 'Stop'
Set-Location C:\Coins\TRI-LAPTOP
schtasks /end /tn tri7gui-laptop 2>$null | Out-Null
Start-Sleep -Seconds 3
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
$gh = (Get-FileHash C:\Users\sami\tri7_wallet_gui.new.exe -Algorithm SHA256).Hash
"uploaded gui: $gh"
Remove-Item .\tri7_wallet_gui.exe -Force -ErrorAction SilentlyContinue
Move-Item C:\Users\sami\tri7_wallet_gui.new.exe .\tri7_wallet_gui.exe
$gh2 = (Get-FileHash .\tri7_wallet_gui.exe -Algorithm SHA256).Hash
"in-place gui: $gh2"
if ($gh -ne $gh2) { throw 'gui hash mismatch after move' }
Start-ScheduledTask -TaskName tri7gui-laptop
Start-Sleep -Seconds 12
$st = (Get-ScheduledTask -TaskName tri7gui-laptop).State
"tri7gui-laptop state: $st"
"SWAP_DONE"
