
$gui = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($gui) { "gui pid=" + $gui[0].Id + " MainWindowHandle=" + $gui[0].MainWindowHandle } else { "gui DOWN" }
