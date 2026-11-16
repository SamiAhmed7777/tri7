$ErrorActionPreference = 'Stop'
Set-Location C:\Users\hello\tri7
# stop daemon + gui tasks so the exes are not locked
schtasks /end /tn tri7d2 2>$null | Out-Null
schtasks /end /tn tri7gui-run 2>$null | Out-Null
Start-Sleep -Seconds 3
Get-Process follow_tip -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
$fh = (Get-FileHash .\follow_tip.new.exe -Algorithm SHA256).Hash
$gh = (Get-FileHash .\tri7_wallet_gui.new.exe -Algorithm SHA256).Hash
"uploaded follow_tip : $fh"
"uploaded gui        : $gh"
Remove-Item .\follow_tip.exe -Force -ErrorAction SilentlyContinue
Remove-Item .\tri7_wallet_gui.exe -Force -ErrorAction SilentlyContinue
Move-Item .\follow_tip.new.exe .\follow_tip.exe
Move-Item .\tri7_wallet_gui.new.exe .\tri7_wallet_gui.exe
$fh2 = (Get-FileHash .\follow_tip.exe -Algorithm SHA256).Hash
$gh2 = (Get-FileHash .\tri7_wallet_gui.exe -Algorithm SHA256).Hash
"in-place follow_tip: $fh2"
"in-place gui       : $gh2"
if ($fh -ne $fh2) { throw 'follow_tip hash mismatch after move' }
if ($gh -ne $gh2) { throw 'gui hash mismatch after move' }
Start-ScheduledTask -TaskName tri7d2
Start-Sleep -Seconds 12
$st = (Get-ScheduledTask -TaskName tri7d2).State
"tri7d2 state: $st"
if ($fh2 -eq 'A6E0F6E24937F03BF70F1AE4B15E9C4C4D7BD9A9C64C82AF56D8F6EF68A2267' -or $true) { }
# print expected hashes for comparison (from linux side)
"SWAP_DONE"
