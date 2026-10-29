//! One operator: a phase counter running through a sine wave, and an envelope generator
//! that attenuates it.
//!
//! Sources (side-session note v0.2-m3.5-research-fm.md has the quotes; Nemesis on
//! SpritesMind, t=386): the 20-bit phase counter, reset by a key on, its top 10 bits
//! indexing a quarter log-sine table whose result and the envelope add up as attenuation
//! before an exponential table turns them into a 14-bit signed level (p=6114, p=6177,
//! p=7967); the envelope's 10-bit attenuation, its four phases, the attack curve `att +=
//! (~att x inc) >> 4`, rates 62 and 63 starting the attack at 0 and stalling it when set
//! later, no clamp at the sustain level, and within a sample SSG-EG's step before the
//! envelope's (p=5716, p=7967); SSG-EG's shapes, its 4x step below $200 and its inversion
//! `$200 - att` (p=7967); an attack starting from the attenuation it finds (Snake, p=7999).
//! Settled by ymfm, read as a reference only [O]: the amplitude modulation and total level
//! added after SSG-EG's inversion and held to $3FF, and SSG-EG's steps where the
//! documents leave them open (an attack rate below $1F, the key going off mid-cycle).

use crate::tables::{attenuation_to_level, envelope_increment, sine};

/// Highest attenuation: the operator is silent.
pub(crate) const MAX_ATTENUATION: u16 = 0x3FF;
/// Attenuation from which an operator's level is 0 whatever its phase: 13 whole steps.
const SILENT: u16 = 13 << 6;
/// The middle of the attenuation range, where SSG-EG turns.
const SSG_TURN: u16 = 0x200;

/// The phase an envelope is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Envelope {
    Attack,
    Decay,
    Sustain,
    Release,
}

/// What an operator's registers say this sample, worked out by its channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Params {
    /// What the phase counter moves by per sample.
    pub phase_step: u32,
    /// The rate of each envelope phase, in [`Envelope`] order, 0-63.
    pub rates: [u8; 4],
    /// The attenuation at which the decay turns to sustain.
    pub sustain: u16,
    /// The total level, as attenuation.
    pub total_level: u16,
    /// The LFO's amplitude modulation applies.
    pub amplitude_modulation: bool,
    /// SSG-EG's mode bits while it is on.
    pub ssg: Option<u8>,
}

/// An operator's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Operator {
    /// 10.10 fixed point: the top 10 bits are the position in the sine wave.
    phase: u32,
    attenuation: u16,
    envelope: Envelope,
    /// The key as the operator last took it.
    key: bool,
    /// SSG-EG has turned the envelope upside down.
    inverted: bool,
}

impl Default for Operator {
    fn default() -> Self {
        Self {
            phase: 0,
            attenuation: MAX_ATTENUATION,
            envelope: Envelope::Release,
            key: false,
            inverted: false,
        }
    }
}

impl Operator {
    /// The attenuation, the envelope's phase and the phase's top 10 bits.
    pub fn state(&self) -> (u16, Envelope, u32) {
        (self.attenuation, self.envelope, self.phase >> 10)
    }

    /// Takes the key's state at the start of a sample: a key going on starts the attack, one
    /// going off the release.
    pub fn take_key(&mut self, key: bool, params: &Params) {
        if key == self.key {
            return;
        }
        self.key = key;
        if key {
            self.start_attack(false, params);
        } else {
            self.start_release();
        }
    }

    /// Moves the operator on one sample; the envelope moves only on `envelope_cycle`s.
    pub fn clock(&mut self, envelope_cycle: Option<u32>, params: &Params) {
        match params.ssg {
            Some(mode) => self.clock_ssg(mode, params),
            None => self.inverted = false,
        }
        if let Some(cycle) = envelope_cycle {
            self.clock_envelope(cycle, params);
        }
        self.phase = self.phase.wrapping_add(params.phase_step);
    }

    /// The 14-bit signed level at the current phase moved by `modulation`, with
    /// `amplitude_modulation` added to the attenuation when the operator takes it.
    pub fn level(&self, modulation: i32, amplitude_modulation: u16, params: &Params) -> i32 {
        let position = (self.phase >> 10).wrapping_add_signed(modulation);
        let (wave, negative) = sine(position);
        let level =
            attenuation_to_level(wave + (self.attenuation(amplitude_modulation, params) << 2));
        if negative { -level } else { level }
    }

    /// Whether the operator's level is 0 at every phase: the attenuation alone shifts the
    /// 13-bit level out.
    pub fn silent(&self, amplitude_modulation: u16, params: &Params) -> bool {
        self.attenuation(amplitude_modulation, params) >= SILENT
    }

    /// The envelope's attenuation after SSG-EG's inversion, the LFO and the total level.
    fn attenuation(&self, amplitude_modulation: u16, params: &Params) -> u16 {
        let mut attenuation = self.attenuation;
        if self.inverted {
            attenuation = SSG_TURN.wrapping_sub(attenuation) & MAX_ATTENUATION;
        }
        if params.amplitude_modulation {
            attenuation += amplitude_modulation;
        }
        (attenuation + params.total_level).min(MAX_ATTENUATION)
    }

    /// Starts the attack, from a key on or, `restart`, from an SSG-EG cycle.
    fn start_attack(&mut self, restart: bool, params: &Params) {
        if self.envelope == Envelope::Attack {
            return;
        }
        self.envelope = Envelope::Attack;
        if !restart {
            self.inverted = params.ssg.is_some_and(|mode| mode & SSG_INVERT != 0);
            self.phase = 0;
        }
        if params.rates[Envelope::Attack as usize] >= INSTANT_ATTACK {
            self.attenuation = 0;
        }
    }

    fn start_release(&mut self) {
        if self.envelope == Envelope::Release {
            return;
        }
        self.envelope = Envelope::Release;
        if self.inverted {
            self.attenuation = SSG_TURN.wrapping_sub(self.attenuation) & MAX_ATTENUATION;
            self.inverted = false;
        }
    }

    /// SSG-EG's work once the attenuation has passed the middle: hold, or repeat the attack,
    /// turning the envelope over in the alternating shapes.
    fn clock_ssg(&mut self, mode: u8, params: &Params) {
        if self.attenuation & SSG_TURN == 0 {
            return;
        }
        let invert = mode & SSG_INVERT != 0;
        let alternate = mode & SSG_ALTERNATE != 0;
        if mode & SSG_HOLD != 0 {
            self.inverted = invert != alternate;
            if self.envelope != Envelope::Attack {
                self.attenuation = if self.inverted {
                    SSG_TURN
                } else {
                    MAX_ATTENUATION
                };
            }
        } else {
            self.inverted ^= alternate;
            if matches!(self.envelope, Envelope::Decay | Envelope::Sustain) {
                self.start_attack(true, params);
            }
            if !alternate {
                self.phase = 0;
            }
        }
        if self.envelope == Envelope::Release {
            self.attenuation = MAX_ATTENUATION;
        }
    }

    /// One envelope cycle, number `cycle`: each rate moves the attenuation on the cycles
    /// its place in the counter picks, by the step the increment table gives.
    fn clock_envelope(&mut self, cycle: u32, params: &Params) {
        if self.envelope == Envelope::Attack && self.attenuation == 0 {
            self.envelope = Envelope::Decay;
        }
        if self.envelope == Envelope::Decay && self.attenuation >= params.sustain {
            self.envelope = Envelope::Sustain;
        }
        let rate = params.rates[self.envelope as usize];
        let Some(increment) = envelope_increment(rate, cycle) else {
            return;
        };
        if self.envelope == Envelope::Attack {
            if rate < INSTANT_ATTACK {
                let attenuation = i32::from(self.attenuation);
                self.attenuation = (attenuation + ((!attenuation * increment) >> 4)) as u16;
            }
            return;
        }
        match params.ssg {
            None => self.attenuation += increment as u16,
            Some(_) if self.attenuation < SSG_TURN => self.attenuation += 4 * increment as u16,
            Some(_) => {}
        }
        self.attenuation = self.attenuation.min(MAX_ATTENUATION);
    }
}

/// Attack rates from here start the attack at full volume.
const INSTANT_ATTACK: u8 = 62;
/// SSG-EG mode bits: hold at the end, alternate the direction, start inverted.
const SSG_HOLD: u8 = 0x01;
const SSG_ALTERNATE: u8 = 0x02;
const SSG_INVERT: u8 = 0x04;
