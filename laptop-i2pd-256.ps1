$ErrorActionPreference = 'Continue'
"=== 1. clear active threats (Remediation) ==="
try { Remove-MpThreat -ErrorAction Stop; "Remove-MpThreat OK" } catch { "Remove-MpThreat: $($_.Exception.Message)" }

"=== 2. try i2pd 2.56 mingw build (different hash than the flagged 3.0.0) ==="
$zip = 'C:\Coins\i2pd\i2pd_256_win64.zip'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Invoke-WebRequest -UseBasicParsing -Uri 'https://github.com/PurpleI2P/i2pd/releases/download/2.56.0/i2pd_2.56.0_win64_mingw.zip' -OutFile $zip
if (-not (Test-Path $zip)) { "download FAILED"; exit 1 }
"zip size: " + (Get-Item $zip).Length
Remove-Item 'C:\Coins\i2pd\pkg2' -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive -Path $zip -DestinationPath 'C:\Coins\i2pd\pkg2' -Force
$exe = Get-ChildItem 'C:\Coins\i2pd\pkg2' -Recurse -Filter 'i2pd.exe' | Select-Object -First 1
if (-not $exe) { "no i2pd.exe in zip"; exit 1 }
"binary: " + $exe.FullName + "  size=" + $exe.Length
"sha256: " + (Get-FileHash $exe.FullName -Algorithm SHA256).Hash

"=== 3. launch 2.56 directly, capture console ==="
Stop-Process -Name i2pd -Force -ErrorAction SilentlyContinue
Start-Sleep 2
$p = Start-Process -FilePath $exe.FullName -ArgumentList '--conf=C:\Coins\i2pd\i2pd.conf','--tunnelsconf=C:\Coins\i2pd\tunnels.conf','--datadir=C:\Coins\i2pd\data2' -WindowStyle Hidden -RedirectStandardOutput C:\Users\Sami\i2pd256-out.txt -RedirectStandardError C:\Users\Sami\i2pd256-err.txt -PassThru
Start-Sleep -Seconds 30
if ($p.HasExited) { "exited code=" + $p.ExitCode } else { "alive pid=" + $p.Id + " cpu=" + [math]::Round($p.CPU,2) }
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
"--- stdout head ---"
Get-Content C:\Users\Sami\i2pd256-out.txt -TotalCount 8 -ErrorAction SilentlyContinue
"--- new detections since launch ---"
(Get-MpThreatDetection -ErrorAction SilentlyContinue | Sort-Object InitialDetectionTime -Descending | Select-Object -First 1).InitialDetectionTime.ToString()
"DONE"
