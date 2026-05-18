# Probe which codepoints are covered by the fonts egui bundles by default.
# Reads the TTF cmap via WPF/GlyphTypeface so no external deps are required.
[CmdletBinding()]
param()

Add-Type -AssemblyName PresentationCore

$fontDir = "$env:USERPROFILE\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\epaint_default_fonts-0.34.2\fonts"
$fonts = @(
    "Ubuntu-Light.ttf",
    "Hack-Regular.ttf",
    "NotoEmoji-Regular.ttf",
    "emoji-icon-font.ttf"
)

$codepoints = @(
    # Currently used in the app
    0x00B7, 0x00D7, 0x2014, 0x2022, 0x2026, 0x201C, 0x201D, 0x2212,
    0x21BA, 0x21BB, 0x2600, 0x2601, 0x2713, 0x23F8,
    0x25B6, 0x25CB, 0x25CF,
    0x1F30C, 0x1F319, 0x1F3B5, 0x1F4CA, 0x1F4DD, 0x1F4F6, 0x1F4FB,
    0x1F4BE, 0x1F5BC, 0x1F5D1, 0x1F697,
    # Candidate replacements for ✓
    0x2192, 0x25B8, 0x2605, 0x2606, 0x2714, 0x2705, 0x2611, 0x2612,
    0x25A0, 0x25A1, 0x25C6
)

$maps = @{}
foreach ($f in $fonts) {
    $uri = [System.Uri]::new((Join-Path $fontDir $f))
    $gt = [System.Windows.Media.GlyphTypeface]::new($uri)
    $maps[$f] = $gt.CharacterToGlyphMap
}

"{0,-8} {1,-3} {2,-8} {3,-8} {4,-8} {5,-8}" -f 'CP', 'C', 'Ubuntu', 'Hack', 'NotoE', 'EmojiI'
"-" * 50
foreach ($cp in $codepoints) {
    $char = [char]::ConvertFromUtf32($cp)
    $row = "0x{0:X4}  {1,-3}" -f $cp, $char
    foreach ($f in $fonts) {
        $has = $maps[$f].ContainsKey($cp)
        $row += " {0,-8}" -f $(if ($has) { "YES" } else { "." })
    }
    $row
}
