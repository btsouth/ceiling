#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$StatusOutput,

    [switch]$RecoverFailedUpload
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# The CLI writes a progress line before its JSON response.
$jsonStart = $StatusOutput.IndexOf('{')
if ($jsonStart -lt 0) {
    throw "Microsoft Store submission status did not contain JSON."
}
$status = $StatusOutput.Substring($jsonStart) | ConvertFrom-Json -ErrorAction Stop
if ($status.IsSuccess -ne $true) {
    throw "Microsoft Store reported an unsuccessful submission status."
}

$uploadFailed = @($status.Errors | Where-Object { $_.Code -eq 'packageuploaderror' }).Count -gt 0
if ($RecoverFailedUpload) {
    if (-not $uploadFailed) {
        throw "Recovery was requested, but the Store did not report a failed package upload."
    }
    if (-not [string]::IsNullOrWhiteSpace($status.ResponseData.OngoingSubmissionId)) {
        throw "Cannot replace a failed package while a submission is ongoing."
    }
    Write-Host "::warning::A failed Store package upload will be replaced using --skipInitialPolling."
} elseif ($uploadFailed) {
    throw "Microsoft Store package upload failed. Inspect Partner Center before retrying."
}
