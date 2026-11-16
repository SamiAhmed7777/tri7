$ErrorActionPreference = 'Continue'
"--- 1. stop the E: gui instance ---"
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Where-Object { $_.Path -like 'E:*' } | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep 3
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep 2

"--- 2. current binaries C: -> E: ---"
Copy-Item 'C:\Users\hello\tri7\tri7_wallet_gui.exe' 'E:\Coins\TRI\bin\tri7_wallet_gui.exe' -Force
Copy-Item 'C:\Users\hello\tri7\follow_tip.exe' 'E:\Coins\TRI\bin\follow_tip.exe' -Force
"gui:  " + (Get-FileHash 'E:\Coins\TRI\bin\tri7_wallet_gui.exe' -Algorithm SHA256).Hash.Substring(0,16)
"ftip: " + (Get-FileHash 'E:\Coins\TRI\bin\follow_tip.exe' -Algorithm SHA256).Hash.Substring(0,16)

"--- 3. bundled tor (binaries only, fresh data) ---"
New-Item -ItemType Directory -Force -Path 'E:\Coins\TRI\bin\tor' | Out-Null
Copy-Item 'C:\Users\hello\tri7\tor\tor.exe' 'E:\Coins\TRI\bin\tor\tor.exe' -Force
Copy-Item 'C:\Users\hello\tri7\tor\tor-gencert.exe' 'E:\Coins\TRI\bin\tor\tor-gencert.exe' -Force -ErrorAction SilentlyContinue
if (Test-Path 'C:\Users\hello\tri7\tor\pluggable_transports') {
  Copy-Item 'C:\Users\hello\tri7\tor\pluggable_transports' 'E:\Coins\TRI\bin\tor\pluggable_transports' -Recurse -Force
}
"tor.exe: " + (Get-Item 'E:\Coins\TRI\bin\tor\tor.exe').Length + " bytes"

"--- 4. bundled i2pd ---"
New-Item -ItemType Directory -Force -Path 'E:\Coins\TRI\bin\i2pd' | Out-Null
Copy-Item 'C:\Users\hello\tri7\i2pd\pkg\i2pd.exe' 'E:\Coins\TRI\bin\i2pd\i2pd.exe' -Force
"i2pd.exe: " + (Get-Item 'E:\Coins\TRI\bin\i2pd\i2pd.exe').Length + " bytes"

"--- 5. relaunch the wallet from E: ---"
Start-Process -FilePath 'E:\Coins\TRI\bin\tri7_wallet_gui.exe' -WorkingDirectory 'E:\Coins\TRI\bin'
Start-Sleep -Seconds 20
$g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($g) { "gui RUNNING pid=" + $g[0].Id + " path=" + $g[0].Path } else { "gui NOT RUNNING" }
"E-PROVISION-DONE"
