<#
.SYNOPSIS
Runs MusicScanSort on the samples, each into its next out-N folder, and reports run times.

.DESCRIPTION
Uses the release build (build it first with tools\eval\build.cmd build --release). The
executable is copied to %TEMP% first, so the project can be rebuilt while a run is going.
Each output folder gets run.log (the verbose log, read by duplicates.py and page_table.py)
and summary.txt (the program's summary).

.EXAMPLE
.\tools\eval\run.ps1
.\tools\eval\run.ps1 'Piece A', 'Piece B'
#>
param(
    [string[]]$Samples,
    [int]$Dpi = 150,
    [int]$Workers = 4
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$sampleRoot = Join-Path $root 'sample-files'
$built = Join-Path $root 'target\release\musicscansort.exe'
if (-not (Test-Path $built)) { throw "No release build at $built" }
$exe = Join-Path $env:TEMP 'musicscansort-eval.exe'
Copy-Item $built $exe -Force
if (-not $Samples) { $Samples = (Get-ChildItem $sampleRoot -Directory).Name }

$results = @()
foreach ($name in $Samples) {
    $dir = Join-Path $sampleRoot $name
    # Same rule as the program: one above the highest existing out-N.
    $last = Get-ChildItem $dir -Directory -Filter 'out-*' |
        Where-Object { $_.Name.Substring(4) -match '^\d+$' } |
        ForEach-Object { [int]$_.Name.Substring(4) } |
        Measure-Object -Maximum
    $out = Join-Path $dir ("out-{0}" -f ([int]$last.Maximum + 1))
    $log = Join-Path $env:TEMP 'musicscansort-eval.log'
    $summary = Join-Path $env:TEMP 'musicscansort-eval.txt'
    Write-Host "$name -> $out"
    $arguments = "--dir `"$dir`" --title `"$name`" --out `"$out`" --dpi $Dpi --workers $Workers --verbose --no-gui"
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process $exe -ArgumentList $arguments -NoNewWindow -Wait -PassThru `
        -RedirectStandardError $log -RedirectStandardOutput $summary
    $clock.Stop()
    if (Test-Path $out) {
        Move-Item $log (Join-Path $out 'run.log') -Force
        Move-Item $summary (Join-Path $out 'summary.txt') -Force
    }
    $pages = if (Test-Path (Join-Path $out 'summary.txt')) {
        $line = Select-String -Path (Join-Path $out 'summary.txt') -Pattern 'Input:\s+(\d+) pages' | Select-Object -First 1
        if ($line) { [int]$line.Matches[0].Groups[1].Value }
    }
    $seconds = $clock.Elapsed.TotalSeconds
    $results += [pscustomobject]@{
        Sample   = $name
        Output   = Split-Path $out -Leaf
        Exit     = $process.ExitCode
        Pages    = $pages
        Time     = $clock.Elapsed.ToString('m\:ss')
        PerPage  = if ($pages) { '{0:N1} s' -f ($seconds / $pages) } else { '' }
    }
}
$results | Format-Table -AutoSize
