// =============================================================================
// font8x16.rs — police bitmap VGA 8×16 pour tous les caractères ASCII imprimables.
//
// Chaque caractère = 16 octets (1 bit par pixel, 8 pixels/ligne, MSB = gauche).
// On couvre 0x20..0x7E + quelques caractères de contrôle essentiels.
// Les bitmaps viennent du BIOS VGA standard (domaine public, ROMs IBM).
//
// Note : seuls les caractères ASCII 0x00..0x7F sont indexés directement. Pour
// l'Unicode on fallback sur '?' (0x3F).
// =============================================================================

pub const CHAR_WIDTH: usize = 8;
pub const CHAR_HEIGHT: usize = 16;

/// Retourne le bitmap du caractère (16 octets). Fallback '?' pour inconnu.
pub fn glyph(ch: char) -> &'static [u8; 16] {
    let code = ch as u32;
    if code > 0x7F { return &FONT[b'?' as usize]; }
    &FONT[code as usize]
}

/// Police VGA 8×16 classique — glyphs pour 0..0x7F.
/// Généré depuis la ROM BIOS IBM PC (domaine public).
static FONT: [[u8; 16]; 128] = include!("font8x16_data.rs");
