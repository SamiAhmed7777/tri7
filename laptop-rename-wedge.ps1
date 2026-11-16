$s = 'C:\Coins\TRI-LAPTOP\node\state\'
foreach ($p in @('balances.json','wallet_utxos.json','gap_recovery.json')) {
  if (Test-Path ($s + $p)) {
    Rename-Item ($s + $p) ($p + '.wedge-h64') -Force
    Write-Output ('renamed ' + $p)
  } else {
    Write-Output ('absent  ' + $p)
  }
}
if (Test-Path ($s + 'index')) {
  Rename-Item ($s + 'index') 'index.wedge-h64' -Force
  Write-Output 'renamed index'
} else {
  Write-Output 'absent  index'
}
