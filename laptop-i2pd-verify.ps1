
schtasks /query /tn i2pd /v /fo list | Select-String -Pattern 'Task To Run|Run As|Status'
Start-Sleep -Seconds 40
"--- ports after wait ---"
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { "i2pd still RUNNING pid=" + $p[0].Id + " cpu=" + [math]::Round($p[0].CPU,1) } else { "i2pd DIED" }
"--- tunnels page b32s ---"
try { (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7070/?page=i2p_tunnels -TimeoutSec 6).Content | Select-String -Pattern '[a-z2-7]{52}\.b32\.i2p' -AllMatches | ForEach-Object { $_.Matches.Value } | Sort-Object -Unique } catch { "webconsole: $($_.Exception.Message)" }
