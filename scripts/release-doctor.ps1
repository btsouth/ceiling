#Requires -Version 5.1
<#
.SYNOPSIS
    Check whether a Ceiling release is ready or complete.

.DESCRIPTION
    Verifies version-file consistency, changelog presence, optional local
    Windows assets, asset SHA-256 sidecars, Git tag presence, and GitHub release
    asset presence when gh is authenticated.
#>

param(
    [string]$Version = "",
    [string]$AssetsDir = "C:\code\Ceiling-release\assets",
    [string]$ExpectedSigner = "CN=Brandon South",
    [switch]$SkipGitHub
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$Failures = New-Object System.Collections.Generic.List[string]
$Warnings = New-Object System.Collections.Generic.List[string]

function Write-Ok {
    param([string]$Message)
    Write-Host "[ok] $Message"
}

function Write-Warn {
    param([string]$Message)
    $Warnings.Add($Message)
    Write-Host "[warn] $Message" -ForegroundColor Yellow
}

function Write-Fail {
    param([string]$Message)
    $Failures.Add($Message)
    Write-Host "[fail] $Message" -ForegroundColor Red
}

function Get-CargoVersion {
    param([string]$Path)
    $line = Get-Content $Path | Where-Object { $_ -match '^version = "([^"]+)"' } | Select-Object -First 1
    if ($line -and $line -match '^version = "([^"]+)"') {
        return $Matches[1]
    }
    return ""
}

function Get-VersionEnvValue {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        return ""
    }
    $line = Get-Content $Path | Where-Object { $_ -match '^MARKETING_VERSION=(.+)$' } | Select-Object -First 1
    if ($line -and $line -match '^MARKETING_VERSION=(.+)$') {
        return $Matches[1].Trim()
    }
    return ""
}

function Assert-Version {
    param(
        [string]$Label,
        [string]$Actual,
        [string]$Expected
    )
    if ($Actual -eq $Expected) {
        Write-Ok "$Label version is $Actual"
    } else {
        Write-Fail "$Label version is $Actual, expected $Expected"
    }
}

function Assert-FileContains {
    param(
        [string]$Label,
        [string]$Path,
        [string]$Pattern
    )
    if ((Test-Path $Path) -and (Select-String -Path $Path -Pattern $Pattern -Quiet)) {
        Write-Ok "$Label"
    } else {
        Write-Fail "$Label"
    }
}

function Test-AssetHash {
    param([string]$AssetPath)

    $shaPath = "$AssetPath.sha256"
    if (-not (Test-Path $AssetPath)) {
        Write-Fail "missing asset: $AssetPath"
        return
    }
    if (-not (Test-Path $shaPath)) {
        Write-Fail "missing sha256 sidecar: $shaPath"
        return
    }

    $expected = ((Get-Content $shaPath | Select-Object -First 1) -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $AssetPath).Hash.ToLowerInvariant()
    if ($actual -eq $expected) {
        Write-Ok "$(Split-Path $AssetPath -Leaf) hash matches sidecar"
    } else {
        Write-Fail "$(Split-Path $AssetPath -Leaf) hash mismatch: expected $expected, got $actual"
    }
}

function Test-AssetSignature {
    param([string]$AssetPath)

    if (-not (Test-Path -LiteralPath $AssetPath)) {
        return
    }

    $signature = Get-AuthenticodeSignature -FilePath $AssetPath
    if ($signature.Status -ne "Valid") {
        Write-Fail "$(Split-Path $AssetPath -Leaf) signature is $($signature.Status)"
        return
    }
    if ($signature.SignerCertificate.Subject -notlike "*$ExpectedSigner*") {
        Write-Fail "$(Split-Path $AssetPath -Leaf) has unexpected signer $($signature.SignerCertificate.Subject)"
        return
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        Write-Fail "$(Split-Path $AssetPath -Leaf) is missing an RFC 3161 timestamp"
        return
    }

    Write-Ok "$(Split-Path $AssetPath -Leaf) is signed by $ExpectedSigner"
}

$rustVersion = Get-CargoVersion (Join-Path $RepoRoot "rust\Cargo.toml")
if (-not $Version) {
    $Version = $rustVersion
}
if (-not $Version) {
    throw "Could not determine release version."
}

$tag = "v$Version"
Write-Host "Release doctor: Ceiling $Version"
Write-Host ""

Assert-Version "rust/Cargo.toml" $rustVersion $Version
Assert-Version "apps/desktop-tauri/src-tauri/Cargo.toml" (Get-CargoVersion (Join-Path $RepoRoot "apps\desktop-tauri\src-tauri\Cargo.toml")) $Version
Assert-Version "version.env" (Get-VersionEnvValue (Join-Path $RepoRoot "version.env")) $Version

$packageJsonPath = Join-Path $RepoRoot "apps\desktop-tauri\package.json"
$packageVersion = ((Get-Content -Raw $packageJsonPath) | ConvertFrom-Json).version
Assert-Version "apps/desktop-tauri/package.json" $packageVersion $Version

$tauriConfigPath = Join-Path $RepoRoot "apps\desktop-tauri\src-tauri\tauri.conf.json"
$tauriVersion = ((Get-Content -Raw $tauriConfigPath) | ConvertFrom-Json).version
Assert-Version "tauri.conf.json" $tauriVersion $Version
$tauriConfig = Get-Content -Raw $tauriConfigPath | ConvertFrom-Json
if ($tauriConfig.productName -eq "Ceiling") {
    Write-Ok "Tauri product name is Ceiling"
} else {
    Write-Fail "Tauri product name is '$($tauriConfig.productName)', expected Ceiling"
}

if ($tauriConfig.identifier -eq "io.github.tsouth89.ceiling") {
    Write-Ok "Tauri identifier is io.github.tsouth89.ceiling"
} else {
    Write-Fail "Tauri identifier is '$($tauriConfig.identifier)', expected io.github.tsouth89.ceiling"
}

Assert-FileContains "Cargo repository points to Ceiling" `
    (Join-Path $RepoRoot "rust\Cargo.toml") `
    'repository = "https://github\.com/btsouth/ceiling"'
Assert-FileContains "Installer product name is Ceiling" `
    (Join-Path $RepoRoot "rust\installer\codexbar.iss") `
    '#define MyAppName "Ceiling"'
Assert-FileContains "Installer has Ceiling application id" `
    (Join-Path $RepoRoot "rust\installer\codexbar.iss") `
    'AppId=io\.github\.tsouth89\.ceiling'
Assert-FileContains "README documents upstream lineage" `
    (Join-Path $RepoRoot "README.md") `
    'independent Windows-focused fork'
Assert-FileContains "Security policy exists" `
    (Join-Path $RepoRoot "SECURITY.md") `
    '# Security policy'

$git = Get-Command git -ErrorAction SilentlyContinue
if ($git) {
    Push-Location $RepoRoot
    try {
        & $git.Source rev-parse --verify --quiet "$tag^{commit}" *> $null
        if ($LASTEXITCODE -eq 0) {
            Write-Ok "Git tag exists: $tag"
        } else {
            Write-Warn "Git tag not found locally: $tag"
        }
    } finally {
        Pop-Location
    }
} else {
    Write-Warn "git not found; skipped local tag check"
}

$changelogPath = Join-Path $RepoRoot "CHANGELOG.md"
if ((Test-Path $changelogPath) -and (Select-String -Path $changelogPath -Pattern ([regex]::Escape($Version)) -Quiet)) {
    Write-Ok "CHANGELOG.md mentions $Version"
} else {
    Write-Warn "CHANGELOG.md does not mention $Version"
}

if (Test-Path $AssetsDir) {
    $installerAsset = Join-Path $AssetsDir "Ceiling-$Version-Setup.exe"
    $storeInstallerAsset = Join-Path $AssetsDir "Ceiling-$Version-Store-Setup.exe"
    $portableAsset = Join-Path $AssetsDir "Ceiling-$Version-portable.exe"
    Test-AssetHash $installerAsset
    Test-AssetSignature $installerAsset
    Test-AssetHash $storeInstallerAsset
    Test-AssetSignature $storeInstallerAsset
    if ((Test-Path -LiteralPath $storeInstallerAsset) -and
        ((Get-Item -LiteralPath $storeInstallerAsset).Length -ge 50MB)) {
        Write-Ok "$(Split-Path $storeInstallerAsset -Leaf) is large enough to contain the offline WebView2 runtime"
    } else {
        Write-Fail "$(Split-Path $storeInstallerAsset -Leaf) is too small to contain the offline WebView2 runtime"
    }
    Test-AssetHash $portableAsset
    Test-AssetSignature $portableAsset
} else {
    Write-Warn "local assets directory not found: $AssetsDir"
}

if (-not $SkipGitHub) {
    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if ($gh) {
        Push-Location $RepoRoot
        try {
            $ghJsonPath = Join-Path $env:TEMP "ceiling-release-doctor-gh.json"
            $ghErrPath = Join-Path $env:TEMP "ceiling-release-doctor-gh.err"
            # `gh release view` writing "release not found" to stderr is an
            # expected answer before the release exists, not a script fault.
            # Under $ErrorActionPreference = "Stop" a native command's stderr
            # becomes a terminating error, which killed the doctor here and
            # skipped its summary: every pre-tag run failed identically, so a
            # genuine failure was indistinguishable from the normal case. Let
            # the exit code below do the deciding, as this block already
            # intended.
            $previousErrorAction = $ErrorActionPreference
            $ErrorActionPreference = "Continue"
            try {
                & $gh.Source release view $tag --json assets,url 1>$ghJsonPath 2>$ghErrPath
            } finally {
                $ErrorActionPreference = $previousErrorAction
            }
            if ($LASTEXITCODE -eq 0) {
                $release = Get-Content -Raw $ghJsonPath | ConvertFrom-Json
                Write-Ok "GitHub release exists: $($release.url)"
                $assetNames = @($release.assets | ForEach-Object { $_.name })
                foreach ($name in @(
                    "Ceiling-$Version-Setup.exe",
                    "Ceiling-$Version-Setup.exe.sha256",
                    "Ceiling-$Version-Store-Setup.exe",
                    "Ceiling-$Version-Store-Setup.exe.sha256",
                    "Ceiling-$Version-portable.exe",
                    "Ceiling-$Version-portable.exe.sha256"
                )) {
                    if ($assetNames -contains $name) {
                        Write-Ok "GitHub release has $name"
                    } else {
                        Write-Fail "GitHub release missing $name"
                    }
                }
            } else {
                $err = Get-Content -Raw $ghErrPath
                Write-Warn "GitHub release $tag not found or gh is not authenticated: $err"
            }
        } finally {
            Pop-Location
        }
    } else {
        Write-Warn "gh not found; skipped GitHub release checks"
    }
}

Write-Host ""
Write-Host "Winget reminder: after GitHub assets are stable, copy the previous manifest folder and update PackageVersion, InstallerUrl, InstallerSha256, DisplayVersion, ReleaseNotes, and ReleaseNotesUrl."

if ($Failures.Count -gt 0) {
    Write-Host ""
    Write-Host "$($Failures.Count) release doctor check(s) failed." -ForegroundColor Red
    exit 1
}

if ($Warnings.Count -gt 0) {
    Write-Host ""
    Write-Host "$($Warnings.Count) warning(s)." -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Release doctor passed."
