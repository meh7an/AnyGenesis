use std::collections::HashMap;

use anygenesis_shared::Address;
use serde::Deserialize;
use toml::Spanned;

use crate::{
    ConfigError, DataRegion, EntryKind, FunctionHint, GameConfig, IndirectHint, JumpTable,
    Location, Problem, ProblemKind, TargetKind,
};

/// The file as written; values keep their byte spans until validation turns them into
/// locations.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    sha1: Spanned<String>,
    #[serde(default)]
    function: Vec<RawFunction>,
    #[serde(default)]
    jump_table: Vec<RawJumpTable>,
    #[serde(default)]
    data: Vec<RawData>,
    #[serde(default)]
    indirect: Vec<RawIndirect>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFunction {
    address: Spanned<u32>,
    #[serde(default)]
    noreturn: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJumpTable {
    address: Spanned<u32>,
    count: Spanned<u32>,
    kind: EntryKind,
    #[serde(default)]
    site: Option<Spanned<u32>>,
    #[serde(default)]
    target: Option<TargetKind>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawData {
    start: Spanned<u32>,
    end: Spanned<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndirect {
    site: Spanned<u32>,
    targets: Spanned<Vec<Spanned<u32>>>,
}

pub(crate) fn parse(text: &str) -> Result<GameConfig, ConfigError> {
    let raw: RawConfig = toml::from_str(text).map_err(|error| ConfigError::Parse {
        location: error
            .span()
            .map(|span| Location::of_offset(text, span.start)),
        message: error.message().to_owned(),
    })?;
    let mut validator = Validator {
        text,
        problems: Vec::new(),
    };
    let config = validator.config(raw);
    if validator.problems.is_empty() {
        Ok(config)
    } else {
        Err(ConfigError::Invalid {
            problems: validator.problems,
        })
    }
}

/// Builds the config while collecting every rule broken, so one run reports them all.
struct Validator<'a> {
    text: &'a str,
    problems: Vec<Problem>,
}

impl Validator<'_> {
    fn config(&mut self, raw: RawConfig) -> GameConfig {
        let sha1 = self.sha1(&raw.sha1);
        let data_regions = self.data_regions(&raw.data);
        let functions = self.functions(&raw.function, &data_regions);
        let jump_tables = self.jump_tables(&raw.jump_table, &data_regions);
        let indirect = self.indirect(&raw.indirect, &data_regions);
        GameConfig {
            sha1,
            functions,
            jump_tables,
            data_regions,
            indirect,
        }
    }

    fn sha1(&mut self, value: &Spanned<String>) -> String {
        let text = value.get_ref();
        if text.len() != 40 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            self.report(self.location(value), ProblemKind::BadSha1);
        }
        text.to_ascii_lowercase()
    }

    fn data_regions(&mut self, raw: &[RawData]) -> Vec<DataRegion> {
        let regions: Vec<DataRegion> = raw
            .iter()
            .map(|entry| {
                let (start, location) = self.address(&entry.start);
                let (end, end_location) = self.address(&entry.end);
                if end <= start {
                    self.report(end_location, ProblemKind::EmptyRegion { start, end });
                }
                DataRegion {
                    start,
                    end,
                    location,
                }
            })
            .collect();
        let mut by_start: Vec<&DataRegion> = regions
            .iter()
            .filter(|region| region.start < region.end)
            .collect();
        by_start.sort_by_key(|region| region.start);
        for pair in by_start.windows(2) {
            if pair[1].start < pair[0].end {
                self.report(
                    pair[1].location,
                    ProblemKind::OverlappingRegions {
                        other: pair[0].location,
                    },
                );
            }
        }
        regions
    }

    fn functions(&mut self, raw: &[RawFunction], data: &[DataRegion]) -> Vec<FunctionHint> {
        let mut seen = HashMap::new();
        raw.iter()
            .map(|entry| {
                let (address, location) = self.code_address(&entry.address, data);
                self.unique(&mut seen, address, location);
                FunctionHint {
                    address,
                    noreturn: entry.noreturn,
                    location,
                }
            })
            .collect()
    }

    fn jump_tables(&mut self, raw: &[RawJumpTable], data: &[DataRegion]) -> Vec<JumpTable> {
        let mut seen = HashMap::new();
        raw.iter()
            .map(|entry| {
                let (address, location) = self.even_address(&entry.address);
                self.unique(&mut seen, address, location);
                if *entry.count.get_ref() == 0 {
                    self.report(self.location(&entry.count), ProblemKind::ZeroCount);
                }
                let site = entry
                    .site
                    .as_ref()
                    .map(|site| self.code_address(site, data).0);
                JumpTable {
                    address,
                    count: *entry.count.get_ref(),
                    kind: entry.kind,
                    site,
                    target: entry.target,
                    location,
                }
            })
            .collect()
    }

    fn indirect(&mut self, raw: &[RawIndirect], data: &[DataRegion]) -> Vec<IndirectHint> {
        let mut seen = HashMap::new();
        raw.iter()
            .map(|entry| {
                let (site, location) = self.code_address(&entry.site, data);
                self.unique(&mut seen, site, location);
                if entry.targets.get_ref().is_empty() {
                    self.report(self.location(&entry.targets), ProblemKind::NoTargets);
                }
                let targets = entry
                    .targets
                    .get_ref()
                    .iter()
                    .map(|target| self.code_address(target, data).0)
                    .collect();
                IndirectHint {
                    site,
                    targets,
                    location,
                }
            })
            .collect()
    }

    /// Reads an address that must hold code: even and outside every data region.
    fn code_address(&mut self, value: &Spanned<u32>, data: &[DataRegion]) -> (Address, Location) {
        let (address, location) = self.even_address(value);
        if let Some(region) = data.iter().find(|region| region.contains(address)) {
            self.report(
                location,
                ProblemKind::InsideData {
                    address,
                    region: region.location,
                },
            );
        }
        (address, location)
    }

    fn even_address(&mut self, value: &Spanned<u32>) -> (Address, Location) {
        let (address, location) = self.address(value);
        if *value.get_ref() <= Address::MASK && !address.is_even() {
            self.report(location, ProblemKind::OddAddress { address });
        }
        (address, location)
    }

    fn address(&mut self, value: &Spanned<u32>) -> (Address, Location) {
        let location = self.location(value);
        let raw = *value.get_ref();
        if raw > Address::MASK {
            self.report(location, ProblemKind::AddressOutOfRange { value: raw });
        }
        (Address::new(raw), location)
    }

    fn unique(
        &mut self,
        seen: &mut HashMap<Address, Location>,
        address: Address,
        location: Location,
    ) {
        if let Some(&first) = seen.get(&address) {
            self.report(location, ProblemKind::Duplicate { address, first });
        } else {
            seen.insert(address, location);
        }
    }

    fn location<T>(&self, value: &Spanned<T>) -> Location {
        Location::of_offset(self.text, value.span().start)
    }

    fn report(&mut self, location: Location, kind: ProblemKind) {
        self.problems.push(Problem { location, kind });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA1: &str = "6ddb7de1e17e7f6cdb88927bd906352030daa194";

    fn with_sha1(body: &str) -> String {
        format!("sha1 = \"{SHA1}\"\n{body}")
    }

    fn problems(text: &str) -> Vec<Problem> {
        match parse(text) {
            Err(ConfigError::Invalid { problems }) => problems,
            other => panic!("expected validation problems, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_sha1_only_gives_empty_config() {
        let config = parse(&with_sha1("")).expect("valid");
        assert_eq!(config.sha1, SHA1);
        assert!(config.functions.is_empty());
        assert!(config.jump_tables.is_empty());
        assert!(config.data_regions.is_empty());
        assert!(config.indirect.is_empty());
    }

    #[test]
    fn test_parse_every_section_reads_values_and_lines() {
        let text = with_sha1(
            "\n[[function]]\naddress = 0x1000\nnoreturn = true\n\
             \n[[jump_table]]\naddress = 0x2000\ncount = 3\nkind = \"word_offset\"\n\
             site = 0x1FF8\ntarget = \"block\"\n\
             \n[[jump_table]]\naddress = 0x2100\ncount = 2\nkind = \"bra_w\"\n\
             \n[[data]]\nstart = 0x3000\nend = 0x3001\n\
             \n[[indirect]]\nsite = 0x1010\ntargets = [0x1100, 0x1200]\n",
        );
        let config = parse(&text).expect("valid");
        assert_eq!(
            config.functions,
            [FunctionHint {
                address: Address::new(0x1000),
                noreturn: true,
                location: Location {
                    line: 4,
                    column: 11
                },
            }]
        );
        assert_eq!(config.jump_tables.len(), 2);
        assert_eq!(config.jump_tables[0].kind, EntryKind::WordOffset);
        assert_eq!(config.jump_tables[0].site, Some(Address::new(0x1FF8)));
        assert_eq!(config.jump_tables[0].target, Some(TargetKind::Block));
        assert_eq!(config.jump_tables[0].location.line, 8);
        assert_eq!(config.jump_tables[1].kind, EntryKind::BraW);
        assert_eq!(config.jump_tables[1].target, None);
        assert_eq!(config.data_regions[0].end, Address::new(0x3001));
        assert_eq!(
            config.indirect[0].targets,
            [Address::new(0x1100), Address::new(0x1200)]
        );
        assert!(config.data_region(Address::new(0x3000)).is_some());
        assert!(config.data_region(Address::new(0x3001)).is_none());
    }

    #[test]
    fn test_parse_unknown_key_returns_parse_error_with_line() {
        let error = parse(&with_sha1("\n[[function]]\nadress = 0x1000\n")).expect_err("typo");
        match error {
            ConfigError::Parse { location, message } => {
                assert_eq!(location.map(|location| location.line), Some(4));
                assert!(message.contains("adress"), "{message}");
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_unknown_entry_kind_returns_parse_error() {
        let text = with_sha1("[[jump_table]]\naddress = 0x2000\ncount = 1\nkind = \"short\"\n");
        assert!(matches!(parse(&text), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn test_parse_negative_address_returns_parse_error() {
        let text = with_sha1("[[function]]\naddress = -2\n");
        assert!(matches!(parse(&text), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn test_parse_missing_sha1_returns_parse_error() {
        assert!(matches!(parse(""), Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn test_parse_bad_sha1_reports_its_line() {
        let found = problems("\nsha1 = \"abc\"\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProblemKind::BadSha1);
        assert_eq!(found[0].location, Location { line: 2, column: 8 });
    }

    #[test]
    fn test_parse_uppercase_sha1_is_lowercased() {
        let config = parse(&format!("sha1 = \"{}\"\n", SHA1.to_uppercase())).expect("valid");
        assert_eq!(config.sha1, SHA1);
    }

    #[test]
    fn test_parse_odd_and_out_of_range_addresses_reported() {
        let found = problems(&with_sha1(
            "[[function]]\naddress = 0x1001\n[[function]]\naddress = 0x1000000\n",
        ));
        let kinds: Vec<_> = found.iter().map(|problem| &problem.kind).collect();
        assert_eq!(
            kinds,
            [
                &ProblemKind::OddAddress {
                    address: Address::new(0x1001)
                },
                &ProblemKind::AddressOutOfRange { value: 0x100_0000 },
            ]
        );
        assert_eq!(found[0].location.line, 3);
        assert_eq!(found[1].location.line, 5);
    }

    #[test]
    fn test_parse_duplicate_function_names_first_line() {
        let found = problems(&with_sha1(
            "[[function]]\naddress = 0x1000\n[[function]]\naddress = 0x1000\n",
        ));
        assert_eq!(
            found,
            [Problem {
                location: Location {
                    line: 5,
                    column: 11
                },
                kind: ProblemKind::Duplicate {
                    address: Address::new(0x1000),
                    first: Location {
                        line: 3,
                        column: 11
                    },
                },
            }]
        );
    }

    #[test]
    fn test_parse_zero_count_table_reported() {
        let found = problems(&with_sha1(
            "[[jump_table]]\naddress = 0x2000\ncount = 0\nkind = \"long\"\n",
        ));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProblemKind::ZeroCount);
        assert_eq!(found[0].location.line, 4);
    }

    #[test]
    fn test_parse_empty_and_overlapping_regions_reported() {
        let found = problems(&with_sha1(
            "[[data]]\nstart = 0x3000\nend = 0x3000\n\
             [[data]]\nstart = 0x4000\nend = 0x4010\n\
             [[data]]\nstart = 0x400F\nend = 0x4020\n",
        ));
        let kinds: Vec<_> = found.iter().map(|problem| &problem.kind).collect();
        assert_eq!(
            kinds,
            [
                &ProblemKind::EmptyRegion {
                    start: Address::new(0x3000),
                    end: Address::new(0x3000),
                },
                &ProblemKind::OverlappingRegions {
                    other: Location { line: 6, column: 9 },
                },
            ]
        );
        assert_eq!(found[1].location.line, 9);
    }

    #[test]
    fn test_parse_code_address_inside_data_reported() {
        let found = problems(&with_sha1(
            "[[data]]\nstart = 0x3000\nend = 0x3100\n\
             [[function]]\naddress = 0x3010\n\
             [[indirect]]\nsite = 0x1000\ntargets = [0x30FE]\n",
        ));
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|problem| matches!(
            problem.kind,
            ProblemKind::InsideData { region, .. } if region.line == 3
        )));
    }

    #[test]
    fn test_parse_indirect_without_targets_reported() {
        let found = problems(&with_sha1("[[indirect]]\nsite = 0x1000\ntargets = []\n"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProblemKind::NoTargets);
    }

    #[test]
    fn test_config_error_display_lists_every_problem_with_lines() {
        let error = parse(&with_sha1(
            "[[function]]\naddress = 0x1001\n[[jump_table]]\naddress = 0x2000\ncount = 0\n\
             kind = \"long\"\n",
        ))
        .expect_err("two problems");
        assert_eq!(
            error.to_string(),
            "line 3, column 11: 0x001001 is odd\n\
             line 6, column 9: a jump table needs at least one entry"
        );
    }
}
