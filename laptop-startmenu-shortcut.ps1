$ws = New-Object -ComObject WScript.Shell
$dir = 'C:\Users\Sami\AppData\Roaming\Microsoft\Windows\Start Menu\Programs'
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
$lnk = Join-Path $dir 'Triangles Wallet.lnk'
$sc = $ws.CreateShortcut($lnk)
$sc.TargetPath = 'C:\Coins\TRI-LAPTOP\tri7_wallet_gui.exe'
$sc.Arguments = '--wallet C:\Coins\TRI-LAPTOP\wallet.dat --rpc 127.0.0.1:19114'
$sc.WorkingDirectory = 'C:\Coins\TRI-LAPTOP'
$sc.IconLocation = 'C:\Coins\TRI-LAPTOP\tri7_wallet_gui.exe,0'
$sc.Description = 'Triangles Wallet (tri7)'
$sc.Save()
if (Test-Path $lnk) {
  Write-Output ('created ' + $lnk)
  Write-Output ('size ' + (Get-Item $lnk).Length)
} else {
  Write-Output 'FAILED to create shortcut'
}
