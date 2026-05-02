param(
    [string]$InstallDir = $env:CLAUDEX_INSTALL_DIR,
    [string]$Repo = $env:CLAUDEX_REPO,
    [string]$Profile = $env:CLAUDEX_PROFILE,
    [switch]$Yes,
    [switch]$NoSetup,
    [switch]$NoPath,
    [switch]$NoSourceFallback,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

if (-not $Repo) { $Repo = "pilc80/claudex" }
if (-not $InstallDir) { $InstallDir = Join-Path $HOME ".local\bin" }
if (-not $Profile) { $Profile = "codex-sub" }

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # PowerShell 7+ on modern .NET may ignore this legacy networking setting.
}

function Assert-ValidRepo {
    param([string]$Name)
    if ($Name -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') {
        throw "Repository must use OWNER/REPO format, got: $Name"
    }
}

function Test-Interactive {
    try {
        return [Environment]::UserInteractive -and -not [Console]::IsInputRedirected
    } catch {
        return $false
    }
}

function Ask-YesNo {
    param([string]$Question, [bool]$Default = $false)
    if ($Yes) { return $true }
    if (-not (Test-Interactive)) { return $Default }
    $suffix = if ($Default) { "[Y/n]" } else { "[y/N]" }
    $answer = Read-Host "$Question $suffix"
    if ([string]::IsNullOrWhiteSpace($answer)) { return $Default }
    return $answer -match '^(y|yes|true|1)$'
}

function Read-InstallerInput {
    param([string]$Question, [string]$Default)
    if ($Yes -or -not (Test-Interactive)) {
        return $Default
    }

    $answer = Read-Host "$Question [$Default]"
    if ([string]::IsNullOrWhiteSpace($answer)) {
        return $Default
    }
    return $answer
}

function Resolve-FullPath {
    param([string]$Path)
    return [IO.Path]::GetFullPath($Path)
}

function Test-InstallerTempSource {
    param([string]$Path)
    if (-not $Path) {
        return $false
    }
    $tempRoot = Resolve-FullPath ([IO.Path]::GetTempPath())
    $fullPath = Resolve-FullPath $Path
    return $fullPath.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
        ((Split-Path -Leaf $fullPath) -like "claudex-*.exe")
}

function Invoke-InstallerRestMethod {
    param([string]$Uri)
    $params = @{
        Uri = $Uri
        UserAgent = "claudex-installer"
        ErrorAction = "Stop"
    }
    if ((Get-Command Invoke-RestMethod).Parameters.ContainsKey("UseBasicParsing")) {
        $params.UseBasicParsing = $true
    }
    Invoke-RestMethod @params
}

function Invoke-InstallerWebRequest {
    param([string]$Uri, [string]$OutFile)
    $params = @{
        Uri = $Uri
        OutFile = $OutFile
        UserAgent = "claudex-installer"
        ErrorAction = "Stop"
    }
    if ((Get-Command Invoke-WebRequest).Parameters.ContainsKey("UseBasicParsing")) {
        $params.UseBasicParsing = $true
    }
    Invoke-WebRequest @params
}

function Invoke-Native {
    param([string]$Name, [string]$FilePath, [string[]]$Arguments = @())
    $global:LASTEXITCODE = 0
    $output = & $FilePath @Arguments
    if ($null -ne $output) {
        $output | ForEach-Object { Write-Host $_ }
    }
    if ($global:LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $global:LASTEXITCODE"
    }
}

function Get-Target {
    if ((Get-Variable IsWindows -ErrorAction SilentlyContinue) -and -not $IsWindows) {
        throw "install.ps1 is for Windows. Use install.sh on macOS/Linux."
    }

    $architecture = $env:PROCESSOR_ARCHITEW6432
    if (-not $architecture) {
        $architecture = $env:PROCESSOR_ARCHITECTURE
    }

    switch ($architecture) {
        "AMD64" { return "x86_64-pc-windows-msvc" }
        "x86_64" { return "x86_64-pc-windows-msvc" }
        "ARM64" { throw "Windows ARM64 is not supported by current release assets." }
        default { throw "Unsupported Windows architecture: $architecture" }
    }
}

function Add-UserPath {
    param([string]$Dir)
    if ($NoPath) {
        Write-Host "Skipping PATH update. Add this directory manually: $Dir"
        return
    }
    if (-not (Ask-YesNo "Add install directory to your User PATH?" $true)) {
        Write-Host "Add this directory to PATH manually: $Dir"
        return
    }
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @()
    if ($userPath) {
        $entries = $userPath -split ';' | Where-Object { $_ }
    }
    if ($entries -notcontains $Dir) {
        $newPath = (@($entries) + $Dir) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        $env:Path = "$env:Path;$Dir"
        Write-Host "Added $Dir to the user PATH. Open a new terminal to inherit it."
    }
}

function Verify-Checksum {
    param([string]$File, [string]$ChecksumFile)
    $expected = ((Get-Content -Raw -LiteralPath $ChecksumFile).Trim() -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $File).Hash.ToLowerInvariant()
    if ($expected -ne $actual) {
        throw "Checksum mismatch for $(Split-Path -Leaf $File). Expected $expected, got $actual"
    }
    Write-Host "Verified SHA256: $actual"
}

function Backup-Existing {
    param([string]$Path)
    if (Test-Path $Path) {
        $stamp = Get-Date -Format "yyyyMMddHHmmss"
        $backup = "$Path.backup.$stamp"
        Copy-Item -LiteralPath $Path -Destination $backup -Force
        Write-Host "Backed up existing binary to $backup"
    }
}

function Maybe-StopRunningProxy {
    param([string]$Path)
    if (-not (Test-Path $Path)) {
        return
    }

    $status = (& $Path @("proxy", "status") 2>$null) -join "`n"
    if ($status -match "^Proxy is running") {
        Write-Host $status
        if (Ask-YesNo "Stop the running claudex proxy so the new binary can be replaced?") {
            Invoke-Native "claudex-config proxy stop" $Path @("proxy", "stop")
        } else {
            throw "Running claudex proxy must be stopped before replacing $Path"
        }
    }
}

function Install-FromRelease {
    param([string]$Target)

    $manifestUrl = "https://github.com/$Repo/releases/latest/download/claudex-release-manifest.json"
    $url = $null
    $checksumUrl = $null
    $expectedSha = $null
    $archiveName = $null

    try {
        $manifest = Invoke-InstallerRestMethod $manifestUrl
        $artifact = $manifest.artifacts | Where-Object { $_.target -eq $Target } | Select-Object -First 1
        if (-not $artifact) {
            throw "Release manifest does not contain target $Target"
        }
        $url = $artifact.url
        $expectedSha = $artifact.sha256
        $archiveName = $artifact.name
        Write-Host "Using release manifest: $manifestUrl"
    } catch {
        if ($DryRun) {
            Write-Host "Dry run: release manifest was not available; would query GitHub Releases API as fallback"
            Write-Host "Dry run: would download, verify, unpack, and install to $(Join-Path $InstallDir 'claudex.exe')"
            Write-Host "Dry run: would install claudex-config.exe to $(Join-Path $InstallDir 'claudex-config.exe')"
            return $null
        }
        $release = Invoke-InstallerRestMethod "https://api.github.com/repos/$Repo/releases/latest"
        $version = $release.tag_name
        if (-not $version) {
            throw "Failed to determine latest release"
        }
        Write-Host "Latest release: $version"
        $archiveName = "claudex-$version-$Target.zip"
        $url = "https://github.com/$Repo/releases/download/$version/$archiveName"
        $checksumUrl = "$url.sha256"
    }

    Write-Host "Downloading: $url"
    if ($expectedSha) {
        Write-Host "Expected SHA256: $expectedSha"
    } else {
        Write-Host "Checksum:   $checksumUrl"
    }

    if ($DryRun) {
        Write-Host "Dry run: would download, verify, unpack, and install to $(Join-Path $InstallDir 'claudex.exe')"
        Write-Host "Dry run: would install claudex-config.exe to $(Join-Path $InstallDir 'claudex-config.exe')"
        return $null
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("claudex-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        $archive = Join-Path $tmp $archiveName
        $checksum = Join-Path $tmp "$archiveName.sha256"

        Invoke-InstallerWebRequest $url $archive
        if ($expectedSha) {
            Set-Content -LiteralPath $checksum -Value "$expectedSha  $archiveName"
        } else {
            Invoke-InstallerWebRequest $checksumUrl $checksum
        }

        Verify-Checksum $archive $checksum
        Expand-Archive -LiteralPath $archive -DestinationPath $tmp -Force
        $source = Join-Path $tmp "claudex.exe"
        if (-not (Test-Path $source)) {
            throw "Archive did not contain claudex.exe"
        }

        $stableSource = Join-Path ([IO.Path]::GetTempPath()) ("claudex-" + [Guid]::NewGuid() + ".exe")
        Copy-Item -LiteralPath $source -Destination $stableSource -Force
        return $stableSource
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-FromSource {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -or -not (Get-Command git -ErrorAction SilentlyContinue)) {
        throw "cargo and git are required for source install fallback"
    }
    $source = Join-Path $HOME ".cargo\bin\claudex.exe"
    $configSource = Join-Path $HOME ".cargo\bin\claudex-config.exe"
    if ($DryRun) {
        Write-Host "Dry run: would run cargo install --git https://github.com/$Repo --force"
        return $source
    }
    if (Test-Path $configSource) {
        Maybe-StopRunningProxy $configSource
    } else {
        Maybe-StopRunningProxy $source
    }
    Invoke-Native "cargo install" "cargo" @("install", "--git", "https://github.com/$Repo", "--force")
    if (-not (Test-Path $source)) {
        throw "cargo install finished but $source was not found"
    }
    return $source
}

function Deploy-Binary {
    param([string]$source)

    $dest = Join-Path $InstallDir "claudex.exe"
    $configDest = Join-Path $InstallDir "claudex-config.exe"
    if ($DryRun) {
        Write-Host "Dry run: would install $source to $dest"
        Write-Host "Dry run: would install claudex-config.exe to $configDest"
        return $dest
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    if (Test-Path $configDest) {
        Maybe-StopRunningProxy $configDest
    } else {
        Maybe-StopRunningProxy $dest
    }
    if ((Test-Path $dest) -and ((Resolve-FullPath $source) -eq (Resolve-FullPath $dest))) {
        Write-Host "Skipping copy because source and destination are the same file: $dest"
    } else {
        Backup-Existing $dest
        Copy-Item -LiteralPath $source -Destination $dest -Force
    }
    if ((Test-Path $configDest) -and ((Resolve-FullPath $source) -eq (Resolve-FullPath $configDest))) {
        Write-Host "Skipping copy because source and destination are the same file: $configDest"
    } else {
        Backup-Existing $configDest
        Copy-Item -LiteralPath $source -Destination $configDest -Force
    }
    Add-UserPath $InstallDir

    Write-Host ""
    Write-Host "Installed claudex to $dest"
    Write-Host "Installed claudex-config to $configDest"
    Invoke-Native "claudex-config --version" $configDest @("--version")
    return $dest
}

function Maybe-SetupChatGpt {
    param([string]$Path)

    if ($NoSetup -or $DryRun) {
        return
    }
    if (-not (Ask-YesNo "Set up a ChatGPT/Codex OAuth profile now?")) {
        return
    }

    $script:Profile = Read-InstallerInput "Profile name" $Profile

    $args = @("auth", "login", "chatgpt", "--profile", $Profile)
    if (Ask-YesNo "Use headless device-code login?") { $args += "--headless" }
    if (Ask-YesNo "Force browser/device login instead of reusing existing credentials?") {
        $args += "--force"
    }

    $configPath = Join-Path (Split-Path -Parent $Path) "claudex-config.exe"
    Invoke-Native "claudex-config auth login" $configPath $args
    Write-Host ""
    Write-Host "Run Claude Code through this profile with:"
    Write-Host "  `$env:CLAUDEX_PROFILE = `"$Profile`"; claudex"
}

Write-Host "Claudex Windows Installer"
Write-Host "========================="
Write-Host "Repository: $Repo"
Write-Host "Install dir: $InstallDir"
Write-Host ""

Assert-ValidRepo $Repo

if (-not (Get-Command claude -ErrorAction SilentlyContinue)) {
    Write-Host "Warning: Claude Code was not found in PATH."
}

$target = Get-Target
Write-Host "Detected target: $target"

$source = $null
try {
    $source = Install-FromRelease $target
} catch {
    Write-Host ""
    Write-Host "Release install failed: $($_.Exception.Message)"
    if ($NoSourceFallback) {
        throw
    }
    if (Ask-YesNo "Release install failed. Try source install with cargo instead?" $true) {
        $source = Install-FromSource
    } else {
        throw "Release install failed. Try: cargo install --git https://github.com/$Repo --force"
    }
}

try {
    if ($DryRun) {
        Write-Host ""
        Write-Host "Dry run complete."
        exit 0
    }

    $installed = Deploy-Binary $source
    Maybe-SetupChatGpt $installed
} finally {
    if (Test-InstallerTempSource $source) {
        Remove-Item -LiteralPath $source -Force -ErrorAction SilentlyContinue
    }
}
