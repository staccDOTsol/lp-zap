//! End-to-end tests of `compose` on Mollusk against the built SBF binary
//! (`cargo build-sbf` first: these load `target/deploy/lp_zap.so`).
//!
//! Token accounts (165 bytes) and mints (82 bytes) are hand-packed; the step
//! instructions are hand-encoded (`[3] + u64` token transfer, `[2,0,0,0] +
//! u64` system transfer) so nothing here depends on an SPL client crate.
use mollusk_svm::{program::loader_keys, result::InstructionResult, Mollusk};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::ProgramError;
use solana_pubkey::Pubkey;
use std::str::FromStr;

const TOKEN: Pubkey = mollusk_svm_programs_token::token::ID;
const TOKEN_2022: Pubkey = mollusk_svm_programs_token::token2022::ID;
const ATA: Pubkey = mollusk_svm_programs_token::associated_token::ID;
const SYSTEM: Pubkey = Pubkey::new_from_array([0; 32]);
const SOL: u64 = 1_000_000_000;

fn program_id() -> Pubkey {
    let id = Pubkey::from_str("BHYw1FAWPriW9Gh7BG49X4UVe96CDjaxFrFFUtGSQmRx").unwrap();
    assert_eq!(id.as_ref(), lp_zap::ID.as_ref());
    id
}
fn recipient() -> Pubkey {
    Pubkey::from_str("331nEBz4i3XjyaUHVyHnpw9xBoW7D6P1qMPnUPd76Mth").unwrap()
}
fn fee_ata(token_program: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[recipient().as_ref(), token_program.as_ref(), mint.as_ref()],
        &ATA,
    )
    .0
}

fn vm() -> Mollusk {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/target/deploy/lp_zap");
    let mut m = Mollusk::new(&program_id(), path);
    mollusk_svm_programs_token::token::add_program(&mut m);
    mollusk_svm_programs_token::token2022::add_program(&mut m);
    mollusk_svm_programs_token::associated_token::add_program(&mut m);
    m
}

// ---------------------------------------------------------------- packing

fn mint_data(decimals: u8) -> Vec<u8> {
    let mut d = vec![0u8; 82];
    // mint_authority: None (COption tag 0), supply @36 irrelevant for transfers.
    d[36..44].copy_from_slice(&u64::MAX.to_le_bytes());
    d[44] = decimals;
    d[45] = 1; // is_initialized
    d
}
fn token_data(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; 165];
    d[0..32].copy_from_slice(mint.as_ref());
    d[32..64].copy_from_slice(owner.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1; // AccountState::Initialized
    d
}
fn acct(owner: Pubkey, lamports: u64, data: Vec<u8>) -> Account {
    Account {
        lamports,
        data,
        owner,
        executable: false,
        rent_epoch: 0,
    }
}
fn token_ix(amount: u64) -> Vec<u8> {
    let mut d = vec![3u8];
    d.extend_from_slice(&amount.to_le_bytes());
    d
}
fn system_transfer_ix(lamports: u64) -> Vec<u8> {
    let mut d = vec![2u8, 0, 0, 0];
    d.extend_from_slice(&lamports.to_le_bytes());
    d
}

// --------------------------------------------------------------- encoding

#[derive(Clone, Copy)]
struct P {
    offset: u16,
    watch: u8,
    from: u8,
    mode: u8,
    num: u32,
    den: u32,
    sub: u64,
}
fn p(offset: u16, watch: u8, from: u8, mode: u8, num: u32, den: u32, sub: u64) -> P {
    P {
        offset,
        watch,
        from,
        mode,
        num,
        den,
        sub,
    }
}
#[derive(Clone)]
struct S {
    program: u8,
    accounts: Vec<u8>,
    data: Vec<u8>,
    patches: Vec<P>,
    fee: Option<[u8; 6]>,
}
fn step(program: u8, accounts: &[u8], data: Vec<u8>) -> S {
    S {
        program,
        accounts: accounts.to_vec(),
        data,
        patches: vec![],
        fee: None,
    }
}
impl S {
    fn patch(mut self, p: P) -> Self {
        self.patches.push(p);
        self
    }
    fn fee(mut self, f: [u8; 6]) -> Self {
        self.fee = Some(f);
        self
    }
}
fn encode(watch: &[u8], steps: &[S], checks: &[(u8, u8, i64)]) -> Vec<u8> {
    let mut v = vec![0u8, watch.len() as u8];
    v.extend_from_slice(watch);
    v.push(steps.len() as u8);
    for s in steps {
        v.push(s.program);
        v.push(s.accounts.len() as u8);
        v.extend_from_slice(&s.accounts);
        v.extend_from_slice(&(s.data.len() as u16).to_le_bytes());
        v.extend_from_slice(&s.data);
        v.push(s.patches.len() as u8);
        for p in &s.patches {
            v.extend_from_slice(&p.offset.to_le_bytes());
            v.extend_from_slice(&[p.watch, p.from, p.mode]);
            v.extend_from_slice(&p.num.to_le_bytes());
            v.extend_from_slice(&p.den.to_le_bytes());
            v.extend_from_slice(&p.sub.to_le_bytes());
        }
        match s.fee {
            None => v.push(0),
            Some(f) => {
                v.push(1);
                v.extend_from_slice(&f);
            }
        }
    }
    v.push(checks.len() as u8);
    for &(w, k, b) in checks {
        v.extend_from_slice(&[w, k]);
        v.extend_from_slice(&b.to_le_bytes());
    }
    v
}

// ------------------------------------------------------------ environment

#[derive(Default, Clone)]
struct Env {
    keys: Vec<Pubkey>,
    accts: Vec<Account>,
    flags: Vec<(bool, bool)>, // (signer, writable)
}
impl Env {
    fn add(&mut self, k: Pubkey, a: Account, signer: bool, writable: bool) -> u8 {
        self.keys.push(k);
        self.accts.push(a);
        self.flags.push((signer, writable));
        (self.keys.len() - 1) as u8
    }
    fn ix(&self, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: program_id(),
            accounts: self
                .keys
                .iter()
                .zip(&self.flags)
                .map(|(k, &(s, w))| {
                    if w {
                        AccountMeta::new(*k, s)
                    } else {
                        AccountMeta::new_readonly(*k, s)
                    }
                })
                .collect(),
            data,
        }
    }
    fn run(&self, m: &Mollusk, data: Vec<u8>) -> InstructionResult {
        let accounts: Vec<(Pubkey, Account)> =
            self.keys.iter().cloned().zip(self.accts.iter().cloned()).collect();
        m.process_instruction(&self.ix(data), &accounts)
    }
}
fn get<'a>(r: &'a InstructionResult, k: &Pubkey) -> &'a Account {
    &r.resulting_accounts.iter().find(|(x, _)| x == k).unwrap().1
}
fn amount(r: &InstructionResult, k: &Pubkey) -> u64 {
    u64::from_le_bytes(get(r, k).data[64..72].try_into().unwrap())
}
fn ok(r: &InstructionResult) {
    assert!(r.program_result.is_ok(), "{:?}", r.program_result);
}
fn custom(r: &InstructionResult, code: u32) {
    assert_eq!(
        r.program_result,
        mollusk_svm::result::ProgramResult::Failure(ProgramError::Custom(code)),
        "raw {:?}",
        r.raw_result
    );
}

/// Standard world. Indices:
/// 0 user (signer, writable), 1 token program, 2 mint, 3 A (user, 10M),
/// 4 B (user, 0), 5 C (other, 0), 6 X (system wallet), 7 system program.
struct World {
    env: Env,
    mint: Pubkey,
    a: Pubkey,
    b: Pubkey,
    c: Pubkey,
    x: Pubkey,
}
const A_START: u64 = 10_000_000;
fn world_with(a_amount: u64) -> World {
    let mut env = Env::default();
    let user = Pubkey::new_unique();
    let other = Pubkey::new_unique();
    let mint = Pubkey::new_unique();
    let (a, b, c, x) = (
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    );
    env.add(user, acct(SYSTEM, 100 * SOL, vec![]), true, true);
    let (tk, ta) = mollusk_svm_programs_token::token::keyed_account();
    env.add(tk, ta, false, false);
    env.add(mint, acct(TOKEN, SOL, mint_data(6)), false, false);
    env.add(a, acct(TOKEN, SOL, token_data(&mint, &user, a_amount)), false, true);
    env.add(b, acct(TOKEN, SOL, token_data(&mint, &user, 0)), false, true);
    env.add(c, acct(TOKEN, SOL, token_data(&mint, &other, 0)), false, true);
    env.add(x, acct(SYSTEM, SOL, vec![]), false, true);
    let (sk, sa) = mollusk_svm::program::keyed_account_for_system_program();
    env.add(sk, sa, false, false);
    World {
        env,
        mint,
        a,
        b,
        c,
        x,
    }
}
fn world() -> World {
    world_with(A_START)
}

// ------------------------------------------------------------------ cases

/// (1) step 2's amount is step 1's measured delta (mode 0).
#[test]
fn two_transfers_mode0_delta() {
    let m = vm();
    let w = world();
    let data = encode(
        &[4], // watch B
        &[
            step(1, &[3, 4, 0], token_ix(10_000)),
            step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 0, 0, 0, 0, 0, 0)),
        ],
        &[],
    );
    let r = w.env.run(&m, data);
    ok(&r);
    assert_eq!(amount(&r, &w.a), A_START - 10_000);
    assert_eq!(amount(&r, &w.b), 0);
    assert_eq!(amount(&r, &w.c), 10_000);
}

fn run_mode(mode: u8, num: u32, den: u32, sub: u64) -> (World, InstructionResult) {
    let m = vm();
    let w = world();
    let data = encode(
        &[4],
        &[
            step(1, &[3, 4, 0], token_ix(10_000)),
            step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 0, 0, mode, num, den, sub)),
        ],
        &[],
    );
    let r = w.env.run(&m, data);
    (w, r)
}

/// (2) modes 1, 2, 3 and the refusals.
#[test]
fn mode1_scale() {
    let (w, r) = run_mode(1, 3, 4, 0);
    ok(&r);
    assert_eq!(amount(&r, &w.c), 7_500);
    assert_eq!(amount(&r, &w.b), 2_500);
}
#[test]
fn mode2_scale_sub_and_saturation() {
    let (w, r) = run_mode(2, 3, 4, 500);
    ok(&r);
    assert_eq!(amount(&r, &w.c), 7_000);
    let (w, r) = run_mode(2, 3, 4, 1_000_000);
    ok(&r);
    assert_eq!(amount(&r, &w.c), 0);
}
#[test]
fn mode3_min() {
    let (w, r) = run_mode(3, 4_000, 0, 0);
    ok(&r);
    assert_eq!(amount(&r, &w.c), 4_000);
    let (w, r) = run_mode(3, 40_000, 0, 0);
    ok(&r);
    assert_eq!(amount(&r, &w.c), 10_000);
}
#[test]
fn mode1_overflow_and_div0_refused() {
    let m = vm();
    let w = world_with(u64::MAX);
    let big = 1u64 << 63;
    let data = encode(
        &[4],
        &[
            step(1, &[3, 4, 0], token_ix(big)),
            step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 0, 0, 1, 2, 1, 0)),
        ],
        &[],
    );
    custom(&w.env.run(&m, data), 4);
    let (_, r) = run_mode(1, 1, 0, 0);
    custom(&r, 5);
    let (_, r) = run_mode(2, 1, 0, 0);
    custom(&r, 5);
    let (_, r) = run_mode(4, 1, 1, 0);
    custom(&r, 0);
}

/// (3) a native-SOL watch (System-owned wallet lamports) sizes a token transfer.
#[test]
fn native_sol_watch_patches_token_transfer() {
    let m = vm();
    let w = world();
    let data = encode(
        &[6], // watch X lamports
        &[
            step(7, &[0, 6], system_transfer_ix(5_000)),
            step(1, &[3, 5, 0], token_ix(0)).patch(p(1, 0, 0, 0, 0, 0, 0)),
        ],
        &[(0, 0, 5_000)],
    );
    let r = w.env.run(&m, data);
    ok(&r);
    assert_eq!(get(&r, &w.x).lamports, SOL + 5_000);
    assert_eq!(amount(&r, &w.c), 5_000);
    assert_eq!(amount(&r, &w.a), A_START - 5_000);
}

/// (4) final checks: a failing min/max rolls the whole thing back.
#[test]
fn final_checks() {
    let m = vm();
    let w = world();
    let steps = [step(1, &[3, 5, 0], token_ix(10_000))];
    // watch 0 = C, watch 1 = A
    let r = w.env.run(&m, encode(&[5, 3], &steps, &[(0, 0, 10_001)]));
    custom(&r, 6);
    assert_eq!(amount(&r, &w.c), 0);
    assert_eq!(amount(&r, &w.a), A_START);
    let r = w.env.run(&m, encode(&[5, 3], &steps, &[(1, 1, -10_001)]));
    custom(&r, 6);
    let r = w.env.run(&m, encode(&[5, 3], &steps, &[(0, 0, 10_000), (1, 1, -10_000)]));
    ok(&r);
    assert_eq!(amount(&r, &w.c), 10_000);
    assert_eq!(amount(&r, &w.a), A_START - 10_000);
}

/// (5) structural / binding refusals.
#[test]
fn reentrancy_refused() {
    let m = vm();
    let mut w = world();
    let me = w.env.add(
        program_id(),
        Account {
            lamports: SOL,
            data: vec![],
            owner: loader_keys::LOADER_V2,
            executable: true,
            rent_epoch: 0,
        },
        false,
        false,
    );
    let r = w.env.run(&m, encode(&[], &[step(me, &[], vec![0, 0, 0])], &[]));
    custom(&r, 7);
}
#[test]
fn bad_indices_refused() {
    let m = vm();
    let w = world();
    // step account index past the outer list
    custom(&w.env.run(&m, encode(&[], &[step(1, &[3, 99, 0], token_ix(1))], &[])), 1);
    // program index past the outer list
    custom(&w.env.run(&m, encode(&[], &[step(42, &[], vec![])], &[])), 1);
    // watch index past the outer list
    custom(&w.env.run(&m, encode(&[200], &[], &[])), 1);
    // patch watch slot past the watch list
    let s = [
        step(1, &[3, 4, 0], token_ix(1)),
        step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 1, 0, 0, 0, 0, 0)),
    ];
    custom(&w.env.run(&m, encode(&[4], &s, &[])), 1);
    // from_step not before the patched step
    let s = [
        step(1, &[3, 4, 0], token_ix(1)),
        step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 0, 1, 0, 0, 0, 0)),
    ];
    custom(&w.env.run(&m, encode(&[4], &s, &[])), 1);
    // patch offset past the data
    let s = [
        step(1, &[3, 4, 0], token_ix(1)),
        step(1, &[4, 5, 0], token_ix(0)).patch(p(2, 0, 0, 0, 0, 0, 0)),
    ];
    custom(&w.env.run(&m, encode(&[4], &s, &[])), 0);
    // program index points at a non-executable account
    custom(&w.env.run(&m, encode(&[], &[step(3, &[], vec![])], &[])), 9);
    // watched account that is neither a token account nor System-owned
    custom(&w.env.run(&m, encode(&[2], &[], &[])), 2);
}
#[test]
fn oversized_and_malformed_layout_refused() {
    let m = vm();
    let w = world();
    let one = step(1, &[3, 4, 0], token_ix(1));
    // 9 steps > MAX_STEPS
    custom(&w.env.run(&m, encode(&[], &vec![one.clone(); 9], &[])), 8);
    // 9 watches > MAX_WATCH
    custom(&w.env.run(&m, encode(&[4; 9], &[], &[])), 8);
    // data longer than MAX_STEP_DATA
    custom(&w.env.run(&m, encode(&[], &[step(1, &[], vec![0; 1025])], &[])), 8);
    // 49 accounts > MAX_STEP_ACCOUNTS
    custom(&w.env.run(&m, encode(&[], &[step(1, &[0; 49], vec![])], &[])), 8);
    // 9 patches > MAX_PATCHES
    let mut s = step(1, &[4, 5, 0], token_ix(0));
    for _ in 0..9 {
        s = s.patch(p(1, 0, 0, 0, 0, 0, 0));
    }
    custom(&w.env.run(&m, encode(&[4], &[one.clone(), s], &[])), 8);
    // 17 checks > MAX_CHECKS
    custom(&w.env.run(&m, encode(&[4], &[], &[(0, 0, 0); 17])), 8);
    // trailing byte, truncated payload, unknown fee flag, unknown check kind
    let mut v = encode(&[], &[one.clone()], &[]);
    v.push(0);
    custom(&w.env.run(&m, v.clone()), 0);
    v.truncate(v.len() - 2);
    custom(&w.env.run(&m, v), 0);
    let mut v = encode(&[], &[one.clone()], &[]);
    let fee_flag = v.len() - 2;
    v[fee_flag] = 2;
    custom(&w.env.run(&m, v), 0);
    custom(&w.env.run(&m, encode(&[4], &[], &[(0, 2, 0)])), 0);
    // unknown tag
    let r = w.env.run(&m, vec![1]);
    assert_eq!(
        r.program_result,
        mollusk_svm::result::ProgramResult::Failure(ProgramError::InvalidInstructionData)
    );
}
#[test]
fn negative_delta_refused() {
    let m = vm();
    let w = world();
    // watch A, which only decreases; patching from it must fail.
    let s = [
        step(1, &[3, 4, 0], token_ix(10)),
        step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 0, 0, 0, 0, 0, 0)),
    ];
    custom(&w.env.run(&m, encode(&[3], &s, &[])), 3);
}

/// (6) flags are copied from the outer transaction: a step whose
/// instruction needs a signer the outer transaction did not grant fails in the
/// callee, and nothing moves.
#[test]
fn no_signer_escalation() {
    let m = vm();
    let mut w = world();
    let victim = Pubkey::new_unique();
    let vi = w.env.add(victim, acct(SYSTEM, SOL, vec![]), false, false);
    let va = Pubkey::new_unique();
    let vai = w
        .env
        .add(va, acct(TOKEN, SOL, token_data(&w.mint, &victim, 1_000)), false, true);
    let r = w.env.run(&m, encode(&[], &[step(1, &[vai, 5, vi], token_ix(1_000))], &[]));
    assert!(!r.program_result.is_ok());
    assert_eq!(
        r.raw_result,
        Err(solana_instruction::error::InstructionError::MissingRequiredSignature)
    );
    assert_eq!(amount(&r, &va), 1_000);
    assert_eq!(amount(&r, &w.c), 0);
}
#[test]
fn no_writable_escalation() {
    let m = vm();
    let mut w = world();
    // C passed readonly in the outer transaction: the transfer into it fails.
    w.env.flags[5].1 = false;
    let r = w.env.run(&m, encode(&[], &[step(1, &[3, 5, 0], token_ix(1))], &[]));
    assert!(!r.program_result.is_ok());
    assert_eq!(amount(&r, &w.c), 0);
}

// ------------------------------------------------------------------- fees

struct FeeWorld {
    w: World,
    tp: Pubkey,
    mint: Pubkey,
    fee_ata: Pubkey,
    /// [fee_ata, mint, token program, ata program, system program] indices.
    ix: [u8; 5],
}
/// Adds the fee accounts to a world whose token accounts live under `tp`.
fn fee_world(token_2022: bool, existing_fee_balance: Option<u64>) -> FeeWorld {
    let mut w = world();
    let tp = if token_2022 { TOKEN_2022 } else { TOKEN };
    let mint = w.mint;
    if token_2022 {
        let (k, a) = mollusk_svm_programs_token::token2022::keyed_account();
        w.env.keys[1] = k;
        w.env.accts[1] = a;
        for i in 2..=5 {
            w.env.accts[i].owner = TOKEN_2022;
        }
    }
    let fa = fee_ata(&tp, &mint);
    let fee_acct = match existing_fee_balance {
        None => Account::default(),
        Some(v) => {
            let mut d = token_data(&mint, &recipient(), v);
            if token_2022 {
                // what the ATA program creates for a Token-2022 mint:
                // base + AccountType::Account + ImmutableOwner (empty) TLV.
                d.push(2);
                d.extend_from_slice(&[7, 0, 0, 0]);
            }
            acct(tp, SOL, d)
        }
    };
    let fi = w.env.add(fa, fee_acct, false, true);
    let (ak, aa) = mollusk_svm_programs_token::associated_token::keyed_account();
    let ai = w.env.add(ak, aa, false, false);
    w.env.add(recipient(), Account::default(), false, false);
    FeeWorld {
        w,
        tp,
        mint,
        fee_ata: fa,
        ix: [fi, 2, 1, ai, 7],
    }
}
fn fee_of(f: &FeeWorld, watch: u8) -> [u8; 6] {
    [watch, f.ix[0], f.ix[1], f.ix[2], f.ix[3], f.ix[4]]
}
/// One hop: A -> B for `n`, fee on B's delta.
fn fee_hop(f: &FeeWorld, n: u64) -> Vec<u8> {
    encode(&[4], &[step(1, &[3, 4, 0], token_ix(n)).fee(fee_of(f, 0))], &[])
}

#[test]
fn fee_exact_10bps_creates_missing_ata() {
    let m = vm();
    let f = fee_world(false, None);
    let r = f.w.env.run(&m, fee_hop(&f, 1_000_000));
    ok(&r);
    let ata = get(&r, &f.fee_ata);
    assert_eq!(ata.owner, TOKEN);
    assert_eq!(&ata.data[0..32], f.mint.as_ref());
    assert_eq!(&ata.data[32..64], recipient().as_ref());
    assert_eq!(amount(&r, &f.fee_ata), 1_000);
    assert_eq!(amount(&r, &f.w.b), 999_000);
    assert_eq!(amount(&r, &f.w.a), A_START - 1_000_000);
    // rent for the new ATA came from the user (account 0)
    let user = f.w.env.keys[0];
    assert!(get(&r, &user).lamports < 100 * SOL);
}
#[test]
fn fee_reuses_existing_ata() {
    let m = vm();
    let f = fee_world(false, Some(7));
    let r = f.w.env.run(&m, fee_hop(&f, 2_500_000));
    ok(&r);
    assert_eq!(amount(&r, &f.fee_ata), 7 + 2_500);
    assert_eq!(amount(&r, &f.w.b), 2_497_500);
    // no account creation: the user paid nothing
    let user = f.w.env.keys[0];
    assert_eq!(get(&r, &user).lamports, 100 * SOL);
}
#[test]
fn fee_token_2022_transfer_checked() {
    let m = vm();
    let f = fee_world(true, None);
    assert_eq!(f.tp, TOKEN_2022);
    let r = f.w.env.run(&m, fee_hop(&f, 1_000_000));
    ok(&r);
    let ata = get(&r, &f.fee_ata);
    assert_eq!(ata.owner, TOKEN_2022);
    assert_eq!(amount(&r, &f.fee_ata), 1_000);
    assert_eq!(amount(&r, &f.w.b), 999_000);
    // and an existing Token-2022 ATA is reused
    let f = fee_world(true, Some(1));
    let r = f.w.env.run(&m, fee_hop(&f, 1_000_000));
    ok(&r);
    assert_eq!(amount(&r, &f.fee_ata), 1_001);
}
#[test]
fn fee_zero_on_tiny_delta() {
    let m = vm();
    let f = fee_world(false, None);
    let r = f.w.env.run(&m, fee_hop(&f, 999));
    ok(&r);
    assert_eq!(amount(&r, &f.w.b), 999);
    // nothing created
    assert_eq!(get(&r, &f.fee_ata).owner, SYSTEM);
    assert_eq!(get(&r, &f.fee_ata).lamports, 0);
}
#[test]
fn fee_then_patch_uses_post_fee_delta() {
    let m = vm();
    let f = fee_world(false, None);
    let data = encode(
        &[4],
        &[
            step(1, &[3, 4, 0], token_ix(1_000_000)).fee(fee_of(&f, 0)),
            step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 0, 0, 0, 0, 0, 0)),
        ],
        &[],
    );
    let r = f.w.env.run(&m, data);
    ok(&r);
    assert_eq!(amount(&r, &f.fee_ata), 1_000);
    assert_eq!(amount(&r, &f.w.c), 999_000);
    assert_eq!(amount(&r, &f.w.b), 0);
}
#[test]
fn fee_refusals() {
    let m = vm();
    // wrong fee ATA (a random writable account)
    let mut f = fee_world(false, None);
    let bogus = f.w.env.add(Pubkey::new_unique(), Account::default(), false, true);
    let mut fee = fee_of(&f, 0);
    fee[1] = bogus;
    let d = encode(&[4], &[step(1, &[3, 4, 0], token_ix(1_000_000)).fee(fee)], &[]);
    custom(&f.w.env.run(&m, d), 10);
    // fee ATA derived for a different token program
    let mut f = fee_world(false, None);
    f.w.env.keys[f.ix[0] as usize] = fee_ata(&TOKEN_2022, &f.mint);
    custom(&f.w.env.run(&m, fee_hop(&f, 1_000_000)), 10);
    // wrong mint
    let mut f = fee_world(false, None);
    let other = f.w.env.add(Pubkey::new_unique(), acct(TOKEN, SOL, mint_data(6)), false, false);
    let mut fee = fee_of(&f, 0);
    fee[2] = other;
    let d = encode(&[4], &[step(1, &[3, 4, 0], token_ix(1_000_000)).fee(fee)], &[]);
    custom(&f.w.env.run(&m, d), 11);
    // wrong ATA program / system program / token program
    let f = fee_world(false, None);
    for (slot, bad) in [(4usize, 1u8), (5, 1), (3, 7)] {
        let mut fee = fee_of(&f, 0);
        fee[slot] = bad;
        let d = encode(&[4], &[step(1, &[3, 4, 0], token_ix(1_000_000)).fee(fee)], &[]);
        custom(&f.w.env.run(&m, d), 12);
    }
    // fee recipient missing from the outer list
    let mut f = fee_world(false, None);
    f.w.env.keys.pop();
    f.w.env.accts.pop();
    f.w.env.flags.pop();
    custom(&f.w.env.run(&m, fee_hop(&f, 1_000_000)), 13);
    // fee index out of range
    let f = fee_world(false, None);
    let mut fee = fee_of(&f, 0);
    fee[1] = 250;
    let d = encode(&[4], &[step(1, &[3, 4, 0], token_ix(1_000_000)).fee(fee)], &[]);
    custom(&f.w.env.run(&m, d), 1);
    // account 0 is not a signer: refused before any fee CPI
    let mut f = fee_world(false, None);
    let payer = f.w.env.keys[0];
    let signer = f.w.env.add(payer, acct(SYSTEM, SOL, vec![]), true, true);
    let _ = signer;
    f.w.env.keys[0] = Pubkey::new_unique();
    f.w.env.flags[0] = (false, true);
    // A and B are owned by `payer`, now at the last index.
    let last = (f.w.env.keys.len() - 1) as u8;
    let d = encode(&[4], &[step(1, &[3, 4, last], token_ix(1_000_000)).fee(fee_of(&f, 0))], &[]);
    custom(&f.w.env.run(&m, d), 14);
}
#[test]
fn fee_skipped_for_native_sol_watch() {
    let m = vm();
    let f = fee_world(false, None);
    let d = encode(
        &[6],
        &[step(7, &[0, 6], system_transfer_ix(10 * SOL)).fee(fee_of(&f, 0))],
        &[],
    );
    let r = f.w.env.run(&m, d);
    ok(&r);
    assert_eq!(get(&r, &f.w.x).lamports, 11 * SOL);
    assert_eq!(get(&r, &f.fee_ata).lamports, 0);
}

// ---------------------------------------------------------------- compute

/// Prints compute units for the README (`cargo test --test compose compute -- --nocapture`).
#[test]
fn compute_report() {
    let m = vm();
    let w = world();
    // bare token transfer, invoked directly
    let accounts: Vec<(Pubkey, Account)> =
        w.env.keys.iter().cloned().zip(w.env.accts.iter().cloned()).collect();
    let direct = m.process_instruction(
        &Instruction {
            program_id: TOKEN,
            accounts: vec![
                AccountMeta::new(w.a, false),
                AccountMeta::new(w.c, false),
                AccountMeta::new_readonly(w.env.keys[0], true),
            ],
            data: token_ix(10_000),
        },
        &accounts,
    );
    ok(&direct);
    let empty = w.env.run(&m, encode(&[], &[], &[]));
    ok(&empty);
    let mut cus = vec![];
    for n in 1..=8usize {
        let mut steps = vec![step(1, &[3, 4, 0], token_ix(10_000))];
        for _ in 1..n {
            steps.push(step(1, &[4, 5, 0], token_ix(0)).patch(p(1, 0, 0, 3, 1, 0, 0)));
        }
        let r = w.env.run(&m, encode(&[4], &steps, &[(0, 0, 0)]));
        ok(&r);
        cus.push(r.compute_units_consumed);
    }
    let f = fee_world(false, None);
    let fee_new = f.w.env.run(&m, fee_hop(&f, 1_000_000));
    ok(&fee_new);
    let f = fee_world(false, Some(0));
    let fee_existing = f.w.env.run(&m, fee_hop(&f, 1_000_000));
    ok(&fee_existing);
    let nofee = f.w.env.run(&m, encode(&[4], &[step(1, &[3, 4, 0], token_ix(1_000_000))], &[]));
    ok(&nofee);
    let f = fee_world(true, None);
    let fee_2022_new = f.w.env.run(&m, fee_hop(&f, 1_000_000));
    ok(&fee_2022_new);
    eprintln!("CU direct SPL transfer: {}", direct.compute_units_consumed);
    eprintln!("CU compose, 0 steps: {}", empty.compute_units_consumed);
    for (i, c) in cus.iter().enumerate() {
        eprintln!("CU compose, {} SPL transfer steps (1 watch, 1 check): {}", i + 1, c);
    }
    eprintln!("CU 1 hop no fee: {}", nofee.compute_units_consumed);
    eprintln!("CU 1 hop + fee, ATA exists: {}", fee_existing.compute_units_consumed);
    eprintln!("CU 1 hop + fee, ATA created: {}", fee_new.compute_units_consumed);
    eprintln!("CU 1 hop + fee, Token-2022, ATA created: {}", fee_2022_new.compute_units_consumed);
}

/// Cross-language vectors: `client/encode.test.ts` asserts the same addresses.
#[test]
fn fee_ata_vectors() {
    let usdc = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").unwrap();
    let pyusd = Pubkey::from_str("2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo").unwrap();
    let wsol = Pubkey::from_str("So11111111111111111111111111111111111111112").unwrap();
    let s = |p: Pubkey| p.to_string();
    assert_eq!(s(fee_ata(&TOKEN, &usdc)), "BgfAes9oQmYS6sa2LefzNwdkAA1mwTq9iDZxVDhaxKDZ");
    assert_eq!(s(fee_ata(&TOKEN_2022, &pyusd)), "2WZ6DPdnxEVWDaWDxgWPKZEBGQ1Rs2fbrDXYTuBRyk9s");
    assert_eq!(s(fee_ata(&TOKEN, &wsol)), "DrfWXM7aEjsYNBCYHWRv57aor2etiuum97HedhcPmPdu");
}
