
Start-Sleep -Seconds 60
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { "i2pd pid=" + $p[0].Id + " cpu=" + [math]::Round($p[0].CPU,1) + " mem=" + [math]::Round($p[0].WorkingSet64/1MB,1) + "MB" }
"log file:"
Get-Item C:\Coins\i2pd\data\i2pd.log -ErrorAction SilentlyContinue | ForEach-Object { "size=" + $_.Length }
Get-Content C:\Coins\i2pd\data\i2pd.log -Tail 10 -ErrorAction SilentlyContinue
