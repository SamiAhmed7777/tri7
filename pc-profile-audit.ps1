
"--- gui processes now:"
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | ForEach-Object { "pid=" + $_.Id + " path=" + $_.Path + " start=" + $_.StartTime }
"--- profile dir:"
Get-ChildItem 'C:\Users\hello\AppData\Local\Tri7Wallet' -Recurse -ErrorAction SilentlyContinue | Select-Object -First 12 | ForEach-Object { $_.FullName.Replace('C:\Users\hello\AppData\Local\Tri7Wallet','~profile') + "  " + (@{isDir='/';file=$_.Length})[$_.PSIsContainer] }
"--- profile node.log tail:"
$nl = 'C:\Users\hello\AppData\Local\Tri7Wallet\profile\node.log'
if (Test-Path $nl) { Get-Content $nl -Tail 16 } else { Get-ChildItem 'C:\Users\hello\AppData\Local\Tri7Wallet' -Recurse -Filter 'node.log' -ErrorAction SilentlyContinue | ForEach-Object { "found: " + $_.FullName; Get-Content $_.FullName -Tail 12 } }
