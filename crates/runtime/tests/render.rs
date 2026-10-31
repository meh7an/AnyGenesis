//! Golden render tests: scenes written through the VDP's ports, drawn over a frame, and
//! compared region by region with grids of characters, one per pixel, that stand for CRAM
//! indices. The expected grids come from the hardware documents, not from the renderer.
//!
//! CRAM is loaded so that every index has its own colour, which maps a drawn pixel back to
//! its index. A failing test writes its frame to `dumps/` at the workspace root.

use std::{collections::HashMap, fs, path::PathBuf};

use anygenesis_abi::RAM_SIZE;
use anygenesis_runtime::{
    Hardware, Memory,
    hardware::port,
    timing::{CPU_DIVIDER, FRAME_CLOCKS, LINE_CLOCKS},
    vdp::{
        Frame, Vdp,
        render::{H32_WIDTH, HEIGHT, MAX_WIDTH},
        status,
    },
};
use anygenesis_shared::Size;

/// Enough 68000 cycles for a whole frame to be drawn from any starting point on a line.
const FRAME_CYCLES: u32 = (FRAME_CLOCKS + LINE_CLOCKS).div_ceil(CPU_DIVIDER) as u32;

const PLANE_A: u16 = 0xC000;
const PLANE_B: u16 = 0xE000;
const WINDOW: u16 = 0xB000;
const SPRITES: u16 = 0xD800;
const HSCROLL: u16 = 0xDC00;

/// Pattern numbers the scenes use.
const SOLID: u16 = 1;
const ARROW: u16 = 2;
/// Solid colour 3, the window's.
const SOLID3: u16 = 3;
/// Solid colours 14 and 15, shadow/highlight's operators in palette 3.
const SOLID14: u16 = 4;
const SOLID15: u16 = 5;
/// Four patterns of solid colours 1-4, for telling a sprite's cells apart.
const QUAD: u16 = 8;

/// Palette lines of the layers, so each shows in its own characters.
const PAL_B: u16 = 1;
const PAL_A: u16 = 2;
const PAL_S: u16 = 3;

/// Name-table and pattern-word bits.
const PRIORITY: u16 = 0x8000;
const VFLIP: u16 = 0x1000;
const HFLIP: u16 = 0x0800;

/// Characters for CRAM indices: `.` the backdrop (index 0), `b`/`a`/`s` colour 1 of the
/// plane B, plane A and sprite palettes, `B`/`A`/`S` their colour 2, `w` the window's
/// colour 3 (plane A's palette), `t`/`u` the sprite palette's colours 3 and 4.
const LEGEND: [(char, u8); 10] = [
    ('.', 0),
    ('b', 0x11),
    ('B', 0x12),
    ('a', 0x21),
    ('A', 0x22),
    ('w', 0x23),
    ('s', 0x31),
    ('S', 0x32),
    ('t', 0x33),
    ('u', 0x34),
];

/// A unique colour per CRAM index: red from bits 2-0, green from bits 5-3.
fn cram_word(index: u8) -> u16 {
    let index = u16::from(index);
    (index & 7) << 1 | (index >> 3 & 7) << 5
}

/// The colour the frame should hold for a CRAM word: each channel scaled from 3 to 8 bits.
fn rgb(word: u16) -> u32 {
    let channel = |shift: u16| u32::from(word >> shift & 7) * 255 / 7;
    channel(1) << 16 | channel(5) << 8 | channel(9)
}

/// Brightness under shadow/highlight.
#[derive(Debug, Clone, Copy)]
enum Shade {
    Shadow,
    Normal,
    Highlight,
}

/// The colour the frame should hold for CRAM index `index` at `shade`: each channel v on
/// the documented 0-14 ladder (shadow v, normal 2v, highlight v + 7), scaled to 8 bits.
fn shaded(index: u8, shade: Shade) -> u32 {
    let word = cram_word(index);
    let channel = |shift: u16| {
        let value = u32::from(word >> shift & 7);
        let level = match shade {
            Shade::Shadow => value,
            Shade::Normal => value * 2,
            Shade::Highlight => value + 7,
        };
        level * 255 / 14
    };
    channel(1) << 16 | channel(5) << 8 | channel(9)
}

fn name(pattern: u16, palette: u16) -> u16 {
    palette << 13 | pattern
}

struct Scene {
    vdp: Vdp,
    ram: Box<[u8; RAM_SIZE]>,
}

impl Scene {
    /// Mode 5 with the display on, auto-increment 2, the tables at the constants above,
    /// 32x32-cell planes, the backdrop at index 0 and the test patterns loaded.
    fn new(h40: bool) -> Self {
        let mut scene = Self {
            vdp: Vdp::default(),
            ram: Box::new([0; RAM_SIZE]),
        };
        let mode = if h40 { 0x81 } else { 0x00 };
        for (register, value) in [
            (0, 0x04),
            (1, 0x44),
            (2, (PLANE_A >> 10) as u8),
            (3, (WINDOW >> 10) as u8),
            (4, (PLANE_B >> 13) as u8),
            (5, (SPRITES >> 9) as u8),
            (7, 0x00),
            (11, 0x00),
            (12, mode),
            (13, (HSCROLL >> 10) as u8),
            (15, 0x02),
            (16, 0x00),
            (17, 0x00),
            (18, 0x00),
        ] {
            scene.register(register, value);
        }
        let colours: Vec<u16> = (0..64).map(cram_word).collect();
        scene.cram(0, &colours);
        scene.pattern(SOLID, ["11111111"; 8]);
        scene.pattern(SOLID3, ["33333333"; 8]);
        scene.pattern(SOLID14, ["EEEEEEEE"; 8]);
        scene.pattern(SOLID15, ["FFFFFFFF"; 8]);
        for (offset, row) in ["11111111", "22222222", "33333333", "44444444"]
            .into_iter()
            .enumerate()
        {
            scene.pattern(QUAD + offset as u16, [row; 8]);
        }
        scene.pattern(
            ARROW,
            [
                "12000000", "11200000", "11120000", "11112000", "11110000", "11000000", "10000000",
                "20000002",
            ],
        );
        scene
    }

    fn control(&mut self, word: u16) {
        let memory = Memory {
            rom: &[],
            ram: &self.ram,
        };
        self.vdp.write(port::CONTROL, Size::Word, word, memory);
    }

    fn data(&mut self, words: &[u16]) {
        for &word in words {
            let memory = Memory {
                rom: &[],
                ram: &self.ram,
            };
            self.vdp.write(port::DATA, Size::Word, word, memory);
        }
    }

    fn register(&mut self, register: u8, value: u8) {
        self.control(0x8000 | u16::from(register) << 8 | u16::from(value));
    }

    /// Points the data port at `address` with write code `code` (CD3-CD0).
    fn command(&mut self, code: u16, address: u16) {
        self.control((code & 3) << 14 | address & 0x3FFF);
        self.control((code & 0x3C) << 2 | address >> 14);
    }

    fn vram(&mut self, address: u16, words: &[u16]) {
        self.command(0x01, address);
        self.data(words);
    }

    fn cram(&mut self, index: u16, words: &[u16]) {
        self.command(0x03, index * 2);
        self.data(words);
    }

    fn vsram(&mut self, index: u16, words: &[u16]) {
        self.command(0x05, index * 2);
        self.data(words);
    }

    /// Stores pattern `number` from eight rows of eight hex digits, left pixel first.
    fn pattern(&mut self, number: u16, rows: [&str; 8]) {
        let words: Vec<u16> = rows
            .iter()
            .flat_map(|row| {
                let value = u32::from_str_radix(row, 16).expect("eight hex digits");
                [(value >> 16) as u16, value as u16]
            })
            .collect();
        self.vram(number * 32, &words);
    }

    /// Sets the name-table entry at `column`/`row` of a 32-cell-wide table at `base`.
    fn cell(&mut self, base: u16, column: u16, row: u16, entry: u16) {
        self.cell_in(base, 32, column, row, entry);
    }

    fn cell_in(&mut self, base: u16, width: u16, column: u16, row: u16, entry: u16) {
        self.vram(base + (row * width + column) * 2, &[entry]);
    }

    /// Writes sprite `index`: screen position (without the 128 offset), size in cells,
    /// link and pattern word.
    fn sprite(&mut self, index: u16, x: u16, y: u16, cells: (u16, u16), link: u16, word: u16) {
        self.sprite_at(index, x + 128, y, cells, link, word);
    }

    /// Writes sprite `index` with its raw X field (128 is the picture's left edge).
    fn sprite_at(
        &mut self,
        index: u16,
        raw_x: u16,
        y: u16,
        cells: (u16, u16),
        link: u16,
        word: u16,
    ) {
        let (width, height) = cells;
        let size = (width - 1) << 2 | (height - 1);
        self.vram(
            SPRITES + index * 8,
            &[y + 128, size << 8 | link, word, raw_x],
        );
    }

    /// Writes the H-scroll table entry of `line`: plane A's value, then plane B's.
    fn hscroll(&mut self, line: u16, a: u16, b: u16) {
        self.vram(HSCROLL + line * 4, &[a, b]);
    }

    /// Lets a whole frame be drawn and returns it.
    fn frame(&mut self) -> &Frame {
        self.vdp.advance(FRAME_CYCLES);
        self.vdp.frame()
    }

    /// Reads the status register through the control port, which clears the sprite bits.
    fn sprite_status(&mut self) -> u16 {
        let sprite_bits = status::SPRITE_OVERFLOW | status::SPRITE_COLLISION;
        self.vdp.read(port::CONTROL, Size::Word) & sprite_bits
    }
}

/// The characters for `width` x `rows` pixels of `frame` from `x`/`y`.
fn grid(frame: &Frame, x: usize, y: usize, width: usize, rows: usize) -> Vec<String> {
    let indices: HashMap<u32, u8> = (0..64)
        .map(|index| (rgb(cram_word(index)), index))
        .collect();
    let characters: HashMap<u8, char> = LEGEND.iter().map(|&(c, index)| (index, c)).collect();
    (y..y + rows)
        .map(|row| {
            frame.row(row)[x..x + width]
                .iter()
                .map(|pixel| {
                    indices
                        .get(pixel)
                        .and_then(|index| characters.get(index))
                        .copied()
                        .unwrap_or('?')
                })
                .collect()
        })
        .collect()
}

/// Checks the region at `x`/`y` against `expected` (spaces in its rows only separate cells
/// for the reader), writing the frame to `dumps/` first when they differ.
fn assert_region(frame: &Frame, name: &str, x: usize, y: usize, expected: &[&str]) {
    let expected: Vec<String> = expected.iter().map(|row| row.replace(' ', "")).collect();
    let actual = grid(frame, x, y, expected[0].len(), expected.len());
    if actual != expected {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dumps");
        let _ = fs::create_dir_all(&folder);
        let _ = fs::write(folder.join(format!("render-{name}.bmp")), frame.to_bmp());
    }
    assert_eq!(actual, expected, "{name}: region at {x},{y}");
}

/// One grid row from a character per cell: each repeated over the cell's eight pixels.
fn cells(row: &str) -> String {
    row.chars().map(|c| c.to_string().repeat(8)).collect()
}

/// `count` copies of `row`.
fn rows(row: &str, count: usize) -> Vec<String> {
    vec![row.to_owned(); count]
}

fn assert_rows(frame: &Frame, name: &str, x: usize, y: usize, expected: &[String]) {
    let expected: Vec<&str> = expected.iter().map(String::as_str).collect();
    assert_region(frame, name, x, y, &expected);
}

#[test]
fn test_empty_planes_show_the_backdrop_and_display_off_shows_only_it() {
    let mut scene = Scene::new(true);
    scene.register(7, 0x12);
    let frame = scene.frame();
    assert_eq!(frame.width(), MAX_WIDTH);
    assert_eq!(frame.height(), HEIGHT);
    assert!((0..HEIGHT).all(|y| frame.row(y).iter().all(|&p| p == rgb(cram_word(0x12)))));

    let mut scene = Scene::new(true);
    scene.cell(PLANE_B, 0, 0, name(SOLID, PAL_B));
    scene.register(1, 0x04);
    scene.register(7, 0x22);
    let frame = scene.frame();
    assert!((0..HEIGHT).all(|y| frame.row(y).iter().all(|&p| p == rgb(cram_word(0x22)))));
}

#[test]
fn test_plane_cells_flip_horizontally_and_vertically() {
    let mut scene = Scene::new(true);
    for (column, flips) in [0, HFLIP, VFLIP, HFLIP | VFLIP].into_iter().enumerate() {
        scene.cell(PLANE_B, column as u16, 0, name(ARROW, PAL_B) | flips);
    }
    assert_region(
        scene.frame(),
        "flips",
        0,
        0,
        &[
            "bB...... ......Bb B......B B......B",
            "bbB..... .....Bbb b....... .......b",
            "bbbB.... ....Bbbb bb...... ......bb",
            "bbbbB... ...Bbbbb bbbb.... ....bbbb",
            "bbbb.... ....bbbb bbbbB... ...Bbbbb",
            "bb...... ......bb bbbB.... ....Bbbb",
            "b....... .......b bbB..... .....Bbb",
            "B......B B......B bB...... ......Bb",
        ],
    );
}

#[test]
fn test_layers_stack_by_priority_then_sprite_a_b() {
    let mut scene = Scene::new(true);
    let b = name(SOLID, PAL_B);
    let a = name(SOLID, PAL_A);
    let s = name(SOLID, PAL_S);
    // Per cell column: plane B, plane A, sprite (None for nothing there).
    let columns = [
        (Some(b), None, None),
        (Some(b), Some(a), None),
        (Some(b | PRIORITY), Some(a), None),
        (Some(b), Some(a), Some(s)),
        (Some(b | PRIORITY), None, Some(s)),
        (None, Some(a | PRIORITY), Some(s | PRIORITY)),
        (None, Some(a | PRIORITY), Some(s)),
        (Some(b | PRIORITY), Some(a | PRIORITY), None),
        // A high-priority cell of colour 0 hides nothing.
        (Some(b), Some(PRIORITY | PAL_A << 13), None),
        (Some(b | PRIORITY), Some(a | PRIORITY), Some(s | PRIORITY)),
    ];
    let sprites: Vec<(u16, u16)> = columns
        .iter()
        .enumerate()
        .filter_map(|(column, &(_, _, sprite))| Some((column as u16, sprite?)))
        .collect();
    for (column, &(plane_b, plane_a, _)) in columns.iter().enumerate() {
        if let Some(entry) = plane_b {
            scene.cell(PLANE_B, column as u16, 0, entry);
        }
        if let Some(entry) = plane_a {
            scene.cell(PLANE_A, column as u16, 0, entry);
        }
    }
    for (index, &(column, word)) in sprites.iter().enumerate() {
        let link = if index + 1 < sprites.len() {
            index as u16 + 1
        } else {
            0
        };
        scene.sprite(index as u16, column * 8, 0, (1, 1), link, word);
    }
    let frame = scene.frame();
    assert_rows(frame, "priority", 0, 0, &rows(&cells("babsbsaabs."), 8));
    assert_rows(frame, "priority", 0, 8, &rows(&cells("..........."), 1));
}

#[test]
fn test_full_horizontal_scroll_moves_each_plane_right_and_wraps() {
    let mut scene = Scene::new(true);
    scene.cell(PLANE_B, 0, 0, name(SOLID, PAL_B));
    scene.cell(PLANE_A, 0, 1, name(SOLID, PAL_A));
    // Plane A 2 pixels left; plane B 3 right, with bits above the tenth ignored.
    scene.hscroll(0, 0x03FE, 0x0403);
    let frame = scene.frame();
    assert_region(frame, "hscroll full", 0, 0, &["...bbbbb bbb....."]);
    assert_region(frame, "hscroll full", 0, 8, &["aaaaaa.. ........"]);
    // Every line takes line 0's values.
    assert_region(frame, "hscroll full", 0, 7, &["...bbbbb bbb....."]);
    assert_region(frame, "hscroll full", 0, 15, &["aaaaaa.. ........"]);
}

#[test]
fn test_line_and_cell_horizontal_scroll_take_their_lines_entries() {
    let mut scene = Scene::new(true);
    scene.register(11, 0x03);
    scene.cell(PLANE_B, 0, 0, name(SOLID, PAL_B));
    scene.cell(PLANE_B, 0, 1, name(SOLID, PAL_B));
    scene.hscroll(1, 0, 1);
    scene.hscroll(2, 0, 2);
    scene.hscroll(9, 0, 4);
    let frame = scene.frame();
    assert_region(
        frame,
        "hscroll line",
        0,
        0,
        &[
            "bbbbbbbb ....",
            ".bbbbbbb b...",
            "..bbbbbb bb..",
            "bbbbbbbb ....",
        ],
    );
    assert_region(frame, "hscroll line", 0, 9, &["....bbbb bbbb"]);

    let mut scene = Scene::new(true);
    scene.register(11, 0x02);
    scene.cell(PLANE_B, 0, 0, name(SOLID, PAL_B));
    scene.cell(PLANE_B, 0, 1, name(SOLID, PAL_B));
    scene.hscroll(0, 0, 1);
    // Ignored: cell scrolling reads every eighth line's entry.
    scene.hscroll(1, 0, 7);
    scene.hscroll(8, 0, 5);
    let frame = scene.frame();
    assert_region(
        frame,
        "hscroll cell",
        0,
        0,
        &[".bbbbbbb b....", ".bbbbbbb b...."],
    );
    assert_region(
        frame,
        "hscroll cell",
        0,
        8,
        &[".....bbb bbbbb.", ".....bbb bbbbb."],
    );
}

#[test]
fn test_full_vertical_scroll_moves_each_plane_up_and_wraps() {
    let mut scene = Scene::new(true);
    scene.cell(PLANE_B, 0, 1, name(SOLID, PAL_B));
    scene.cell(PLANE_A, 1, 1, name(SOLID, PAL_A));
    // Plane A 2 lines down; plane B 4 up, past a whole plane height.
    scene.vsram(0, &[0x00FE, 0x0104]);
    let frame = scene.frame();
    let mut expected = rows(&cells("..."), 3);
    expected.extend(rows(&cells("b.."), 6));
    expected.extend(rows(&cells("ba."), 2));
    expected.extend(rows(&cells(".a."), 6));
    expected.extend(rows(&cells("..."), 1));
    assert_rows(frame, "vscroll full", 0, 1, &expected);
}

#[test]
fn test_two_cell_vertical_scroll_follows_the_scrolled_columns() {
    let mut scene = Scene::new(true);
    scene.register(11, 0x04);
    for column in 0..4 {
        scene.cell(PLANE_B, column, 1, name(SOLID, PAL_B));
    }
    // Plane B: column pair 0 unscrolled, pair 1 eight lines up.
    scene.vsram(0, &[0, 0, 0, 8]);
    let frame = scene.frame();
    let mut expected = rows(&cells("..bb."), 8);
    expected.extend(rows(&cells("bb..."), 8));
    expected.extend(rows(&cells("....."), 1));
    assert_rows(frame, "vscroll columns", 0, 0, &expected);

    // Scrolled 4 pixels right, the column pairs move with the plane; the part-shown pair
    // on the left takes VSRAM words 38 AND 39 in H40 (12 & 10 = 8 lines up), 0 in H32.
    for h40 in [true, false] {
        let mut scene = Scene::new(h40);
        scene.register(11, 0x04);
        for column in 0..4 {
            scene.cell(PLANE_B, column, 1, name(SOLID, PAL_B));
        }
        scene.cell(PLANE_B, 31, 1, name(SOLID, PAL_B));
        scene.vsram(0, &[0, 0, 0, 8]);
        scene.vsram(38, &[12, 10]);
        scene.hscroll(0, 0, 4);
        let frame = scene.frame();
        let (top, bottom) = if h40 {
            ("bbbb", "....")
        } else {
            ("....", "bbbb")
        };
        assert_region(
            frame,
            "vscroll columns scrolled",
            0,
            0,
            &[&format!("{top}.... ........ ....bbbb bbbbbbbb bbbb....")],
        );
        assert_region(
            frame,
            "vscroll columns scrolled",
            0,
            8,
            &[&format!("{bottom}bbbb bbbbbbbb bbbb.... ........ ........")],
        );
    }
}

#[test]
fn test_window_replaces_plane_a_by_columns_and_rows_without_scrolling() {
    let mut scene = Scene::new(true);
    let window = name(SOLID3, PAL_A);
    // Left of 2 cells, the window; plane A scrolled 4 right does not move it.
    scene.register(17, 0x01);
    scene.cell_in(WINDOW, 64, 0, 0, window);
    scene.cell_in(WINDOW, 64, 1, 0, window);
    for column in 0..4 {
        scene.cell(PLANE_A, column, 0, name(SOLID, PAL_A));
    }
    scene.hscroll(0, 4, 0);
    assert_region(
        scene.frame(),
        "window left",
        0,
        0,
        &["wwwwwwww wwwwwwww aaaaaaaa aaaaaaaa aaaa...."],
    );

    let mut scene = Scene::new(true);
    scene.register(17, 0x80 | 18);
    for column in 36..40 {
        scene.cell_in(WINDOW, 64, column, 0, window);
    }
    scene.cell(PLANE_A, 3, 0, name(SOLID, PAL_A));
    let frame = scene.frame();
    // Plane A is 32 cells wide, so its cell 3 shows again at x 280.
    assert_rows(frame, "window right", 280, 0, &rows(&cells("awwww"), 8));
    assert_rows(frame, "window right", 0, 0, &rows(&cells("...a"), 8));

    // From cell row 1 down, the window takes whole lines.
    let mut scene = Scene::new(true);
    scene.register(17, 0x81);
    scene.register(18, 0x81);
    scene.cell(PLANE_A, 0, 0, name(SOLID, PAL_A));
    scene.cell(PLANE_A, 0, 1, name(SOLID, PAL_A));
    scene.cell_in(WINDOW, 64, 39, 0, window);
    scene.cell_in(WINDOW, 64, 1, 1, window);
    let frame = scene.frame();
    let mut expected = rows(&cells("a."), 8);
    expected.extend(rows(&cells(".w"), 8));
    assert_rows(frame, "window down", 0, 0, &expected);

    // Above cell row 1, the window takes whole lines.
    let mut scene = Scene::new(true);
    scene.register(18, 0x01);
    scene.cell(PLANE_A, 0, 0, name(SOLID, PAL_A));
    scene.cell(PLANE_A, 0, 1, name(SOLID, PAL_A));
    scene.cell_in(WINDOW, 64, 1, 0, window);
    let frame = scene.frame();
    let mut expected = rows(&cells(".w"), 8);
    expected.extend(rows(&cells("a."), 8));
    assert_rows(frame, "window up", 0, 0, &expected);
}

#[test]
fn test_h32_draws_256_pixels_with_a_32_cell_window() {
    let mut scene = Scene::new(false);
    scene.register(18, 0x02);
    scene.cell_in(WINDOW, 32, 1, 1, name(SOLID3, PAL_A));
    scene.cell(PLANE_B, 31, 2, name(SOLID, PAL_B));
    let frame = scene.frame();
    assert_eq!(frame.width(), H32_WIDTH);
    assert_eq!(frame.row(0).len(), H32_WIDTH);
    assert_rows(frame, "h32", 0, 8, &rows(&cells(".w."), 8));
    assert_rows(frame, "h32", 248, 16, &rows(&cells("b"), 8));
}

#[test]
fn test_left_column_blank_shows_the_backdrop() {
    let mut scene = Scene::new(true);
    scene.register(0, 0x24);
    scene.cell(PLANE_B, 0, 0, name(SOLID, PAL_B));
    scene.cell(PLANE_B, 1, 0, name(SOLID, PAL_B));
    assert_rows(scene.frame(), "left blank", 0, 0, &rows(&cells(".b."), 8));
}

#[test]
fn test_lines_are_drawn_from_the_state_where_their_pixels_start() {
    let mut scene = Scene::new(true);
    scene.register(7, 0x09);
    // Up to the start of line 100, whose pixels start a little later.
    let start = 100 * LINE_CLOCKS;
    scene.vdp.advance((start / CPU_DIVIDER) as u32);
    assert!(scene.vdp.frame().row(0).iter().all(|&pixel| pixel == 0));
    scene.register(7, 0x12);
    let frame = scene.frame();
    assert!((0..100).all(|y| frame.row(y).iter().all(|&p| p == rgb(cram_word(0x09)))));
    assert!((100..HEIGHT).all(|y| frame.row(y).iter().all(|&p| p == rgb(cram_word(0x12)))));
}

#[test]
fn test_sprite_cells_run_down_columns_and_flips_reorder_them() {
    let mut scene = Scene::new(true);
    let word = name(QUAD, PAL_S);
    for (index, flips) in [0, HFLIP, VFLIP, HFLIP | VFLIP].into_iter().enumerate() {
        let index = index as u16;
        scene.sprite(index, index * 24, 0, (2, 2), index + 1, word | flips);
    }
    scene.sprite(4, 96, 16, (1, 1), 0, name(ARROW, PAL_S) | HFLIP | VFLIP);
    let frame = scene.frame();
    let mut expected = rows(&cells("st.ts.Su.uS"), 8);
    expected.extend(rows(&cells("Su.uS.st.ts"), 8));
    assert_rows(frame, "sprite cells", 0, 0, &expected);
    assert_region(
        frame,
        "sprite flips",
        96,
        16,
        &[
            "S......S", ".......s", "......ss", "....ssss", "...Sssss", "....Ssss", ".....Sss",
            "......Ss",
        ],
    );
}

#[test]
fn test_sprites_follow_the_link_list_and_earlier_ones_stay_in_front() {
    let mut scene = Scene::new(true);
    // Sprite 0 links to 2; sprite 1 is not in the list. Sprite 2's priority bit puts it
    // above the planes, not above sprite 0.
    scene.sprite(0, 0, 0, (1, 1), 2, name(SOLID, PAL_S));
    scene.sprite(1, 40, 0, (1, 1), 0, name(SOLID, PAL_S));
    scene.sprite(2, 4, 0, (1, 1), 0, name(QUAD + 1, PAL_S) | PRIORITY);
    scene.cell(PLANE_A, 1, 0, name(SOLID, PAL_A) | PRIORITY);
    let frame = scene.frame();
    assert_rows(
        frame,
        "sprite links",
        0,
        0,
        &rows("ssssssss SSSSaaaa ........ ........ ........ ........", 8),
    );
}

#[test]
fn test_a_line_takes_20_sprites_in_h40_and_16_in_h32() {
    for (h40, limit) in [(true, 20), (false, 16)] {
        let mut scene = Scene::new(h40);
        // One sprite off the picture counts as well.
        scene.sprite_at(0, 1, 0, (1, 1), 1, name(SOLID, PAL_S));
        for index in 1..=limit {
            scene.sprite(
                index,
                (index - 1) * 8,
                0,
                (1, 1),
                index + 1,
                name(SOLID, PAL_S),
            );
        }
        scene.sprite(limit + 1, 0, 8, (1, 1), 0, name(SOLID, PAL_S));
        let frame = scene.frame();
        let visible = "s".repeat(usize::from(limit) - 1) + ".";
        assert_rows(frame, "sprites per line", 0, 0, &rows(&cells(&visible), 8));
        // Sprites past the limit on one line still show on lines where they fit.
        assert_rows(frame, "sprites per line", 0, 8, &rows(&cells("s."), 8));
    }
}

#[test]
fn test_a_line_takes_320_sprite_pixels_in_h40_and_cuts_the_sprite_that_overruns() {
    let mut scene = Scene::new(true);
    // 296 pixels off the picture: nine sprites of 4 cells and one of 1.
    for index in 0..9 {
        scene.sprite_at(index, 1, 0, (4, 1), index + 1, name(SOLID, PAL_S));
    }
    scene.sprite_at(9, 1, 0, (1, 1), 10, name(SOLID, PAL_S));
    scene.sprite(10, 0, 0, (4, 1), 11, name(QUAD, PAL_S));
    scene.sprite(11, 64, 0, (1, 1), 0, name(SOLID, PAL_S));
    assert_rows(
        scene.frame(),
        "sprite pixels",
        0,
        0,
        &rows(&cells("sSt....."), 8),
    );
}

#[test]
fn test_sprite_limit_off_draws_past_both_limits_with_the_hardware_status() {
    // The count: the 21st sprite on the line shows, and one more on top of the first
    // visible one stays behind it without a collision.
    let mut scene = Scene::new(true);
    scene.vdp.set_sprite_limit(false);
    scene.sprite_at(0, 1, 0, (1, 1), 1, name(SOLID, PAL_S));
    for index in 1..=20 {
        scene.sprite(
            index,
            (index - 1) * 8,
            0,
            (1, 1),
            index + 1,
            name(SOLID, PAL_S),
        );
    }
    scene.sprite(21, 0, 0, (1, 1), 0, name(QUAD + 1, PAL_S));
    let frame = scene.frame();
    assert_rows(frame, "count", 0, 0, &rows(&cells(&"s".repeat(20)), 8));
    assert_eq!(scene.sprite_status(), status::SPRITE_OVERFLOW);

    // The pixels: the sprite that overruns shows whole, and so do the ones after it.
    let mut scene = Scene::new(true);
    scene.vdp.set_sprite_limit(false);
    for index in 0..9 {
        scene.sprite_at(index, 1, 0, (4, 1), index + 1, name(SOLID, PAL_S));
    }
    scene.sprite_at(9, 1, 0, (1, 1), 10, name(SOLID, PAL_S));
    scene.sprite(10, 0, 0, (4, 1), 11, name(QUAD, PAL_S));
    scene.sprite(11, 64, 0, (1, 1), 0, name(SOLID, PAL_S));
    assert_rows(scene.frame(), "pixels", 0, 0, &rows(&cells("sStu....s"), 8));
    assert_eq!(scene.sprite_status(), 0);
}

#[test]
fn test_an_x_0_sprite_masks_later_sprites_after_one_at_another_x() {
    let mut scene = Scene::new(true);
    // Lines 0-7: a sprite off the picture, then the mask, then a hidden sprite.
    scene.sprite_at(0, 1, 0, (1, 1), 1, name(SOLID, PAL_S));
    scene.sprite_at(1, 0, 0, (1, 1), 2, name(SOLID, PAL_S));
    scene.sprite(2, 0, 0, (1, 1), 3, name(SOLID, PAL_S));
    // Lines 16-23: the mask comes first, so it masks nothing.
    scene.sprite_at(3, 0, 16, (1, 1), 4, name(SOLID, PAL_S));
    scene.sprite(4, 0, 16, (1, 1), 0, name(SOLID, PAL_S));
    let frame = scene.frame();
    assert_rows(frame, "masking", 0, 0, &rows(&cells("."), 8));
    assert_rows(frame, "masking", 0, 16, &rows(&cells("s"), 8));
}

#[test]
fn test_an_x_0_sprite_masks_from_the_start_after_a_line_used_all_its_pixels() {
    let mut scene = Scene::new(true);
    // Lines 0-7 use all 320 sprite pixels off the picture.
    for index in 0..10 {
        scene.sprite_at(index, 1, 0, (4, 1), index + 1, name(SOLID, PAL_S));
    }
    // From line 8: a mask first, then a sprite it hides on line 8 only.
    scene.sprite_at(10, 0, 8, (1, 1), 11, name(SOLID, PAL_S));
    scene.sprite(11, 0, 8, (1, 1), 0, name(SOLID, PAL_S));
    let frame = scene.frame();
    let mut expected = rows(&cells("."), 1);
    expected.extend(rows(&cells("s"), 7));
    assert_rows(frame, "masking after overflow", 0, 8, &expected);
}

#[test]
fn test_h32_links_64_to_79_read_sprites_0_to_15_and_80_ends_the_walk() {
    // H40: link 65 reads sprite 65.
    let mut scene = Scene::new(true);
    scene.sprite(0, 0, 0, (1, 1), 65, name(SOLID, PAL_S));
    scene.sprite(1, 8, 0, (1, 1), 0, name(QUAD + 1, PAL_S));
    scene.sprite(65, 16, 0, (1, 1), 0, name(SOLID, PAL_S));
    assert_rows(scene.frame(), "links h40", 0, 0, &rows(&cells("s.s."), 8));

    // H32: link 65 reads sprite 1, whose link of 80 ends the walk before sprite 16.
    let mut scene = Scene::new(false);
    scene.sprite(0, 0, 0, (1, 1), 65, name(SOLID, PAL_S));
    scene.sprite(1, 8, 0, (1, 1), 80, name(QUAD + 1, PAL_S));
    scene.sprite(16, 16, 0, (1, 1), 0, name(SOLID, PAL_S));
    assert_rows(scene.frame(), "links h32", 0, 0, &rows(&cells("sS.."), 8));
}

#[test]
fn test_status_reports_sprite_overflow_and_collision_until_read() {
    let sprite = name(SOLID, PAL_S);
    // Two sprites' opaque pixels meeting in the picture collide.
    let mut scene = Scene::new(true);
    scene.sprite(0, 0, 0, (1, 1), 1, sprite);
    scene.sprite(1, 4, 0, (1, 1), 0, sprite);
    scene.frame();
    assert_eq!(scene.sprite_status(), status::SPRITE_COLLISION);
    assert_eq!(scene.sprite_status(), 0);

    // Off the picture, or hidden by an x = 0 mask, they do not.
    let mut scene = Scene::new(true);
    scene.sprite_at(0, 1, 0, (1, 1), 1, sprite);
    scene.sprite_at(1, 1, 0, (1, 1), 2, sprite);
    scene.sprite_at(2, 0, 0, (1, 1), 3, sprite);
    scene.sprite(3, 0, 0, (1, 1), 4, sprite);
    scene.sprite(4, 0, 0, (1, 1), 0, sprite);
    scene.frame();
    assert_eq!(scene.sprite_status(), 0);

    // One sprite past a line's count overflows, also once its pixels ran out; running out
    // of pixels alone does not.
    for (h40, count, flag) in [
        (true, 21, status::SPRITE_OVERFLOW),
        (false, 17, status::SPRITE_OVERFLOW),
        (true, 20, 0),
        (false, 16, 0),
    ] {
        let mut scene = Scene::new(h40);
        for index in 0..count {
            let link = if index + 1 < count { index + 1 } else { 0 };
            scene.sprite_at(index, 1, 0, (4, 1), link, sprite);
        }
        scene.frame();
        assert_eq!(scene.sprite_status(), flag, "h40 {h40}, {count} sprites");
    }
}

#[test]
fn test_shadow_highlight_shades_by_priority_and_sprite_operators() {
    let b = name(SOLID, PAL_B);
    let a = name(SOLID, PAL_A);
    let s = name(SOLID, PAL_S);
    let highlight = name(SOLID14, PAL_S);
    let shadow = name(SOLID15, PAL_S);
    let b_high = b | PRIORITY;
    let backdrop = 0x09;
    let (dark, normal, bright) = (Shade::Shadow, Shade::Normal, Shade::Highlight);
    // Per cell column: plane B, plane A (0, pattern 0, is empty and low), sprites front to
    // back, and the index and shade that show.
    let columns: [(u16, u16, &[u16], u8, Shade); 14] = [
        (b, 0, &[], 0x11, dark),
        (b_high, 0, &[], 0x11, normal),
        // A high-priority cell of colour 0 lifts the shadow too.
        (b, PRIORITY, &[], 0x11, normal),
        (0, 0, &[], backdrop, dark),
        (b, 0, &[s], 0x31, dark),
        (b, 0, &[s | PRIORITY], 0x31, normal),
        // Sprite colour 14 outside palette 3 is never shadowed.
        (b, 0, &[name(SOLID14, PAL_A)], 0x2E, normal),
        // Operators: a highlight undoes a shadow, two shadows stay one; over a
        // high-priority plane only a high-priority operator is in front.
        (b, 0, &[highlight], 0x11, normal),
        (b_high, 0, &[highlight | PRIORITY], 0x11, bright),
        (b_high, 0, &[shadow | PRIORITY], 0x11, dark),
        (b, 0, &[shadow], 0x11, dark),
        // A plane in front of the sprite leaves its operator out.
        (0, a | PRIORITY, &[highlight], 0x21, normal),
        (0, 0, &[highlight], backdrop, normal),
        // An operator hides the sprites behind it and acts on the planes.
        (b_high, 0, &[shadow | PRIORITY, s], 0x11, dark),
    ];
    let sprites: Vec<(u16, u16)> = columns
        .iter()
        .enumerate()
        .flat_map(|(column, &(_, _, words, _, _))| {
            words.iter().map(move |&word| (column as u16, word))
        })
        .collect();
    let scene = |shadow_highlight: bool| {
        let mut scene = Scene::new(true);
        scene.register(12, if shadow_highlight { 0x89 } else { 0x81 });
        scene.register(7, backdrop);
        for (column, &(plane_b, plane_a, _, _, _)) in columns.iter().enumerate() {
            scene.cell(PLANE_B, column as u16, 0, plane_b);
            scene.cell(PLANE_A, column as u16, 0, plane_a);
        }
        for (index, &(column, word)) in sprites.iter().enumerate() {
            let link = if index + 1 < sprites.len() {
                index as u16 + 1
            } else {
                0
            };
            scene.sprite(index as u16, column * 8, 0, (1, 1), link, word);
        }
        scene
    };
    let mut lit = scene(true);
    let frame = lit.frame();
    for (column, &(_, _, _, index, shade)) in columns.iter().enumerate() {
        let expected = shaded(index, shade);
        let cell = |y: usize| &frame.row(y)[column * 8..][..8];
        if !(0..8).all(|y| cell(y).iter().all(|&pixel| pixel == expected)) {
            let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dumps");
            let _ = fs::create_dir_all(&folder);
            let _ = fs::write(folder.join("render-shadow-highlight.bmp"), frame.to_bmp());
            panic!("column {column}: expected index {index:02X} {shade:?}");
        }
    }

    // Normal brightness is the colour drawn without shadow/highlight, and with it off the
    // operator colours draw as plain sprite colours.
    assert!((0..64).all(|index| shaded(index, Shade::Normal) == rgb(cram_word(index))));
    let mut plain = scene(false);
    let frame = plain.frame();
    assert_eq!(frame.row(0)[7 * 8], rgb(cram_word(0x3E)));
    assert_eq!(frame.row(0)[0], rgb(cram_word(0x11)));
}

#[test]
fn test_frames_save_as_bottom_up_24_bit_bmp() {
    let mut scene = Scene::new(true);
    scene.cell(PLANE_B, 0, 27, name(SOLID, PAL_B));
    let bmp = scene.frame().to_bmp();
    let le32 = |at: usize| u32::from_le_bytes(bmp[at..at + 4].try_into().expect("4 bytes"));
    assert_eq!(&bmp[..2], b"BM");
    assert_eq!(bmp.len(), 54 + MAX_WIDTH * HEIGHT * 3);
    assert_eq!(le32(2) as usize, bmp.len());
    assert_eq!(le32(10), 54);
    assert_eq!(le32(18) as usize, MAX_WIDTH);
    assert_eq!(le32(22) as usize, HEIGHT);
    assert_eq!(u16::from_le_bytes([bmp[28], bmp[29]]), 24);
    // The first stored row is the bottom one, blue first.
    let [_, red, green, blue] = rgb(cram_word(0x11)).to_be_bytes();
    assert_eq!(bmp[54..57], [blue, green, red]);
    assert_eq!(bmp[54 + 8 * 3..57 + 8 * 3], [0, 0, 0]);
}

/// A scene drawn with `margin` widescreen pixels on each side.
fn wide_scene(h40: bool, margin: usize) -> Scene {
    let mut scene = Scene::new(h40);
    scene.vdp.set_widescreen(margin);
    scene
}

#[test]
fn test_widescreen_planes_continue_past_both_edges_in_both_modes() {
    // A 32-cell plane is 256 pixels wide: the left margin shows its last columns and the
    // right margin wraps on into its first ones.
    let mut scene = wide_scene(true, 16);
    scene.cell(PLANE_B, 31, 0, name(SOLID, PAL_B));
    scene.cell(PLANE_B, 8, 0, name(SOLID, PAL_B));
    scene.cell(PLANE_A, 0, 0, name(SOLID, PAL_A));
    let frame = scene.frame();
    assert_eq!(frame.width(), MAX_WIDTH + 32);
    assert_rows(frame, "left", 0, 0, &rows(&cells(".ba."), 8));
    assert_rows(frame, "right", 336, 0, &rows(&cells("b."), 8));
    assert_eq!(frame.to_bmp().len(), 54 + (MAX_WIDTH + 32) * HEIGHT * 3);

    // Scrolled right by 4, the plane's last column moves on by 4 pixels in the margin.
    scene.hscroll(0, 0, 4);
    // The frame in progress may hold lines from before the change.
    scene.frame();
    let frame = scene.frame();
    let expected = format!("{}{}", ".".repeat(12), "b".repeat(4));
    assert_rows(frame, "scrolled", 0, 0, &rows(&expected, 8));

    let mut scene = wide_scene(false, 16);
    scene.cell(PLANE_B, 31, 0, name(SOLID, PAL_B));
    let frame = scene.frame();
    assert_eq!(frame.width(), H32_WIDTH + 32);
    assert_rows(frame, "H32 left", 0, 0, &rows(&cells(".b"), 8));
}

#[test]
fn test_widescreen_sprites_show_where_the_hardware_hides_them_without_status() {
    let mut scene = wide_scene(true, 16);
    // Two overlapping sprites in the left margin and one past the right edge.
    scene.sprite_at(0, 128 - 8, 0, (1, 1), 1, name(SOLID, PAL_S));
    scene.sprite_at(1, 128 - 8, 0, (1, 1), 2, name(QUAD + 1, PAL_S));
    let past_right = 128 + MAX_WIDTH as u16 + 8;
    scene.sprite_at(2, past_right, 0, (1, 1), 0, name(QUAD + 1, PAL_S));
    let frame = scene.frame();
    assert_rows(frame, "left", 0, 0, &rows(&cells(".s."), 8));
    assert_rows(frame, "right", 336, 0, &rows(&cells(".S"), 8));
    assert_eq!(scene.sprite_status(), 0);
}

#[test]
fn test_widescreen_draws_sprites_past_the_line_limits_in_the_margins_only() {
    // The count: 20 sprites in the left margin fill the line, so the next one, in the
    // picture, stays hidden as on the hardware, and one more in the right margin shows.
    let mut scene = wide_scene(true, 16);
    for index in 0..20 {
        scene.sprite_at(index, 128 - 16, 0, (1, 1), index + 1, name(SOLID, PAL_S));
    }
    scene.sprite(20, 0, 0, (1, 1), 21, name(SOLID, PAL_S));
    let past_right = 128 + MAX_WIDTH as u16 + 8;
    scene.sprite_at(21, past_right, 0, (1, 1), 0, name(QUAD + 1, PAL_S));
    let frame = scene.frame();
    assert_rows(frame, "count left", 0, 0, &rows(&cells("s.."), 8));
    assert_rows(frame, "count right", 336, 0, &rows(&cells(".S"), 8));
    assert_eq!(scene.sprite_status(), status::SPRITE_OVERFLOW);

    // The pixels: ten 32-pixel sprites off the picture use the line's 320, as a game's
    // masking does; the sprite after them stays hidden in the picture and shows in the
    // right margin.
    let mut scene = wide_scene(true, 16);
    for index in 0..10 {
        scene.sprite_at(index, 1, 0, (4, 1), index + 1, name(SOLID, PAL_S));
    }
    scene.sprite(10, 0, 0, (1, 1), 11, name(SOLID, PAL_S));
    scene.sprite_at(11, past_right, 0, (1, 1), 0, name(QUAD + 1, PAL_S));
    let frame = scene.frame();
    assert_rows(frame, "pixels left", 0, 0, &rows(&cells("..."), 8));
    assert_rows(frame, "pixels right", 336, 0, &rows(&cells(".S"), 8));
    assert_eq!(scene.sprite_status(), 0);
}

#[test]
fn test_widescreen_view_hides_the_margins_in_black_from_the_next_frame() {
    let mut scene = wide_scene(true, 16);
    scene.register(7, 0x11);
    let backdrop = rgb(cram_word(0x11));
    let frame = scene.frame();
    assert!(frame.row(0).iter().all(|&pixel| pixel == backdrop));

    scene.vdp.set_view(4, 0);
    scene.frame();
    let frame = scene.frame();
    let left = format!("{}{}", ".".repeat(12), "b".repeat(4));
    assert_rows(frame, "left", 0, 0, &rows(&left, HEIGHT));
    assert_rows(frame, "middle", 16, 0, &rows(&"b".repeat(MAX_WIDTH), 1));
    assert_rows(frame, "right", 336, 0, &rows(&".".repeat(16), HEIGHT));

    // A view past the margin shows all of it; 0 and 0 pillarbox the picture.
    scene.vdp.set_view(99, 99);
    scene.frame();
    assert_rows(scene.frame(), "all", 0, 0, &rows(&"b".repeat(352), 1));
    scene.vdp.set_view(0, 0);
    scene.frame();
    let frame = scene.frame();
    let edge = ".".repeat(16);
    let pillarbox = format!("{edge}{}{edge}", "b".repeat(MAX_WIDTH));
    assert_rows(frame, "pillarbox", 0, 0, &rows(&pillarbox, 1));
}

#[test]
fn test_widescreen_window_stays_inside_the_picture_and_the_blank_moves_left() {
    // The window covers lines 0-7 from edge to edge of the console's picture; the margins
    // beside it show plane A.
    let mut scene = wide_scene(true, 8);
    scene.register(18, 0x01);
    scene.cell_in(WINDOW, 64, 0, 0, name(SOLID3, PAL_A));
    scene.cell_in(WINDOW, 64, 39, 0, name(SOLID3, PAL_A));
    scene.cell(PLANE_A, 31, 0, name(SOLID, PAL_A));
    scene.cell(PLANE_A, 8, 0, name(SOLID, PAL_A));
    let frame = scene.frame();
    assert_rows(frame, "left", 0, 0, &rows(&cells("aw"), 8));
    assert_rows(frame, "right", 320, 0, &rows(&cells("wa"), 8));

    // The left-column blank covers the wide picture's first eight pixels, not the console's.
    let mut scene = wide_scene(true, 8);
    scene.register(0, 0x24);
    scene.register(7, 0x12);
    scene.cell(PLANE_B, 31, 0, name(SOLID, PAL_B));
    scene.cell(PLANE_B, 0, 0, name(SOLID, PAL_B));
    assert_rows(scene.frame(), "blank", 0, 0, &rows(&cells("Bb"), 8));
}

#[test]
fn test_widescreen_two_cell_vscroll_continues_the_edge_columns_into_the_margins() {
    // Lines 0-7 of plane B show its row 1 where a column scrolls by 8.
    let mut scene = wide_scene(true, 16);
    scene.register(11, 0x04);
    scene.cell(PLANE_B, 31, 1, name(SOLID, PAL_B));
    scene.cell(PLANE_B, 8, 1, name(SOLID, PAL_B));
    scene.cell(PLANE_B, 30, 0, name(SOLID, PAL_B));
    // The part-shown column (words 38 AND 39) is also the last column's pair.
    scene.vsram(38, &[8, 8]);
    let frame = scene.frame();
    assert_rows(frame, "left", 0, 0, &rows(&cells(".b"), 8));
    assert_rows(frame, "right", 336, 0, &rows(&cells("b."), 8));
}
