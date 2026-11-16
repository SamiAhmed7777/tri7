
"--- kill suspended husk + relaunch ---"
Stop-Process -Name i2pd -Force -ErrorAction SilentlyContinue
Start-Sleep 3
schtasks /run /tn i2pd | Out-Null
Start-Sleep -Seconds 45
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { "i2pd pid=" + $p[0].Id + " cpu=" + [math]::Round($p[0].CPU,2) + " mem=" + [math]::Round($p[0].WorkingSet64/1MB,1) + "MB" } else { "i2pd NOT RUNNING" }
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
"--- log ---"
Get-Content C:\Coins\i2pd\data\i2pd.log -Tail 8 -ErrorAction SilentlyContinue
"--- threat detections after relaunch ---"
(Get-MpThreatDetection -ErrorAction SilentlyContinue | Sort-Object InitialDetectionTime -Descending | Select-Object -First 1).InitialDetectionTime
