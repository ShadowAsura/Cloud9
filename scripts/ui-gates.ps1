# UI gates for cloud9-app: every piece checked on every run.
# Gate 1: compiles. Gate 2: unit tests (incl. gallery layout invariants).
# Gate 3: every screen renders (launched, window appears, capture is real
# pixels, correct size, not blank). Any failure aborts with a message.
param()
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Logan\Downloads\Cloud9'
Set-Location $root

function Fail($msg) { Write-Output "GATE FAILED: $msg"; exit 1 }

Write-Output '== gate 1: cargo check =='
cargo check -p cloud9-app; if ($LASTEXITCODE -ne 0) { Fail 'cargo check' }

Write-Output '== gate 1b: fmt + clippy (mirrors CI verify.yml) =='
cargo fmt --all --check; if ($LASTEXITCODE -ne 0) { Fail 'cargo fmt --check' }
cargo clippy --locked --workspace --all-targets -- -D warnings; if ($LASTEXITCODE -ne 0) { Fail 'cargo clippy -D warnings' }

Write-Output '== gate 2: cargo test =='
cargo test -p cloud9-app; if ($LASTEXITCODE -ne 0) { Fail 'cargo test' }

Write-Output '== gate 3: build =='
Stop-Process -Name cloud9-app -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
cargo build -p cloud9-app; if ($LASTEXITCODE -ne 0) { Fail 'cargo build' }

Add-Type -AssemblyName System.Drawing
function Test-Shot($path, $w, $h) {
  if (-not (Test-Path $path)) { Fail "missing $path" }
  $bmp = [System.Drawing.Bitmap]::FromFile($path)
  try {
    $bw = $bmp.Width; $bh = $bmp.Height
    if ($bw -ne $w -or $bh -ne $h) {
      Fail "$path size ${bw}x${bh}, expected ${w}x${h}"
    }
    $seen = @{}
    for ($i = 0; $i -lt 100; $i++) {
      $x = ($i * 37 + 11) % $bw; $y = ($i * 53 + 7) % $bh
      $seen[$bmp.GetPixel($x, $y).ToArgb()] = $true
    }
    if ($seen.Count -lt 10) { Fail "$path looks blank ($($seen.Count) distinct samples)" }
  } finally { $bmp.Dispose() }
  Write-Output ("ok " + $path + " " + $bw + "x" + $bh + " " + $seen.Count + " colors")
}

Write-Output '== gate 4: every screen renders =='
$exe = "$root\target\debug\cloud9-app.exe"
$shots = @(
  @{name='episodes.png'; screen=''; dialog=''; select=''},
  @{name='instances.png'; screen='instances'; dialog=''; select=''},
  @{name='running.png'; screen='running'; dialog=''; select=''},
  @{name='servers.png'; screen='servers'; dialog=''; select=''},
  @{name='episode-setup-selected.png'; screen=''; dialog='1'; select='1'}
)
foreach ($s in $shots) {
  Stop-Process -Name cloud9-app -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 800
  if ($s.screen -ne '') { $env:CLOUD9_SCREEN = $s.screen } else { Remove-Item Env:\CLOUD9_SCREEN -ErrorAction SilentlyContinue }
  if ($s.dialog -ne '') { $env:CLOUD9_DIALOG = '1' } else { Remove-Item Env:\CLOUD9_DIALOG -ErrorAction SilentlyContinue }
  if ($s.select -ne '') { $env:CLOUD9_SELECT_FIRST = '1' } else { Remove-Item Env:\CLOUD9_SELECT_FIRST -ErrorAction SilentlyContinue }
  $p = Start-Process $exe -PassThru
  $ready = $false
  for ($i = 0; $i -lt 14; $i++) {
    Start-Sleep -Milliseconds 500
    $c = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
    if ($c -and $c.MainWindowHandle -ne 0) { $ready = $true; break }
  }
  if (-not $ready) { Fail "no window for $($s.name)" }
  Start-Sleep -Seconds 2
  $cap = powershell -File "$root\screenshots\capture_pid.ps1" -OutFile ("$root\screenshots\" + $s.name) -ProcId $p.Id | Out-String
  Stop-Process -Name cloud9-app -Force -ErrorAction SilentlyContinue
  if ($cap -notmatch '(\d+)x(\d+)') { Fail "capture reported no size for $($s.name): $cap" }
  Test-Shot ("$root\screenshots\" + $s.name) $Matches[1] $Matches[2]
}
Remove-Item Env:\CLOUD9_SCREEN -ErrorAction SilentlyContinue
Remove-Item Env:\CLOUD9_DIALOG -ErrorAction SilentlyContinue
Remove-Item Env:\CLOUD9_SELECT_FIRST -ErrorAction SilentlyContinue
Write-Output 'ALL UI GATES PASSED'
