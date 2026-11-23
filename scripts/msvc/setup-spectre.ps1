# Supplement the portable MSVC 14.44 toolchain with Microsoft's Spectre CRT.
$ErrorActionPreference = 'Stop'
$toolset = $env:VCToolsInstallDir
if (-not $toolset) { throw 'Run msvc-env.bat before installing Spectre libraries.' }
$required = Join-Path $toolset 'lib/spectre/x64/libcmt.lib'
if (Test-Path -LiteralPath $required) { return }
if ((Split-Path $toolset.TrimEnd('\') -Leaf) -ne '14.44.35207') {
    throw 'Automatic Spectre setup supports MSVC 14.44.35207 only. Install the matching Spectre CRT for your toolchain.'
}

$root = [IO.Path]::GetFullPath((Join-Path $toolset '../../../..'))
$downloads = Join-Path (Split-Path $root -Parent) 'downloads'
New-Item -ItemType Directory -Path $downloads -Force | Out-Null
$name = 'Microsoft.VC.14.44.17.14.CRT.x64.Desktop.spectre.base.vsix'
$hash = '6d7cb93cc7fdc206f44999850b414600ae1041d0db7d2fe9d0ff658abfb97b58'
$archive = Join-Path $downloads $name
if (-not (Test-Path -LiteralPath $archive)) {
    Write-Host 'Downloading Microsoft Spectre CRT (about 59 MB; Visual Studio component license applies)...'
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -UseBasicParsing -Uri "https://download.visualstudio.microsoft.com/download/pr/67cf767c-5e71-47c2-a54a-cd5631e28942/$hash/$name" -OutFile $archive
}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $hash) {
    throw "Spectre CRT checksum mismatch. Remove the invalid download and retry: $archive"
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($archive)
try {
    $prefix = 'Contents/VC/Tools/MSVC/14.44.35207/lib/spectre/x64/'
    foreach ($entry in $zip.Entries) {
        if (-not $entry.FullName.StartsWith($prefix, [StringComparison]::Ordinal)) { continue }
        $relative = $entry.FullName.Substring($prefix.Length)
        if (-not $relative -or $relative.Contains('/') -or $relative.Contains('\')) { continue }
        $destination = Join-Path $toolset ('lib/spectre/x64/' + $relative)
        New-Item -ItemType Directory -Path (Split-Path $destination -Parent) -Force | Out-Null
        [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destination, $true)
    }
} finally {
    $zip.Dispose()
}
if (-not (Test-Path -LiteralPath $required)) { throw 'Spectre CRT archive did not contain the required libraries.' }
Write-Host "Spectre CRT installed: $toolset/lib/spectre/x64"
