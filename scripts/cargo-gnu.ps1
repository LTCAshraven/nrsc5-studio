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

$env:PATH = "$env:USERPROFILE\.cargo\bin;$ToolRoot;$env:PATH"

rustup toolchain install stable-x86_64-pc-windows-gnullvm
cargo +stable-x86_64-pc-windows-gnullvm build --target x86_64-pc-windows-gnullvm
