
$ft = Get-Process follow_tip -ErrorAction SilentlyContinue
if ($ft) { "follow_tip RUNNING pid=" + $ft[0].Id + " start=" + $ft[0].StartTime } else { "follow_tip NOT RUNNING" }
