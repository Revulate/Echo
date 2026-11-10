<#
.SYNOPSIS
Snapshot a running process's virtual memory regions and working-set residency
(VirtualQueryEx plus QueryWorkingSetEx) and its largest allocations into a run
directory.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 2147483647)]
    [int]$TargetProcessId,

    [string]$RunDir = "",
    [string]$Label = "snapshot",
    [ValidateRange(1, 200)]
    [int]$TopAllocationCount = 30
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

$nativeSource = @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class EchoProcessMemoryMap
{
    private const uint PROCESS_QUERY_INFORMATION = 0x0400;
    private const uint PROCESS_VM_READ = 0x0010;
    private const uint MEM_COMMIT = 0x1000;

    [StructLayout(LayoutKind.Sequential)]
    private struct MEMORY_BASIC_INFORMATION64
    {
        public ulong BaseAddress;
        public ulong AllocationBase;
        public uint AllocationProtect;
        public uint Alignment1;
        public ulong RegionSize;
        public uint State;
        public uint Protect;
        public uint Type;
        public uint Alignment2;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PSAPI_WORKING_SET_EX_INFORMATION
    {
        public IntPtr VirtualAddress;
        public UIntPtr VirtualAttributes;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(uint access, bool inheritHandle, int processId);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern UIntPtr VirtualQueryEx(
        IntPtr process,
        IntPtr address,
        out MEMORY_BASIC_INFORMATION64 buffer,
        UIntPtr length);

    [DllImport("psapi.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryWorkingSetEx(
        IntPtr process,
        [In, Out] PSAPI_WORKING_SET_EX_INFORMATION[] info,
        int length);

    [DllImport("psapi.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetMappedFileName(
        IntPtr process,
        IntPtr address,
        StringBuilder fileName,
        uint size);

    public sealed class Region
    {
        public ulong BaseAddress { get; set; }
        public ulong AllocationBase { get; set; }
        public ulong RegionSize { get; set; }
        public ulong ResidentBytes { get; set; }
        public uint AllocationProtect { get; set; }
        public uint Protect { get; set; }
        public uint State { get; set; }
        public uint Type { get; set; }
        public string MappedPath { get; set; }
        public bool WorkingSetQuerySucceeded { get; set; }
    }

    public static Region[] Capture(int processId)
    {
        IntPtr process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, processId);
        if (process == IntPtr.Zero)
            throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcess failed");

        try
        {
            var regions = new List<Region>();
            ulong address = 0;
            ulong maximumAddress = 0x00007FFFFFFEFFFFUL;
            int mbiSize = Marshal.SizeOf(typeof(MEMORY_BASIC_INFORMATION64));
            int wsSize = Marshal.SizeOf(typeof(PSAPI_WORKING_SET_EX_INFORMATION));
            ulong pageSize = (ulong)Environment.SystemPageSize;
            const int ChunkPages = 8192;

            while (address < maximumAddress)
            {
                MEMORY_BASIC_INFORMATION64 mbi;
                UIntPtr result = VirtualQueryEx(
                    process,
                    new IntPtr(unchecked((long)address)),
                    out mbi,
                    new UIntPtr((uint)mbiSize));
                if (result == UIntPtr.Zero || mbi.RegionSize == 0)
                    break;

                ulong next = mbi.BaseAddress + mbi.RegionSize;
                if (mbi.State == MEM_COMMIT)
                {
                    ulong residentBytes = 0;
                    bool allQueriesSucceeded = true;
                    ulong pageCount = (mbi.RegionSize + pageSize - 1) / pageSize;
                    ulong pageOffset = 0;
                    while (pageOffset < pageCount)
                    {
                        int count = (int)Math.Min((ulong)ChunkPages, pageCount - pageOffset);
                        var entries = new PSAPI_WORKING_SET_EX_INFORMATION[count];
                        for (int i = 0; i < count; i++)
                        {
                            ulong pageAddress = mbi.BaseAddress + ((pageOffset + (ulong)i) * pageSize);
                            entries[i].VirtualAddress = new IntPtr(unchecked((long)pageAddress));
                        }
                        if (QueryWorkingSetEx(process, entries, wsSize * count))
                        {
                            for (int i = 0; i < count; i++)
                            {
                                if ((entries[i].VirtualAttributes.ToUInt64() & 1UL) != 0)
                                    residentBytes += pageSize;
                            }
                        }
                        else
                        {
                            allQueriesSucceeded = false;
                        }
                        pageOffset += (ulong)count;
                    }

                    var pathBuffer = new StringBuilder(32768);
                    uint pathLength = GetMappedFileName(
                        process,
                        new IntPtr(unchecked((long)mbi.BaseAddress)),
                        pathBuffer,
                        (uint)pathBuffer.Capacity);

                    regions.Add(new Region {
                        BaseAddress = mbi.BaseAddress,
                        AllocationBase = mbi.AllocationBase,
                        RegionSize = mbi.RegionSize,
                        ResidentBytes = residentBytes,
                        AllocationProtect = mbi.AllocationProtect,
                        Protect = mbi.Protect,
                        State = mbi.State,
                        Type = mbi.Type,
                        MappedPath = pathLength > 0 ? pathBuffer.ToString() : "",
                        WorkingSetQuerySucceeded = allQueriesSucceeded,
                    });
                }

                if (next <= address)
                    break;
                address = next;
            }
            return regions.ToArray();
        }
        finally
        {
            CloseHandle(process);
        }
    }
}
'@

if ($null -eq ("EchoProcessMemoryMap" -as [type])) {
    Add-Type -TypeDefinition $nativeSource -Language CSharp
}

function Get-MemoryTypeName([uint32]$Type) {
    switch ($Type) {
        0x00020000 { "private" }
        0x00040000 { "mapped" }
        0x01000000 { "image" }
        default { "unknown_0x{0:x}" -f $Type }
    }
}

function Get-ProtectionName([uint32]$Protect) {
    $base = $Protect -band 0xFF
    $name = switch ($base) {
        0x01 { "noaccess" }
        0x02 { "readonly" }
        0x04 { "readwrite" }
        0x08 { "writecopy" }
        0x10 { "execute" }
        0x20 { "execute_read" }
        0x40 { "execute_readwrite" }
        0x80 { "execute_writecopy" }
        default { "unknown_0x{0:x}" -f $base }
    }
    if (($Protect -band 0x100) -ne 0) { $name += "+guard" }
    if (($Protect -band 0x200) -ne 0) { $name += "+nocache" }
    if (($Protect -band 0x400) -ne 0) { $name += "+writecombine" }
    return $name
}

$repoRoot = (Resolve-Path (Join-Path (Split-Path -Parent $PSCommandPath) "..\..")).Path
if ([string]::IsNullOrWhiteSpace($RunDir)) {
    $RunDir = Join-Path $repoRoot ("benchmarks\results\{0}-process-memory-map" -f (Get-Date -Format "yyyyMMdd-HHmmss"))
}
$runPath = (New-Item -ItemType Directory -Force -Path $RunDir).FullName
$safeLabel = ($Label.ToLowerInvariant() -replace "[^a-z0-9._-]+", "-").Trim("-")
if ([string]::IsNullOrWhiteSpace($safeLabel)) { $safeLabel = "snapshot" }
$prefix = "memory-map-$safeLabel"
$rawPath = Join-Path $runPath "$prefix.raw.csv"
$summaryPath = Join-Path $runPath "$prefix.summary.json"

$process = Get-Process -Id $TargetProcessId -ErrorAction Stop
$process.Refresh()
$capturedAt = [DateTime]::UtcNow
$regions = @([EchoProcessMemoryMap]::Capture($TargetProcessId))

$modules = @()
try {
    $modules = @($process.Modules | ForEach-Object {
        [pscustomobject]@{
            start = [uint64]$_.BaseAddress.ToInt64()
            end = [uint64]$_.BaseAddress.ToInt64() + [uint64]$_.ModuleMemorySize
            name = $_.ModuleName
            path = $_.FileName
        }
    })
} catch {
    $modules = @()
}

$rows = foreach ($region in $regions) {
    $module = $modules | Where-Object {
        [uint64]$region.BaseAddress -ge $_.start -and [uint64]$region.BaseAddress -lt $_.end
    } | Select-Object -First 1
    [pscustomobject][ordered]@{
        base_address = "0x{0:x16}" -f [uint64]$region.BaseAddress
        allocation_base = "0x{0:x16}" -f [uint64]$region.AllocationBase
        region_size_bytes = [uint64]$region.RegionSize
        resident_bytes = [uint64]$region.ResidentBytes
        type = Get-MemoryTypeName $region.Type
        protection = Get-ProtectionName $region.Protect
        allocation_protection = Get-ProtectionName $region.AllocationProtect
        mapped_path = [string]$region.MappedPath
        module_name = if ($null -ne $module) { $module.name } else { "" }
        module_path = if ($null -ne $module) { $module.path } else { "" }
        working_set_query_succeeded = [bool]$region.WorkingSetQuerySucceeded
    }
}
$rows | Export-Csv -NoTypeInformation -Encoding utf8 -Path $rawPath

$byType = [ordered]@{}
foreach ($group in ($rows | Group-Object type)) {
    $byType[$group.Name] = [ordered]@{
        region_count = $group.Count
        committed_bytes = [uint64](($group.Group | Measure-Object region_size_bytes -Sum).Sum)
        resident_bytes = [uint64](($group.Group | Measure-Object resident_bytes -Sum).Sum)
    }
}

$byProtection = [ordered]@{}
foreach ($group in ($rows | Group-Object protection)) {
    $byProtection[$group.Name] = [ordered]@{
        region_count = $group.Count
        committed_bytes = [uint64](($group.Group | Measure-Object region_size_bytes -Sum).Sum)
        resident_bytes = [uint64](($group.Group | Measure-Object resident_bytes -Sum).Sum)
    }
}

$topAllocations = @($rows | Group-Object allocation_base | ForEach-Object {
    $firstPath = $_.Group | Where-Object { -not [string]::IsNullOrWhiteSpace($_.mapped_path) } | Select-Object -First 1
    $firstModule = $_.Group | Where-Object { -not [string]::IsNullOrWhiteSpace($_.module_name) } | Select-Object -First 1
    [pscustomobject][ordered]@{
        allocation_base = $_.Name
        region_count = $_.Count
        type = ($_.Group.type | Select-Object -Unique) -join ","
        committed_bytes = [uint64](($_.Group | Measure-Object region_size_bytes -Sum).Sum)
        resident_bytes = [uint64](($_.Group | Measure-Object resident_bytes -Sum).Sum)
        mapped_path = if ($null -ne $firstPath) { $firstPath.mapped_path } else { "" }
        module_name = if ($null -ne $firstModule) { $firstModule.module_name } else { "" }
    }
} | Sort-Object resident_bytes -Descending | Select-Object -First $TopAllocationCount)

$process.Refresh()
$residentTotal = [uint64](($rows | Measure-Object resident_bytes -Sum).Sum)
$committedTotal = [uint64](($rows | Measure-Object region_size_bytes -Sum).Sum)
$failedRegionCount = @($rows | Where-Object { -not $_.working_set_query_succeeded }).Count
$summary = [ordered]@{
    schema_version = 1
    tool = "VirtualQueryEx plus QueryWorkingSetEx"
    process_id = $TargetProcessId
    process_name = $process.ProcessName
    captured_at_utc = $capturedAt.ToString("o")
    label = $safeLabel
    page_size_bytes = [Environment]::SystemPageSize
    process_working_set_bytes = [uint64]$process.WorkingSet64
    process_private_memory_bytes = [uint64]$process.PrivateMemorySize64
    virtual_query_committed_bytes = $committedTotal
    working_set_query_resident_bytes = $residentTotal
    working_set_coverage_percent = if ($process.WorkingSet64 -gt 0) {
        ($residentTotal / [double]$process.WorkingSet64) * 100.0
    } else { $null }
    region_count = $rows.Count
    failed_working_set_region_count = $failedRegionCount
    by_type = $byType
    by_protection = $byProtection
    top_allocations_by_resident_bytes = $topAllocations
    raw_artifact = $rawPath
    interpretation = "Private regions include Rust/mimalloc, GPUI, stacks, and driver-owned process allocations; this is lower-level OS attribution, not allocator call-site attribution."
}
$summary | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 -Path $summaryPath
Write-Output $summaryPath
