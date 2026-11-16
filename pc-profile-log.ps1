
$st = 'C:\Users\hello\AppData\Local\Tri7Wallet\node\state'
"--- profile node state dir:"
Get-ChildItem $st -ErrorAction SilentlyContinue | ForEach-Object { $_.Name + "  " + $_.Length + "  " + $_.LastWriteTime }
"--- node.log tail:"
Get-Content (Join-Path $st 'node.log') -Tail 20 -ErrorAction SilentlyContinue
"--- options.json:"
Get-Content 'C:\Users\hello\AppData\Local\Tri7Wallet\profile\options.json' -ErrorAction SilentlyContinue
"--- last_wallet.txt:"
Get-Content 'C:\Users\hello\AppData\Local\Tri7Wallet\profile\last_wallet.txt' -ErrorAction SilentlyContinue
