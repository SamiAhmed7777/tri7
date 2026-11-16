$ErrorActionPreference = 'Continue'
"=== integrity / elevation check ==="
whoami /groups | Select-String 'S-1-16|Label' | Select-Object -First 2
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
"is-admin: $admin"

"=== 1. Defender exclusions (path + process) ==="
try {
  Add-MpPreference -ExclusionPath 'C:\Coins\i2pd' -ErrorAction Stop
  Add-MpPreference -ExclusionProcess 'i2pd.exe' -ErrorAction Stop
  "exclusions ADDED"
} catch {
  "Add-MpPreference FAILED: $($_.Exception.Message)"
}
"current exclusions:"
(Get-MpPreference | Select-Object -ExpandProperty ExclusionPath -ErrorAction SilentlyContinue)
(Get-MpPreference | Select-Object -ExpandProperty ExclusionProcess -ErrorAction SilentlyContinue)

"=== 2. locate or re-extract i2pd.exe ==="
New-Item -ItemType Directory -Force -Path 'C:\Coins\i2pd' | Out-Null
$zip = 'C:\Coins\i2pd\i2pd_win64.zip'
if (-not (Test-Path $zip)) {
  "zip missing - re-downloading official 3.0.0"
  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
  Invoke-WebRequest -UseBasicParsing -Uri 'https://github.com/PurpleI2P/i2pd-windows/releases/download/3.0.0/i2pd_3.0.0_win64.zip' -OutFile $zip
}
$exe = Get-ChildItem 'C:\Coins\i2pd' -Recurse -Filter 'i2pd.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $exe) {
  "no i2pd.exe anywhere - extracting zip fresh"
  Expand-Archive -Path $zip -DestinationPath 'C:\Coins\i2pd\pkg' -Force
  $exe = Get-ChildItem 'C:\Coins\i2pd\pkg' -Recurse -Filter 'i2pd.exe' | Select-Object -First 1
}
if ($exe) { "i2pd.exe at: $($exe.FullName)  size=$($exe.Length)" } else { "STILL NO i2pd.exe - abort"; exit 1 }

"=== 3. sha256 of the binary ==="
(Get-FileHash $exe.FullName -Algorithm SHA256).Hash

"=== 4. start i2pd (scheduled task or direct) ==="
$t = Get-ScheduledTask -TaskName 'i2pd' -ErrorAction SilentlyContinue
if ($t) {
  "task exists state=" + $t.State + " - starting"
  schtasks /end /tn i2pd 2>$null | Out-Null
  Start-Sleep 2
  schtasks /run /tn i2pd | Out-Null
} else {
  "no task - starting direct hidden child"
  Start-Process -FilePath $exe.FullName -ArgumentList '--conf','C:\Coins\i2pd\i2pd.conf','--tunnelscfg','C:\Coins\i2pd\tunnels.conf','--datadir','C:\Coins\i2pd\data' -WindowStyle Hidden
}
Start-Sleep -Seconds 25

"=== 5. survival + ports ==="
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { "i2pd RUNNING pid=" + $p[0].Id } else { "i2pd NOT RUNNING (Defender may have killed it again)" }
foreach ($port in 7070,4447) {
  $open = Test-NetConnection 127.0.0.1 -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue
  "port ${port}: $open"
}
"DONE"
