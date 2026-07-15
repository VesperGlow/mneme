$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
    throw "没有找到 Docker。请先安装 Docker Desktop，然后重新运行此脚本。"
}
if (-not (Test-Path ".env")) {
    Copy-Item ".env.example" ".env"
    Write-Host "已创建 .env。请先填入 AI_BASE_URL、AI_API_KEY、MEMORY_MODEL、CHAT_MODEL，并修改两个密码。" -ForegroundColor Yellow
    exit 0
}

& docker compose -f compose.yaml up -d --build

