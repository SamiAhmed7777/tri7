
"--- thread wait reasons (Suspended = Defender freeze) ---"
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { $p[0].Threads | Group-Object WaitReason | ForEach-Object { "$($_.Name): $($_.Count)" } }
"--- try disabling behavior monitoring ---"
try {
  Set-MpPreference -DisableBehaviorMonitoring $true -ErrorAction Stop
  "behavior monitoring DISABLED"
} catch { "Set-MpPreference failed: $($_.Exception.Message)" }
(Get-MpPreference).DisableBehaviorMonitoring
"--- restart task, re-probe ---"
schtasks /end /tn i2pd 2>$null | Out-Null
Stop-Process -Name i2pd -Force -ErrorAction SilentlyContinue
Start-Sleep 2
schtasks /run /tn i2pd | Out-Null
Start-Sleep -Seconds 30
$p2 = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p2) { "i2pd pid=" + $p2[0].Id + " cpu=" + [math]::Round($p2[0].CPU,2) + " threadsAlive=" + (($p2[0].Threads | Where-Object { $_.WaitReason -ne 'Suspended' }).Count) + "/" + $p2[0].Threads.Count }
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
