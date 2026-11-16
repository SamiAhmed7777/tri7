$c = New-Object Net.Sockets.TcpClient('127.0.0.1', 9051)
$s = $c.GetStream()
$w = New-Object IO.StreamWriter($s); $w.AutoFlush = $true
$r = New-Object IO.StreamReader($s)
$w.WriteLine("AUTHENTICATE")
Start-Sleep -Milliseconds 600
$auth = ""
while ($s.DataAvailable) { $auth += $r.ReadLine() + "`n"; Start-Sleep -Milliseconds 80 }
$w.WriteLine('SETCONF Log="info file C:/Users/hello/tri7/tor/torinfo.log"')
Start-Sleep -Milliseconds 1200
$set = ""
while ($s.DataAvailable) { $set += $r.ReadLine() + "`n"; Start-Sleep -Milliseconds 80 }
try { $w.WriteLine("QUIT") } catch {}
Write-Output ("AUTH: " + $auth)
Write-Output ("SETCONF: " + $set)
$c.Close()
Write-Output "DONE"
