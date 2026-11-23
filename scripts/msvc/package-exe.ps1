[CmdletBinding()]
param(
    [ValidateSet('release-fast', 'release')]
    [string]$Profile = 'release-fast'
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$target = Join-Path $root "target/$Profile"
$dist = Join-Path $root 'dist'
$destination = Join-Path $dist 'autocode'
$archive = Join-Path $dist 'autocode-x64.zip'
$stage = $null

Push-Location $root
try {
    $hostInfo = & rustc -vV
    if ($LASTEXITCODE -ne 0) { throw 'Could not query the Rust toolchain.' }
    if ($hostInfo -notcontains 'host: x86_64-pc-windows-msvc') {
        throw 'This local package requires the x86_64-pc-windows-msvc Rust host.'
    }

    & (Join-Path $PSScriptRoot 'setup-spectre.ps1')

    Write-Host "[1/3] Building autocode ($Profile)..."
    & cargo build --locked --profile $Profile --target-dir (Join-Path $root 'target') --package autocode --package cli --package auto_update_helper
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed (exit $LASTEXITCODE)." }

    $files = @{
        'autocode.exe' = 'autocode.exe'
        'cli.exe' = 'bin/autocode.exe'
        'auto_update_helper.exe' = 'tools/auto_update_helper.exe'
        'conpty.dll' = 'conpty.dll'
        'OpenConsole.exe' = 'OpenConsole.exe'
    }
    foreach ($source in $files.Keys) {
        if (-not (Test-Path -LiteralPath (Join-Path $target $source) -PathType Leaf)) {
            throw "Required build output is missing: $target/$source"
        }
    }

    Write-Host '[2/3] Collecting portable runtime files...'
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    $stage = Join-Path $dist ('.autocode-package-' + [guid]::NewGuid().ToString('N'))
    $payload = Join-Path $stage 'autocode'
    New-Item -ItemType Directory -Path (Join-Path $payload 'tools') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $payload 'bin') -Force | Out-Null
    foreach ($source in $files.Keys) {
        Copy-Item -LiteralPath (Join-Path $target $source) -Destination (Join-Path $payload $files[$source])
    }

    Write-Host '[3/3] Creating portable archive...'
    Compress-Archive -LiteralPath $payload -DestinationPath $archive -CompressionLevel Optimal -Force
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    Copy-Item -Path (Join-Path $payload '*') -Destination $destination -Recurse -Force
    Write-Host "EXE: $destination/autocode.exe"
    Write-Host "ZIP: $archive"
    Write-Host 'For isolated local testing, pass --user-data-dir <test-directory>.'
    Write-Host 'Independent autocode build; no upstream product updater is used.'
} catch {
    Write-Error $_ -ErrorAction Continue
    exit 1
} finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    Pop-Location
}
