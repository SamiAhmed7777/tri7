
"--- current i2pd.conf ---"
Get-Content C:\Coins\i2pd\i2pd.conf
"--- append socksproxy if missing ---"
$conf = Get-Content C:\Coins\i2pd\i2pd.conf -Raw
if ($conf -notmatch 'socksproxy') {
  Add-Content C:\Coins\i2pd\i2pd.conf "`r`n[socksproxy]`r`nenabled = true`r`naddress = 127.0.0.1`r`nport = 4447"
  "appended [socksproxy] 4447"
} else { "already present" }
"--- restart under the scheduled task ---"
Stop-Process -Name i2pd -Force -ErrorAction SilentlyContinue
Start-Sleep 2
schtasks /run /tn i2pd | Out-Null
Start-Sleep -Seconds 30
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { "i2pd pid=" + $p[0].Id + " cpu=" + [math]::Round($p[0].CPU,2) } else { "NOT RUNNING" }
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
