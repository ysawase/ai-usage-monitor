param()

$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
$devExe = [IO.Path]::GetFullPath(
    (Join-Path $repo "target\debug\claude-code-usage-monitor.exe")
)

$devWasRunning = $false
$failedStep = $null

function Invoke-Check {
    param(
        [string]$Name,
        [scriptblock]$Action
    )

    Write-Host ""
    Write-Host "=== $Name ==="
    & $Action

    if($LASTEXITCODE -ne 0){
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

# cargo build cannot replace the debug exe while that exact dev build
# is running. Stop only the repo's own target\debug process, then restore it.
$devProcesses = @(
    Get-Process "claude-code-usage-monitor" -ErrorAction SilentlyContinue |
    Where-Object {
        try {
            $_.Path -and
            ([IO.Path]::GetFullPath($_.Path) -eq $devExe)
        } catch {
            $false
        }
    }
)

if($devProcesses.Count -gt 0){
    $devWasRunning = $true
    $devProcesses | Stop-Process -Force
    Start-Sleep -Milliseconds 300
}

Push-Location $repo

try {
    Invoke-Check "FMT" {
        cargo fmt -- --check
    }

    Invoke-Check "TEST" {
        cargo test
    }

    Invoke-Check "BUILD" {
        cargo build
    }

    Invoke-Check "DIFF_CHECK" {
        git diff --check
    }
}
catch {
    $failedStep = $_.Exception.Message
}
finally {
    Pop-Location

    if($devWasRunning -and (Test-Path $devExe)){
        Start-Process $devExe
    }
}

Write-Host ""
Write-Host "===================="

if($null -eq $failedStep){
    $result="PREFLIGHT=PASS"
    Write-Host $result
    Write-Host "fmt / test / build / diff-check: all passed"
    try{$result | Set-Clipboard}catch{}
    exit 0
}

$result="PREFLIGHT=FAIL`r`n$failedStep"
Write-Host "PREFLIGHT=FAIL"
Write-Host $failedStep
try{$result | Set-Clipboard}catch{}
exit 1