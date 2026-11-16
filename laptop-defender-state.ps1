
"--- exclusions NOW ---"
"paths: " + ((Get-MpPreference).ExclusionPath -join '; ')
"procs: " + ((Get-MpPreference).ExclusionProcess -join '; ')
"tamper: " + (Get-MpComputerStatus).TamperProtectionSource
"--- newest 2 detections ---"
Get-MpThreatDetection -ErrorAction SilentlyContinue | Sort-Object InitialDetectionTime -Descending | Select-Object -First 2 | ForEach-Object { $_.InitialDetectionTime.ToString() + " -> " + ($_.Resources -join ",") }
"--- threat names ---"
Get-MpThreat -ErrorAction SilentlyContinue | Select-Object -First 3 | ForEach-Object { $_.ThreatName + " id=" + $_.ThreatID }
