
$ft = Get-Process follow_tip -ErrorAction SilentlyContinue
if ($ft) { "daemon up pid=" + $ft[0].Id + " cpu=" + [math]::Round($ft[0].CPU,1) } else { "DAEMON DOWN" }
$gui = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($gui) { $h = $gui[0].MainWindowHandle; "gui pid=" + $gui[0].Id + " MainWindowHandle=" + $h }
