use std::fmt;

/// A 68000 bus address.
///
/// The 68000 drives only 24 address lines, so the top byte of a 32-bit address never reaches
/// the bus and is dropped on construction. Arithmetic wraps within the 16 MiB space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(u32);

impl Address {
    /// The bits the 68000 puts on the bus.
    pub const MASK: u32 = 0x00FF_FFFF;

    /// Builds an address from a 32-bit value, dropping its top byte.
    pub const fn new(raw: u32) -> Self {
        Self(raw & Self::MASK)
    }

    /// Returns the 24-bit address value.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Adds an unsigned offset, wrapping within the 24-bit space.
    pub const fn wrapping_add(self, offset: u32) -> Self {
        Self::new(self.0.wrapping_add(offset))
    }

    /// Subtracts an unsigned offset, wrapping within the 24-bit space.
    pub const fn wrapping_sub(self, offset: u32) -> Self {
        Self::new(self.0.wrapping_sub(offset))
    }

    /// Adds a signed displacement, as branches and indexed addressing do.
    pub const fn offset(self, displacement: i32) -> Self {
        Self::new(self.0.wrapping_add_signed(displacement))
    }

    /// Returns whether word and long accesses at this address are aligned.
    pub const fn is_even(self) -> bool {
        self.0 & 1 == 0
    }
}

impl From<Address> for u32 {
    fn from(address: Address) -> Self {
        address.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:06X}", self.0)
    }
}

impl fmt::UpperHex for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_new_top_byte_set_dropped() {
        assert_eq!(Address::new(0xFFFF_8000).get(), 0x00FF_8000);
    }

    #[test]
    fn test_address_wrapping_add_past_top_wraps_to_zero() {
        assert_eq!(Address::new(0x00FF_FFFE).wrapping_add(4).get(), 0x0000_0002);
    }

    #[test]
    fn test_address_wrapping_sub_below_zero_wraps_to_top() {
        assert_eq!(Address::new(0x0000_0002).wrapping_sub(4).get(), 0x00FF_FFFE);
    }

    #[test]
    fn test_address_offset_negative_displacement_moves_back() {
        assert_eq!(Address::new(0x0000_0200).offset(-2).get(), 0x0000_01FE);
        assert_eq!(Address::new(0x0000_0000).offset(-2).get(), 0x00FF_FFFE);
    }

    #[test]
    fn test_address_is_even_odd_address_false() {
        assert!(Address::new(0x0000_0200).is_even());
        assert!(!Address::new(0x0000_0201).is_even());
    }

    #[test]
    fn test_address_display_pads_six_digits() {
        assert_eq!(Address::new(0x200).to_string(), "0x000200");
        assert_eq!(format!("{:06x}", Address::new(0xC0_0004)), "c00004");
    }
}
