
"--- i2pd.log tail ---"
Get-Content C:\Coins\i2pd\data\i2pd.log -Tail 15 -ErrorAction SilentlyContinue
"--- conf check: socks enabled? ---"
Select-String -Path C:\Coins\i2pd\tunnels.conf -Pattern 'socks|4447|tri7' -ErrorAction SilentlyContinue | Select-Object -First 6 | ForEach-Object { $_.Line }
Select-String -Path C:\Coins\i2pd\i2pd.conf -Pattern 'socks|4447|reseed|floodfill' -ErrorAction SilentlyContinue | Select-Object -First 6 | ForEach-Object { $_.Line }
