$s = 'C:\Users\hello\tri7\state2\'
foreach ($p in @('blk0001.dat','tipstate.json','balances.json','wallet_utxos.json')) {
  if (Test-Path ($s + $p)) {
    Rename-Item ($s + $p) ($p + '.fork-2209593') -Force
    Write-Output ('renamed ' + $p)
  } else {
    Write-Output ('absent  ' + $p)
  }
}
if (Test-Path ($s + 'index')) {
  Rename-Item ($s + 'index') 'index.fork-2209593' -Force
  Write-Output 'renamed index'
} else {
  Write-Output 'absent  index'
}
