param(
	[ValidateSet("debug", "release")]
	[string]$Configuration = "debug",
	[ValidateSet("build", "check", "test")]
	[string]$Command = "build"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$ToolRoot = Join-Path $Root ".toolchains\llvm-mingw-20260505-ucrt-x86_64\bin"
if (-not (Test-Path $ToolRoot)) {
	throw "llvm-mingw toolchain not found: $ToolRoot (run scripts/bootstrap-deps.ps1 and toolchain bootstrap first)"
}

$DllToolAlias = Join-Path $ToolRoot "dlltool.exe"
if (-not (Test-Path $DllToolAlias)) {
	Copy-Item (Join-Path $ToolRoot "x86_64-w64-mingw32-dlltool.exe") $DllToolAlias -Force
}

$WinResAlias = Join-Path $ToolRoot "windres.exe"
if (-not (Test-Path $WinResAlias)) {
	Copy-Item (Join-Path $ToolRoot "x86_64-w64-mingw32-windres.exe") $WinResAlias -Force
}

# PATH order matters:
#   1) C:\msys64\mingw64\bin  -- libclang.dll's transitive deps
#                                (libstdc++, libwinpthread) live here.
#                                soapysdr-sys's build script invokes
#                                bindgen, which loads libclang via
#                                LoadLibraryExW; without these on PATH
#                                it fails with "LoadLibraryExW failed".
#   2) %USERPROFILE%\.cargo\bin -- cargo + rustup shims.
#   3) llvm-mingw\bin           -- our pinned cross-compiler.
$MsysBin = "C:\msys64\mingw64\bin"
if (-not (Test-Path $MsysBin)) {
	Write-Warning "MSYS2 mingw64 not found at $MsysBin -- bindgen-based build scripts may fail to load libclang."
} else {
	$env:PATH = "$MsysBin;$env:USERPROFILE\.cargo\bin;$ToolRoot;$env:PATH"
	# bindgen also reads LIBCLANG_PATH directly; setting it explicitly
	# avoids relying purely on PATH ordering for libclang.dll discovery.
	$env:LIBCLANG_PATH = $MsysBin
}
if (-not $env:PATH.StartsWith($MsysBin)) {
	$env:PATH = "$env:USERPROFILE\.cargo\bin;$ToolRoot;$env:PATH"
}

# rustup writes its "info: syncing channel updates..." progress to
# stderr; under $ErrorActionPreference = "Stop" PowerShell 5.1 wraps
# every native stderr line as a RemoteException error record and aborts
# the script before cargo runs. Temporarily downgrade to "Continue" for
# the native-command sections and rely on $LASTEXITCODE for real failure
# detection.
$prev = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try {
	& rustup toolchain install stable-x86_64-pc-windows-gnullvm
	if ($LASTEXITCODE -ne 0) {
		throw "rustup toolchain install failed with exit code $LASTEXITCODE"
	}

	$cargoArgs = @("+stable-x86_64-pc-windows-gnullvm", $Command, "--target", "x86_64-pc-windows-gnullvm")
	if ($Configuration -eq "release") {
		$cargoArgs += "--release"
	}

	& cargo @cargoArgs
	$cargoExit = $LASTEXITCODE
}
finally {
	$ErrorActionPreference = $prev
}
exit $cargoExit
