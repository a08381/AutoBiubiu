param(
    [Parameter(Mandatory = $true)][string]$Source,
    [Parameter(Mandatory = $true)][string]$Destination,
    [Parameter(Mandatory = $true)][string]$EntryName
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Get-StreamHash($Stream) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [BitConverter]::ToString($sha.ComputeHash($Stream))
    } finally {
        $sha.Dispose()
    }
}

# .NET Framework 枚举文件时可能保留输入路径的写法，先统一暂存目录路径。
$Source = [System.IO.Path]::GetFullPath($Source)
$executable = [System.IO.Path]::GetFullPath([System.IO.Path]::Combine($Source, $EntryName))
$files = @([System.IO.Directory]::EnumerateFiles($Source, '*', [System.IO.SearchOption]::AllDirectories))
if ($files.Count -ne 1 -or -not [string]::Equals($files[0], $executable, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Unexpected portable ZIP contents; expected only $EntryName"
}
$output = [System.IO.Compression.ZipFile]::Open($Destination, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    # .NET Framework CreateFromDirectory writes backslashes; explicit entry names keep standard ZIP paths.
    [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
        $output, $executable, $EntryName, [System.IO.Compression.CompressionLevel]::Optimal
    )
} finally {
    $output.Dispose()
}
$archive = [System.IO.Compression.ZipFile]::OpenRead($Destination)
try {
    if ($archive.Entries.Count -ne 1 -or $archive.Entries[0].FullName -cne $EntryName) {
        throw "Unexpected portable ZIP contents; expected only $EntryName"
    }
    $file = [System.IO.File]::OpenRead($executable)
    $entry = $archive.Entries[0].Open()
    try {
        if ((Get-StreamHash $file) -cne (Get-StreamHash $entry)) {
            throw 'The archived executable does not match the built executable'
        }
    } finally {
        $file.Dispose()
        $entry.Dispose()
    }
    Write-Output "Portable ZIP verified: $EntryName ($($archive.Entries[0].Length) bytes)"
} finally {
    $archive.Dispose()
}
