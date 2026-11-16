
"--- kill everything i2pd ---"
schtasks /end /tn i2pd 2>$null | Out-Null
Get-Process i2pd -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 3
"--- clean main conf ---"
New-Item -ItemType Directory -Force -Path 'C:\Coins\i2pd\main' | Out-Null
@"
log=info
logfile=C:\Coins\i2pd\main\i2pd.log
[http]
enabled=true
address=127.0.0.1
port=7070
[socksproxy]
enabled=true
address=127.0.0.1
port=4447
"@ | Set-Content 'C:\Coins\i2pd\main\i2pd.conf'
"--- rewrite the scheduled task: 2.56 binary + clean conf ---"
schtasks /end /tn i2pd 2>$null | Out-Null
schtasks /delete /tn i2pd /f 2>$null | Out-Null
schtasks /create /tn i2pd /tr 'C:\Coins\i2pd\pkg2\i2pd.exe --conf=C:\Coins\i2pd\main\i2pd.conf --datadir=C:\Coins\i2pd\main\data' /sc onstart /ru SYSTEM /rl highest /f | Out-Null
schtasks /run /tn i2pd | Out-Null
Start-Sleep -Seconds 30
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { "i2pd pid=" + $p[0].Id + " cpu=" + [math]::Round($p[0].CPU,2) + " path=" + $p[0].Path } else { "NOT RUNNING" }
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
"--- log tail (bootstrap proof) ---"
Get-Content C:\Coins\i2pd\main\i2pd.log -Tail 5 -ErrorAction SilentlyContinue
"--- task state ---"
(Get-ScheduledTask -TaskName i2pd).State
