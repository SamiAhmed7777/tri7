Get-Process tri7_wallet_gui,tor -ErrorAction SilentlyContinue |
  Select-Object ProcessName,Id | Format-Table -AutoSize
Write-Output '--- torrc ---'
Get-Content C:\Coins\TRI-LAPTOP\node\state\torrc -ErrorAction SilentlyContinue
Write-Output '--- tor lines in node.log (last 6) ---'
Select-String -Path C:\Coins\TRI-LAPTOP\node\node.log -Pattern 'tor-supervisor','Invalid escape','Bootstrapped','NOT usable' -ErrorAction SilentlyContinue |
  Select-Object -Last 6 | ForEach-Object { $_.Line }
Write-Output '--- tip ---'
Get-Content C:\Coins\TRI-LAPTOP\node\node.log -Tail 4
