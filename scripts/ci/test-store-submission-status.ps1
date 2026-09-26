#Requires -Version 5.1

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$scriptPath = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) "scripts\validate-store-submission-status.ps1"

function Assert-Fails {
    param(
        [string]$StatusOutput,
        [bool]$RecoverFailedUpload = $false
    )
    try {
        & $scriptPath -StatusOutput $StatusOutput -RecoverFailedUpload:$RecoverFailedUpload
    } catch {
        return
    }
    throw "Store status validation unexpectedly succeeded."
}

$failedUpload = @'
Retrieving submission status
{
  "ResponseData": { "IsReady": false, "OngoingSubmissionId": null },
  "IsSuccess": true,
  "Errors": [
    { "Code": "modulenotready", "Target": "packages" },
    { "Code": "packageuploaderror", "Message": "Package Upload Status for Current Request - ProcessFailed", "Target": "packages" }
  ]
}
'@
$ready = @'
Retrieving submission status
{
  "ResponseData": { "IsReady": true, "OngoingSubmissionId": null },
  "IsSuccess": true,
  "Errors": []
}
'@
$ongoingFailedUpload = $failedUpload.Replace('"OngoingSubmissionId": null', '"OngoingSubmissionId": "submission-123"')
$unsuccessful = $ready.Replace('"IsSuccess": true', '"IsSuccess": false')

Assert-Fails -StatusOutput $failedUpload
& $scriptPath -StatusOutput $failedUpload -RecoverFailedUpload
Assert-Fails -StatusOutput $ongoingFailedUpload -RecoverFailedUpload $true
& $scriptPath -StatusOutput $ready
Assert-Fails -StatusOutput $ready -RecoverFailedUpload $true
Assert-Fails -StatusOutput $unsuccessful
Assert-Fails -StatusOutput 'Retrieving submission status'

Write-Host "Store status handling distinguishes failed uploads from ready submissions."
