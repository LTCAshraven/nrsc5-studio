# Download corresponding source tarballs for every GPL/LGPL component
# bundled with the NRSC5 Studio binary distribution. Output goes into
# dist/corresponding-source/, which is uploaded to the GitHub Release
# alongside the .deb/.rpm/portable.zip.
#
# Run once per release after building, then attach the entire directory
# to the GitHub Release. The same tarballs satisfy GPL-3.0 Section 6(d)
# corresponding-source obligations for the Windows zip, Linux .deb, and
# Linux .rpm.
#
# Re-running is safe: existing files are kept (delete a file to refetch).

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$Root = Split-Path -Parent $PSScriptRoot
$Out  = Join-Path $Root "dist\corresponding-source"
if (-not (Test-Path $Out)) {
    New-Item -ItemType Directory -Path $Out -Force | Out-Null
}

$downloads = @(
    @{ Url = "https://github.com/theori-io/nrsc5/archive/refs/tags/v3.1.0.tar.gz";                         Name = "nrsc5-v3.1.0.tar.gz" },
    @{ Url = "https://www.fftw.org/fftw-3.3.10.tar.gz";                                                    Name = "fftw-3.3.10.tar.gz" },
    @{ Url = "https://github.com/libusb/libusb/archive/refs/tags/v1.0.27.tar.gz";                          Name = "libusb-v1.0.27.tar.gz" },
    @{ Url = "https://github.com/libusb/libusb/archive/refs/tags/v1.0.28.tar.gz";                          Name = "libusb-v1.0.28.tar.gz" },
    @{ Url = "https://github.com/osmocom/rtl-sdr/archive/refs/tags/v2.0.2.tar.gz";                       Name = "rtl-sdr-v2.0.2.tar.gz" },
    @{ Url = "https://github.com/knik0/faad2/archive/refs/tags/2.11.2.tar.gz";                             Name = "faad2-2.11.2.tar.gz" },
    @{ Url = "https://github.com/pothosware/SoapySDR/archive/refs/tags/soapy-sdr-0.8.1.tar.gz";            Name = "SoapySDR-0.8.1.tar.gz" },
    @{ Url = "https://github.com/pothosware/SoapyRTLSDR/archive/refs/tags/soapy-rtl-sdr-0.3.3.tar.gz";     Name = "SoapyRTLSDR-0.3.3.tar.gz" },
    @{ Url = "https://github.com/pothosware/SoapyHackRF/archive/refs/tags/soapy-hackrf-0.3.4.tar.gz";    Name = "SoapyHackRF-soapy-hackrf-0.3.4.tar.gz" },
    @{ Url = "https://github.com/pothosware/SoapySDRPlay3/archive/refs/heads/master.tar.gz";               Name = "SoapySDRPlay3-master.tar.gz" }
)

foreach ($d in $downloads) {
    $dest = Join-Path $Out $d.Name
    if (Test-Path $dest) {
        $size = "{0:N1} KB" -f ((Get-Item $dest).Length / 1KB)
        Write-Host ("skip   {0,-40} {1,12}" -f $d.Name, $size)
        continue
    }
    try {
        Invoke-WebRequest -Uri $d.Url -OutFile $dest -UseBasicParsing
        $size = "{0:N1} KB" -f ((Get-Item $dest).Length / 1KB)
        Write-Host ("ok     {0,-40} {1,12}" -f $d.Name, $size)
    } catch {
        Write-Host ("FAIL   {0,-40} {1}" -f $d.Name, $_.Exception.Message) -ForegroundColor Red
    }
}

Write-Host ""
Write-Host "--- $Out ---"
Get-ChildItem $Out | Sort-Object Name | Format-Table Name, @{N='Size';E={"{0:N1} KB" -f ($_.Length / 1KB)}} -AutoSize
$totalKb = (Get-ChildItem $Out | Measure-Object Length -Sum).Sum / 1KB
Write-Host ("Total: {0:N1} KB ({1:N1} MB)" -f $totalKb, ($totalKb / 1024))
