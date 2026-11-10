param()
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$python = Join-Path $projectRoot '.venv\Scripts\pythonw.exe'
$icon = Join-Path $projectRoot 'src\image_atlas\branding\image-atlas.ico'
$viewer = Join-Path $projectRoot 'viewer\target\release\image-atlas-viewer.exe'
foreach ($path in @($python, $icon, $viewer)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required file is missing: $path. Install ImageAtlas and build its release viewer first."
    }
}
$folder = Join-Path ([Environment]::GetFolderPath('Programs')) 'ImageAtlas'
New-Item -ItemType Directory -Path $folder -Force | Out-Null
$path = Join-Path $folder 'ImageAtlas.lnk'
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($path)
$shortcut.TargetPath = $python
$shortcut.Arguments = '-m image_atlas.launcher'
$shortcut.WorkingDirectory = $projectRoot
$shortcut.IconLocation = "$icon,0"
$shortcut.Description = 'ImageAtlas — find, explore, and organize your image library'
$shortcut.Save()
Write-Output "Installed Start menu shortcut: $path"
