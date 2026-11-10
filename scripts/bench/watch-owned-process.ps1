[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 2147483647)]
    [int]$TargetProcessId,

    [Parameter(Mandatory = $true)]
    [int64]$ExpectedStartTimeFileTimeUtc,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 86400)]
    [int]$DelaySeconds,

    [ValidateRange(1, 300)]
    [int]$ExitWaitSeconds = 15,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

Start-Sleep -Seconds $DelaySeconds

$result = [ordered]@{
    schema_version = 1
    process_id = $TargetProcessId
    expected_start_time_file_time_utc = $ExpectedStartTimeFileTimeUtc
    delay_seconds = $DelaySeconds
    inspected_at_utc = [DateTime]::UtcNow.ToString("o")
    matching_process_found = $false
    close_requested = $false
    force_killed = $false
    final_state = "not-found"
}

$process = Get-Process -Id $TargetProcessId -ErrorAction SilentlyContinue
if ($null -ne $process) {
    $actualStart = $process.StartTime.ToUniversalTime().ToFileTimeUtc()
    if ($actualStart -eq $ExpectedStartTimeFileTimeUtc) {
        $result.matching_process_found = $true
        $result.process_name = $process.ProcessName
        $result.close_requested = $true
        $null = $process.CloseMainWindow()
        if (-not $process.WaitForExit($ExitWaitSeconds * 1000)) {
            $result.force_killed = $true
            $process.Kill($true)
            $process.WaitForExit()
        }
        $result.final_state = "closed"
    } else {
        $result.final_state = "pid-reused-start-time-mismatch"
        $result.actual_start_time_file_time_utc = $actualStart
    }
}

$directory = Split-Path -Parent ([System.IO.Path]::GetFullPath($OutputPath))
New-Item -ItemType Directory -Force -Path $directory | Out-Null
$result | ConvertTo-Json -Depth 4 | Set-Content -Path $OutputPath -Encoding utf8
