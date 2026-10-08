//! Byte-level parser for the `compose` instruction. Pure: no account access.
//! Every read is bounds-checked; the structural limits are enforced here so
//! the processor can walk the same bytes again without re-validating.
use crate::error::ZapError as E;
use pinocchio::error::ProgramError;

pub const TAG_COMPOSE: u8 = 0;
pub const MAX_WATCH: usize = 8;
pub const MAX_STEPS: usize = 8;
pub const MAX_STEP_ACCOUNTS: usize = 48;
pub const MAX_PATCHES: usize = 8;
pub const MAX_STEP_DATA: usize = 1024;
pub const MAX_CHECKS: usize = 16;

/// Fixed encoded sizes.
pub const PATCH_LEN: usize = 2 + 1 + 1 + 1 + 4 + 4 + 8;
pub const CHECK_LEN: usize = 1 + 1 + 8;
pub const FEE_LEN: usize = 6;

pub const MODE_DELTA: u8 = 0;
pub const MODE_SCALE: u8 = 1;
pub const MODE_SCALE_SUB: u8 = 2;
pub const MODE_MIN: u8 = 3;
pub const CHECK_MIN: u8 = 0;
pub const CHECK_MAX: u8 = 1;

pub struct Cursor<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    #[inline(always)]
    pub fn new(d: &'a [u8]) -> Self {
        Self { d, pos: 0 }
    }
    #[inline(always)]
    pub fn at(d: &'a [u8], pos: usize) -> Self {
        Self { d, pos }
    }
    #[inline(always)]
    pub fn pos(&self) -> usize {
        self.pos
    }
    #[inline(always)]
    pub fn done(&self) -> bool {
        self.pos == self.d.len()
    }
    #[inline(always)]
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], ProgramError> {
        let end = self.pos.checked_add(n).ok_or(E::BadLayout)?;
        if end > self.d.len() {
            return Err(E::BadLayout.into());
        }
        let s = &self.d[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    #[inline(always)]
    pub fn u8(&mut self) -> Result<u8, ProgramError> {
        Ok(self.take(1)?[0])
    }
    #[inline(always)]
    pub fn u16(&mut self) -> Result<u16, ProgramError> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
}

#[inline(always)]
pub fn le16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
#[inline(always)]
pub fn le32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
#[inline(always)]
pub fn le64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        d[o],
        d[o + 1],
        d[o + 2],
        d[o + 3],
        d[o + 4],
        d[o + 5],
        d[o + 6],
        d[o + 7],
    ])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch {
    pub offset: u16,
    pub watch: u8,
    pub from_step: u8,
    pub mode: u8,
    pub num: u32,
    pub den: u32,
    pub sub: u64,
}

impl Patch {
    /// `b.len() == PATCH_LEN` is guaranteed by the caller (`chunks_exact`).
    #[inline(always)]
    pub fn read(b: &[u8]) -> Self {
        Self {
            offset: le16(b, 0),
            watch: b[2],
            from_step: b[3],
            mode: b[4],
            num: le32(b, 5),
            den: le32(b, 9),
            sub: le64(b, 13),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fee {
    pub watch: u8,
    pub fee_ata: u8,
    pub mint: u8,
    pub token_program: u8,
    pub ata_program: u8,
    pub system_program: u8,
}

impl Fee {
    #[inline(always)]
    pub fn read(b: &[u8]) -> Self {
        Self {
            watch: b[0],
            fee_ata: b[1],
            mint: b[2],
            token_program: b[3],
            ata_program: b[4],
            system_program: b[5],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Check {
    pub watch: u8,
    pub kind: u8,
    pub bound: i64,
}

impl Check {
    #[inline(always)]
    pub fn read(b: &[u8]) -> Self {
        Self {
            watch: b[0],
            kind: b[1],
            bound: le64(b, 2) as i64,
        }
    }
}

/// One step as laid out in the instruction data (all slices borrow the data).
pub struct Step<'a> {
    pub program: u8,
    pub accounts: &'a [u8],
    pub data: &'a [u8],
    /// `patch_count * PATCH_LEN` bytes.
    pub patches: &'a [u8],
    pub fee: Option<Fee>,
}

impl<'a> Step<'a> {
    /// Parse one step at the cursor, enforcing the per-step limits.
    pub fn parse(c: &mut Cursor<'a>) -> Result<Self, ProgramError> {
        let program = c.u8()?;
        let ac = c.u8()? as usize;
        if ac > MAX_STEP_ACCOUNTS {
            return Err(E::LimitExceeded.into());
        }
        let accounts = c.take(ac)?;
        let dl = c.u16()? as usize;
        if dl > MAX_STEP_DATA {
            return Err(E::LimitExceeded.into());
        }
        let data = c.take(dl)?;
        let pc = c.u8()? as usize;
        if pc > MAX_PATCHES {
            return Err(E::LimitExceeded.into());
        }
        let patches = c.take(pc * PATCH_LEN)?;
        let fee = match c.u8()? {
            0 => None,
            1 => Some(Fee::read(c.take(FEE_LEN)?)),
            _ => return Err(E::BadLayout.into()),
        };
        Ok(Self {
            program,
            accounts,
            data,
            patches,
            fee,
        })
    }

    #[inline(always)]
    pub fn patches(&self) -> impl Iterator<Item = Patch> + 'a {
        self.patches.chunks_exact(PATCH_LEN).map(Patch::read)
    }

    /// Structural checks that do not need accounts. `index` is this step's
    /// position, `watch_count` the number of watched slots.
    pub fn validate(&self, index: usize, watch_count: usize) -> Result<(), ProgramError> {
        for p in self.patches() {
            let end = (p.offset as usize).checked_add(8).ok_or(E::BadLayout)?;
            if end > self.data.len() {
                return Err(E::BadLayout.into());
            }
            if p.watch as usize >= watch_count || p.from_step as usize >= index {
                return Err(E::IndexOutOfRange.into());
            }
            match p.mode {
                MODE_DELTA | MODE_MIN => {}
                MODE_SCALE | MODE_SCALE_SUB => {
                    if p.den == 0 {
                        return Err(E::DivideByZero.into());
                    }
                }
                _ => return Err(E::BadLayout.into()),
            }
        }
        if let Some(f) = self.fee {
            if f.watch as usize >= watch_count {
                return Err(E::IndexOutOfRange.into());
            }
        }
        Ok(())
    }
}

/// Top-level view of a validated `compose` payload.
#[derive(Debug)]
pub struct Layout<'a> {
    /// Outer account indices of the watched accounts.
    pub watch: &'a [u8],
    pub step_count: usize,
    /// Byte offset of step 0 inside the instruction data.
    pub steps_at: usize,
    /// `check_count * CHECK_LEN` bytes.
    pub checks: &'a [u8],
}

impl<'a> Layout<'a> {
    /// Walk and validate the whole payload (structure + limits). Account-bound
    /// checks (index ranges, executable, reentrancy) live in the processor.
    pub fn parse(data: &'a [u8]) -> Result<Self, ProgramError> {
        let mut c = Cursor::new(data);
        if c.u8()? != TAG_COMPOSE {
            return Err(ProgramError::InvalidInstructionData);
        }
        let wc = c.u8()? as usize;
        if wc > MAX_WATCH {
            return Err(E::LimitExceeded.into());
        }
        let watch = c.take(wc)?;
        let sc = c.u8()? as usize;
        if sc > MAX_STEPS {
            return Err(E::LimitExceeded.into());
        }
        let steps_at = c.pos();
        for i in 0..sc {
            Step::parse(&mut c)?.validate(i, wc)?;
        }
        let cc = c.u8()? as usize;
        if cc > MAX_CHECKS {
            return Err(E::LimitExceeded.into());
        }
        let checks = c.take(cc * CHECK_LEN)?;
        for ch in checks.chunks_exact(CHECK_LEN).map(Check::read) {
            if ch.watch as usize >= wc {
                return Err(E::IndexOutOfRange.into());
            }
            if ch.kind > CHECK_MAX {
                return Err(E::BadLayout.into());
            }
        }
        if !c.done() {
            return Err(E::BadLayout.into());
        }
        Ok(Self {
            watch,
            step_count: sc,
            steps_at,
            checks,
        })
    }

    #[inline(always)]
    pub fn checks(&self) -> impl Iterator<Item = Check> + 'a {
        self.checks.chunks_exact(CHECK_LEN).map(Check::read)
    }
}

/// Shared fixture: the same bytes are asserted by `client/encode.test.ts`.
/// Description: watch [2,5]; step 0 = program 6, accounts [1,2,3], data
/// `03 10 27 00..` (token transfer 10000), no patches, no fee; step 1 =
/// program 6, accounts [2,4,1], data `03 00..`, one patch {offset 1, watch 0,
/// from 0, mode 1, num 3, den 4, sub 0}, fee {watch 0, ata 7, mint 8,
/// token 6, ata-program 9, system 10}; one check {watch 1, kind 0, bound -5}.
#[cfg(test)]
pub const FIXTURE: &[u8] = &[
    0x00, // tag
    0x02, 0x02, 0x05, // watch
    0x02, // step_count
    // step 0
    0x06, 0x03, 0x01, 0x02, 0x03, 0x09, 0x00, 0x03, 0x10, 0x27, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, // step 1
    0x06, 0x03, 0x02, 0x04, 0x01, 0x09, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x07, 0x08, 0x06, 0x09, 0x0a,
    // checks
    0x01, 0x01, 0x00, 0xfb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    #[test]
    fn fixture_round_trip() {
        let l = Layout::parse(FIXTURE).unwrap();
        assert_eq!(l.watch, &[2, 5]);
        assert_eq!(l.step_count, 2);
        let mut c = Cursor::at(FIXTURE, l.steps_at);
        let s0 = Step::parse(&mut c).unwrap();
        assert_eq!(s0.program, 6);
        assert_eq!(s0.accounts, &[1, 2, 3]);
        assert_eq!(s0.data, &[3, 0x10, 0x27, 0, 0, 0, 0, 0, 0]);
        assert_eq!(s0.patches().count(), 0);
        assert!(s0.fee.is_none());
        let s1 = Step::parse(&mut c).unwrap();
        assert_eq!(s1.program, 6);
        assert_eq!(s1.accounts, &[2, 4, 1]);
        assert_eq!(s1.data, &[3, 0, 0, 0, 0, 0, 0, 0, 0]);
        let p: Vec<Patch> = s1.patches().collect();
        assert_eq!(
            p,
            std::vec![Patch {
                offset: 1,
                watch: 0,
                from_step: 0,
                mode: 1,
                num: 3,
                den: 4,
                sub: 0
            }]
        );
        assert_eq!(
            s1.fee,
            Some(Fee {
                watch: 0,
                fee_ata: 7,
                mint: 8,
                token_program: 6,
                ata_program: 9,
                system_program: 10
            })
        );
        let ch: Vec<Check> = l.checks().collect();
        assert_eq!(
            ch,
            std::vec![Check {
                watch: 1,
                kind: 0,
                bound: -5
            }]
        );
        // The check block starts right after step 1.
        assert_eq!(c.pos(), FIXTURE.len() - 1 - CHECK_LEN);
    }

    #[test]
    fn rejects_trailing_and_truncated() {
        let mut v = FIXTURE.to_vec();
        v.push(0);
        assert_eq!(
            Layout::parse(&v).unwrap_err(),
            ProgramError::Custom(E::BadLayout as u32)
        );
        assert_eq!(
            Layout::parse(&FIXTURE[..FIXTURE.len() - 1]).unwrap_err(),
            ProgramError::Custom(E::BadLayout as u32)
        );
    }

    #[test]
    fn rejects_bad_patch_and_from_step() {
        // from_step must be < step index: step 0 with a patch is invalid.
        let v: Vec<u8> = [
            0u8, 1, 0, 1, // tag, 1 watch (idx 0), 1 step
            5, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, // program 5, 0 accounts, 8 data bytes
            1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 1 patch
            0, 0, // fee none, 0 checks
        ]
        .to_vec();
        assert_eq!(
            Layout::parse(&v).unwrap_err(),
            ProgramError::Custom(E::IndexOutOfRange as u32)
        );
    }
}
