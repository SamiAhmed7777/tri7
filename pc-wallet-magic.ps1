
$s = [System.IO.File]::OpenRead('E:\Coins\TRI\wallet.dat')
$buf = New-Object byte[] 24
[void]$s.Read($buf, 0, 24)
$s.Close()
"first 24 bytes (hex): " + (($buf | ForEach-Object { $_.ToString('X2') }) -join ' ')
"as text: " + [System.Text.Encoding]::ASCII.GetString($buf)
