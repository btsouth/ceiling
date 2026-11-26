#Requires -Version 7.0

# Exercises the credential handling in publish-store-installer.ps1 against a
# stub aws command and a local HTTP server standing in for both the Cloudflare
# token verify API and the public download origin. Nothing leaves the machine.

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$scriptPath = Join-Path $repoRoot "scripts/publish-store-installer.ps1"
$version = "1.2.3"
$accountId = "0123456789abcdef0123456789abcdef"

$workDir = Join-Path ([System.IO.Path]::GetTempPath()) "ceiling-upload-test-$([guid]::NewGuid().ToString('N'))"
$binDir = Join-Path $workDir "bin"
$assetsDir = Join-Path $workDir "assets"
$awsLog = Join-Path $workDir "aws.log"
New-Item -ItemType Directory -Path $binDir, $assetsDir | Out-Null

$installerPath = Join-Path $assetsDir "Ceiling-$version-Store-Setup.exe"
[System.IO.File]::WriteAllText($installerPath, "fake installer bytes")
$installerHash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText("$installerPath.sha256", "$installerHash  Ceiling-$version-Store-Setup.exe`n")

# The stub records the AWS_* variables it was started with, one block per call.
if ($IsWindows) {
    Set-Content -LiteralPath (Join-Path $binDir "aws.cmd") -Value @(
        "@echo off",
        "echo ---call>> `"%STUB_AWS_LOG%`"",
        "set AWS_>> `"%STUB_AWS_LOG%`" 2>nul",
        "exit /b %STUB_AWS_EXIT%"
    )
} else {
    $awsStub = Join-Path $binDir "aws"
    Set-Content -LiteralPath $awsStub -Value @(
        "#!/bin/sh",
        "echo ---call >> `"`$STUB_AWS_LOG`"",
        "env | grep '^AWS_' >> `"`$STUB_AWS_LOG`"",
        "exit `"`${STUB_AWS_EXIT:-0}`""
    )
    # The script calls curl.exe, which only exists under that name on Windows.
    $curlShim = Join-Path $binDir "curl.exe"
    Set-Content -LiteralPath $curlShim -Value @("#!/bin/sh", 'exec curl "$@"')
    & chmod +x $awsStub $curlShim
}
$env:PATH = "$binDir$([System.IO.Path]::PathSeparator)$env:PATH"
$env:STUB_AWS_LOG = $awsLog

function Get-FreePort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try { return $listener.LocalEndpoint.Port } finally { $listener.Stop() }
}

$port = Get-FreePort
$baseUrl = "http://127.0.0.1:$port"
$requests = [System.Collections.Concurrent.ConcurrentQueue[string]]::new()
$httpListener = [System.Net.HttpListener]::new()
$httpListener.Prefixes.Add("$baseUrl/")
$httpListener.Start()

# Account-owned token "acct-token" verifies only on the account endpoint and
# user-owned token "user-token" only on the user endpoint, as on Cloudflare.
$server = [powershell]::Create()
$null = $server.AddScript({
    param($Listener, $Requests, $AccountId)
    while ($Listener.IsListening) {
        try { $context = $Listener.GetContext() } catch { break }
        $path = $context.Request.Url.AbsolutePath
        $auth = [string]$context.Request.Headers["Authorization"]
        $Requests.Enqueue("$path|$auth")
        $status = 404
        $body = '{"success":false,"errors":[{"code":1000,"message":"Invalid API Token"}]}'
        if ($path -eq "/accounts/$AccountId/tokens/verify" -and $auth -eq "Bearer acct-token") {
            $status = 200
            $body = '{"success":true,"errors":[],"messages":[],"result":{"id":"acct-token-id","status":"active"}}'
        } elseif ($path -eq "/user/tokens/verify" -and $auth -eq "Bearer user-token") {
            $status = 200
            $body = '{"success":true,"errors":[],"messages":[],"result":{"id":"user-token-id","status":"active"}}'
        } elseif ($path -eq "/user/tokens/verify" -and $auth -eq "Bearer disabled-token") {
            $status = 200
            $body = '{"success":true,"errors":[],"messages":[],"result":{"id":"disabled-token-id","status":"disabled"}}'
        } elseif ($path -like "*/tokens/verify") {
            $status = 401
        }
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($body)
        $context.Response.StatusCode = $status
        $context.Response.ContentType = "application/json"
        $context.Response.OutputStream.Write($bytes, 0, $bytes.Length)
        $context.Response.Close()
    }
}).AddArgument($httpListener).AddArgument($requests).AddArgument($accountId)
$serverHandle = $server.BeginInvoke()

$managedNames = @(
    "CLOUDFLARE_R2_ACCESS_KEY_ID", "CLOUDFLARE_R2_SECRET_ACCESS_KEY",
    "CLOUDFLARE_R2_API_TOKEN", "CLOUDFLARE_API_TOKEN",
    "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_PROFILE",
    "AWS_DEFAULT_REGION", "AWS_EC2_METADATA_DISABLED",
    "AWS_REQUEST_CHECKSUM_CALCULATION", "AWS_RESPONSE_CHECKSUM_VALIDATION",
    "STUB_AWS_EXIT"
)

function Invoke-Case {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][hashtable]$Environment,
        [string]$ExpectError
    )

    foreach ($variable in $managedNames) {
        Remove-Item -LiteralPath "Env:$variable" -ErrorAction SilentlyContinue
    }
    foreach ($key in $Environment.Keys) {
        Set-Item -LiteralPath "Env:$key" -Value $Environment[$key]
    }
    Remove-Item -LiteralPath $awsLog -ErrorAction SilentlyContinue
    $null = $requests.Clear()

    $failure = $null
    try {
        $null = & $scriptPath `
            -Version $version `
            -AssetsDir $assetsDir `
            -BucketName "test-bucket" `
            -AccountId $accountId `
            -DownloadOrigin $baseUrl `
            -CloudflareApiBaseUrl $baseUrl `
            -SkipPublicVerification 6>$null
    } catch {
        $failure = $_.Exception.Message
    }

    if ($ExpectError) {
        if ($null -eq $failure) {
            throw "${Name}: expected failure matching '$ExpectError', but the upload succeeded."
        }
        if ($failure -notmatch $ExpectError) {
            throw "${Name}: expected failure matching '$ExpectError', got: $failure"
        }
    } elseif ($null -ne $failure) {
        throw "${Name}: unexpected failure: $failure"
    }

    # Every variable the script touched must be back exactly as the case set it.
    foreach ($variable in $managedNames) {
        $expected = if ($Environment.ContainsKey($variable)) { $Environment[$variable] } else { $null }
        $actual = [Environment]::GetEnvironmentVariable($variable)
        if ($actual -ne $expected) {
            throw "${Name}: $variable was '$actual' after the run, expected '$expected'."
        }
        if ($null -eq $expected -and (Test-Path -LiteralPath "Env:$variable")) {
            throw "${Name}: $variable should be unset after the run but still exists."
        }
    }

    $calls = @()
    if (Test-Path -LiteralPath $awsLog) {
        $calls = @((Get-Content -LiteralPath $awsLog -Raw) -split "---call\r?\n" | Where-Object { $_.Trim() })
    }
    $parsed = foreach ($call in $calls) {
        $vars = @{}
        foreach ($line in ($call -split "\r?\n")) {
            if ($line -match '^(AWS_[A-Z_]+)=(.*)$') { $vars[$Matches[1]] = $Matches[2] }
        }
        $vars
    }
    return [pscustomobject]@{
        AwsCalls = @($parsed)
        Requests = @($requests.ToArray())
    }
}

function Assert-UploadEnvironment {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)]$Result,
        [Parameter(Mandatory = $true)][string]$AccessKeyId,
        [Parameter(Mandatory = $true)][string]$SecretAccessKey
    )

    if ($Result.AwsCalls.Count -ne 2) {
        throw "${Name}: expected 2 aws uploads, got $($Result.AwsCalls.Count)."
    }
    foreach ($call in $Result.AwsCalls) {
        if ($call["AWS_ACCESS_KEY_ID"] -cne $AccessKeyId) {
            throw "${Name}: aws saw AWS_ACCESS_KEY_ID '$($call["AWS_ACCESS_KEY_ID"])', expected '$AccessKeyId'."
        }
        if ($call["AWS_SECRET_ACCESS_KEY"] -cne $SecretAccessKey) {
            throw "${Name}: aws saw the wrong AWS_SECRET_ACCESS_KEY."
        }
        # Present at all, even empty, makes the AWS CLI look up a profile or
        # send a session token, so these must be absent from the child.
        foreach ($cleared in @("AWS_PROFILE", "AWS_SESSION_TOKEN")) {
            if ($call.ContainsKey($cleared)) {
                throw "${Name}: aws was started with $cleared='$($call[$cleared])'; it must be unset."
            }
        }
        if ($call["AWS_DEFAULT_REGION"] -ne "auto" -or $call["AWS_REQUEST_CHECKSUM_CALCULATION"] -ne "WHEN_REQUIRED") {
            throw "${Name}: aws did not get the R2 compatibility settings."
        }
    }
}

function Get-ExpectedSecret {
    param([string]$Token)
    $stream = [System.IO.MemoryStream]::new([System.Text.Encoding]::UTF8.GetBytes($Token))
    try { return (Get-FileHash -InputStream $stream -Algorithm SHA256).Hash.ToLowerInvariant() } finally { $stream.Dispose() }
}

$verifyRequests = { param($Result) @($Result.Requests | Where-Object { $_ -like "*/tokens/verify|*" }) }

try {
    $name = "explicit CLOUDFLARE_R2 pair wins"
    $result = Invoke-Case -Name $name -Environment @{
        CLOUDFLARE_R2_ACCESS_KEY_ID     = "cf-key"
        CLOUDFLARE_R2_SECRET_ACCESS_KEY = "cf-secret"
        AWS_ACCESS_KEY_ID               = "aws-key"
        AWS_SECRET_ACCESS_KEY           = "aws-secret"
        CLOUDFLARE_R2_API_TOKEN         = "acct-token"
        AWS_PROFILE                     = "stray-profile"
        AWS_SESSION_TOKEN               = "stray-session"
    }
    Assert-UploadEnvironment -Name $name -Result $result -AccessKeyId "cf-key" -SecretAccessKey "cf-secret"
    if (@(& $verifyRequests $result).Count -ne 0) { throw "${name}: the token was verified although explicit keys were set." }
    Write-Output "PASS: $name"

    foreach ($half in @("CLOUDFLARE_R2_ACCESS_KEY_ID", "CLOUDFLARE_R2_SECRET_ACCESS_KEY")) {
        $missing = if ($half -eq "CLOUDFLARE_R2_ACCESS_KEY_ID") { "CLOUDFLARE_R2_SECRET_ACCESS_KEY" } else { "CLOUDFLARE_R2_ACCESS_KEY_ID" }
        $name = "partial CLOUDFLARE_R2 pair ($half only) fails"
        $result = Invoke-Case -Name $name -ExpectError "Only half of the R2 S3 key pair is set: $missing is missing" -Environment @{
            $half                   = "cf-half"
            AWS_ACCESS_KEY_ID       = "aws-key"
            AWS_SECRET_ACCESS_KEY   = "aws-secret"
            CLOUDFLARE_R2_API_TOKEN = "acct-token"
        }
        if ($result.AwsCalls.Count -ne 0) { throw "${name}: aws ran." }
        if (@(& $verifyRequests $result).Count -ne 0) { throw "${name}: the token was verified." }
        Write-Output "PASS: $name"
    }

    $name = "explicit AWS pair fallback"
    $result = Invoke-Case -Name $name -Environment @{
        AWS_ACCESS_KEY_ID       = "aws-key"
        AWS_SECRET_ACCESS_KEY   = "aws-secret"
        CLOUDFLARE_R2_API_TOKEN = "acct-token"
        AWS_PROFILE             = "stray-profile"
    }
    Assert-UploadEnvironment -Name $name -Result $result -AccessKeyId "aws-key" -SecretAccessKey "aws-secret"
    if (@(& $verifyRequests $result).Count -ne 0) { throw "${name}: the token was verified although AWS keys were set." }
    Write-Output "PASS: $name"

    $name = "account token derivation"
    $result = Invoke-Case -Name $name -Environment @{ CLOUDFLARE_R2_API_TOKEN = "acct-token" }
    Assert-UploadEnvironment -Name $name -Result $result -AccessKeyId "acct-token-id" -SecretAccessKey (Get-ExpectedSecret "acct-token")
    $verify = @(& $verifyRequests $result)
    if ($verify.Count -ne 1 -or $verify[0] -ne "/accounts/$accountId/tokens/verify|Bearer acct-token") {
        throw "${name}: unexpected verify requests: $($verify -join ', ')"
    }
    Write-Output "PASS: $name"

    $name = "user token derivation after account verify fails"
    $result = Invoke-Case -Name $name -Environment @{
        CLOUDFLARE_R2_API_TOKEN = "user-token"
        AWS_PROFILE             = "stray-profile"
        AWS_SESSION_TOKEN       = "stray-session"
        AWS_ACCESS_KEY_ID       = "lonely-aws-key"
    }
    Assert-UploadEnvironment -Name $name -Result $result -AccessKeyId "user-token-id" -SecretAccessKey (Get-ExpectedSecret "user-token")
    $verify = @(& $verifyRequests $result)
    if ($verify.Count -ne 2 -or
        $verify[0] -ne "/accounts/$accountId/tokens/verify|Bearer user-token" -or
        $verify[1] -ne "/user/tokens/verify|Bearer user-token") {
        throw "${name}: unexpected verify requests: $($verify -join ', ')"
    }
    Write-Output "PASS: $name"

    $name = "CLOUDFLARE_API_TOKEN alias"
    $result = Invoke-Case -Name $name -Environment @{ CLOUDFLARE_API_TOKEN = "acct-token" }
    Assert-UploadEnvironment -Name $name -Result $result -AccessKeyId "acct-token-id" -SecretAccessKey (Get-ExpectedSecret "acct-token")
    Write-Output "PASS: $name"

    # Pin the derivation to an externally computed value as well:
    # printf %s acct-token | sha256sum
    $name = "derived secret matches a known SHA-256"
    $knownSecret = "1df9ffe386ed0953319e7bcd98cc585f7afbe08c6d82ae86aea20b968c76e198"
    $result = Invoke-Case -Name $name -Environment @{ CLOUDFLARE_R2_API_TOKEN = "acct-token" }
    Assert-UploadEnvironment -Name $name -Result $result -AccessKeyId "acct-token-id" -SecretAccessKey $knownSecret
    Write-Output "PASS: $name"

    $name = "aws failure under derived credentials"
    $result = Invoke-Case -Name $name -ExpectError "may be a standard Cloudflare API token rather than an R2 API token\. Add CLOUDFLARE_R2_ACCESS_KEY_ID and CLOUDFLARE_R2_SECRET_ACCESS_KEY" -Environment @{
        CLOUDFLARE_R2_API_TOKEN = "acct-token"
        STUB_AWS_EXIT           = "1"
        AWS_PROFILE             = "stray-profile"
    }
    Write-Output "PASS: $name"

    $name = "aws failure under explicit credentials has no token hint"
    $result = Invoke-Case -Name $name -ExpectError "AWS CLI failed to upload .*\(exit 1\)\.$" -Environment @{
        CLOUDFLARE_R2_ACCESS_KEY_ID     = "cf-key"
        CLOUDFLARE_R2_SECRET_ACCESS_KEY = "cf-secret"
        STUB_AWS_EXIT                   = "1"
    }
    Write-Output "PASS: $name"

    $name = "token rejected by both verify endpoints"
    $result = Invoke-Case -Name $name -ExpectError "Could not verify CLOUDFLARE_R2_API_TOKEN" -Environment @{ CLOUDFLARE_R2_API_TOKEN = "bad-token" }
    if ($result.AwsCalls.Count -ne 0) { throw "${name}: aws ran." }
    if (@(& $verifyRequests $result).Count -ne 2) { throw "${name}: expected both verify endpoints to be tried." }
    Write-Output "PASS: $name"

    $name = "inactive token is not used"
    $result = Invoke-Case -Name $name -ExpectError "Could not verify CLOUDFLARE_R2_API_TOKEN" -Environment @{ CLOUDFLARE_R2_API_TOKEN = "disabled-token" }
    if ($result.AwsCalls.Count -ne 0) { throw "${name}: aws ran." }
    Write-Output "PASS: $name"

    $name = "no credentials at all"
    $result = Invoke-Case -Name $name -ExpectError "R2 S3 credentials are required" -Environment @{ AWS_ACCESS_KEY_ID = "lonely-aws-key" }
    if ($result.AwsCalls.Count -ne 0) { throw "${name}: aws ran." }
    Write-Output "PASS: $name"

    if (-not $IsWindows) {
        # A child started after the script returns must not inherit an empty
        # AWS_PROFILE or AWS_SESSION_TOKEN from the restore step.
        $name = "no empty AWS_PROFILE or AWS_SESSION_TOKEN left for later children"
        $null = Invoke-Case -Name $name -Environment @{ CLOUDFLARE_R2_API_TOKEN = "acct-token" }
        $childEnv = & sh -c 'env | grep "^AWS_" || true'
        if ($childEnv) { throw "${name}: child process still sees: $($childEnv -join ', ')" }
        Write-Output "PASS: $name"
    }
} finally {
    foreach ($variable in $managedNames) {
        Remove-Item -LiteralPath "Env:$variable" -ErrorAction SilentlyContinue
    }
    $httpListener.Stop()
    $httpListener.Close()
    $null = $server.EndInvoke($serverHandle)
    $server.Dispose()
    Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output "All Store installer upload credential tests passed."
