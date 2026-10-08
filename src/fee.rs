//! Protocol fee: `FEE_BPS` of the value a hop produced in a watched token
//! account, paid in kind to the fee recipient's canonical ATA.
use crate::{
    error::ZapError as E,
    layout::{le64, Fee},
    processor::{is_token_account, AMOUNT_OFFSET, SYSTEM_PROGRAM, TOKEN_2022_PROGRAM, TOKEN_PROGRAM},
};
use pinocchio::{
    cpi::invoke,
    error::ProgramError,
    instruction::{InstructionAccount, InstructionView},
    AccountView, Address, ProgramResult,
};

/// 0.1% per hop.
pub const FEE_BPS: u64 = 10;
pub const BPS_DENOMINATOR: u64 = 10_000;
/// Hardcoded fee recipient wallet (owner of the fee ATAs).
pub const FEE_RECIPIENT: Address =
    Address::from_str_const("331nEBz4i3XjyaUHVyHnpw9xBoW7D6P1qMPnUPd76Mth");
pub const ATA_PROGRAM: Address =
    Address::from_str_const("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

const ATA_CREATE_IDEMPOTENT: u8 = 1;
const TOKEN_TRANSFER: u8 = 3;
const TOKEN_TRANSFER_CHECKED: u8 = 12;
const MINT_DECIMALS_OFFSET: usize = 44;
const MINT_LEN: usize = 82;

/// Settle the fee for one hop. `pre` is the watched balance snapshotted
/// before the step. Native-SOL watches are skipped (no fee).
#[inline(never)]
pub fn settle(accounts: &[AccountView], watch: &[u8], f: &Fee, pre: u64) -> ProgramResult {
    let watched = &accounts[watch[f.watch as usize] as usize];
    let owner = *watched.owner();
    if owner == SYSTEM_PROGRAM {
        return Ok(());
    }
    let is_2022 = if owner == TOKEN_PROGRAM {
        false
    } else if owner == TOKEN_2022_PROGRAM {
        true
    } else {
        return Err(E::UnsupportedBalanceOwner.into());
    };
    let (post, watched_mint) = {
        let d = watched.try_borrow()?;
        if !is_token_account(&d) {
            return Err(E::UnsupportedBalanceOwner.into());
        }
        let mut m = [0u8; 32];
        m.copy_from_slice(&d[..32]);
        (le64(&d, AMOUNT_OFFSET), Address::new_from_array(m))
    };
    let delta = post as i128 - pre as i128;
    if delta < 0 {
        return Err(E::NegativeDelta.into());
    }
    // delta < 2^64, FEE_BPS small: fits.
    let amount = (delta as u128 * FEE_BPS as u128 / BPS_DENOMINATOR as u128) as u64;
    if amount == 0 {
        return Ok(());
    }

    let user = &accounts[0];
    if !user.is_signer() {
        return Err(E::UserNotSigner.into());
    }
    let mint = &accounts[f.mint as usize];
    if *mint.address() != watched_mint {
        return Err(E::FeeMintMismatch.into());
    }
    let token_program = &accounts[f.token_program as usize];
    let ata_program = &accounts[f.ata_program as usize];
    let system_program = &accounts[f.system_program as usize];
    if *token_program.address() != owner
        || *ata_program.address() != ATA_PROGRAM
        || *system_program.address() != SYSTEM_PROGRAM
    {
        return Err(E::FeeProgramMismatch.into());
    }
    let fee_ata = &accounts[f.fee_ata as usize];
    let (expected, _) = Address::find_program_address(
        &[FEE_RECIPIENT.as_ref(), owner.as_ref(), mint.address().as_ref()],
        &ATA_PROGRAM,
    );
    if *fee_ata.address() != expected {
        return Err(E::FeeAtaMismatch.into());
    }
    let recipient = accounts
        .iter()
        .find(|a| *a.address() == FEE_RECIPIENT)
        .ok_or(E::FeeRecipientMissing)?;

    // (a) Create the fee ATA if missing. Payer = user, owner = FEE_RECIPIENT.
    invoke(
        &InstructionView {
            program_id: &ATA_PROGRAM,
            accounts: &[
                InstructionAccount::writable_signer(user.address()),
                InstructionAccount::writable(fee_ata.address()),
                InstructionAccount::readonly(recipient.address()),
                InstructionAccount::readonly(mint.address()),
                InstructionAccount::readonly(system_program.address()),
                InstructionAccount::readonly(token_program.address()),
            ],
            data: &[ATA_CREATE_IDEMPOTENT],
        },
        &[user, fee_ata, recipient, mint, system_program, token_program],
    )?;

    // (b) Move the fee in kind, authority = user.
    let amt = amount.to_le_bytes();
    if is_2022 {
        let decimals = {
            let d = mint.try_borrow()?;
            if d.len() < MINT_LEN {
                return Err(E::FeeMintMismatch.into());
            }
            d[MINT_DECIMALS_OFFSET]
        };
        let mut data = [0u8; 10];
        data[0] = TOKEN_TRANSFER_CHECKED;
        data[1..9].copy_from_slice(&amt);
        data[9] = decimals;
        invoke(
            &InstructionView {
                program_id: token_program.address(),
                accounts: &[
                    InstructionAccount::writable(watched.address()),
                    InstructionAccount::readonly(mint.address()),
                    InstructionAccount::writable(fee_ata.address()),
                    InstructionAccount::readonly_signer(user.address()),
                ],
                data: &data,
            },
            &[watched, mint, fee_ata, user],
        )
    } else {
        let mut data = [0u8; 9];
        data[0] = TOKEN_TRANSFER;
        data[1..9].copy_from_slice(&amt);
        invoke(
            &InstructionView {
                program_id: token_program.address(),
                accounts: &[
                    InstructionAccount::writable(watched.address()),
                    InstructionAccount::writable(fee_ata.address()),
                    InstructionAccount::readonly_signer(user.address()),
                ],
                data: &data,
            },
            &[watched, fee_ata, user],
        )
    }
}

#[inline(always)]
pub fn fee_for(delta: u64) -> u64 {
    (delta as u128 * FEE_BPS as u128 / BPS_DENOMINATOR as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fee_math() {
        assert_eq!(fee_for(0), 0);
        assert_eq!(fee_for(999), 0);
        assert_eq!(fee_for(1_000), 1);
        assert_eq!(fee_for(1_000_000), 1_000);
        assert_eq!(fee_for(u64::MAX), u64::MAX / 1_000);
    }
}
