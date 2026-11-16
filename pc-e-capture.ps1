
$p = Start-Process -FilePath 'E:\Coins\TRI\bin\tri7_wallet_gui.exe' -WorkingDirectory 'E:\Coins\TRI\bin' -WindowStyle Hidden -RedirectStandardOutput 'E:\Coins\TRI\bin\gui-out.txt' -RedirectStandardError 'E:\Coins\TRI\bin\gui-err.txt' -PassThru
Start-Sleep -Seconds 30
if ($p.HasExited) { "exited code=" + $p.ExitCode } else { "alive pid=" + $p.Id }
"--- stdout ---"
Get-Content 'E:\Coins\TRI\bin\gui-out.txt' -Tail 12 -ErrorAction SilentlyContinue
"--- stderr ---"
Get-Content 'E:\Coins\TRI\bin\gui-err.txt' -Tail 12 -ErrorAction SilentlyContinue
