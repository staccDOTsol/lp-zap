//! `compose` execution: snapshot, patch, CPI, fee, check.
use crate::{
    error::ZapError as E,
    fee,
    layout::{le64, Cursor, Layout, Step, CHECK_MIN, MAX_STEPS, MAX_STEP_ACCOUNTS, MAX_STEP_DATA, MAX_WATCH, MODE_DELTA, MODE_MIN, MODE_SCALE, MODE_SCALE_SUB},
};
use core::mem::MaybeUninit;
use pinocchio::{
    cpi::{invoke_unchecked, CpiAccount},
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

pub const SYSTEM_PROGRAM: Address = Address::new_from_array([0; 32]);
pub const TOKEN_PROGRAM: Address =
    Address::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const TOKEN_2022_PROGRAM: Address =
    Address::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// SPL token account: 165 bytes, or longer with the Token-2022 account-type
/// byte (`2 = Account`) at offset 165 (mints with extensions carry `1`).
pub const TOKEN_ACCOUNT_LEN: usize = 165;
pub const AMOUNT_OFFSET: usize = 64;

/// Working memory. Lives in the (otherwise unused) heap region on-chain so the
/// BPF stack frames stay small; a static on the host.
#[repr(C)]
pub struct Scratch {
    pub data: [u8; MAX_STEP_DATA],
    pub snaps: [[u64; MAX_WATCH]; MAX_STEPS + 1],
    pub metas: [MaybeUninit<InstructionAccount<'static>>; MAX_STEP_ACCOUNTS],
    pub cpi: [MaybeUninit<CpiAccount<'static>>; MAX_STEP_ACCOUNTS],
}

#[cfg(target_os = "solana")]
#[inline(always)]
fn scratch() -> &'static mut Scratch {
    // SAFETY: `no_allocator!` is installed, so nothing else uses the heap; the
    // region is zero-initialised, 8-aligned and far larger than `Scratch`.
    unsafe { &mut *(pinocchio::entrypoint::HEAP_START_ADDRESS as usize as *mut Scratch) }
}

#[cfg(not(target_os = "solana"))]
#[inline(always)]
fn scratch() -> &'static mut Scratch {
    static mut S: Scratch = Scratch {
        data: [0; MAX_STEP_DATA],
        snaps: [[0; MAX_WATCH]; MAX_STEPS + 1],
        metas: [const { MaybeUninit::uninit() }; MAX_STEP_ACCOUNTS],
        cpi: [const { MaybeUninit::uninit() }; MAX_STEP_ACCOUNTS],
    };
    // SAFETY: host builds never execute `compose` concurrently.
    unsafe { &mut *core::ptr::addr_of_mut!(S) }
}

/// Detach a lifetime so a reference can be parked in `Scratch` for the
/// duration of one CPI. Never outlives `process_instruction`.
#[inline(always)]
fn forever<'a, T: ?Sized>(r: &'a T) -> &'static T {
    unsafe { &*(r as *const T) }
}

#[inline(always)]
pub fn is_token_account(d: &[u8]) -> bool {
    d.len() >= TOKEN_ACCOUNT_LEN && (d.len() == TOKEN_ACCOUNT_LEN || d[TOKEN_ACCOUNT_LEN] == 2)
}

/// Balance of a watched account: token `amount` for Token / Token-2022 token
/// accounts, lamports for System-owned accounts; anything else is refused.
pub fn balance(v: &AccountView) -> Result<u64, ProgramError> {
    let owner = v.owner();
    if *owner == SYSTEM_PROGRAM {
        return Ok(v.lamports());
    }
    if *owner == TOKEN_PROGRAM || *owner == TOKEN_2022_PROGRAM {
        let d = v.try_borrow()?;
        if !is_token_account(&d) {
            return Err(E::UnsupportedBalanceOwner.into());
        }
        return Ok(le64(&d, AMOUNT_OFFSET));
    }
    Err(E::UnsupportedBalanceOwner.into())
}

#[inline(never)]
fn snapshot(accounts: &[AccountView], watch: &[u8], out: &mut [u64; MAX_WATCH]) -> ProgramResult {
    for (slot, &ai) in watch.iter().enumerate() {
        out[slot] = balance(&accounts[ai as usize])?;
    }
    Ok(())
}

/// Value for one patch of step `s`, from the cumulative change between the
/// snapshot before `from_step` and the snapshot before `s`.
pub fn patch_value(
    snaps: &[[u64; MAX_WATCH]],
    s: usize,
    watch: u8,
    from_step: u8,
    mode: u8,
    num: u32,
    den: u32,
    sub: u64,
) -> Result<u64, ProgramError> {
    let now = snaps[s][watch as usize] as i128;
    let then = snaps[from_step as usize][watch as usize] as i128;
    let delta = now - then;
    if delta < 0 {
        return Err(E::NegativeDelta.into());
    }
    let delta = delta as u128;
    match mode {
        MODE_DELTA => Ok(delta as u64),
        MODE_SCALE | MODE_SCALE_SUB => {
            if den == 0 {
                return Err(E::DivideByZero.into());
            }
            // delta < 2^64 and num < 2^32: the product cannot overflow u128.
            let v = delta * num as u128 / den as u128;
            if v > u64::MAX as u128 {
                return Err(E::PatchOverflow.into());
            }
            let v = v as u64;
            Ok(if mode == MODE_SCALE_SUB { v.saturating_sub(sub) } else { v })
        }
        MODE_MIN => Ok(core::cmp::min(delta as u64, num as u64)),
        _ => Err(E::BadLayout.into()),
    }
}

/// Account-bound validation of the whole payload before anything executes.
fn bind(program_id: &Address, accounts: &[AccountView], data: &[u8], l: &Layout) -> ProgramResult {
    let n = accounts.len();
    for &w in l.watch {
        if w as usize >= n {
            return Err(E::IndexOutOfRange.into());
        }
    }
    let mut c = Cursor::at(data, l.steps_at);
    for _ in 0..l.step_count {
        let st = Step::parse(&mut c)?;
        let pi = st.program as usize;
        if pi >= n {
            return Err(E::IndexOutOfRange.into());
        }
        let prog = &accounts[pi];
        if prog.address() == program_id {
            return Err(E::Reentrancy.into());
        }
        if !prog.executable() {
            return Err(E::ProgramNotExecutable.into());
        }
        for &ai in st.accounts {
            if ai as usize >= n {
                return Err(E::IndexOutOfRange.into());
            }
        }
        if let Some(f) = st.fee {
            for ix in [f.fee_ata, f.mint, f.token_program, f.ata_program, f.system_program] {
                if ix as usize >= n {
                    return Err(E::IndexOutOfRange.into());
                }
            }
        }
    }
    Ok(())
}

/// Build the patched data copy and CPI tables for one step, then invoke it.
#[inline(never)]
fn run_step(accounts: &[AccountView], sc: &mut Scratch, s: usize, st: &Step) -> ProgramResult {
    let dl = st.data.len();
    sc.data[..dl].copy_from_slice(st.data);
    for p in st.patches() {
        let v = patch_value(&sc.snaps, s, p.watch, p.from_step, p.mode, p.num, p.den, p.sub)?;
        let o = p.offset as usize;
        sc.data[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }
    let ac = st.accounts.len();
    for (i, &ai) in st.accounts.iter().enumerate() {
        let v = forever(&accounts[ai as usize]);
        // Flags come from the outer transaction only: never escalated.
        sc.metas[i].write(InstructionAccount::new(v.address(), v.is_writable(), v.is_signer()));
        sc.cpi[i].write(CpiAccount::from(v));
    }
    // SAFETY: the first `ac` entries of both tables were just written.
    let (metas, cpi) = unsafe {
        (
            core::slice::from_raw_parts(sc.metas.as_ptr() as *const InstructionAccount, ac),
            core::slice::from_raw_parts(sc.cpi.as_ptr() as *const CpiAccount, ac),
        )
    };
    let ix = InstructionView {
        program_id: accounts[st.program as usize].address(),
        accounts: metas,
        data: &sc.data[..dl],
    };
    // SAFETY: no account data is borrowed at this point (snapshots and fee
    // reads release their borrows before returning). A failed CPI aborts the
    // whole transaction, so there is no partial effect to unwind.
    unsafe { invoke_unchecked(&ix, cpi) };
    Ok(())
}

#[inline(never)]
pub fn process_instruction(program_id: &Address, accounts: &mut [AccountView], data: &[u8]) -> ProgramResult {
    let accounts: &[AccountView] = accounts;
    let l = Layout::parse(data)?;
    bind(program_id, accounts, data, &l)?;
    let sc = scratch();
    let n = l.step_count;
    let mut c = Cursor::at(data, l.steps_at);
    for s in 0..n {
        snapshot(accounts, l.watch, &mut sc.snaps[s])?;
        let st = Step::parse(&mut c)?;
        run_step(accounts, sc, s, &st)?;
        if let Some(f) = st.fee {
            // Settled before the next snapshot, so later patches see the net delta.
            fee::settle(accounts, l.watch, &f, sc.snaps[s][f.watch as usize])?;
        }
    }
    snapshot(accounts, l.watch, &mut sc.snaps[n])?;
    for ch in l.checks() {
        let w = ch.watch as usize;
        let d = sc.snaps[n][w] as i128 - sc.snaps[0][w] as i128;
        let ok = if ch.kind == CHECK_MIN { d >= ch.bound as i128 } else { d <= ch.bound as i128 };
        if !ok {
            return Err(E::CheckFailed.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_modes() {
        let mut snaps = [[0u64; MAX_WATCH]; MAX_STEPS + 1];
        snaps[0][0] = 100;
        snaps[1][0] = 10_100;
        let pv = |mode, num, den, sub| patch_value(&snaps, 1, 0, 0, mode, num, den, sub);
        assert_eq!(pv(0, 0, 0, 0).unwrap(), 10_000);
        assert_eq!(pv(1, 3, 4, 0).unwrap(), 7_500);
        assert_eq!(pv(2, 3, 4, 500).unwrap(), 7_000);
        assert_eq!(pv(2, 3, 4, 9_000).unwrap(), 0);
        assert_eq!(pv(3, 4_000, 0, 0).unwrap(), 4_000);
        assert_eq!(pv(3, 40_000, 0, 0).unwrap(), 10_000);
        assert_eq!(pv(1, 1, 0, 0).unwrap_err(), ProgramError::Custom(E::DivideByZero as u32));
        snaps[1][0] = 1u64 << 63;
        snaps[0][0] = 0;
        assert_eq!(pv(1, 4, 1, 0).unwrap_err(), ProgramError::Custom(E::PatchOverflow as u32));
        assert_eq!(pv(1, 2, 1, 0).unwrap_err(), ProgramError::Custom(E::PatchOverflow as u32));
        assert_eq!(pv(1, 1, 1, 0).unwrap(), 1u64 << 63);
        snaps[1][0] = 5;
        assert_eq!(patch_value(&snaps, 1, 0, 0, 0, 0, 0, 0).unwrap(), 5);
        snaps[0][0] = 6;
        assert_eq!(
            patch_value(&snaps, 1, 0, 0, 0, 0, 0, 0).unwrap_err(),
            ProgramError::Custom(E::NegativeDelta as u32)
        );
    }
}
