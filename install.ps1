# Installs the latest Talyxel Sound release on Windows, no Rust toolchain needed:
#
#   irm https://raw.githubusercontent.com/RenzoWit35/talyxel-sound/master/install.ps1 | iex
#
# $env:TALYXEL_INSTALL_DIR  folder for talyxel.exe (default: %LOCALAPPDATA%\Programs\Talyxel Sound)
# $env:TALYXEL_REPO         GitHub repo to install from (default: RenzoWit35/talyxel-sound)

# Runs in its own scope so `irm | iex` doesn't leave variables or preferences in the caller's session.
& {
    $ErrorActionPreference = 'Stop'
    # The progress bar makes Invoke-WebRequest very slow on Windows PowerShell 5.1.
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = if ($env:TALYXEL_REPO) { $env:TALYXEL_REPO } else { 'RenzoWit35/talyxel-sound' }
    $dir = if ($env:TALYXEL_INSTALL_DIR) { $env:TALYXEL_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\Talyxel Sound' }
    $target = 'x86_64-pc-windows-msvc'

    try {
        $release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'talyxel-installer' }
    } catch {
        throw "No release found at https://github.com/$repo/releases (none published yet, the repository is private, or GitHub could not be reached)."
    }
    $asset = $release.assets | Where-Object { $_.name -like "*-$target.zip" } | Select-Object -First 1
    if (-not $asset) { throw "Release $($release.tag_name) has no Windows download." }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("talyxel-" + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "Downloading Talyxel Sound $($release.tag_name)..."
        $zip = Join-Path $tmp $asset.name
        Invoke-WebRequest $asset.browser_download_url -OutFile $zip -UseBasicParsing
        Expand-Archive $zip -DestinationPath $tmp -Force
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        Copy-Item (Join-Path $tmp 'talyxel.exe') (Join-Path $dir 'talyxel.exe') -Force
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    Write-Host "Installed Talyxel Sound $($release.tag_name) to $dir"

    # Add the folder to the user PATH, keeping the registry value's %VAR% entries unexpanded.
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    $userPath = [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    $entries = @($userPath -split ';' | Where-Object { $_ })
    if ($entries -notcontains $dir) {
        $key.SetValue('Path', (($entries + $dir) -join ';'), 'ExpandString')
        # Setting and clearing a variable through .NET broadcasts the change to Explorer,
        # so terminals opened from now on see the new PATH.
        [Environment]::SetEnvironmentVariable('TALYXEL_INSTALLER', '1', 'User')
        [Environment]::SetEnvironmentVariable('TALYXEL_INSTALLER', $null, 'User')
        $env:Path = "$env:Path;$dir"
        Write-Host "Added $dir to your PATH. Open a new terminal to use 'talyxel' everywhere."
    }
    $key.Close()
    Write-Host "Run 'talyxel' to start."
}
