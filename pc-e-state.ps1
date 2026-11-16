
$g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($g) { $g | ForEach-Object { "gui pid=" + $_.Id + " path=" + $_.Path } } else { "gui NOT RUNNING" }
"--- E: tree:"
Get-ChildItem 'E:\Coins\TRI\bin' -ErrorAction SilentlyContinue | ForEach-Object { $_.Name + (@{isDir='/'})[$_.PSIsContainer] }
"--- node dir:"
Get-ChildItem 'E:\Coins\TRI\bin\node' -Recurse -ErrorAction SilentlyContinue | Select-Object -First 14 | ForEach-Object { $_.FullName.Replace('E:\Coins\TRI\bin\','') + "  " + (@{isDir='/'})[$_.PSIsContainer] }
"--- node.log lines (tor/i2p/seeds):"
$nl = 'E:\Coins\TRI\bin\node\state\node.log'
if (Test-Path $nl) { Get-Content $nl -Tail 14 } else { "no node.log yet" }
