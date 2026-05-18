# ============================================================================
#  hd-scan.ps1
# ----------------------------------------------------------------------------
#  Walk the US FM broadcast band (87.9 - 107.9 MHz, 0.2 MHz steps) and report
#  which channels produce an NRSC-5 (HD Radio) lock at gain=20 from the
#  currently-connected RTL-SDR. Useful for identifying which DFW stations
#  have HD lit up at your antenna's vantage point today and for picking
#  marginal candidates for closed-loop AGC testing.
#
#  Per-channel budget: SecondsPerChannel (default 6 s) of streamed cu8 piped
#  through nrsc5 with -l 1. After each channel we parse nrsc5 stderr for
#  Synchronized / MER / BER / Station name lines, append a row to the
#  in-memory result table, and stream the row to a CSV on disk so a crash
#  partway through doesn't lose progress.
#
#  Output:
#    target\hd-scan.csv     - tabular results (one row per channel)
#    target\hd-scan.log     - this script's progress trace
#    target\hd-scan-raw\    - per-channel nrsc5 stderr (for forensics)
#
#  Tunables (override on the command line):
#    -StartMhz 87.9         - first channel to scan
#    -StopMhz  107.9        - last channel
#    -StepMhz   0.2         - channel spacing (US FM is on odd-tenths only)
#    -Gain     20           - dB; mid-table on the R820T's 29-step ladder
#    -SecondsPerChannel 6   - capture window per frequency
# ============================================================================
[CmdletBinding()]
param(
    [double] $StartMhz          = 87.9,
    [double] $StopMhz           = 107.9,
    [double] $StepMhz           = 0.2,
    [double] $Gain              = 20,
    [int]    $SecondsPerChannel = 6
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
if (-not $root) { $root = (Get-Location).Path }
Set-Location $root

# llvm-mingw runtime DLLs for iq_capture.exe
$tc = Join-Path $root '.toolchains\llvm-mingw-20260505-ucrt-x86_64\bin'
if (Test-Path $tc) { $env:PATH = "$tc;$env:PATH" }

# Sanity: required binaries
foreach ($p in @('target\iq_capture.exe','bin\nrsc5.exe','bin\librtlsdr.dll')) {
    if (-not (Test-Path $p)) { throw "missing: $p" }
}

$outDir   = 'target'
$rawDir   = Join-Path $outDir 'hd-scan-raw'
$csvPath  = Join-Path $outDir 'hd-scan.csv'
$logPath  = Join-Path $outDir 'hd-scan.log'
New-Item -ItemType Directory -Force -Path $rawDir | Out-Null
if (Test-Path $csvPath) { Remove-Item $csvPath -Force }
if (Test-Path $logPath) { Remove-Item $logPath -Force }

# 2.976 MB/s = 1.488 Msps cu8. Add 0.5 s pad for startup.
$bytesPerChannel = [int](($SecondsPerChannel + 0.5) * 2976750)

# Build channel list (0.2 MHz steps, US FM is odd-tenths only)
$channels = @()
for ($f = $StartMhz; $f -le ($StopMhz + 1e-6); $f += $StepMhz) {
    $channels += [math]::Round($f, 1)
}

$header = "freq_mhz,synced,mer_lower_db,mer_upper_db,first_ber,station_name,slogan,title,log_bytes"
$header | Out-File -FilePath $csvPath -Encoding ascii

$startedAt = Get-Date
Write-Host ("=== HD-radio band scan @ gain={0} dB, {1}s/channel ===" -f $Gain, $SecondsPerChannel) -ForegroundColor Cyan
Write-Host ("    {0} channels ({1:N1} - {2:N1} MHz step {3:N1}), ETA ~{4:N1} min" -f `
    $channels.Count, $StartMhz, $StopMhz, $StepMhz, ($channels.Count * ($SecondsPerChannel + 1.5) / 60)) -ForegroundColor Cyan
Write-Host ("    output: $csvPath") -ForegroundColor Cyan
Write-Host ""
Write-Host ("{0,-7}  {1,-7}  {2,-12}  {3,-12}  {4,-12}  {5,-28}  {6}" -f `
    "MHz","sync","MER_L (dB)","MER_U (dB)","first BER","station","title") -ForegroundColor White
Write-Host ("{0}" -f ('-' * 100)) -ForegroundColor DarkGray

$results = @()
$syncCount = 0
$chanIdx = 0
foreach ($mhz in $channels) {
    $chanIdx++
    $rawLog = Join-Path $rawDir ("nrsc5-{0:N1}.log" -f $mhz)

    # Run iq_capture | nrsc5 for this channel
    & cmd /c "target\iq_capture.exe --freq $mhz --gain $Gain --bytes $bytesPerChannel 2> NUL | bin\nrsc5.exe -l 1 -r - 0 2> `"$rawLog`"" | Out-Null
    $logBytes = if (Test-Path $rawLog) { (Get-Item $rawLog).Length } else { 0 }

    $synced       = $false
    $merLowerDb   = ''
    $merUpperDb   = ''
    $firstBer     = ''
    $stationName  = ''
    $slogan       = ''
    $title        = ''

    if ($logBytes -gt 0) {
        $lines = Get-Content $rawLog -ErrorAction SilentlyContinue
        foreach ($line in $lines) {
            if (-not $synced -and $line -match 'Synchronized') { $synced = $true; continue }
            if ($merLowerDb -eq '' -and $line -match 'MER:\s+([-\d.]+)\s+dB\s+\(lower\),\s+([-\d.]+)\s+dB\s+\(upper\)') {
                $merLowerDb = $matches[1]; $merUpperDb = $matches[2]; continue
            }
            if ($firstBer -eq '' -and $line -match 'BER:\s+([\d.e-]+)') { $firstBer = $matches[1]; continue }
            if ($stationName -eq '' -and $line -match 'Station name:\s+(.+)$') { $stationName = $matches[1].Trim(); continue }
            if ($slogan -eq ''      -and $line -match 'Slogan:\s+(.+)$')      { $slogan      = $matches[1].Trim(); continue }
            if ($title -eq ''       -and $line -match 'Title:\s+(.+)$')       { $title       = $matches[1].Trim(); continue }
        }
    }

    if ($synced) { $syncCount++ }

    $row = [PSCustomObject]@{
        freq_mhz      = $mhz
        synced        = if ($synced) { 'Y' } else { '' }
        mer_lower_db  = $merLowerDb
        mer_upper_db  = $merUpperDb
        first_ber     = $firstBer
        station_name  = $stationName
        slogan        = $slogan
        title         = $title
        log_bytes     = $logBytes
    }
    $results += $row

    # Append to CSV (stream-as-you-go, survives crashes)
    $stationCsv = ($stationName -replace '"','""')
    $sloganCsv  = ($slogan      -replace '"','""')
    $titleCsv   = ($title       -replace '"','""')
    $csvLine = '{0:N1},{1},{2},{3},{4},"{5}","{6}","{7}",{8}' -f `
        $mhz, $row.synced, $merLowerDb, $merUpperDb, $firstBer, $stationCsv, $sloganCsv, $titleCsv, $logBytes
    Add-Content -Path $csvPath -Value $csvLine -Encoding ascii

    # Live console row
    $color = if ($synced) { 'Green' } else { 'DarkGray' }
    $merCell = if ($merLowerDb) { '{0,5} / {1,-5}' -f $merLowerDb, $merUpperDb } else { '' }
    $line = "{0,7:N1}  {1,-7}  {2,-12}  {3,-12}  {4,-28}  {5}" -f `
        $mhz, ($(if ($synced) { 'SYNC' } else { '' })), $merCell, $firstBer, $stationName, $title
    Write-Host $line -ForegroundColor $color

    $progress = "[$chanIdx/$($channels.Count)] $mhz MHz synced=$synced mer=$merLowerDb/$merUpperDb"
    Add-Content -Path $logPath -Value $progress
}

$elapsed = (Get-Date) - $startedAt
Write-Host ""
Write-Host ("=== Done. {0}/{1} channels with HD lock in {2:N1} min ===" -f $syncCount, $channels.Count, $elapsed.TotalMinutes) -ForegroundColor Cyan
Write-Host ""

# Final summary: only the synced ones, sorted by MER descending
$synced = $results | Where-Object { $_.synced -eq 'Y' }
if ($synced.Count -gt 0) {
    Write-Host "Stations with HD lock (sorted by MER lower, descending):" -ForegroundColor Yellow
    $synced |
        Sort-Object @{Expression={[double]($_.mer_lower_db -as [double])}; Descending=$true} |
        Format-Table freq_mhz, mer_lower_db, mer_upper_db, first_ber, station_name, slogan, title -AutoSize
} else {
    Write-Host "No channels produced HD lock. Try a different gain or check antenna." -ForegroundColor Yellow
}

Write-Host ("Results: $csvPath") -ForegroundColor Cyan
