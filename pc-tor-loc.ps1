
"--- C: tor bundle location:"
foreach ($p in 'C:\Users\hello\tri7\tor\tor.exe','C:\Users\hello\tri7\tor.exe') {
  if (Test-Path $p) { "found: $p (" + (Get-Item $p).Length + " bytes)" }
}
Get-ChildItem 'C:\Users\hello\tri7\tor' -ErrorAction SilentlyContinue | Select-Object -First 8 | ForEach-Object { $_.Name }
