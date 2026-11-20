$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$modelDir = Join-Path $repoRoot "models"
New-Item -ItemType Directory -Force -Path $modelDir | Out-Null

$models = @{
    "text-detection.onnx" = "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx"
    "text-recognition.onnx" = "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx"
}

foreach ($model in $models.GetEnumerator()) {
    $destination = Join-Path $modelDir $model.Key
    if (-not (Test-Path -LiteralPath $destination)) {
        Write-Host "Downloading $($model.Key)..."
        Invoke-WebRequest -Uri $model.Value -OutFile $destination
    }
}

Get-ChildItem -LiteralPath $modelDir -Filter "*.onnx" | Select-Object Name, Length
