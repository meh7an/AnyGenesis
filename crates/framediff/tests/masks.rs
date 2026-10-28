//! The signed-off mask files in tests/masks/, one per game, parse and keep their masks.

use std::{fs, path::Path};

use anygenesis_framediff::Masks;

fn masks_folder() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/masks"))
}

#[test]
fn test_every_mask_file_parses() {
    let mut files = 0;
    for entry in fs::read_dir(masks_folder()).expect("tests/masks is readable") {
        let path = entry.expect("tests/masks lists").path();
        let masks = Masks::load(&path).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            !masks.masks().is_empty(),
            "{} holds no mask",
            path.display()
        );
        files += 1;
    }
    assert!(files > 0, "tests/masks holds no file");
}

#[test]
fn test_sonic1_masks_are_the_two_stacked_pc_bytes() {
    let masks =
        Masks::load(&masks_folder().join("sonic1.toml")).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(masks.masks().len(), 2);
}
