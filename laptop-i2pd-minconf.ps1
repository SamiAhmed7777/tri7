
"--- all i2pd processes ---"
Get-Process i2pd -ErrorAction SilentlyContinue | ForEach-Object { "pid=" + $_.Id + " path=" + $_.Path }
"--- port owners ---"
netstat -ano | Select-String ':7070|:4447' | ForEach-Object { $_.Line.Trim() }
"--- minimal-conf isolation test (7081/4449, own datadir) ---"
New-Item -ItemType Directory -Force -Path 'C:\Coins\i2pd\minconf' | Out-Null
@"
log=info
logfile=C:\Coins\i2pd\minconf\i2pd.log
[http]
enabled=true
address=127.0.0.1
port=7081
[socksproxy]
enabled=true
address=127.0.0.1
port=4449
"@ | Set-Content 'C:\Coins\i2pd\minconf\i2pd.conf'
$exe = 'C:\Coins\i2pd\pkg2\i2pd.exe'
$p = Start-Process -FilePath $exe -ArgumentList '--conf=C:\Coins\i2pd\minconf\i2pd.conf','--datadir=C:\Coins\i2pd\minconf\data' -WindowStyle Hidden -PassThru -RedirectStandardOutput C:\Coins\i2pd\minconf\out.txt -RedirectStandardError C:\Coins\i2pd\minconf\err.txt
Start-Sleep -Seconds 25
if ($p.HasExited) { "exited code=" + $p.ExitCode } else { "alive pid=" + $p.Id + " cpu=" + [math]::Round($p.CPU,2) }
foreach ($port in 7081,4449) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
"--- log ---"
Get-Content C:\Coins\i2pd\minconf\i2pd.log -Tail 8 -ErrorAction SilentlyContinue
"--- out ---"
Get-Content C:\Coins\i2pd\minconf\out.txt -Tail 8 -ErrorAction SilentlyContinue
"--- err ---"
Get-Content C:\Coins\i2pd\minconf\err.txt -Tail 5 -ErrorAction SilentlyContinue
