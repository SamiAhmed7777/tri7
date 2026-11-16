
"--- AVG verdicts around the E: launch:"
Get-Content 'C:\ProgramData\AVG\Antivirus\log\autosandbox.log' -Tail 300000 -ErrorAction SilentlyContinue | Select-String -SimpleMatch 'E:\Coins\TRI\bin' -Context 0,4 | Select-Object -Last 12 | ForEach-Object { $_.Line.Substring(0,[Math]::Min(200,$_.Line.Length)); if ($_.Context.PostContext) { $_.Context.PostContext | ForEach-Object { "    >> " + $_.Substring(0,[Math]::Min(200,$_.Length)) } } }
