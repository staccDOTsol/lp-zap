//! Stable custom error codes (`ProgramError::Custom(code)`). See README.md.
use pinocchio::error::ProgramError;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZapError {
    /// Instruction data is truncated, has trailing bytes, an unknown mode/kind,
    /// or a patch offset that does not satisfy `offset + 8 <= data_len`.
    BadLayout = 0,
    /// An account index, watch slot or `from_step` is out of range.
    IndexOutOfRange = 1,
    /// A watched account is neither a Token / Token-2022 token account nor a
    /// System-owned account.
    UnsupportedBalanceOwner = 2,
    /// A patch (or fee) delta is negative.
    NegativeDelta = 3,
    /// A scaled patch value does not fit in a `u64`.
    PatchOverflow = 4,
    /// `den == 0` for mode 1 or 2.
    DivideByZero = 5,
    /// A final check did not hold.
    CheckFailed = 6,
    /// A step tried to CPI this program.
    Reentrancy = 7,
    /// Too many watches / steps / accounts / patches / checks, or step data
    /// longer than `MAX_STEP_DATA`.
    LimitExceeded = 8,
    /// `program_index` does not point at an executable account.
    ProgramNotExecutable = 9,
    /// `fee_ata_index` is not the fee recipient's canonical ATA for the
    /// watched account's mint and token program.
    FeeAtaMismatch = 10,
    /// `mint_index` is not the mint of the watched token account.
    FeeMintMismatch = 11,
    /// `token_program_index` / `ata_program_index` / `system_program_index`
    /// do not point at the expected programs.
    FeeProgramMismatch = 12,
    /// The fee recipient pubkey is not present in the outer account list.
    FeeRecipientMissing = 13,
    /// Account 0 (the user) must sign when a fee is settled.
    UserNotSigner = 14,
}

impl From<ZapError> for ProgramError {
    #[inline(always)]
    fn from(e: ZapError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
