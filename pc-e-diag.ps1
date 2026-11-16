$ErrorActionPreference = 'Continue'
Stop-Process -Name tri7_wallet_gui -Force -ErrorAction SilentlyContinue
Start-Sleep 2
Start-Process -FilePath 'E:\Coins\TRI\bin\tri7_wallet_gui.exe' -WorkingDirectory 'E:\Coins\TRI\bin' -WindowStyle Hidden -PassThru | Out-Null
foreach ($t in 15,40,75) {
  Start-Sleep -Seconds ($t - ($t - 15))
}
foreach ($wait in 0,25,60) {
  if ($wait -gt 0) { Start-Sleep -Seconds ($wait - 25) }
  $g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
  $tor = Get-Process tor -ErrorAction SilentlyContinue
  $i2 = Get-Process i2pd -ErrorAction SilentlyContinue
  "--- t=+$wait s ---"
  if ($g) { "gui alive pid=" + $g[0].Id } else { "gui DEAD" }
  if ($tor) { "tor alive pid=" + $tor[0].Id } else { "tor not running" }
  if ($i2) { "i2pd alive pid=" + $i2[0].Id } else { "i2pd not running" }
  if (-not $g) { break }
}
"--- node dir created?"
Test-Path 'E:\Coins\TRI\bin\node'
"--- AVG sandbox log last lines mentioning E:"
Get-Content 'C:\ProgramData\AVG\Antivirus\log\autosandbox.log' -Tail 200000 -ErrorAction SilentlyContinue | Select-String -Pattern 'E:\\Coins' | Select-Object -Last 3 | ForEach-Object { $_.Line.Substring(0, [Math]::Min(180, $_.Line.Length)) }
"E-DIAG-DONE"
