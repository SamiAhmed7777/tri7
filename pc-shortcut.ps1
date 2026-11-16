$ErrorActionPreference = 'Stop'
$desktop = [Environment]::GetFolderPath('Desktop')
$lnk = Join-Path $desktop 'Triangles Wallet.lnk'
$ws = New-Object -ComObject WScript.Shell
$s = $ws.CreateShortcut($lnk)
$s.TargetPath = 'E:\Coins\TRI\bin\tri7_wallet_gui.exe'
$s.WorkingDirectory = 'E:\Coins\TRI\bin'
$s.IconLocation = 'E:\Coins\TRI\bin\tri7_wallet_gui.exe,0'
$s.Description = 'Triangles wallet (Tor + I2P)'
$s.Save()
"created: $lnk"
Test-Path $lnk
