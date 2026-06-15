$file = "C:\Users\abora\AppData\Local\Packages\Microsoft.IntelligentTerminal_8wekyb3d8bbwe\LocalCache\Local\IntelligentTerminal\hook-bundle-staging\claude\wt-agent-hooks\hooks\send-event.ps1"
$size = (Get-Item $file -ErrorAction SilentlyContinue).Length
Write-Host "File size: $size bytes"
if ($size -lt 100) {
    Write-Host "File is empty or missing -- cannot patch, need to restore first"
    exit 1
}
$content = Get-Content $file -Raw
if ($content -match 'UseShellExecute = \$false' -and $content -match 'CreateNoWindow') {
    Write-Host "Already patched"
    exit 0
}
$content = $content -replace '\$psi\.UseShellExecute = \$true', '$psi.UseShellExecute = $false'
$content = $content -replace "\`$psi\.WindowStyle = 'Hidden'", '$psi.CreateNoWindow = $true'
[System.IO.File]::WriteAllText($file, $content, [System.Text.Encoding]::UTF8)
Write-Host "Patched OK"
