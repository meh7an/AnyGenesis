//! Loader for SingleStepTests/m68000 `.json.bin` files.
//!
//! The layout follows upstream's `decode.py`: little-endian, every block opens with a `u32`
//! size (unused here) and a `u32` magic number.

use std::{fs, io, path::Path};

use crate::Registers;

const FILE_MAGIC: u32 = 0x1A3F_5D71;
const CASE_MAGIC: u32 = 0xABC1_2367;
const NAME_MAGIC: u32 = 0x89AB_CDEF;
const STATE_MAGIC: u32 = 0x0123_4567;
const TRANSACTIONS_MAGIC: u32 = 0x4567_89AB;

/// One test: a single instruction from `initial` to `expected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    pub name: String,
    pub initial: State,
    pub expected: State,
    /// Total cycles the instruction took on MAME's microcoded core.
    pub cycles: u32,
    pub transactions: Vec<Transaction>,
}

/// CPU and memory state before or after a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// `pc` is MAME's `m_au`, the address of the next prefetch, not of the opcode.
    pub registers: Registers,
    /// The two prefetch queue words (IR and IRC).
    pub prefetch: [u32; 2],
    /// Memory contents as `(address, byte)`, split from the file's word entries.
    pub ram: Vec<(u32, u8)>,
}

/// Bus cycle kinds as the file encodes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransactionKind {
    Read,
    Write,
    /// The indivisible read-modify-write cycle of TAS.
    ReadModifyWrite,
    ReadAddressError,
    WriteAddressError,
}

/// One entry of a test's bus transaction log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Transaction {
    Idle {
        cycles: u32,
    },
    Access {
        kind: TransactionKind,
        cycles: u32,
        function_code: u32,
        address: u32,
        data: u32,
        upper_strobe: bool,
        lower_strobe: bool,
    },
}

/// Why a `.json.bin` file could not be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    #[error("reading {path}: {source}")]
    Io { path: String, source: io::Error },
    #[error("data ends early at offset {offset}")]
    Truncated { offset: usize },
    #[error("bad {block} magic {found:#010x} at offset {offset}")]
    BadMagic {
        block: &'static str,
        found: u32,
        offset: usize,
    },
    #[error("test name at offset {offset} is not UTF-8")]
    BadName { offset: usize },
    #[error("unknown transaction kind {kind} at offset {offset}")]
    UnknownTransaction { kind: u8, offset: usize },
    #[error("{count} bytes left over after the last test")]
    TrailingBytes { count: usize },
}

/// Reads and decodes one `.json.bin` file.
///
/// # Errors
/// Returns [`LoadError::Io`] if the file cannot be read, or another variant if its contents
/// do not follow the format.
pub fn load_file(path: &Path) -> Result<Vec<TestCase>, LoadError> {
    let bytes = fs::read(path).map_err(|source| LoadError::Io {
        path: path.display().to_string(),
        source,
    })?;
    parse(&bytes)
}

/// Decodes the contents of one `.json.bin` file.
///
/// # Errors
/// Returns an error if the bytes do not follow the format.
pub fn parse(bytes: &[u8]) -> Result<Vec<TestCase>, LoadError> {
    let mut reader = Reader { bytes, offset: 0 };
    reader.magic("file", FILE_MAGIC)?;
    let count = reader.u32()?;
    let cases = (0..count)
        .map(|_| reader.test_case())
        .collect::<Result<Vec<_>, _>>()?;
    match bytes.len() - reader.offset {
        0 => Ok(cases),
        count => Err(LoadError::TrailingBytes { count }),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], LoadError> {
        let slice = self
            .offset
            .checked_add(len)
            .and_then(|end| self.bytes.get(self.offset..end))
            .ok_or(LoadError::Truncated {
                offset: self.offset,
            })?;
        self.offset += len;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], LoadError> {
        let mut array = [0; N];
        array.copy_from_slice(self.take(N)?);
        Ok(array)
    }

    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, LoadError> {
        self.array().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        self.array().map(u32::from_le_bytes)
    }

    /// Reads a block header: the unused size, then the magic number.
    fn block(&mut self, block: &'static str, expected: u32) -> Result<(), LoadError> {
        self.u32()?;
        self.magic(block, expected)
    }

    fn magic(&mut self, block: &'static str, expected: u32) -> Result<(), LoadError> {
        let offset = self.offset;
        match self.u32()? {
            found if found == expected => Ok(()),
            found => Err(LoadError::BadMagic {
                block,
                found,
                offset,
            }),
        }
    }

    fn test_case(&mut self) -> Result<TestCase, LoadError> {
        self.block("test", CASE_MAGIC)?;
        let name = self.name()?;
        let initial = self.state()?;
        let expected = self.state()?;
        self.block("transactions", TRANSACTIONS_MAGIC)?;
        let cycles = self.u32()?;
        let count = self.u32()?;
        let transactions = (0..count)
            .map(|_| self.transaction())
            .collect::<Result<_, _>>()?;
        Ok(TestCase {
            name,
            initial,
            expected,
            cycles,
            transactions,
        })
    }

    fn name(&mut self) -> Result<String, LoadError> {
        self.block("name", NAME_MAGIC)?;
        let len = self.u32()? as usize;
        let offset = self.offset;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| LoadError::BadName { offset })
    }

    fn state(&mut self) -> Result<State, LoadError> {
        self.block("state", STATE_MAGIC)?;
        let mut words = [0; 19];
        for word in &mut words {
            *word = self.u32()?;
        }
        let [d @ .., a0, a1, a2, a3, a4, a5, a6, usp, ssp, sr, pc] = words;
        let registers = Registers {
            d,
            a: [a0, a1, a2, a3, a4, a5, a6],
            usp,
            ssp,
            sr,
            pc,
        };
        let prefetch = [self.u32()?, self.u32()?];
        let count = self.u32()?;
        let mut ram = Vec::with_capacity(count as usize * 2);
        for _ in 0..count {
            let address = self.u32()?;
            let [high, low] = self.u16()?.to_be_bytes();
            ram.push((address, high));
            ram.push((address | 1, low));
        }
        Ok(State {
            registers,
            prefetch,
            ram,
        })
    }

    fn transaction(&mut self) -> Result<Transaction, LoadError> {
        let offset = self.offset;
        let code = self.u8()?;
        let cycles = self.u32()?;
        let kind = match code {
            0 => return Ok(Transaction::Idle { cycles }),
            1 => TransactionKind::Write,
            2 => TransactionKind::Read,
            3 => TransactionKind::ReadModifyWrite,
            4 => TransactionKind::ReadAddressError,
            5 => TransactionKind::WriteAddressError,
            kind => return Err(LoadError::UnknownTransaction { kind, offset }),
        };
        Ok(Transaction::Access {
            kind,
            cycles,
            function_code: self.u32()?,
            address: self.u32()?,
            data: self.u32()?,
            upper_strobe: self.u32()? != 0,
            lower_strobe: self.u32()? != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds files in the upstream layout, so the tests do not need the real suite.
    #[derive(Default)]
    struct Writer(Vec<u8>);

    impl Writer {
        fn u32(&mut self, value: u32) -> &mut Self {
            self.0.extend_from_slice(&value.to_le_bytes());
            self
        }

        fn block(&mut self, magic: u32) -> &mut Self {
            self.u32(0).u32(magic)
        }

        fn state(&mut self, base: u32, ram: &[(u32, u16)]) -> &mut Self {
            self.block(STATE_MAGIC);
            for register in 0..19 {
                self.u32(base + register);
            }
            self.u32(0x4E71).u32(0x4E71).u32(ram.len() as u32);
            for &(address, word) in ram {
                self.u32(address);
                self.0.extend_from_slice(&word.to_le_bytes());
            }
            self
        }

        fn test_case(&mut self, name: &str) -> &mut Self {
            self.block(CASE_MAGIC)
                .block(NAME_MAGIC)
                .u32(name.len() as u32);
            self.0.extend_from_slice(name.as_bytes());
            self.state(0, &[(0x1000, 0x4E71)])
                .state(100, &[(0x1000, 0xABCD)])
                .block(TRANSACTIONS_MAGIC)
                .u32(4)
                .u32(2);
            self.0.push(0);
            self.u32(2);
            self.0.push(2);
            self.u32(2).u32(6).u32(0x1000).u32(0x4E71).u32(1).u32(1)
        }

        fn file(cases: &[&str]) -> Vec<u8> {
            let mut writer = Self::default();
            writer.u32(FILE_MAGIC).u32(cases.len() as u32);
            for name in cases {
                writer.test_case(name);
            }
            writer.0
        }
    }

    #[test]
    fn test_parse_well_formed_file_decodes_every_field() {
        let cases = parse(&Writer::file(&["4e71 [NOP] 1", "4e71 [NOP] 2"])).expect("parses");

        assert_eq!(cases.len(), 2);
        let case = &cases[1];
        assert_eq!(case.name, "4e71 [NOP] 2");
        assert_eq!(case.initial.registers.d, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(case.initial.registers.a, [8, 9, 10, 11, 12, 13, 14]);
        assert_eq!(case.expected.registers.usp, 115);
        assert_eq!(case.expected.registers.ssp, 116);
        assert_eq!(case.expected.registers.sr, 117);
        assert_eq!(case.expected.registers.pc, 118);
        assert_eq!(case.initial.prefetch, [0x4E71, 0x4E71]);
        assert_eq!(case.initial.ram, [(0x1000, 0x4E), (0x1001, 0x71)]);
        assert_eq!(case.expected.ram, [(0x1000, 0xAB), (0x1001, 0xCD)]);
        assert_eq!(case.cycles, 4);
        assert_eq!(
            case.transactions,
            [
                Transaction::Idle { cycles: 2 },
                Transaction::Access {
                    kind: TransactionKind::Read,
                    cycles: 2,
                    function_code: 6,
                    address: 0x1000,
                    data: 0x4E71,
                    upper_strobe: true,
                    lower_strobe: true,
                },
            ]
        );
    }

    #[test]
    fn test_parse_wrong_file_magic_rejected() {
        let mut bytes = Writer::file(&["x"]);
        bytes[0] ^= 1;
        assert!(matches!(
            parse(&bytes),
            Err(LoadError::BadMagic { block: "file", .. })
        ));
    }

    #[test]
    fn test_parse_truncated_file_rejected() {
        let bytes = Writer::file(&["x"]);
        assert!(matches!(
            parse(&bytes[..bytes.len() - 1]),
            Err(LoadError::Truncated { .. })
        ));
    }

    #[test]
    fn test_parse_trailing_bytes_rejected() {
        let mut bytes = Writer::file(&["x"]);
        bytes.push(0);
        assert!(matches!(
            parse(&bytes),
            Err(LoadError::TrailingBytes { count: 1 })
        ));
    }
}
