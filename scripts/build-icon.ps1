#!/usr/bin/env pwsh
# Rebuild the Windows icon: a multi-size ICO (embedded in the executable; the
# tray and Explorer pick the frame for their size and DPI) and the 256-pixel
# PNG for the window icon. Every frame is drawn for its own size: the hexagon
# reaches all four canvas edges, its slate fill stays visible on a dark taskbar,
# and pixel-aligned bars keep their gaps at tray sizes.
[CmdletBinding()]
param(
    [string]$OutputDirectory = (Join-Path (Split-Path -Parent $PSScriptRoot) 'assets')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

$hexColor = [System.Drawing.ColorTranslator]::FromHtml('#3b4555')
$barColor = [System.Drawing.ColorTranslator]::FromHtml('#d4f7d3')
$stripColor = [System.Drawing.ColorTranslator]::FromHtml('#fdae36')

function Get-Round([double]$value) {
    [int][math]::Round($value, [System.MidpointRounding]::AwayFromZero)
}

function New-RoundedRectanglePath([double]$x, [double]$y, [double]$width, [double]$height, [double]$radius) {
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $diameter = [single](2 * $radius)
    $path.AddArc([single]$x, [single]$y, $diameter, $diameter, 180, 90)
    $path.AddArc([single]($x + $width - $diameter), [single]$y, $diameter, $diameter, 270, 90)
    $path.AddArc([single]($x + $width - $diameter), [single]($y + $height - $diameter), $diameter, $diameter, 0, 90)
    $path.AddArc([single]$x, [single]($y + $height - $diameter), $diameter, $diameter, 90, 90)
    $path.CloseFigure()
    $path
}

function New-HexagonPath([int]$size) {
    # The tips sit just outside the canvas so their rounded ends touch its edge.
    $overshoot = 0.0165 * $size
    $shoulder = 0.25 * $size
    $vertices = @(
        @(($size / 2), (-$overshoot)),
        @($size, $shoulder),
        @($size, ($size - $shoulder)),
        @(($size / 2), ($size + $overshoot)),
        @(0, ($size - $shoulder)),
        @(0, $shoulder)
    )
    $radius = 0.06 * $size
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    for ($index = 0; $index -lt 6; $index++) {
        $vertex = $vertices[$index]
        $points = foreach ($neighbour in @($vertices[($index + 5) % 6], $vertices[($index + 1) % 6])) {
            $dx = $neighbour[0] - $vertex[0]
            $dy = $neighbour[1] - $vertex[1]
            $length = [math]::Sqrt($dx * $dx + $dy * $dy)
            ,@(($vertex[0] + $dx / $length * $radius), ($vertex[1] + $dy / $length * $radius))
        }
        $from = $points[0]
        $to = $points[1]
        $path.AddBezier(
            [single]$from[0], [single]$from[1],
            [single]($from[0] + ($vertex[0] - $from[0]) * 0.6), [single]($from[1] + ($vertex[1] - $from[1]) * 0.6),
            [single]($to[0] + ($vertex[0] - $to[0]) * 0.6), [single]($to[1] + ($vertex[1] - $to[1]) * 0.6),
            [single]$to[0], [single]$to[1]
        )
    }
    $path.CloseFigure()
    $path
}

function Fill-Path($graphics, $path, $color) {
    $brush = [System.Drawing.SolidBrush]::new($color)
    try {
        $graphics.FillPath($brush, $path)
    }
    finally {
        $brush.Dispose()
        $path.Dispose()
    }
}

function New-IconFrame([int]$size) {
    $bitmap = [System.Drawing.Bitmap]::new(
        $size,
        $size,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.Clear([System.Drawing.Color]::Transparent)
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        Fill-Path $graphics (New-HexagonPath $size) $hexColor

        # Whole-pixel bars and gaps, so the three racks stay apart at 16 pixels.
        $barHeight = Get-Round (0.175 * $size)
        $gap = [math]::Max(1, (Get-Round (0.025 * $size)))
        $glyphHeight = 3 * $barHeight + 2 * $gap
        $glyphWidth = Get-Round (0.72 * $size)
        $left = Get-Round (($size - $glyphWidth) / 2)
        $top = Get-Round (($size - $glyphHeight) / 2)
        $right = $left + $glyphWidth
        $barRadius = 0.18 * $barHeight
        $dotRadius = [math]::Max(0.5, 0.173 * $barHeight)
        $insets = @(0.297, 0.135, 0.0)
        for ($row = 0; $row -lt 3; $row++) {
            $barLeft = $left + (Get-Round ($insets[$row] * $glyphWidth))
            $barTop = $top + $row * ($barHeight + $gap)
            Fill-Path $graphics (New-RoundedRectanglePath $barLeft $barTop ($right - $barLeft) $barHeight $barRadius) $barColor
            $dotX = [math]::Floor($barLeft + 0.11 * $glyphWidth) + 0.5
            $dotY = $barTop + $barHeight / 2
            $dot = [System.Drawing.Drawing2D.GraphicsPath]::new()
            $dot.AddEllipse([single]($dotX - $dotRadius), [single]($dotY - $dotRadius), [single](2 * $dotRadius), [single](2 * $dotRadius))
            Fill-Path $graphics $dot $hexColor
        }
        $stripLeft = $left + (Get-Round (0.791 * $glyphWidth))
        $stripWidth = [math]::Max(2, (Get-Round (0.144 * $glyphWidth)))
        Fill-Path $graphics (New-RoundedRectanglePath $stripLeft $top $stripWidth $glyphHeight (0.16 * $stripWidth)) $stripColor

        foreach ($corner in @(@(0, 0), @(($size - 1), 0), @(0, ($size - 1)), @(($size - 1), ($size - 1)))) {
            if ($bitmap.GetPixel($corner[0], $corner[1]).A -ne 0) {
                throw "The $size-pixel icon lost its transparent exterior."
            }
        }
        $middle = [int]($size / 2)
        foreach ($edge in @(@($middle, 0), @($middle, ($size - 1)), @(0, $middle), @(($size - 1), $middle))) {
            if ($bitmap.GetPixel($edge[0], $edge[1]).A -lt 128) {
                throw "The $size-pixel icon does not reach the canvas edge at $($edge -join ',')."
            }
        }
        $png = [System.IO.MemoryStream]::new()
        try {
            $bitmap.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
            $png.ToArray()
        }
        finally {
            $png.Dispose()
        }
    }
    finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

# Small icons at 100-300% scaling, Explorer's medium and large views.
$sizes = @(16, 20, 24, 28, 32, 36, 40, 48, 64, 256)
$images = foreach ($size in $sizes) {
    $bytes = [byte[]](New-IconFrame $size)
    if ($size -eq 256) {
        [System.IO.File]::WriteAllBytes((Join-Path $OutputDirectory 'icon-256.png'), $bytes)
    }
    [pscustomobject]@{
        Size = $size
        Bytes = $bytes
    }
}

$icoPath = Join-Path $OutputDirectory 'icon.ico'
$stream = [System.IO.File]::Create($icoPath)
$writer = [System.IO.BinaryWriter]::new($stream)
try {
    $writer.Write([uint16]0) # reserved
    $writer.Write([uint16]1) # icon
    $writer.Write([uint16]$images.Count)
    $offset = 6 + 16 * $images.Count
    foreach ($image in $images) {
        # In ICO directories, zero means 256 pixels.
        $dimension = if ($image.Size -eq 256) { 0 } else { $image.Size }
        $writer.Write([byte]$dimension)
        $writer.Write([byte]$dimension)
        $writer.Write([byte]0) # color count
        $writer.Write([byte]0) # reserved
        $writer.Write([uint16]1) # color planes
        $writer.Write([uint16]32) # bits per pixel
        $writer.Write([uint32]$image.Bytes.Length)
        $writer.Write([uint32]$offset)
        $offset += $image.Bytes.Length
    }
    foreach ($image in $images) {
        $writer.Write([byte[]]$image.Bytes)
    }
}
finally {
    $writer.Dispose()
    $stream.Dispose()
}

Write-Output "Built icon.ico ($($sizes -join ', ') px) and icon-256.png in $OutputDirectory"
