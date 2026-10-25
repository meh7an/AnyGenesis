//! Pictures from the VDP's registers and memories, drawn a line at a time: planes A and B,
//! the window, sprites, horizontal and vertical scrolling, priority and the backdrop, in H32
//! and H40.
//!
//! A line is drawn into three layers (plane B, plane A or the window, sprites) whose pixels
//! are CRAM indices with the priority bit on top; colour 0 of every palette is transparent.
//! The first opaque pixel in the order high sprite, high A, high B, low sprite, low A, low B
//! wins, else the backdrop colour shows. CRAM's 3-bit channels scale linearly to 8 bits.
//! Each line is drawn from one state, so changes in the middle of a line are not seen.
//!
//! Sprites are found by walking the link list from sprite 0 until a link of 0, a link past
//! the table or the table's size; earlier sprites stay in front. A line takes 16 sprites
//! and 256 sprite pixels in H32, 20 and 320 in H40.
//!
//! Sources (behaviour only, no emulator code read): Plutiedev (registers, tile and sprite
//! formats, layer priority, sprite limits), Kabuto's hardware notes on Plutiedev (sprite
//! scan, x = 0 masking), rasterscroll.com's VDP pages (scroll ranges and directions, the
//! window's name table and scrolling), the MegaDrive Wiki (plane sizes, scroll table
//! layout), and SpritesMind posts (the window's row and column rule, masking after a line
//! that used all its sprite pixels). Chosen, unverified: the backdrop under the left column
//! blank and with the display off, the undocumented H-scroll mode 01 repeating the first
//! eight lines' values, prohibited plane sizes drawn as 32 cells, sprite flips reversing
//! the cell order, and the part-shown column of two-cell vertical scrolling taking column
//! 0's value (hardware is reported to give it something else). Left out: shadow/highlight,
//! double-resolution interlace (drawn as progressive), the sprite collision and overflow
//! status flags, and the wrong plane A cell fetched right of a left window.

use super::{CRAM_WORDS, REGISTERS, VRAM_SIZE, VSRAM_WORDS};

/// Pixels in an H40 line, the widest a frame gets.
pub const MAX_WIDTH: usize = 320;
/// Pixels in an H32 line.
pub const H32_WIDTH: usize = 256;
/// Picture lines in NTSC's V28 mode.
pub const HEIGHT: usize = 224;

/// The priority bit of a layer pixel, above its CRAM index.
const PRIORITY: u8 = 0x80;
/// Sprite and plane positions count from this many pixels before the picture's edge.
const SPRITE_OFFSET: usize = 128;
/// Bytes of a 4-bit 8x8 pattern.
const PATTERN_BYTES: usize = 32;
/// The bytes a BMP's file and info headers take.
const BMP_HEADER: usize = 54;

/// A finished picture: [`HEIGHT`] rows of [`MAX_WIDTH`] pixels as 0x00RRGGBB, of which the
/// left [`width`](Self::width) are drawn (256 in H32, 320 in H40) and the rest stay black.
#[derive(Debug, Clone)]
pub struct Frame {
    width: usize,
    pixels: Box<[u32; MAX_WIDTH * HEIGHT]>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            width: H32_WIDTH,
            pixels: Box::new([0; MAX_WIDTH * HEIGHT]),
        }
    }
}

impl Frame {
    /// Drawn pixels per row: the horizontal mode at the frame's first line.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Rows.
    pub fn height(&self) -> usize {
        HEIGHT
    }

    /// Every pixel, row after row, each row [`MAX_WIDTH`] long whatever the width.
    pub fn pixels(&self) -> &[u32; MAX_WIDTH * HEIGHT] {
        &self.pixels
    }

    /// The drawn pixels of row `y`.
    ///
    /// # Panics
    ///
    /// If `y` is [`HEIGHT`] or more.
    pub fn row(&self, y: usize) -> &[u32] {
        &self.pixels[y * MAX_WIDTH..][..self.width]
    }

    /// The drawn pixels as a 24-bit BMP file.
    pub fn to_bmp(&self) -> Vec<u8> {
        // Both widths make rows of a multiple of 4 bytes, so no row needs padding.
        let image = self.width * 3 * HEIGHT;
        let mut bmp = Vec::with_capacity(BMP_HEADER + image);
        let file_size = (BMP_HEADER + image) as u32;
        bmp.extend_from_slice(b"BM");
        for word in [file_size, 0, BMP_HEADER as u32, 40] {
            bmp.extend_from_slice(&word.to_le_bytes());
        }
        // Width, then a positive height: rows are stored bottom to top.
        bmp.extend_from_slice(&(self.width as u32).to_le_bytes());
        bmp.extend_from_slice(&(HEIGHT as u32).to_le_bytes());
        // One plane of 24 bits per pixel.
        bmp.extend_from_slice(&1_u16.to_le_bytes());
        bmp.extend_from_slice(&24_u16.to_le_bytes());
        // No compression, the image size, 72 dpi both ways, no palette.
        for word in [0, image as u32, 2835, 2835, 0, 0] {
            bmp.extend_from_slice(&word.to_le_bytes());
        }
        for y in (0..HEIGHT).rev() {
            for &pixel in self.row(y) {
                let [_, red, green, blue] = pixel.to_be_bytes();
                bmp.extend_from_slice(&[blue, green, red]);
            }
        }
        bmp
    }

    fn row_mut(&mut self, y: usize) -> &mut [u32] {
        // Whole rows, so a line narrower than the frame can clear its tail.
        &mut self.pixels[y * MAX_WIDTH..][..MAX_WIDTH]
    }
}

/// What a line is drawn from.
#[derive(Debug, Clone, Copy)]
pub(super) struct Source<'a> {
    pub registers: &'a [u8; REGISTERS],
    pub vram: &'a [u8; VRAM_SIZE],
    pub cram: &'a [u16; CRAM_WORDS],
    pub vsram: &'a [u16; VSRAM_WORDS],
}

/// The frame being drawn and the last one finished.
#[derive(Debug, Clone, Default)]
pub(super) struct Screen {
    shown: Frame,
    drawing: Frame,
    /// The line drawn last used all its sprite pixels, which lets an x = 0 sprite mask the
    /// next line from its first sprite on.
    pixels_ran_out: bool,
}

impl Screen {
    /// The last finished frame.
    pub fn frame(&self) -> &Frame {
        &self.shown
    }

    /// Draws picture line `line` (below [`HEIGHT`]); the last one finishes the frame.
    pub fn draw_line(&mut self, source: Source<'_>, line: usize) {
        let layout = Layout::of(source.registers);
        if line == 0 {
            self.drawing.width = layout.width;
        }
        // A line drawn narrower than the frame (the mode changed after line 0) ends in black.
        let width = self.drawing.width.min(layout.width);
        let row = &mut self.drawing.row_mut(line)[..];
        row[width..].fill(0);
        let row = &mut row[..width];
        let backdrop = colour(source.cram[usize::from(layout.backdrop)]);
        if !layout.display {
            row.fill(backdrop);
            self.pixels_ran_out = false;
        } else {
            let mut planes = [[0; MAX_WIDTH]; 2];
            let [plane_b, plane_a] = &mut planes;
            draw_plane(source, &layout, Plane::B, line, 0..width, plane_b);
            let window = layout.window_columns(line);
            draw_plane(source, &layout, Plane::A, line, 0..window.start, plane_a);
            draw_window(source, &layout, line, window.clone(), plane_a);
            draw_plane(source, &layout, Plane::A, line, window.end..width, plane_a);
            let mut sprites = [0; MAX_WIDTH];
            self.pixels_ran_out =
                draw_sprites(source, &layout, line, self.pixels_ran_out, &mut sprites);
            for (x, pixel) in row.iter_mut().enumerate() {
                let layers = [sprites[x], plane_a[x], plane_b[x]];
                let index = if x < 8 && layout.left_blank {
                    None
                } else {
                    layers
                        .into_iter()
                        .find(|&layer| layer & PRIORITY != 0 && opaque(layer))
                        .or_else(|| layers.into_iter().find(|&layer| opaque(layer)))
                };
                *pixel = match index {
                    Some(layer) => colour(source.cram[usize::from(layer & 0x3F)]),
                    None => backdrop,
                };
            }
        }
        if line == HEIGHT - 1 {
            std::mem::swap(&mut self.shown, &mut self.drawing);
        }
    }
}

/// A CRAM word (0x0BGR, three bits a channel at 3-1) as 0x00RRGGBB.
fn colour(word: u16) -> u32 {
    let channel = |shift: u16| u32::from(word >> shift & 7) * 255 / 7;
    channel(1) << 16 | channel(5) << 8 | channel(9)
}

fn opaque(layer: u8) -> bool {
    layer & 0x0F != 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plane {
    A,
    B,
}

/// Horizontal scrolling, register 11 bits 1-0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HScroll {
    /// One value for the whole picture.
    Full,
    /// The first eight lines' values, repeated down the picture (the undocumented mode 01).
    Eight,
    /// A value per row of cells.
    Cell,
    /// A value per line.
    Line,
}

/// The registers as the renderer reads them.
#[derive(Debug, Clone, Copy)]
struct Layout {
    width: usize,
    h40: bool,
    display: bool,
    left_blank: bool,
    backdrop: u8,
    plane_a: usize,
    plane_b: usize,
    window: usize,
    sprites: usize,
    hscroll_table: usize,
    hscroll: HScroll,
    /// Vertical scrolling per two-cell column instead of one value per plane.
    vscroll_columns: bool,
    /// Plane size in cells.
    plane_width: usize,
    plane_height: usize,
    /// Register 17: window columns right of the point instead of left, and the point in
    /// two-cell units.
    window_right: bool,
    window_column: usize,
    /// Register 18: window lines below the point instead of above, and the point in cells.
    window_down: bool,
    window_row: usize,
}

impl Layout {
    fn of(registers: &[u8; REGISTERS]) -> Self {
        let h40 = registers[12] & 0x01 != 0;
        let [plane_width, plane_height] = [registers[16], registers[16] >> 4].map(plane_cells);
        Self {
            width: if h40 { MAX_WIDTH } else { H32_WIDTH },
            h40,
            display: registers[1] & 0x40 != 0,
            left_blank: registers[0] & 0x20 != 0,
            backdrop: registers[7] & 0x3F,
            plane_a: usize::from(registers[2] & 0x38) << 10,
            plane_b: usize::from(registers[4] & 0x07) << 13,
            // H40 ignores the lowest bit of each base: its tables are twice the size.
            window: usize::from(registers[3] & if h40 { 0x3C } else { 0x3E }) << 10,
            sprites: usize::from(registers[5] & if h40 { 0x7E } else { 0x7F }) << 9,
            hscroll_table: usize::from(registers[13] & 0x3F) << 10,
            hscroll: match registers[11] & 0x03 {
                0 => HScroll::Full,
                1 => HScroll::Eight,
                2 => HScroll::Cell,
                _ => HScroll::Line,
            },
            vscroll_columns: registers[11] & 0x04 != 0,
            plane_width,
            plane_height,
            window_right: registers[17] & 0x80 != 0,
            window_column: usize::from(registers[17] & 0x1F),
            window_down: registers[18] & 0x80 != 0,
            window_row: usize::from(registers[18] & 0x1F),
        }
    }

    /// The columns of `line` the window covers instead of plane A.
    fn window_columns(&self, line: usize) -> std::ops::Range<usize> {
        let edge = self.window_row * 8;
        let rows = if self.window_down {
            line >= edge
        } else {
            line < edge
        };
        if rows {
            return 0..self.width;
        }
        let edge = (self.window_column * 16).min(self.width);
        if self.window_right {
            edge..self.width
        } else {
            0..edge
        }
    }
}

/// A plane size field of register 16 in cells; the prohibited `10` draws as 32.
pub(super) fn plane_cells(field: u8) -> usize {
    match field & 0x03 {
        1 => 64,
        3 => 128,
        _ => 32,
    }
}

fn word(vram: &[u8; VRAM_SIZE], address: usize) -> u16 {
    let even = address & (VRAM_SIZE - 2);
    u16::from_be_bytes([vram[even], vram[even + 1]])
}

/// The pixel of a name-table entry or sprite pattern word `entry` at `row`/`column` of its
/// cell (before flipping), as a layer pixel.
fn cell_pixel(vram: &[u8; VRAM_SIZE], entry: u16, row: usize, column: usize) -> u8 {
    let row = if entry & 0x1000 != 0 { 7 - row } else { row };
    let column = if entry & 0x0800 != 0 {
        7 - column
    } else {
        column
    };
    pattern_pixel(vram, usize::from(entry & 0x07FF), row, column) | attributes(entry)
}

/// Colour 0-15 of `pattern` at `row`/`column`: the left pixel of a byte is its high nibble.
fn pattern_pixel(vram: &[u8; VRAM_SIZE], pattern: usize, row: usize, column: usize) -> u8 {
    let byte = vram[(pattern * PATTERN_BYTES + row * 4 + column / 2) & (VRAM_SIZE - 1)];
    if column.is_multiple_of(2) {
        byte >> 4
    } else {
        byte & 0x0F
    }
}

/// The priority and palette bits of a name-table entry or sprite pattern word, placed as
/// a layer pixel's.
fn attributes(entry: u16) -> u8 {
    let priority = if entry & 0x8000 != 0 { PRIORITY } else { 0 };
    priority | (entry >> 9 & 0x30) as u8
}

/// Draws `columns` of plane `plane` on `line` into `out`.
fn draw_plane(
    source: Source<'_>,
    layout: &Layout,
    plane: Plane,
    line: usize,
    columns: std::ops::Range<usize>,
    out: &mut [u8; MAX_WIDTH],
) {
    let (base, slot) = match plane {
        Plane::A => (layout.plane_a, 0),
        Plane::B => (layout.plane_b, 1),
    };
    let entry_line = match layout.hscroll {
        HScroll::Full => 0,
        HScroll::Eight => line & 7,
        HScroll::Cell => line & !7,
        HScroll::Line => line,
    };
    let address = layout.hscroll_table + entry_line * 4 + slot * 2;
    let hscroll = usize::from(word(source.vram, address) & 0x03FF);
    let width = layout.plane_width * 8;
    let height = layout.plane_height * 8;
    // Positive values move the plane right; the first column of tiles may show only in
    // part, and two-cell vertical scrolling follows the scrolled columns.
    let fine = hscroll & 15;
    for x in columns {
        let vscroll = if layout.vscroll_columns {
            // The part-shown column left of the first whole one takes column 0's value
            // (chosen; hardware gives it something else).
            let column = x.saturating_sub(fine) / 16;
            source.vsram[column * 2 + slot]
        } else {
            source.vsram[slot]
        };
        let plane_x = x.wrapping_sub(hscroll) & (width - 1);
        let plane_y = (line + usize::from(vscroll)) & (height - 1);
        let cell = (plane_y / 8) * layout.plane_width + plane_x / 8;
        let entry = word(source.vram, base + cell * 2);
        out[x] = cell_pixel(source.vram, entry, plane_y % 8, plane_x % 8);
    }
}

/// Draws `columns` of the window on `line` into `out`; the window never scrolls.
fn draw_window(
    source: Source<'_>,
    layout: &Layout,
    line: usize,
    columns: std::ops::Range<usize>,
    out: &mut [u8; MAX_WIDTH],
) {
    let cells = if layout.h40 { 64 } else { 32 };
    for x in columns {
        let entry = word(
            source.vram,
            layout.window + ((line / 8) * cells + x / 8) * 2,
        );
        out[x] = cell_pixel(source.vram, entry, line % 8, x % 8);
    }
}

/// Draws the sprites on `line` into `out`, walking the link list from sprite 0; returns
/// whether the line used all its sprite pixels.
fn draw_sprites(
    source: Source<'_>,
    layout: &Layout,
    line: usize,
    pixels_ran_out: bool,
    out: &mut [u8; MAX_WIDTH],
) -> bool {
    let (total, per_line) = if layout.h40 { (80, 20) } else { (64, 16) };
    let pixel_limit = layout.width;
    let target = line + SPRITE_OFFSET;
    let mut index = 0;
    let mut found = 0;
    let mut pixels = 0;
    // An x = 0 sprite masks the rest of the line once a sprite with another x came first,
    // or from the start when the previous line used all its pixels. Masked sprites still
    // count against both limits.
    let mut moved_seen = pixels_ran_out;
    let mut masked = false;
    for _ in 0..total {
        let entry = layout.sprites + index * 8;
        let top = usize::from(word(source.vram, entry) & 0x01FF);
        let size = word(source.vram, entry + 2);
        let link = usize::from(size & 0x7F);
        let columns = usize::from(size >> 10 & 3) + 1;
        let rows = usize::from(size >> 8 & 3) + 1;
        let row = target.wrapping_sub(top) & 0x01FF;
        if row < rows * 8 {
            found += 1;
            if found > per_line {
                break;
            }
            let pattern = word(source.vram, entry + 4);
            let left = usize::from(word(source.vram, entry + 6) & 0x01FF);
            if left == 0 {
                masked |= moved_seen;
            } else {
                moved_seen = true;
            }
            let row = if pattern & 0x1000 != 0 {
                rows * 8 - 1 - row
            } else {
                row
            };
            let hflip = pattern & 0x0800 != 0;
            for step in 0..columns {
                if pixels >= pixel_limit {
                    return true;
                }
                pixels += 8;
                if masked {
                    continue;
                }
                let column = if hflip { columns - 1 - step } else { step };
                let first = usize::from(pattern & 0x07FF) + column * rows + row / 8;
                for offset in 0..8 {
                    let Some(x) = (left + step * 8 + offset)
                        .checked_sub(SPRITE_OFFSET)
                        .filter(|&x| x < layout.width)
                    else {
                        continue;
                    };
                    let pixel_column = if hflip { 7 - offset } else { offset };
                    let colour = pattern_pixel(source.vram, first & 0x07FF, row % 8, pixel_column);
                    // Sprites earlier in the list stay in front.
                    if colour != 0 && !opaque(out[x]) {
                        out[x] = colour | attributes(pattern);
                    }
                }
            }
        }
        if link == 0 || link >= total {
            break;
        }
        index = link;
    }
    pixels >= pixel_limit
}
