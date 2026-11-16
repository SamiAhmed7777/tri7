
$ErrorActionPreference = 'Continue'
Set-Location C:\Coins\TRI-LAPTOP
schtasks /end /tn tri7gui-laptop 2>$null | Out-Null
Start-Sleep 3
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 2
$gh = (Get-FileHash C:\Users\Sami\tri7_wallet_gui.new.exe -Algorithm SHA256).Hash
"uploaded: " + $gh.Substring(0,16)
Remove-Item .\tri7_wallet_gui.exe -Force -ErrorAction SilentlyContinue
Move-Item C:\Users\Sami\tri7_wallet_gui.new.exe .\tri7_wallet_gui.exe
Start-ScheduledTask -TaskName tri7gui-laptop
Start-Sleep -Seconds 15
$g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($g) { "gui RUNNING pid=" + $g[0].Id } else { "gui NOT RUNNING (state: " + (Get-ScheduledTask -TaskName tri7gui-laptop).State + ")" }
Get-Content C:\Coins\TRI-LAPTOP\node\state\node.log -Tail 3 -ErrorAction SilentlyContinue
