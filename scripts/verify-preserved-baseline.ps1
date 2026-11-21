[CmdletBinding()]
param(
  [string]$CandidateRoot,
  [string]$ManifestPath,
  [string]$ReferenceExe
)
$ErrorActionPreference='Stop'
$repositoryRoot=Split-Path -Parent $PSScriptRoot
if([string]::IsNullOrWhiteSpace($CandidateRoot)){$CandidateRoot=$repositoryRoot}
if([string]::IsNullOrWhiteSpace($ManifestPath)){
  $ManifestPath=Join-Path $repositoryRoot 'docs\preservation-baseline.json'
}
$issues=New-Object System.Collections.Generic.List[string]
function Fail([string]$reason){$script:issues.Add($reason)}
if(!(Test-Path -LiteralPath $ManifestPath)){Write-Output 'PRESERVATION=FAIL manifest missing';exit 2}
try{$m=Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json}
catch{Write-Output 'PRESERVATION=FAIL invalid manifest';exit 2}
if($m.schema_version -ne 1){Fail 'invalid schema'}
if($m.baseline.status -ne 'VERIFIED'){Fail 'baseline source is UNVERIFIED'}
if($m.baseline.source_provenance -ne 'REPRODUCED_OR_TRACEABLE_BUILD'){Fail 'source-to-EXE provenance not established'}
if([string]::IsNullOrWhiteSpace([string]$m.baseline.acceptance_evidence)){Fail 'no baseline acceptance evidence'}
$commit=[string]$m.baseline.source_commit
if($commit -notmatch '^[a-fA-F0-9]{40}$'){Fail 'no verified baseline Git commit'}
$expected=[string]$m.baseline.exe_sha256
if($expected -notmatch '^[a-fA-F0-9]{64}$'){Fail 'invalid baseline EXE SHA256'}
if($ReferenceExe){
  if(!(Test-Path -LiteralPath $ReferenceExe -PathType Leaf)){Fail 'reference EXE missing'}
  elseif($expected -match '^[a-fA-F0-9]{64}$'){
    $actual=(Get-FileHash -LiteralPath $ReferenceExe -Algorithm SHA256).Hash
    if($actual -ne $expected){Fail 'reference EXE SHA256 differs from accepted baseline'}
  }
}
$mandatory=@('muse-display-selection','muse-runtime-display','muse-ipc-read','codex-quota','codex-auth-recovery','copy-current-and-filtered')
$features=@($m.required_features)
$ids=@($features | ForEach-Object {$_.id})
foreach($id in $mandatory){if($ids -notcontains $id){Fail "missing required capability: $id"}}
if(@($ids | Select-Object -Unique).Count -ne $ids.Count){Fail 'duplicate capability IDs'}
foreach($f in $features){
  if([string]::IsNullOrWhiteSpace([string]$f.acceptance_test)){Fail "no acceptance test: $($f.id)"}
}
try {
  $head=(& git -C $CandidateRoot rev-parse HEAD 2>$null).Trim()
  if($LASTEXITCODE -ne 0){throw 'git HEAD not found'}
  $dirty=(& git -C $CandidateRoot status --porcelain --untracked-files=normal 2>$null)
  if($LASTEXITCODE -ne 0){throw 'git status failed'}
  if($dirty){Fail 'candidate source has uncommitted/untracked changes'}
  if($commit -match '^[a-fA-F0-9]{40}$'){
    & git -C $CandidateRoot cat-file -e ($commit + '^{commit}') 2>$null
    if($LASTEXITCODE -ne 0){Fail 'accepted baseline commit is absent'}
    else {
      & git -C $CandidateRoot merge-base --is-ancestor $commit $head 2>$null
      if($LASTEXITCODE -ne 0){Fail 'candidate does not descend from verified baseline'}
    }
  }
} catch {Fail "Git verification failed"}
if($issues.Count -eq 0){
  foreach($f in $features){
    $test=[string]$f.acceptance_test
    if($test -notmatch '^[A-Za-z0-9_:]+$'){Fail "invalid test selector for $($f.id)";continue}
    $output=(& cargo test --manifest-path (Join-Path $CandidateRoot 'Cargo.toml') --locked --no-default-features $test -- --exact 2>&1) -join [Environment]::NewLine
    if($LASTEXITCODE -ne 0 -or $output -notmatch '(?m)test result: ok\.\s+1 passed;\s+0 failed'){
      Fail "required acceptance test absent or failed: $test"
    }
  }
}
if($issues.Count -gt 0){
  Write-Output 'PRESERVATION=FAIL - candidate cannot replace accepted QA or be released'
  foreach($reason in $issues){Write-Output (' - '+$reason)}
  exit 2
}
Write-Output "PRESERVATION=PASS - $head"
Write-Output 'This is a necessary code gate, not proof of real GUI equivalence.'
exit 0