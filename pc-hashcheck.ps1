
Set-Location C:\Users\hello\tri7
$fh = (Get-FileHash .\follow_tip.exe -Algorithm SHA256).Hash
$gh = (Get-FileHash .\tri7_wallet_gui.exe -Algorithm SHA256).Hash
"follow_tip on disk: " + $fh.Substring(0,16)
"gui on disk:        " + $gh.Substring(0,16)
$ft = Get-Process follow_tip -ErrorAction SilentlyContinue
if ($ft) { "daemon RUNNING pid=" + $ft[0].Id + " start=" + $ft[0].StartTime } else { "daemon NOT RUNNING" }
$g = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($g) { "gui RUNNING pid=" + $g[0].Id } else { "gui NOT RUNNING" }
