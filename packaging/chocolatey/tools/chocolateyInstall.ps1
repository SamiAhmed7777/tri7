# Chocolatey install script for Cryptographic Triangles (tri7).
# The __CHECKSUM_PLACEHOLDER__ token is replaced by the distribute workflow
# with the SHA256 of the release setup.exe at package time.
$ErrorActionPreference = 'Stop'

$packageName   = 'cryptographictriangles'
$toolsDir      = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"
$installerName = 'CryptographicTriangles-7.0.0-win64-setup.exe'
$url           = "https://github.com/SamiAhmed7777/tri7/releases/download/v7.0.0/$installerName"
$checksum64    = '__CHECKSUM_PLACEHOLDER__'
$checksumType  = 'sha256'

# NSIS installer: silent flag is /S (matches the winget manifest InstallerSwitches).
$packageArgs = @{
  packageName    = $packageName
  fileType       = 'exe'
  url64bit       = $url
  checksum64     = $checksum64
  checksumType64 = $checksumType
  silentArgs     = '/S'
  validExitCodes = @(0)
  softwareName   = 'Cryptographic Triangles (tri7)*'
}

Install-ChocolateyPackage @packageArgs
