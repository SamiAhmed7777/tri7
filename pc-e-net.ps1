
$g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($g) { "gui pid=" + $g[0].Id + " alive; listening:"; netstat -ano | Select-String ($g[0].Id) | ForEach-Object { "  " + $_.Line.Trim().Substring(0,[Math]::Min(90,$_.Line.Trim().Length)) } }
$st = 'C:\Users\hello\AppData\Local\Tri7Wallet\node\state'
"--- profile state files:"
Get-ChildItem $st -ErrorAction SilentlyContinue | ForEach-Object { $_.Name + "  " + $_.Length + "  " + $_.LastWriteTime.ToString("HH:mm:ss") }
"--- node.log tail:"
Get-Content (Join-Path $st 'node.log') -Tail 14 -ErrorAction SilentlyContinue
