# Negative self-tests for accepted-QA preservation gate.
# Creates only temporary manifests and never touches a running EXE or settings.
$ErrorActionPreference = 'Stop'
$gate = Join-Path $PSScriptRoot 'verify-preserved-baseline.ps1'
$source = Join-Path $PSScriptRoot '..\docs\preservation-baseline.json'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$temp = Join-Path ([IO.Path]::GetTempPath()) ("aum-preservation-smoke-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp -Force | Out-Null
try {
  $baseline = Get-Content -LiteralPath $source -Raw -Encoding UTF8 | ConvertFrom-Json
  function Assert-Rejected([string]$Name, [string]$ManifestPath, [string]$Expected) {
    $lines = @(& $gate -CandidateRoot $root -ManifestPath $ManifestPath 2>&1)
    $code = $LASTEXITCODE
    $output = $lines -join [Environment]::NewLine
    if ($code -ne 2 -or !$output.Contains($Expected)) {
      throw "FAIL $Name: expected exit 2 and text '$Expected'; got exit $code; output: $output"
    }
    Write-Output "PASS negative case: $Name (exit 2)"
  }
  Assert-Rejected 'missing manifest' (Join-Path $temp 'absent.json') 'PRESERVATION=FAIL manifest missing'

  $invalid = Join-Path $temp 'invalid.json'
  Set-Content -LiteralPath $invalid -Value '{"broken":' -Encoding utf8
  Assert-Rejected 'invalid JSON' $invalid 'PRESERVATION=FAIL invalid manifest'

  $unverified = Join-Path $temp 'unverified.json'
  $m = $baseline | ConvertTo-Json -Depth 24 | ConvertFrom-Json
  $m.baseline.status = 'UNVERIFIED'
  $m.baseline.source_provenance = 'UNKNOWN'
  $m.baseline.acceptance_evidence = $null
  $m | ConvertTo-Json -Depth 24 | Set-Content -LiteralPath $unverified -Encoding utf8
  Assert-Rejected 'unknown accepted source' $unverified 'baseline source is UNVERIFIED'

  $noTests = Join-Path $temp 'missing-tests.json'
  $m.baseline.status = 'VERIFIED'
  $m.baseline.source_provenance = 'REPRODUCED_OR_TRACEABLE_BUILD'
  $m.baseline.acceptance_evidence = 'fixture only; never promote'
  foreach ($feature in $m.required_features) { $feature.acceptance_test = $null }
  $m | ConvertTo-Json -Depth 24 | Set-Content -LiteralPath $noTests -Encoding utf8
  Assert-Rejected 'required features lack acceptance tests' $noTests 'no acceptance test:'
  Write-Output 'PRESERVATION_NEGATIVE_TESTS=PASS'
}
finally {
  Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
}
