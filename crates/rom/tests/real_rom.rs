//! Loads the real ROM named by `ANYGENESIS_ROM` and prints its header and vectors for a
//! manual check. Run with
//! `cargo test -p anygenesis-rom --test real_rom -- --ignored --nocapture`.

use anygenesis_rom::{KNOWN_ROMS, Rom, VectorTable, vector_name};

#[test]
#[ignore = "needs ANYGENESIS_ROM pointing at the Sonic 1 REV00 .bin"]
fn test_load_real_rom_sonic1_rev00_identified() {
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
    match rom.identify() {
        Ok(known) => assert_eq!(known, KNOWN_ROMS[0]),
        Err(error) => panic!("{error}"),
    }
}
