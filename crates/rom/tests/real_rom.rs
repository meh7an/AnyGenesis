//! Loads the real ROM named by `ANYGENESIS_ROM` and prints its header and vectors for a
//! manual check; the title is asserted for the ROMs the project tests with. Run with
//! `cargo test -p anygenesis-rom --test real_rom -- --ignored --nocapture`.

use anygenesis_rom::{Rom, VectorTable, vector_name};

/// SHA-1 and expected title of the ROMs the project tests with. Sega Retro, "Sonic the
/// Hedgehog (Mega Drive)/Technical information" (Revision 00) and "Sonic the Hedgehog 2
/// (Mega Drive)" (Revision 01).
const KNOWN_TITLES: [(&str, &str); 2] = [
    (
        "6ddb7de1e17e7f6cdb88927bd906352030daa194",
        "SONIC THE HEDGEHOG",
    ),
    (
        "8bca5dcef1af3e00098666fd892dc1c2a76333f9",
        "SONIC THE HEDGEHOG 2",
    ),
];

#[test]
#[ignore = "needs ANYGENESIS_ROM pointing at a ROM image"]
fn test_load_real_rom_header_and_vectors_read() {
    let path = std::env::var_os("ANYGENESIS_ROM").expect("ANYGENESIS_ROM is not set");
    let rom = Rom::load(&path).expect("ANYGENESIS_ROM loads");
    let header = rom.header();

    println!("file            {}", path.to_string_lossy());
    println!("size            {:#x} bytes", rom.bytes().len());
    println!("sha1            {}", rom.sha1());
    println!("system type     {}", header.system_type);
    println!("copyright       {}", header.copyright);
    println!("domestic title  {}", header.domestic_title);
    println!("overseas title  {}", header.overseas_title);
    println!("serial          {}", header.serial);
    println!(
        "checksum        header {:#06x}, computed {:#06x}",
        header.checksum,
        rom.computed_checksum()
    );
    println!(
        "rom range       {:#010x}-{:#010x}",
        header.rom_range.start(),
        header.rom_range.end()
    );
    println!(
        "ram range       {:#010x}-{:#010x}",
        header.ram_range.start(),
        header.ram_range.end()
    );
    println!(
        "region          {:?} -> {:?}",
        header.region_code, header.regions
    );
    for warning in rom.warnings() {
        println!("warning         {warning}");
    }
    for (index, value) in rom.vectors().entries().iter().enumerate() {
        let name = vector_name(index).unwrap_or("?");
        println!("vector {index:2}       {value:#010x}  {name}");
    }

    assert_eq!(rom.vectors().entries().len(), VectorTable::LEN);
    assert_eq!(rom.warnings(), []);
    let sha1 = rom.sha1().to_string();
    match KNOWN_TITLES.iter().find(|(known, _)| *known == sha1) {
        Some((_, title)) => assert_eq!(rom.title(), *title),
        None => println!("title           {} (not a known ROM)", rom.title()),
    }
}
