
$p = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue
if ($p) {
  Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class W { [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h); [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); }' 2>$null
  "gui RUNNING pid=" + $p[0].Id + " start=" + $p[0].StartTime
} else { "gui NOT RUNNING - starting task again"; Start-ScheduledTask -TaskName tri7gui-laptop; Start-Sleep -Seconds 15; $p2 = Get-Process tri7_wallet_gui -ErrorAction SilentlyContinue; if ($p2) { "now RUNNING pid=" + $p2[0].Id } else { "STILL NOT RUNNING" } }
Get-Content C:\Coins\TRI-LAPTOP\node\state\node.log -Tail 4 -ErrorAction SilentlyContinue
