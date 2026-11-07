[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-f]{40}$')][string]$ExpectedSha,
    [string]$OutputDirectory = 'dist-windows'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = (Resolve-Path "$PSScriptRoot/../..").Path
Set-Location $repo
function Check-Exit([string]$Operation) {
    if ($LASTEXITCODE -ne 0) { throw "$Operation failed with exit code $LASTEXITCODE" }
}
if ((git rev-parse HEAD) -ne $ExpectedSha) { throw 'Checkout does not match selected commit' }
Check-Exit 'git identity'
if (git status --porcelain --untracked-files=normal) { throw 'Release builds require a clean checkout' }
Check-Exit 'git status'
$metadata = @{}
Get-Content release/metadata.env | ForEach-Object {
    if ($_ -match '^([^#=]+)=(.*)$') { $metadata[$Matches[1]] = $Matches[2] }
}
$target = 'x86_64-pc-windows-msvc'
if ($target -notin $metadata.LIGHTSPEED_ENVD_TARGETS.Split(',')) { throw 'Windows is not a required release target' }
$toolchain = $metadata.LIGHTSPEED_RELEASE_RUST_VERSION
if ((Get-Content rust-toolchain.toml -Raw) -notmatch "channel = `"$([regex]::Escape($toolchain))`"") { throw 'Rust pins disagree' }
$vswhere = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
Check-Exit 'Visual Studio discovery'
if (!$vs) { throw 'Visual Studio C++ Build Tools are required' }
Import-Module "$vs/Common7/Tools/Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64' | Out-Null
$env:LIGHTSPEED_RELEASE_VERSION = $metadata.LIGHTSPEED_PRODUCT_VERSION
$env:LIGHTSPEED_GIT_SHA = $ExpectedSha
$env:LIGHTSPEED_ENVD_TARGETS = $metadata.LIGHTSPEED_ENVD_TARGETS
# Keep release cache and dependencies inside the runner's own checkout/home.
$env:CARGO_TARGET_DIR = Join-Path $repo 'target'
# The standalone daemon must not depend on a separately installed VC runtime.
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
Remove-Item Env:/RUSTFLAGS, Env:/CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
& rustup toolchain install $toolchain --profile minimal --no-self-update
Check-Exit 'Pinned Rust toolchain installation'
& rustup target add --toolchain $toolchain $target
Check-Exit 'Rust target installation'
$rustVersion = (& rustc "+$toolchain" --version | Out-String).Trim()
Check-Exit 'Rust version'
$cargoVersion = (& cargo "+$toolchain" --version | Out-String).Trim()
Check-Exit 'Cargo version'
& cargo "+$toolchain" build --release --locked --target $target -p environment-daemon --bin lightspeed-envd
Check-Exit 'Windows envd compilation'
$output = [IO.Path]::GetFullPath((Join-Path $repo $OutputDirectory))
if (!$output.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Output must be inside the checkout' }
if (Test-Path -LiteralPath $output) { throw 'Output directory already exists; use a clean checkout or a new output directory' }
$archives = New-Item -ItemType Directory -Path "$output/archives"
$provenance = New-Item -ItemType Directory -Path "$output/provenance"
$staging = New-Item -ItemType Directory -Path "$output/staging"
$binary = Join-Path $staging.FullName 'lightspeed-envd.exe'
Copy-Item -LiteralPath "target/$target/release/lightspeed-envd.exe" -Destination $binary
$build = & $binary --print-build | ConvertFrom-Json
Check-Exit 'Packaged build identity'
if ($build.name -ne 'lightspeed-envd' -or $build.gitSha -ne $ExpectedSha -or
    $build.version -ne $metadata.LIGHTSPEED_PRODUCT_VERSION -or $build.target -ne $target -or
    $build.protocolVersion -ne [int]$metadata.LIGHTSPEED_ENVD_PROTOCOL_VERSION) { throw 'Packaged build identity mismatch' }
& $binary --version
Check-Exit 'Packaged version'
$dependencies = & dumpbin /DEPENDENTS $binary | Out-String
Check-Exit 'Executable dependencies'
if ($dependencies -match '(?i)(VCRUNTIME|MSVCP|MSVCR)[0-9].*\.dll') { throw 'Executable depends on an external Visual C++ runtime' }
$filename = "lightspeed-envd-$($metadata.LIGHTSPEED_PRODUCT_VERSION)-$target.tar.gz"
& tar -czf "$($archives.FullName)/$filename" -C $staging.FullName lightspeed-envd.exe
Check-Exit 'Windows archive'
$extracted = New-Item -ItemType Directory -Path "$output/acceptance"
& tar -xzf "$($archives.FullName)/$filename" -C $extracted.FullName
Check-Exit 'Archive extraction'
$env:LIGHTSPEED_TEST_ENVD_BINARY = Join-Path $extracted.FullName 'lightspeed-envd.exe'
try {
    & cargo "+$toolchain" test --locked --target $target -p environment-daemon --test windows_live packaged_release_smoke -- --ignored --exact --nocapture
    Check-Exit 'Packaged protocol acceptance'
} finally { Remove-Item Env:/LIGHTSPEED_TEST_ENVD_BINARY -ErrorAction SilentlyContinue }
$linker = (Get-Command link.exe).Source
$record = [ordered]@{
    version = $build.version; gitSha = $ExpectedSha; target = $target; protocolVersion = $build.protocolVersion
    file = $filename; sha256 = (Get-FileHash "$($archives.FullName)/$filename" -Algorithm SHA256).Hash.ToLowerInvariant()
    rustVersion = $rustVersion; cargoVersion = $cargoVersion
    cargoLockSha256 = (Get-FileHash Cargo.lock -Algorithm SHA256).Hash.ToLowerInvariant()
    osVersion = [Environment]::OSVersion.VersionString
    msvcVersion = $env:VCToolsVersion.Trim(); windowsSdkVersion = $env:WindowsSDKVersion.TrimEnd('\')
    linkerVersion = (Get-Item -LiteralPath $linker).VersionInfo.FileVersion
    rustFlags = $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS
}
$record | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8NoBOM "$($provenance.FullName)/windows-envd.json"
Write-Output "Windows release artifact passed acceptance: $($archives.FullName)/$filename"
