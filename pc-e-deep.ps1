
"--- E: gui hash now (was F72EABA3...):"
if (Test-Path 'E:\Coins\TRI\bin\tri7_wallet_gui.exe') { (Get-FileHash 'E:\Coins\TRI\bin\tri7_wallet_gui.exe' -Algorithm SHA256).Hash.Substring(0,16) + "  mtime=" + (Get-Item 'E:\Coins\TRI\bin\tri7_wallet_gui.exe').LastWriteTime }
"--- update dir:"
Get-ChildItem 'C:\Users\hello\AppData\Local\Tri7Wallet\profile\update' -Recurse -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName + "  " + $_.Length }
"--- any logs under profile:"
Get-ChildItem 'C:\Users\hello\AppData\Local\Tri7Wallet' -Recurse -Include '*.log','*.txt' -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName + "  " + $_.Length + "  " + $_.LastWriteTime }
"--- wallet.dat at E:"
if (Test-Path 'E:\Coins\TRI\wallet.dat') { "exists " + (Get-Item 'E:\Coins\TRI\wallet.dat').Length + " bytes  mtime=" + (Get-Item 'E:\Coins\TRI\wallet.dat').LastWriteTime } else { "MISSING" }
