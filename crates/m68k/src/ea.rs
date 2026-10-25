/// A data or address register, numbered 0 to 7.
///
/// Not `#[non_exhaustive]` (nor are the other model enums): the 68000 instruction set is
/// fixed, and the formatter, analysis and lifting must match every case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Register {
    Data(u8),
    Address(u8),
}

/// Width of the index register in an indexed mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexSize {
    /// The low word, sign-extended.
    Word,
    /// The whole register.
    Long,
}

/// The index of a brief extension word.
///
/// The 68000 has no scale factor and no full extension format: it ignores bits 8-10 of the
/// extension word, so they are not part of the model (the raw word stays available through
/// `Instruction::words`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndexRegister {
    pub register: Register,
    pub size: IndexSize,
}

/// One of the 12 effective-address modes of the 68000, with its extension-word data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ea {
    /// `Dn`.
    DataRegister(u8),
    /// `An`.
    AddressRegister(u8),
    /// `(An)`.
    Indirect(u8),
    /// `(An)+`.
    PostIncrement(u8),
    /// `-(An)`.
    PreDecrement(u8),
    /// `(d16,An)`.
    Displacement { register: u8, displacement: i16 },
    /// `(d8,An,Xn)`.
    Index {
        register: u8,
        displacement: i8,
        index: IndexRegister,
    },
    /// `(xxx).w`: the raw word, which the CPU sign-extends to an address.
    AbsoluteShort(u16),
    /// `(xxx).l`: the raw long word; only its low 24 bits reach the bus.
    AbsoluteLong(u32),
    /// `(d16,PC)`, relative to the address of the extension word.
    PcDisplacement(i16),
    /// `(d8,PC,Xn)`, relative to the address of the extension word.
    PcIndex {
        displacement: i8,
        index: IndexRegister,
    },
    /// `#<data>`: zero-extended from the operation size (a byte sits in the low byte of its
    /// word).
    Immediate(u32),
}

impl Ea {
    /// Returns the addressing mode, without its data.
    pub const fn mode(self) -> EaMode {
        match self {
            Self::DataRegister(_) => EaMode::DataRegister,
            Self::AddressRegister(_) => EaMode::AddressRegister,
            Self::Indirect(_) => EaMode::Indirect,
            Self::PostIncrement(_) => EaMode::PostIncrement,
            Self::PreDecrement(_) => EaMode::PreDecrement,
            Self::Displacement { .. } => EaMode::Displacement,
            Self::Index { .. } => EaMode::Index,
            Self::AbsoluteShort(_) => EaMode::AbsoluteShort,
            Self::AbsoluteLong(_) => EaMode::AbsoluteLong,
            Self::PcDisplacement(_) => EaMode::PcDisplacement,
            Self::PcIndex { .. } => EaMode::PcIndex,
            Self::Immediate(_) => EaMode::Immediate,
        }
    }
}

/// An effective-address mode without its data, as the 6-bit mode/register field selects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EaMode {
    DataRegister,
    AddressRegister,
    Indirect,
    PostIncrement,
    PreDecrement,
    Displacement,
    Index,
    AbsoluteShort,
    AbsoluteLong,
    PcDisplacement,
    PcIndex,
    Immediate,
}

impl EaMode {
    /// Decodes a mode field (bits 5-3 of a source EA) and register field (bits 2-0).
    ///
    /// Returns `None` for mode 7 with register 5 to 7, which the 68000 does not define.
    pub const fn from_fields(mode: u16, register: u16) -> Option<Self> {
        match (mode & 7, register & 7) {
            (0, _) => Some(Self::DataRegister),
            (1, _) => Some(Self::AddressRegister),
            (2, _) => Some(Self::Indirect),
            (3, _) => Some(Self::PostIncrement),
            (4, _) => Some(Self::PreDecrement),
            (5, _) => Some(Self::Displacement),
            (6, _) => Some(Self::Index),
            (7, 0) => Some(Self::AbsoluteShort),
            (7, 1) => Some(Self::AbsoluteLong),
            (7, 2) => Some(Self::PcDisplacement),
            (7, 3) => Some(Self::PcIndex),
            (7, 4) => Some(Self::Immediate),
            _ => None,
        }
    }

    /// Returns this mode's bit in an [`EaModes`] set.
    pub(crate) const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// A set of addressing modes, as the 68000 manual's categories define them per instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EaModes(u16);

impl EaModes {
    pub(crate) const ALL: Self = Self(0x0FFF);
    pub(crate) const DATA: Self = Self::ALL.without(EaMode::AddressRegister);
    pub(crate) const CONTROL: Self = Self(
        EaMode::Indirect.bit()
            | EaMode::Displacement.bit()
            | EaMode::Index.bit()
            | EaMode::AbsoluteShort.bit()
            | EaMode::AbsoluteLong.bit()
            | EaMode::PcDisplacement.bit()
            | EaMode::PcIndex.bit(),
    );
    pub(crate) const ALTERABLE: Self = Self::ALL
        .without(EaMode::PcDisplacement)
        .without(EaMode::PcIndex)
        .without(EaMode::Immediate);
    pub(crate) const DATA_ALTERABLE: Self = Self::ALTERABLE.without(EaMode::AddressRegister);
    pub(crate) const MEMORY_ALTERABLE: Self = Self::DATA_ALTERABLE.without(EaMode::DataRegister);
    pub(crate) const CONTROL_ALTERABLE: Self = Self::CONTROL
        .without(EaMode::PcDisplacement)
        .without(EaMode::PcIndex);

    pub(crate) const fn with(self, mode: EaMode) -> Self {
        Self(self.0 | mode.bit())
    }

    pub(crate) const fn without(self, mode: EaMode) -> Self {
        Self(self.0 & !mode.bit())
    }

    pub(crate) const fn contains(self, mode: EaMode) -> bool {
        self.0 & mode.bit() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ea_mode_from_fields_mode_seven_undefined_registers_are_none() {
        assert_eq!(EaMode::from_fields(7, 4), Some(EaMode::Immediate));
        assert_eq!(EaMode::from_fields(7, 5), None);
        assert_eq!(EaMode::from_fields(7, 7), None);
    }

    #[test]
    fn test_ea_modes_categories_match_manual() {
        assert!(!EaModes::DATA.contains(EaMode::AddressRegister));
        assert!(EaModes::CONTROL.contains(EaMode::PcIndex));
        assert!(!EaModes::CONTROL.contains(EaMode::PostIncrement));
        assert!(!EaModes::MEMORY_ALTERABLE.contains(EaMode::DataRegister));
        assert!(EaModes::MEMORY_ALTERABLE.contains(EaMode::PreDecrement));
        assert!(!EaModes::CONTROL_ALTERABLE.contains(EaMode::PcDisplacement));
        assert!(!EaModes::ALTERABLE.contains(EaMode::Immediate));
    }
}
