
"--- E: legacy copy:"
if (Test-Path 'E:\Coins\TRI\bin\tri7_wallet_gui.exe') {
  $e = Get-Item 'E:\Coins\TRI\bin\tri7_wallet_gui.exe'
  "exists, size=" + $e.Length + " mtime=" + $e.LastWriteTime
  "sha256: " + (Get-FileHash $e.FullName -Algorithm SHA256).Hash.Substring(0,16)
} else { "E:\Coins\TRI\bin\tri7_wallet_gui.exe NOT FOUND" }
"--- C: current copy:"
$c = Get-Item 'C:\Users\hello\tri7\tri7_wallet_gui.exe' -ErrorAction SilentlyContinue
if ($c) { "sha256: " + (Get-FileHash $c.FullName -Algorithm SHA256).Hash.Substring(0,16) + " mtime=" + $c.LastWriteTime }
"--- ALL gui copies on this box:"
Get-ChildItem 'E:\','C:\Users\hello' -Recurse -Filter 'tri7_wallet_gui.exe' -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName + "  " + $_.Length + "  " + $_.LastWriteTime }
"--- running gui processes:"
Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue | ForEach-Object { "pid=" + $_.Id + " path=" + $_.Path }
"--- torrc files near E::"
Get-ChildItem 'E:\Coins\TRI' -Recurse -Include 'torrc*','*.torrc' -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }
