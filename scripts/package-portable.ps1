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

$files = @([System.IO.Directory]::EnumerateFiles($Source, '*', [System.IO.SearchOption]::AllDirectories))
$executable = [System.IO.Path]::GetFullPath((Join-Path $Source $EntryName))
if ($files.Count -ne 1 -or $files[0] -ine $executable) {
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
    $file = [System.IO.File]::OpenRead((Join-Path $Source $EntryName))
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
