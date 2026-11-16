
"--- running gui process path:"
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | ForEach-Object { "pid=" + $_.Id + " path=" + $_.Path }
"--- E: bin contents (top level):"
Get-ChildItem 'E:\Coins\TRI\bin' -ErrorAction SilentlyContinue | Select-Object -First 15 | ForEach-Object { $_.Name + "  " + $_.Length }
"--- E: gui hash:"
if (Test-Path 'E:\Coins\TRI\bin\tri7_wallet_gui.exe') { (Get-FileHash 'E:\Coins\TRI\bin\tri7_wallet_gui.exe' -Algorithm SHA256).Hash.Substring(0,16) } else { "missing" }
"--- C: gui hash:"
(Get-FileHash 'C:\Users\hello\tri7\tri7_wallet_gui.exe' -Algorithm SHA256).Hash.Substring(0,16)
"--- C: follow_tip hash:"
(Get-FileHash 'C:\Users\hello\tri7\follow_tip.exe' -Algorithm SHA256).Hash.Substring(0,16)
"--- i2pd available at C::"
(Test-Path 'C:\Users\hello\tri7\i2pd\pkg\i2pd.exe')
