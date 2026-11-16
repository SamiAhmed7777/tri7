$c = New-Object Net.Sockets.TcpClient('127.0.0.1', 9051)
$s = $c.GetStream()
$w = New-Object IO.StreamWriter($s); $w.AutoFlush = $true
$r = New-Object IO.StreamReader($s)
$w.WriteLine("AUTHENTICATE")
Start-Sleep -Milliseconds 600
$auth = ""
while ($s.DataAvailable) { $auth += $r.ReadLine() + "`n"; Start-Sleep -Milliseconds 80 }
$w.WriteLine("GETINFO circuit-status")
Start-Sleep -Seconds 3
$cs = ""
while ($s.DataAvailable) { $cs += $r.ReadLine() + "`n"; Start-Sleep -Milliseconds 120 }
$w.WriteLine("GETINFO status/bootstrap-phase")
Start-Sleep -Milliseconds 800
$bs = ""
while ($s.DataAvailable) { $bs += $r.ReadLine() + "`n"; Start-Sleep -Milliseconds 80 }
try { $w.WriteLine("QUIT") } catch {}
Write-Output ("AUTH: " + (($auth -replace "`n", " | ") -replace "`r", ""))
Write-Output ("BOOTSTRAP: " + (($bs -replace "`n", " ") -replace "`r", ""))
$all = ($cs -replace "`r", "") -split "`n" | Where-Object { $_ -match "PURPOSE=" -and $_ -notmatch "^250 circuit-status OK" }
Write-Output ("CIRCUIT-LINES: " + $all.Count)
$hs = $all | Where-Object { $_ -match "HS_V3_REND|HS_V3_INTRO|HS_SERVICE|HS_CLIENT|REND|INTRO" }
Write-Output ("HS-CIRCUITS: " + @($hs).Count)
@($hs) | Select-Object -First 12
Write-Output "SAMPLE-OTHERS:"
$all | Select-Object -First 4
$c.Close()
Write-Output "DONE"
