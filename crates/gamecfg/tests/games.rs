//! The configs shipped in `games/` parse and validate.

use std::path::Path;

use anygenesis_gamecfg::GameConfig;

#[test]
fn test_parse_games_sonic1_valid_and_keyed_on_rev00() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/sonic1.toml");
    let text = std::fs::read_to_string(path).expect("games/sonic1.toml is readable");
    let config = match GameConfig::parse(&text) {
        Ok(config) => config,
        Err(error) => panic!("games/sonic1.toml:\n{error}"),
    };
    assert_eq!(config.sha1, "6ddb7de1e17e7f6cdb88927bd906352030daa194");
    assert!(
        config.report.is_some(),
        "the game binary's Sonic 1 checks read [report]"
    );
}
