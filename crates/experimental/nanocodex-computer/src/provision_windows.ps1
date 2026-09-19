# Uses the same Store product and package identity as upstream codex/cli.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$package = Get-AppxPackage -Name 'OpenAI.Codex' | Sort-Object Version -Descending | Select-Object -First 1
if (-not $package -or $env:NANOCODEX_UPSTREAM_REFRESH -eq '1') {
    $winget = Get-Command winget.exe -ErrorAction SilentlyContinue
    if (-not $winget) { throw 'Install Microsoft App Installer (winget), then retry nanocodex2 computer setup. OpenAI Store product: https://apps.microsoft.com/detail/9plm9xgg6vks' }
    $output = & $winget.Source install --id 9PLM9XGG6VKS --source msstore --exact --silent --disable-interactivity --accept-package-agreements --accept-source-agreements 2>&1
    # APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE: already at the current release.
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) { throw "Official OpenAI Store installation failed ($LASTEXITCODE): $output" }
    $package = Get-AppxPackage -Name 'OpenAI.Codex' | Sort-Object Version -Descending | Select-Object -First 1
}
if (-not $package -or $package.PackageFamilyName -ne 'OpenAI.Codex_2p2nqsd0c76g0' -or [string]$package.SignatureKind -ne 'Store' -or $package.Status -ne 'Ok') {
    throw 'A healthy Microsoft Store-signed OpenAI.Codex package is required.'
}
# Store package files can be read but not executed directly by an unpackaged host.
# Copy the exact runtime and native host companions; preserve all original bytes.
$source = Join-Path $package.InstallLocation 'app\resources'
$base = if ($env:NANOCODEX_DIR) { $env:NANOCODEX_DIR } elseif ($env:HOME) { Join-Path $env:HOME '.nanocodex' } else { Join-Path $env:USERPROFILE '.nanocodex' }
$root = [IO.Path]::GetFullPath((Join-Path $base 'runtimes\openai-cua'))
# Node's Windows filesystem implementation handles the package's long paths.
$copyScript = @'
const fs = require('node:fs'), path = require('node:path'), crypto = require('node:crypto');
const [source, target, mode] = process.argv.slice(1);
function files(root, prefix = '') { return fs.readdirSync(root, {withFileTypes:true}).flatMap(e => {
  const relative = path.join(prefix, e.name);
  if (e.isSymbolicLink()) throw new Error('Unexpected link in Store runtime: '+relative);
  return e.isDirectory() ? files(path.join(root,e.name),relative) : [relative];
}); }
const selected = files(path.join(source,'cua_node')).map(p=>path.join('cua_node',p)).concat(fs.readdirSync(source).filter(p=>p.endsWith('.exe') || p==='THIRD_PARTY_NOTICES.txt')).sort();
const hash = p=>crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const manifest=[];
for(const relative of selected) {
  const from=path.join(source,relative), to=path.join(target,relative);
  if(mode==='copy') { fs.mkdirSync(path.dirname(to),{recursive:true});fs.copyFileSync(from,to); }
  const sha256=hash(from);
  if(hash(to)!==sha256) throw new Error('OpenAI CUA copy differs: '+relative);
  manifest.push({path:relative,sha256});
}
if(JSON.stringify(files(target).sort())!==JSON.stringify(selected)) throw new Error('Unexpected file in cached CUA runtime');
if(mode==='copy') fs.writeFileSync(path.join(path.dirname(target),'copy-manifest.json'),JSON.stringify(manifest));
'@
$copyScript = $copyScript.TrimEnd()
$selected = $null
$versions = Join-Path $root 'versions'
New-Item -ItemType Directory -Force -Path $versions | Out-Null
$bootstrap = Join-Path $root ('bootstrap-' + [Guid]::NewGuid().ToString('N') + '.exe')
$stage = $null
try {
    $sourceNode = Join-Path $source 'cua_node\bin\node.exe'
    Copy-Item -LiteralPath $sourceNode -Destination $bootstrap
    if ((Get-FileHash -LiteralPath $sourceNode -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $bootstrap -Algorithm SHA256).Hash) { throw 'Node bootstrap copy differs' }
    $current = Join-Path $root 'provider.json'
    if ((Test-Path -LiteralPath $current -PathType Leaf) -and $env:NANOCODEX_UPSTREAM_REFRESH -ne '1') {
        $previous = Get-Content -LiteralPath $current -Raw | ConvertFrom-Json
        if ($previous.build -eq [string]$package.Version -and $previous.versionDirectory) {
            $candidate = [IO.Path]::GetFullPath([string]$previous.versionDirectory)
            $prefix = $versions + [IO.Path]::DirectorySeparatorChar
            if (-not $candidate.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid cached CUA path' }
            & $bootstrap -e $copyScript $source (Join-Path $candidate 'resources') verify
            if ($LASTEXITCODE -ne 0) { throw 'Managed OpenAI CUA runtime is damaged. Run nanocodex2 computer setup --refresh.' }
            $selected = $candidate
        }
    }
    if (-not $selected) {
        $stage = Join-Path $root ('.staging-' + [Guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $stage | Out-Null
        & $bootstrap -e $copyScript $source (Join-Path $stage 'resources') copy
        if ($LASTEXITCODE -ne 0) { throw 'OpenAI CUA copy verification failed' }
        $selected = Join-Path $versions ([string]$package.Version + '-' + [Guid]::NewGuid().ToString('N'))
        Move-Item -LiteralPath $stage -Destination $selected
    }
} finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) { & $bootstrap -e "require('node:fs').rmSync(process.argv[1],{recursive:true,force:true})" $stage }
    Remove-Item -LiteralPath $bootstrap -Force -ErrorAction SilentlyContinue
}
$resources = Join-Path $selected 'resources'
$runtime = Join-Path $resources 'cua_node'
$manifest = Get-Content -LiteralPath (Join-Path $runtime 'manifest.json') -Raw | ConvertFrom-Json
if ($manifest.platform -ne 'windows' -or $manifest.node_path -ne 'bin/node.exe' -or $manifest.node_repl_path -ne 'bin/node_repl.exe' -or $manifest.node_modules -ne 'bin/node_modules') {
    throw 'The installed OpenAI CUA runtime layout is not supported.'
}
$modules = Join-Path $runtime 'bin\node_modules'
$node = Join-Path $runtime 'bin\node.exe'
$repl = Join-Path $runtime 'bin\node_repl.exe'
$provider = Join-Path $modules '@oai\cua-repl\bin\cua-repl.mjs'
$codex = Join-Path $resources 'codex.exe'
foreach ($file in @($node, $repl, $provider, $codex, (Join-Path $modules '@oai\sky\package.json'))) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "OpenAI CUA runtime is incomplete: $file" }
}
@{
    status = 'installed'; platform = 'windows'; build = [string]$package.Version; versionDirectory = $selected
    executable = $node; args = @($provider); transport = 'mcp'
    environment = @{
        CUA_REPL_NODE_REPL_PATH = $repl; CUA_REPL_ENABLED_SURFACES = 'browser,computer'
        NODE_REPL_NODE_PATH = $node; NODE_REPL_NODE_MODULE_DIRS = $modules
        NODE_REPL_TRUSTED_CODE_PATHS = $modules; CODEX_CLI_PATH = $codex
    }
} | ConvertTo-Json -Depth 4 -Compress
