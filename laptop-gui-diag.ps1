
schtasks /query /tn tri7gui-laptop /v /fo list | Select-String -Pattern "Last Result|Last Run|Task To Run|Status"
"--- direct run ---"
$p = Start-Process -FilePath C:\Coins\TRI-LAPTOP\tri7_wallet_gui.exe -ArgumentList '--wallet','C:\Coins\TRI-LAPTOP\wallet.dat','--rpc','127.0.0.1:19114' -WorkingDirectory C:\Coins\TRI-LAPTOP -PassThru -RedirectStandardError C:\Users\Sami\gui-stderr.txt -RedirectStandardOutput C:\Users\Sami\gui-stdout.txt
Start-Sleep -Seconds 8
if ($p.HasExited) { "exited code=" + $p.ExitCode } else { "still running pid=" + $p.Id }
"--- stderr ---"
Get-Content C:\Users\Sami\gui-stderr.txt -ErrorAction SilentlyContinue | Select-Object -First 6
"--- stdout ---"
Get-Content C:\Users\Sami\gui-stdout.txt -ErrorAction SilentlyContinue | Select-Object -First 6
