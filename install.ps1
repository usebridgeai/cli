# Bridge CLI installer for Windows
# Usage: irm https://raw.githubusercontent.com/usebridgeai/cli/main/install.ps1 | iex
$ErrorActionPreference = "Stop"

$Repo = "usebridgeai/cli"
$InstallDir = "$env:USERPROFILE\.bridge\bin"
$BinaryName = "bridge.exe"
# Detect architecture
$Arch = $env:PROCESSOR_ARCHITECTURE
if ($Arch -eq "ARM64") {
    Write-Error "Windows ARM64 is not yet supported. See https://github.com/$Repo/issues"
    exit 1
}
$Target = "x86_64-pc-windows-msvc"

# Get latest release tag
Write-Host "Fetching latest Bridge release..."
$Release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
$Tag = $Release.tag_name
if (-not $Tag) {
    Write-Error "Could not determine latest release. Check https://github.com/$Repo/releases"
    exit 1
}

$DownloadUrl = "https://github.com/$Repo/releases/download/$Tag/bridge-${Target}.zip"
$ChecksumsUrl = "https://github.com/$Repo/releases/download/$Tag/checksums.txt"

# Download binary
$TmpDir = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "bridge-install-$(Get-Random)")
try {
    Write-Host "Downloading Bridge $Tag for Windows..."
    $ZipPath = Join-Path $TmpDir "bridge.zip"
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $ZipPath -UseBasicParsing

    # Verify checksum (fail hard — never install an unverified binary)
    Write-Host "Verifying checksum..."
    $ChecksumsPath = Join-Path $TmpDir "checksums.txt"
    Invoke-WebRequest -Uri $ChecksumsUrl -OutFile $ChecksumsPath -UseBasicParsing

    $ExpectedLine = Get-Content $ChecksumsPath | Where-Object { $_ -match "bridge-${Target}.zip" }
    if (-not $ExpectedLine) {
        Write-Error "No checksum found for bridge-${Target}.zip in checksums.txt. Aborting."
        exit 1
    }

    $Expected = ($ExpectedLine -split '\s+')[0]
    $Actual = (Get-FileHash -Algorithm SHA256 $ZipPath).Hash.ToLower()
    if ($Expected -ne $Actual) {
        Write-Error "Checksum mismatch! The download may be corrupted or tampered with.`n  Expected: $Expected`n  Got:      $Actual"
        exit 1
    }
    Write-Host "Checksum verified."

    # Extract and install
    Write-Host "Installing to $InstallDir..."
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force
    Copy-Item (Join-Path $TmpDir $BinaryName) (Join-Path $InstallDir $BinaryName) -Force

    # Add to PATH (idempotent - read current, append, write back)
    $CurrentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($CurrentPath -notlike "*$InstallDir*") {
        $NewPath = "$CurrentPath;$InstallDir"
        [Environment]::SetEnvironmentVariable("PATH", $NewPath, "User")
        Write-Host "Added $InstallDir to user PATH."
    }

    # Install PowerShell completions
    try {
        $Bridge = Join-Path $InstallDir $BinaryName
        $CompletionScript = & $Bridge completions powershell
        $ProfileDir = Split-Path $PROFILE
        if (-not (Test-Path $ProfileDir)) {
            New-Item -ItemType Directory -Path $ProfileDir -Force | Out-Null
        }
        if (-not (Test-Path $PROFILE) -or -not (Select-String -Path $PROFILE -Pattern "bridge" -Quiet)) {
            Add-Content -Path $PROFILE -Value "`n# Bridge CLI completions"
            Add-Content -Path $PROFILE -Value $CompletionScript
        }
    } catch {
        # Silently skip if completions fail
    }

    Write-Host ""
    Write-Host "Bridge $Tag installed successfully!" -ForegroundColor Green
    Write-Host ""
    Write-Host "  Location: $InstallDir\$BinaryName"
    Write-Host ""
    Write-Host "  Get started:"
    Write-Host "    bridge init"
    Write-Host "    bridge connect file://./data --as files"
    Write-Host "    bridge ls --from files"
    Write-Host ""
    Write-Host "  Restart your terminal for PATH changes to take effect."
    Write-Host ""
    Write-Host "  Uninstall:"
    Write-Host "    Remove-Item -Recurse $InstallDir"

} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}
