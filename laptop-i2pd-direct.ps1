
"--- kill task instance, run directly as current user with console capture ---"
schtasks /end /tn i2pd 2>$null | Out-Null
Stop-Process -Name i2pd -Force -ErrorAction SilentlyContinue
Start-Sleep 2
$p = Start-Process -FilePath 'C:\Coins\i2pd\pkg\i2pd.exe' -ArgumentList '--conf=C:\Coins\i2pd\i2pd.conf','--tunnelsconf=C:\Coins\i2pd\tunnels.conf','--datadir=C:\Coins\i2pd\data' -WindowStyle Hidden -RedirectStandardOutput C:\Users\Sami\i2pd-out.txt -RedirectStandardError C:\Users\Sami\i2pd-err.txt -PassThru
Start-Sleep -Seconds 30
if ($p.HasExited) { "exited code=" + $p.ExitCode } else { "alive pid=" + $p.Id + " cpu=" + [math]::Round($p.CPU,2) }
"ports:"
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
"--- stdout ---"
Get-Content C:\Users\Sami\i2pd-out.txt -Tail 12 -ErrorAction SilentlyContinue
"--- stderr ---"
Get-Content C:\Users\Sami\i2pd-err.txt -Tail 6 -ErrorAction SilentlyContinue
"--- newest detection ---"
(Get-MpThreatDetection -ErrorAction SilentlyContinue | Sort-Object InitialDetectionTime -Descending | Select-Object -First 1).InitialDetectionTime.ToString()
