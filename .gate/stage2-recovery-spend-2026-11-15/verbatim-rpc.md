## VERBATIM RPC OUTPUTS (re-captured 2026-11-15 12:1x UTC)
### getrawtransaction verbose (confirmed-state)
```json
{"error":null,"id":1,"result":{"blockhash":"fbff5369f862b1f37e9a06191b17dbbaa8353798d524500712f44980e742541f","confirmations":5,"height":2212451,"hex":"010000006048d369010560a738f455fdce20c58c32d1ad40270bf5a85d742ee9d9965f072192e93b35010000004847304402206067f3edfe9d865d3accdebfd0a3bb617380088907013424a6430a5a0a91cab602201ee01fd5d0e29a705b02880c96192c76a3d88f4ba76cdde6d2a126b467a5ed9a01ffffffff02f9c70500000000001976a9140115680603c55dc5459709e5e81950bc9d6d6b9b88ac64000000000000001976a914e81ec8ed12c30a49ade2cbe2ac8d70c057b0fd8488ac00000000","locktime":0,"time":1794744458,"tx_time":1775454304,"txid":"3f16c2167bd74919f7859c4f6acc2bc9dd52c21c167ced73ef4e3ef251fbc124","version":1,"vin":[{"scriptSig":{"hex":"47304402206067f3edfe9d865d3accdebfd0a3bb617380088907013424a6430a5a0a91cab602201ee01fd5d0e29a705b02880c96192c76a3d88f4ba76cdde6d2a126b467a5ed9a01"},"sequence":4294967295,"txid":"353be99221075f96d9e92e745da8f50b2740add1328cc520cefd55f438a76005","vout":1}],"vout":[{"n":0,"scriptPubKey":{"addresses":["TA4wKb9SJ54UwMBjgBroNs3xYLDxSw9H8A"],"asm":"OP_DUP OP_HASH160 0115680603c55dc5459709e5e81950bc9d6d6b9b OP_EQUALVERIFY OP_CHECKSIG","reqSigs":1,"script_hex":"76a9140115680603c55dc5459709e5e81950bc9d6d6b9b88ac","type":"pubkeyhash"},"value":0.378873,"value_sats":378873},{"n":1,"scriptPubKey":{"addresses":["TX8YeXumu1VvDRvDX8BLWTQB9AT65mumfw"],"asm":"OP_DUP OP_HASH160 e81ec8ed12c30a49ade2cbe2ac8d70c057b0fd84 OP_EQUALVERIFY OP_CHECKSIG","reqSigs":1,"script_hex":"76a914e81ec8ed12c30a49ade2cbe2ac8d70c057b0fd8488ac","type":"pubkeyhash"},"value":0.0001,"value_sats":100}]},"source":"tri7"}
```
### getaddresstxids TA4wKb9SJ54UwMBjgBroNs3xYLDxSw9H8A
```json
{"error":null,"id":1,"result":["3f16c2167bd74919f7859c4f6acc2bc9dd52c21c167ced73ef4e3ef251fbc124"],"source":"tri7"}
```
### getaddressbalance TA4wKb9SJ54UwMBjgBroNs3xYLDxSw9H8A
```json
{"error":null,"id":1,"result":{"balance":378873,"chain_tip":2212455,"index_complete":true,"index_height":2212451,"received":378873},"source":"tri7"}
```
### getaddressbalance TX8YeXumu1VvDRvDX8BLWTQB9AT65mumfw
```json
{"error":null,"id":1,"result":{"balance":100,"chain_tip":2212455,"index_complete":true,"index_height":2212451,"received":3528944},"source":"tri7"}
```
### getinfo (tip at capture)
```json
{"error":null,"id":1,"result":{"balance":1905.262478,"balance_coverage_height":2212455,"blocks":2212455,"connections":6,"difficulty":{"proof-of-stake":4.2941611451969826e-7,"proof-of-work":4.6565423739069247e-10,"proof-of-work_note":"next-required-target over the tri7 window (legacy walks the full index for PoW blocks)"},"errors":"","moneysupply":226840.924968,"moneysupply_coverage_height":2207975,"paytxfee":0.0001,"protocolversion":70206,"proxy":"","subversion":"/tri7:7.0.0/","testnet":false,"timeoffset":0,"version":70000,"walletversion":60000},"source":"tri7"}
```