# Downloads FFmpeg (gyan.dev "essentials" build) and puts ffmpeg/ffprobe where
# Tauri expects them, so they're bundled into the ShotSynq installer.
# Run once:  powershell -ExecutionPolicy Bypass -File .\setup-ffmpeg.ps1
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$bin     = Join-Path $PSScriptRoot 'src-tauri\binaries'
$triple  = 'x86_64-pc-windows-msvc'
$ffmpeg  = Join-Path $bin "ffmpeg-$triple.exe"
$ffprobe = Join-Path $bin "ffprobe-$triple.exe"
$licence = Join-Path $bin 'FFMPEG-LICENSE.txt'
$version = Join-Path $bin 'FFMPEG-VERSION.txt'

if ((Test-Path $ffmpeg) -and (Test-Path $ffprobe) -and (Test-Path $licence) -and (Test-Path $version)) {
    Write-Host 'FFmpeg is already in place.'
    exit 0
}

$zip = Join-Path $env:TEMP 'ffmpeg-release-essentials.zip'
$out = Join-Path $env:TEMP 'ffmpeg-shotsynq'

Write-Host 'Downloading FFmpeg (about 100 MB)...'
Invoke-WebRequest 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip' -OutFile $zip

if (Test-Path $out) { Remove-Item $out -Recurse -Force }
Expand-Archive $zip -DestinationPath $out
$exe = Get-ChildItem $out -Recurse -Filter 'ffmpeg.exe' | Select-Object -First 1
New-Item -ItemType Directory -Force -Path $bin | Out-Null
Copy-Item $exe.FullName $ffmpeg
Copy-Item (Join-Path $exe.DirectoryName 'ffprobe.exe') $ffprobe

# Licence text + exact version, shipped with the installer (GPL requirement)
$root = Split-Path $exe.DirectoryName -Parent
$lic  = Get-ChildItem $root -Filter 'LICENSE*' | Select-Object -First 1
if ($lic) { Copy-Item $lic.FullName $licence }
else {
    Invoke-WebRequest 'https://www.gnu.org/licenses/gpl-3.0.txt' -OutFile $licence
}
$v = & $ffmpeg -hide_banner -version 2>$null | Select-Object -First 3
@("FFmpeg build shipped with ShotSynq:", "") + $v + @("", "Built by gyan.dev: https://www.gyan.dev/ffmpeg/builds/", "Source: https://github.com/FFmpeg/FFmpeg") |
    Set-Content -Path $version -Encoding UTF8
Remove-Item $zip -Force
Remove-Item $out -Recurse -Force

Write-Host "Done. FFmpeg is in $bin"
