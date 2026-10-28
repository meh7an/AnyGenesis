//! Loader for SingleStepTests/z80 `.json` files.
//!
//! The files use a small subset of JSON (objects, arrays, ASCII strings without escapes,
//! integers and null), so a parser of that subset is all the suite needs.

use std::{collections::BTreeMap, fs, io, path::Path};

/// One test: a single instruction from `initial` to `expected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCase {
    pub name: String,
    pub initial: State,
    pub expected: State,
    /// One entry per T-state of the instruction.
    pub cycles: Vec<Cycle>,
    /// Port accesses in order: the values reads return and the values writes must send.
    pub ports: Vec<Port>,
}

/// CPU and memory state before or after a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub registers: Registers,
    /// Memory contents as `(address, byte)`.
    pub ram: Vec<(u16, u8)>,
}

/// The Z80 registers and the internal state the suite records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Registers {
    pub pc: u16,
    pub sp: u16,
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub af_: u16,
    pub bc_: u16,
    pub de_: u16,
    pub hl_: u16,
    pub ix: u16,
    pub iy: u16,
    pub i: u8,
    pub r: u8,
    /// The internal MEMPTR register.
    pub wz: u16,
    pub im: u8,
    pub iff1: bool,
    pub iff2: bool,
    /// Whether the instruction was EI, which holds off interrupts for one instruction.
    pub ei: bool,
    /// Whether the instruction was LD A,I or LD A,R.
    pub p: bool,
    /// The flags the instruction wrote, or 0 if it wrote none.
    pub q: u8,
}

/// The bus during one T-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cycle {
    /// `None` while the bus floats.
    pub address: Option<u16>,
    pub data: Option<u8>,
    pub read: bool,
    pub write: bool,
    pub memory_request: bool,
    pub io_request: bool,
}

/// One port access of an I/O instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Port {
    pub address: u16,
    pub value: u8,
    pub write: bool,
}

/// Why a suite file could not be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    #[error("reading {path}: {source}")]
    Io { path: String, source: io::Error },
    #[error("malformed JSON at offset {offset}: {reason}")]
    Syntax { offset: usize, reason: &'static str },
    #[error("bad test data: {0}")]
    Shape(String),
}

/// Reads and decodes one suite file.
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

/// Decodes the contents of one suite file: a list of tests.
///
/// # Errors
/// Returns an error if the bytes are not JSON of the suite's subset or a test lacks a field.
pub fn parse(bytes: &[u8]) -> Result<Vec<TestCase>, LoadError> {
    let mut parser = Parser { text: bytes, at: 0 };
    let value = parser.value()?;
    parser.space();
    if parser.at != bytes.len() {
        return Err(parser.syntax("trailing bytes"));
    }
    list(&value, "file")?.iter().map(test_case).collect()
}

/// A parsed JSON value of the suite's subset.
#[derive(Debug)]
enum Json {
    Null,
    Int(i64),
    Text(String),
    List(Vec<Json>),
    Map(BTreeMap<String, Json>),
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn syntax(&self, reason: &'static str) -> LoadError {
        LoadError::Syntax {
            offset: self.at,
            reason,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn space(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8, reason: &'static str) -> Result<(), LoadError> {
        self.space();
        if self.peek() != Some(byte) {
            return Err(self.syntax(reason));
        }
        self.at += 1;
        Ok(())
    }

    fn value(&mut self) -> Result<Json, LoadError> {
        self.space();
        match self.peek() {
            Some(b'{') => self.map(),
            Some(b'[') => self.list(),
            Some(b'"') => self.string().map(Json::Text),
            Some(b'n') if self.text[self.at..].starts_with(b"null") => {
                self.at += 4;
                Ok(Json::Null)
            }
            Some(b'-' | b'0'..=b'9') => self.int(),
            _ => Err(self.syntax("unexpected byte")),
        }
    }

    fn int(&mut self) -> Result<Json, LoadError> {
        let negative = self.peek() == Some(b'-');
        self.at += usize::from(negative);
        let start = self.at;
        let mut value: i64 = 0;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(i64::from(digit - b'0')))
                .ok_or_else(|| self.syntax("integer overflows"))?;
            self.at += 1;
        }
        if self.at == start || matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err(self.syntax("not an integer"));
        }
        Ok(Json::Int(if negative { -value } else { value }))
    }

    fn string(&mut self) -> Result<String, LoadError> {
        self.expect(b'"', "expected a string")?;
        let start = self.at;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    let text = std::str::from_utf8(&self.text[start..self.at])
                        .map_err(|_| self.syntax("string is not UTF-8"))?;
                    self.at += 1;
                    return Ok(text.to_owned());
                }
                b'\\' => return Err(self.syntax("string escapes are not supported")),
                _ => self.at += 1,
            }
        }
        Err(self.syntax("unterminated string"))
    }

    /// Parses the comma-separated items between `open` and `close`.
    fn items(
        &mut self,
        open: u8,
        close: u8,
        mut item: impl FnMut(&mut Self) -> Result<(), LoadError>,
    ) -> Result<(), LoadError> {
        self.expect(open, "expected an opening bracket")?;
        self.space();
        if self.peek() == Some(close) {
            self.at += 1;
            return Ok(());
        }
        loop {
            item(self)?;
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(byte) if byte == close => {
                    self.at += 1;
                    return Ok(());
                }
                _ => return Err(self.syntax("expected a comma or a closing bracket")),
            }
        }
    }

    fn list(&mut self) -> Result<Json, LoadError> {
        let mut items = Vec::new();
        self.items(b'[', b']', |parser| {
            items.push(parser.value()?);
            Ok(())
        })?;
        Ok(Json::List(items))
    }

    fn map(&mut self) -> Result<Json, LoadError> {
        let mut entries = BTreeMap::new();
        self.items(b'{', b'}', |parser| {
            parser.space();
            let key = parser.string()?;
            parser.expect(b':', "expected a colon")?;
            entries.insert(key, parser.value()?);
            Ok(())
        })?;
        Ok(Json::Map(entries))
    }
}

fn shape<T>(message: impl Into<String>) -> Result<T, LoadError> {
    Err(LoadError::Shape(message.into()))
}

fn map<'a>(value: &'a Json, what: &str) -> Result<&'a BTreeMap<String, Json>, LoadError> {
    match value {
        Json::Map(map) => Ok(map),
        _ => shape(format!("{what} is not an object")),
    }
}

fn list<'a>(value: &'a Json, what: &str) -> Result<&'a [Json], LoadError> {
    match value {
        Json::List(items) => Ok(items),
        _ => shape(format!("{what} is not a list")),
    }
}

fn field<'a>(map: &'a BTreeMap<String, Json>, key: &str) -> Result<&'a Json, LoadError> {
    map.get(key)
        .map_or_else(|| shape(format!("missing field {key}")), Ok)
}

fn int<T: TryFrom<i64>>(value: &Json, what: &str) -> Result<T, LoadError> {
    match value {
        Json::Int(value) => {
            T::try_from(*value).map_or_else(|_| shape(format!("{what} is out of range")), Ok)
        }
        _ => shape(format!("{what} is not an integer")),
    }
}

fn optional<T: TryFrom<i64>>(value: &Json, what: &str) -> Result<Option<T>, LoadError> {
    match value {
        Json::Null => Ok(None),
        value => int(value, what).map(Some),
    }
}

fn number<T: TryFrom<i64>>(map: &BTreeMap<String, Json>, key: &str) -> Result<T, LoadError> {
    int(field(map, key)?, key)
}

fn flag(map: &BTreeMap<String, Json>, key: &str) -> Result<bool, LoadError> {
    match number::<u8>(map, key)? {
        0 => Ok(false),
        1 => Ok(true),
        value => shape(format!("{key} is {value}, not 0 or 1")),
    }
}

fn state(value: &Json) -> Result<State, LoadError> {
    let map = map(value, "state")?;
    let registers = Registers {
        pc: number(map, "pc")?,
        sp: number(map, "sp")?,
        a: number(map, "a")?,
        f: number(map, "f")?,
        b: number(map, "b")?,
        c: number(map, "c")?,
        d: number(map, "d")?,
        e: number(map, "e")?,
        h: number(map, "h")?,
        l: number(map, "l")?,
        af_: number(map, "af_")?,
        bc_: number(map, "bc_")?,
        de_: number(map, "de_")?,
        hl_: number(map, "hl_")?,
        ix: number(map, "ix")?,
        iy: number(map, "iy")?,
        i: number(map, "i")?,
        r: number(map, "r")?,
        wz: number(map, "wz")?,
        im: number(map, "im")?,
        iff1: flag(map, "iff1")?,
        iff2: flag(map, "iff2")?,
        ei: flag(map, "ei")?,
        p: flag(map, "p")?,
        q: number(map, "q")?,
    };
    let ram = list(field(map, "ram")?, "ram")?
        .iter()
        .map(|pair| match list(pair, "ram entry")? {
            [address, value] => Ok((int(address, "ram address")?, int(value, "ram value")?)),
            _ => shape("ram entry is not [address, value]"),
        })
        .collect::<Result<_, _>>()?;
    Ok(State { registers, ram })
}

fn cycle(value: &Json) -> Result<Cycle, LoadError> {
    let [address, data, Json::Text(pins)] = list(value, "cycle")? else {
        return shape("cycle is not [address, data, pins]");
    };
    let &[read, write, memory, io] = pins.as_bytes() else {
        return shape(format!("pins {pins:?} are not four characters"));
    };
    Ok(Cycle {
        address: optional(address, "cycle address")?,
        data: optional(data, "cycle data")?,
        read: read == b'r',
        write: write == b'w',
        memory_request: memory == b'm',
        io_request: io == b'i',
    })
}

fn port(value: &Json) -> Result<Port, LoadError> {
    let [address, value, Json::Text(direction)] = list(value, "port")? else {
        return shape("port is not [address, value, direction]");
    };
    let write = match direction.as_str() {
        "r" => false,
        "w" => true,
        other => return shape(format!("port direction {other:?} is not r or w")),
    };
    Ok(Port {
        address: int(address, "port address")?,
        value: int(value, "port value")?,
        write,
    })
}

fn test_case(value: &Json) -> Result<TestCase, LoadError> {
    let map = map(value, "test")?;
    let Json::Text(name) = field(map, "name")? else {
        return shape("name is not a string");
    };
    let ports = match map.get("ports") {
        None => Vec::new(),
        Some(ports) => list(ports, "ports")?
            .iter()
            .map(port)
            .collect::<Result<_, _>>()?,
    };
    Ok(TestCase {
        name: name.clone(),
        initial: state(field(map, "initial")?)?,
        expected: state(field(map, "final")?)?,
        cycles: list(field(map, "cycles")?, "cycles")?
            .iter()
            .map(cycle)
            .collect::<Result<_, _>>()?,
        ports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = r#"{"pc":1,"sp":2,"a":3,"b":4,"c":5,"d":6,"e":7,"f":8,"h":9,"l":10,
        "i":11,"r":12,"ei":1,"wz":13,"ix":14,"iy":15,"af_":16,"bc_":17,"de_":18,"hl_":19,
        "im":2,"p":0,"q":8,"iff1":1,"iff2":0,"ram":[[1,219],[2,254]]}"#;

    fn file(ports: &str) -> String {
        format!(
            r#"[{{"name":"DB 0000","initial":{STATE},"final":{STATE},
            "cycles":[[1,null,"----"],[1,null,"r-m-"],[65024,219,"----"]]{ports}}}]"#
        )
    }

    #[test]
    fn test_parse_well_formed_file_decodes_every_field() {
        let cases = parse(file(r#","ports":[[65027,170,"r"]]"#).as_bytes()).expect("parses");

        assert_eq!(cases.len(), 1);
        let case = &cases[0];
        assert_eq!(case.name, "DB 0000");
        let registers = case.initial.registers;
        assert_eq!(
            [
                registers.pc,
                registers.sp,
                registers.wz,
                registers.ix,
                registers.iy
            ],
            [1, 2, 13, 14, 15]
        );
        assert_eq!(
            [
                registers.a,
                registers.f,
                registers.b,
                registers.l,
                registers.i,
                registers.r
            ],
            [3, 8, 4, 10, 11, 12]
        );
        assert_eq!(
            [registers.af_, registers.bc_, registers.de_, registers.hl_],
            [16, 17, 18, 19]
        );
        assert_eq!((registers.im, registers.q), (2, 8));
        assert_eq!(
            [registers.iff1, registers.iff2, registers.ei, registers.p],
            [true, false, true, false]
        );
        assert_eq!(case.initial.ram, [(1, 219), (2, 254)]);
        assert_eq!(case.cycles.len(), 3);
        assert_eq!(
            case.cycles[1],
            Cycle {
                address: Some(1),
                data: None,
                read: true,
                write: false,
                memory_request: true,
                io_request: false,
            }
        );
        assert_eq!(case.cycles[2].data, Some(219));
        assert_eq!(
            case.ports,
            [Port {
                address: 65027,
                value: 170,
                write: false,
            }]
        );
    }

    #[test]
    fn test_parse_file_without_ports_has_none() {
        let cases = parse(file("").as_bytes()).expect("parses");
        assert!(cases[0].ports.is_empty());
    }

    #[test]
    fn test_parse_missing_field_rejected() {
        let text = file("").replace(r#""wz":13,"#, "");
        assert!(matches!(parse(text.as_bytes()), Err(LoadError::Shape(_))));
    }

    #[test]
    fn test_parse_trailing_bytes_rejected() {
        let text = file("") + "x";
        assert!(matches!(
            parse(text.as_bytes()),
            Err(LoadError::Syntax { .. })
        ));
    }

    #[test]
    fn test_parse_fraction_rejected() {
        let text = file("").replace(r#""im":2"#, r#""im":2.5"#);
        assert!(matches!(
            parse(text.as_bytes()),
            Err(LoadError::Syntax { .. })
        ));
    }
}
