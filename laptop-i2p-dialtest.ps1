
Start-Sleep -Seconds 30
"--- i2p dial test: laptop SOCKS -> DNS2 tri7 server tunnel ---"
& C:\Windows\System32\curl.exe --socks5-hostname 127.0.0.1:4447 --max-time 30 -s -o NUL -w "http=%{http_code} exit-will-follow" http://ztj5lcg4tgcn7lh7wlrgvmfhf42itoetwgh42doogu7w3gdhxtbq.b32.i2p:24113/
"curl_exit=$LASTEXITCODE  (3=ok-HTTP, 52/56=connected-then-closed-on-garbage = REACHABLE, 7/28=no path)"
"--- process still healthy ---"
$p = Get-Process i2pd -ErrorAction SilentlyContinue
if ($p) { "i2pd pid=" + $p[0].Id + " cpu=" + [math]::Round($p[0].CPU,2) } else { "i2pd DIED" }
