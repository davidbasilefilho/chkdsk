$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Import-Module (Join-Path $PSHOME 'Modules/Storage/Storage.psd1')
$disk = @(Storage\Get-Disk -Path $env:DISKSONAR_DISK_INTERFACE)
if ($disk.Count -ne 1 -or $disk[0].Path -ne $env:DISKSONAR_DISK_INTERFACE) { exit 1 }
$counter = @(Storage\Get-StorageReliabilityCounter -Disk $disk[0])
if ($counter.Count -ne 1) { exit 1 }
$result = [ordered]@{}
foreach ($name in @('Wear','PowerOnHours','Temperature','ReadErrorsUncorrected','WriteErrorsUncorrected')) {
    $property = $counter[0].CimInstanceProperties[$name]
    if ($null -ne $property -and $null -ne $property.Value) { $result[$name] = $property.Value }
}
$result | ConvertTo-Json -Compress
