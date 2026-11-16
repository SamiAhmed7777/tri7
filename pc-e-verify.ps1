
$g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($g) { "gui alive pid=" + $g[0].Id } else { "gui DIED" }
"--- node state dir:"
Get-ChildItem 'E:\Coins\TRI\bin\node\state' -ErrorAction SilentlyContinue | Select-Object -First 10 | ForEach-Object { $_.Name + "  " + $_.Length }
"--- node.log tail:"
Get-Content 'E:\Coins\TRI\bin\node\state\node.log' -Tail 10 -ErrorAction SilentlyContinue
"--- tor usable line:"
Get-ChildItem 'E:\Coins\TRI\bin' -Recurse -Filter '*.log' -ErrorAction SilentlyContinue | ForEach-Object { Select-String -Path $_.FullName -Pattern 'tor.*usable|NOT usable|i2pd|I2P SERVER|tor: OK' -ErrorAction SilentlyContinue } | Select-Object -Last 6 | ForEach-Object { $_.Line.Substring(0, [Math]::Min(150, $_.Line.Length)) }
