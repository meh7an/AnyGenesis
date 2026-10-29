//! The vendored ymfm's YM2612 behind a safe handle, for register-script renders: writes to
//! its four ports, its status register, and stereo samples one 144-clock period each.

use std::ffi::c_void;

unsafe extern "C" {
    fn ag_ymfm_new() -> *mut c_void;
    fn ag_ymfm_free(chip: *mut c_void);
    fn ag_ymfm_reset(chip: *mut c_void);
    fn ag_ymfm_write(chip: *mut c_void, offset: u32, data: u8);
    fn ag_ymfm_read_status(chip: *mut c_void) -> u8;
    fn ag_ymfm_generate(chip: *mut c_void, out: *mut i32, count: u32);
    fn ag_ymfm_generate_channels(chip: *mut c_void, levels: *mut i32);
    fn ag_ymfm_operator(chip: *mut c_void, index: u32, out: *mut u32);
}

/// One operator's state, for finding where two renders part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operator {
    /// The envelope's 10-bit attenuation.
    pub attenuation: u32,
    /// ymfm's envelope state: 1 attack, 2 decay, 3 sustain, 4 release.
    pub state: u32,
    /// The phase's top 10 bits.
    pub phase: u32,
}

/// One YM2612, reset.
pub struct Ym2612 {
    chip: *mut c_void,
}

impl Ym2612 {
    pub fn new() -> Self {
        // SAFETY: the shim allocates and resets a chip; it is freed once, in Drop.
        Self {
            chip: unsafe { ag_ymfm_new() },
        }
    }

    /// Resets the chip as ymfm does: its registers and status flags, not the host's timers.
    pub fn reset(&mut self) {
        // SAFETY: `chip` is live until Drop.
        unsafe { ag_ymfm_reset(self.chip) }
    }

    /// Writes `data` to port `offset` (0 address I, 1 data I, 2 address II, 3 data II).
    pub fn write(&mut self, offset: u32, data: u8) {
        // SAFETY: `chip` is live until Drop.
        unsafe { ag_ymfm_write(self.chip, offset, data) }
    }

    /// Writes register `register` of part `part` (0 or 1).
    pub fn set(&mut self, part: u32, register: u8, data: u8) {
        self.write(2 * part, register);
        self.write(2 * part + 1, data);
    }

    pub fn status(&mut self) -> u8 {
        // SAFETY: `chip` is live until Drop.
        unsafe { ag_ymfm_read_status(self.chip) }
    }

    /// Renders `out.len() / 2` stereo samples (left, right interleaved).
    pub fn generate(&mut self, out: &mut [i32]) {
        let count = u32::try_from(out.len() / 2).expect("fewer than 2^32 samples");
        // SAFETY: `out` holds 2 * count values; `chip` is live until Drop.
        unsafe { ag_ymfm_generate(self.chip, out.as_mut_ptr(), count) }
    }

    /// Renders one sample as each channel's 9-bit level, before ymfm's mix: `[left, right]`
    /// per channel; channel 6 is the DAC while it is on.
    pub fn generate_channels(&mut self) -> [[i32; 2]; 6] {
        let mut levels = [0; 12];
        // SAFETY: `levels` holds the 12 values the shim writes; `chip` is live until Drop.
        unsafe { ag_ymfm_generate_channels(self.chip, levels.as_mut_ptr()) };
        std::array::from_fn(|channel| [levels[2 * channel], levels[2 * channel + 1]])
    }

    /// Operator `index` (0-23, ymfm's order) as it is now.
    pub fn operator(&self, index: u32) -> Operator {
        assert!(index < 24, "the YM2612 has 24 operators");
        let mut out = [0; 3];
        // SAFETY: `out` holds the 3 values the shim writes; `index` is in range.
        unsafe { ag_ymfm_operator(self.chip, index, out.as_mut_ptr()) };
        Operator {
            attenuation: out[0],
            state: out[1],
            phase: out[2],
        }
    }
}

impl Default for Ym2612 {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Ym2612 {
    fn drop(&mut self) {
        // SAFETY: allocated by ag_ymfm_new, freed only here.
        unsafe { ag_ymfm_free(self.chip) }
    }
}
