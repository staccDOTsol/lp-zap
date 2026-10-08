# lp-zap

A venue-agnostic, stateless, PDA-less transaction composer for Solana, written
with [Pinocchio](https://github.com/anza-xyz/pinocchio) 0.11.2 (no Anchor, no
`solana-program`, no Borsh).

One instruction, `compose`, CPIs a client-supplied sequence of opaque
instructions ("steps"). Before every step, and once after the last one, it
snapshots the balances of designated ("watched") accounts. It then patches
`u64` fields of later steps from the real on-chain deltas. This lets one
signature cover, for example, swap then deposit, with the deposit sized from
what the swap actually produced: no estimates and no dust. Final min/max checks
on the net deltas make the whole transaction revert if the result is worse than
the user accepted. An optional 0.1% fee is taken in kind on the output of each
hop.

The program knows nothing about any venue. It holds no funds, owns no
accounts, derives no authority and never signs.

## Mainnet deployment

| | |
| --- | --- |
| Program id | `BHYw1FAWPriW9Gh7BG49X4UVe96CDjaxFrFFUtGSQmRx` |
| Deployment transaction | https://solscan.io/tx/2F3dGfz6G9znv4oVdk33T2qHvoXof6WeL8qArBDiufFoAu3MwzVP7CRa2iLCdTXMwJtLJ1uzHSAn463JzLpQk16b |
| Binary | `target/deploy/lp_zap.so`, 37,288 bytes, SHA-256 `13230d363f50628a4c5e4e05dca6bd9f0101ed22f9d7b924a568b8918ec33901` |
| Upgrade authority | `331nEBz4i3XjyaUHVyHnpw9xBoW7D6P1qMPnUPd76Mth` |
| Fee recipient | `331nEBz4i3XjyaUHVyHnpw9xBoW7D6P1qMPnUPd76Mth`, 10 bps per fee'd hop, in kind |

Deployed 2026-10-08 from commit `befdcfe`.
### First production receipt

2026-10-08, slot 454461597: a two-hop swap composed by the liquidityxyz.fun server, 0.005 USDC → SOL (Meteora DLMM `HRYEjwdo…`) → BORDR (Meteora DLMM `5XQaJzF6…`), hop 2's `amount_in` patched on chain from hop 1's real WSOL delta, 10 bps per hop in kind, one signature, 111,193 compute units, 1,112 bytes.

```text
https://solscan.io/tx/4yosNpvSLK2Sn6SmJbh6M7BxnxYKiBCg7XZMoM2RfHBr2Sn7yxDpvtSHaEiXArC2UtzfZTCCLqX4hLa9fMyME8k2
```
 First production user is the liquidityxyz.fun server ([staccDOTsol/ftl](https://github.com/staccDOTsol/ftl), `server/src/solana/compose.ts`).

## Program id

`BHYw1FAWPriW9Gh7BG49X4UVe96CDjaxFrFFUtGSQmRx` (`declare_id!` in `src/lib.rs`).
The program does not use its own id for authority. It compares the runtime-supplied
`program_id` only to refuse re-entrancy.

`cargo build-sbf` writes `target/deploy/lp_zap-keypair.json` if none exists.
That file is auto-generated and is **not** the reserved id's keypair unless the
deployer put it there. Deploy with the reserved keypair.

## Build and test

```sh
export PATH=~/.local/share/solana/install/active_release/bin:$PATH
cargo build-sbf                  # -> target/deploy/lp_zap.so
cargo test                       # host unit tests + Mollusk tests (need the .so)
cargo test --test compose compute_report -- --nocapture   # CU table below
node --test client/encode.test.ts
```

`Cargo.lock` is committed because `mollusk-svm =0.15.1` needs the Agave 4.2.x
runtime crates (4.3.0 does not compile against it).

| | |
|---|---|
| `lp_zap.so` | **37,288 bytes** (platform-tools v1.57, rustc 1.95.0, opt-level z, fat LTO) |
| ProgramData account (45 + size = 37,333 B) | `solana rent 37333` = **0.19030188 SOL** (locked) |
| Program account (36 B) | 0.00083312 SOL (locked) |
| Deploy buffer (37 + size = 37,325 B) | 0.19026124 SOL, needed during deploy and returned when the buffer is consumed |

Plan for about 0.382 SOL plus transaction fees to be available at deploy time,
with about 0.191 SOL staying locked. A larger `--max-len` grows the locked rent
proportionally.

## Instruction

`compose` is the only instruction. Its accounts are the "outer account list",
referenced everywhere below by `u8` index. **Account 0 is the user.** It must
sign whenever a fee is actually settled, and it pays fee-ATA rent and is the fee
transfer authority.

### Byte layout (all integers little-endian)

```
compose := tag:u8 (= 0)
           watch_count:u8 (<= 8)    watch[watch_count]:u8        outer account indices
           step_count:u8  (<= 8)    step[step_count]
           check_count:u8 (<= 16)   check[check_count]
           <end of data>                                         trailing bytes are refused

step    := program:u8                                            outer index of the program to invoke
           account_count:u8 (<= 48) accounts[account_count]:u8   outer indices, callee order
           data_len:u16 (<= 1024)   data[data_len]
           patch_count:u8 (<= 8)    patch[patch_count]
           fee_flag:u8 (0 | 1)      fee (present iff fee_flag == 1)

patch   := offset:u16  watch:u8  from_step:u8  mode:u8  num:u32  den:u32  sub:u64      (21 bytes)
fee     := watch:u8  fee_ata:u8  mint:u8  token_program:u8  ata_program:u8  system_program:u8   (6 bytes)
check   := watch:u8  kind:u8  bound:i64                                                (10 bytes)
```

Structural rules, all checked before anything executes:

* `patch.offset + 8 <= data_len`
* `patch.watch < watch_count`
* `patch.from_step < (index of this step)`
* `mode` must be 0 to 3; `den != 0` for modes 1 and 2
* `fee.watch < watch_count`
* `check.watch < watch_count`
* `check.kind` must be 0 or 1
* every account, program and fee index must be `< number of outer accounts`
* a step's program may not be lp-zap itself, and the program account must be executable

### Execution

```
for s in 0..step_count:
    snap[s] = balances of all watched accounts
    data'   = step.data with each patch's u64 written at patch.offset
    CPI step.program with data' and step.accounts (flags copied from the outer list)
    if step has a fee: settle it (see Fee)          # before the next snapshot
snap[n] = balances
for each check: d = snap[n][w] - snap[0][w];  kind 0: require d >= bound;  kind 1: require d <= bound
```

A patch's **delta** is `snap[s][watch] - snap[from_step][watch]`. That is the
cumulative change of the watched account over steps `from_step ..= s-1`, net of
any fees taken in those steps. Negative deltas are refused.

| mode | value written |
|---|---|
| 0 | `delta` |
| 1 | `floor(delta * num / den)`; refused if it exceeds `u64` |
| 2 | `max(floor(delta * num / den) - sub, 0)`; refused if the scaled value exceeds `u64` |
| 3 | `min(delta, num)` (the cap is the u32 `num`) |

Balances: a watched account owned by SPL Token or Token-2022 must be a token
account (exactly 165 bytes, or longer with account-type byte `2` at offset 165).
Its balance is `amount` at offset 64. A System-owned account's balance is its
lamports. Any other owner is refused.

Watching an account that does not exist yet reads 0 lamports, because it is
System-owned. Once it is created as a token account it reads `amount`. Patches
should therefore measure such accounts from the step after the one that creates
them, and final checks should not target them. Final checks compare against
`snap[0]`, so lamports someone sent to that address beforehand would skew the
result.

**u128 fields** such as DAMM v2 `liquidity_delta` are patched in their low 8
bytes (`offset` = field offset), and the client zeroes the high 8 bytes. See
[Open issue: u128 liquidity](#open-issue-u128-liquidity).

### Error codes (`ProgramError::Custom(n)`)

| n | name | meaning |
|---|---|---|
| 0 | BadLayout | truncated data, trailing bytes, unknown mode/check kind/fee flag, or patch offset past data |
| 1 | IndexOutOfRange | account, program, watch, fee index or `from_step` out of range |
| 2 | UnsupportedBalanceOwner | watched account is not a token account or System-owned |
| 3 | NegativeDelta | patch or fee delta < 0 |
| 4 | PatchOverflow | scaled value does not fit in u64 |
| 5 | DivideByZero | `den == 0` in mode 1 or 2 |
| 6 | CheckFailed | a final check did not hold |
| 7 | Reentrancy | a step targets lp-zap itself |
| 8 | LimitExceeded | over 8 watches, 8 steps, 48 accounts per step, 8 patches per step, 1024 data bytes per step, or 16 checks |
| 9 | ProgramNotExecutable | `program` index is not an executable account |
| 10 | FeeAtaMismatch | `fee_ata` is not the fee recipient's canonical ATA for (token program, mint) |
| 11 | FeeMintMismatch | `mint` is not the watched token account's mint (or is not a mint-sized account, Token-2022) |
| 12 | FeeProgramMismatch | token program is not the watched account's owner, or the ATA/System program index is wrong |
| 13 | FeeRecipientMissing | the fee recipient wallet is not in the outer account list |
| 14 | UserNotSigner | account 0 did not sign and a non-zero fee is due |

A tag other than 0 returns `InvalidInstructionData`. Errors raised by a step's
own program propagate unchanged.

## Fee

* **10 bps in kind** per step that carries a fee block:
  `fee = floor((post - snap[s][watch]) * 10 / 10_000)`. This is computed on that
  step's own gain in the watched token account.
* Recipient wallet (constant): `331nEBz4i3XjyaUHVyHnpw9xBoW7D6P1qMPnUPd76Mth`.
  The fee goes to its canonical ATA, `find_program_address([recipient, token_program, mint], ATA program)`,
  which is verified on-chain.
* A fee of zero (gain < 1000 base units) is skipped: no CPI, no ATA creation, and the user need not sign.
* A System-owned (native SOL) watch is skipped: no fee.
* Settlement: `CreateIdempotent` (payer = account 0, owner = recipient) creates
  the ATA if missing and reuses it if present. Then `Transfer` (SPL Token) or
  `TransferChecked` (Token-2022, decimals read from mint offset 44) moves the fee
  from the watched account, with account 0 as authority.
* Settled before the next snapshot, so later patches see the post-fee delta.

Outer accounts a fee needs:

| account | flags |
|---|---|
| account 0, the user | signer, writable |
| fee recipient's ATA for the mint | writable |
| mint | readonly |
| token program owning the mint | readonly |
| ATA program `ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL` | readonly |
| System program | readonly |
| fee recipient wallet | readonly, found by key anywhere in the list |

`client/encode.ts` `assemble()` appends these.

The fee's presence is decided by whoever builds the transaction (FTL). The
program verifies a fee block when one is present. It does not force one.

## Security model

* **No escalation.** Each CPI account's writable/signer flags are copied from
  the outer `AccountView`. A step can never sign for, or write to, anything the
  user's transaction did not already grant. The runtime enforces the same rule
  independently. Mollusk tests cover a missing signer
  (`MissingRequiredSignature` from the callee) and a readonly destination.
* **No authority of its own.** There are no PDAs, no `invoke_signed`, no stored
  state, and no owned accounts. lp-zap has nothing to steal and nothing it can
  sign for.
* **No re-entrancy.** A step that targets lp-zap's own id is refused (7).
  Indirect re-entry (lp-zap → X → lp-zap) is refused by the runtime.
* **Validate first, then act.** Pass 1 parses the whole payload with
  bounds-checked reads and enforces every limit and strict end-of-data. Pass 2
  binds every index to the account list and checks executability and
  re-entrancy. Only then does any CPI run. Any failure, including a failed
  final check, aborts the whole transaction.
* **Arithmetic.** Deltas are computed in i128 and scaling in u128. Negative
  deltas, `u64` overflow and division by zero are explicit errors. Overflow
  checks are also enabled in the release profile.
* **Memory.** `no_allocator!`. The working buffers (patched data copy, ≤ 9×8
  snapshots, CPI tables) live at the start of the otherwise-unused heap region.
  `invoke_unchecked` is used for steps because no account data is borrowed at
  CPI time: snapshot and fee borrows end before each CPI.
* **Trust boundary.** lp-zap does not know what a step does. A composed
  transaction is exactly as safe as the instructions inside it, signed directly.
  What lp-zap adds is atomicity, sizing from real deltas, and final min/max
  checks. Wallet simulation and review of the steps remain the user's protection.
* **Native SOL watches** see every lamport movement of that wallet inside the
  transaction, including rent paid for accounts created by steps or by fee-ATA
  creation (about 0.00204 SOL the first time a fee ATA is created). Size final
  SOL checks with that margin.

## Compute

Measured on Mollusk with `tests/compose.rs` `compute_report`. The token program
is the one bundled with `mollusk-svm-programs-token` 0.15.1; a bare transfer
there costs 76 CU.

| scenario | CU |
|---|---|
| `compose` with 0 steps | 337 |
| 1 SPL transfer step (1 watch, 1 check) | 2,817 |
| each additional patched transfer step | +2,543 (about 2,467 of lp-zap/CPI overhead + the callee) |
| 8 steps | 20,618 |
| 1 hop, no fee | 2,678 |
| 1 hop + fee, ATA exists | 12,888 (fee about +10.2k, mostly `find_program_address` + 2 CPIs) |
| 1 hop + fee, ATA created | 21,969 |
| 1 hop + fee, Token-2022, ATA created | 27,473 |

Budget on mainnet: the sum of the venue instructions' own CU, plus about 2.5k
per step, plus about 10–20k per fee hop.

## FTL recipes

Indices below are watch slots (`w0`, `w1`) and step numbers. The client builds
every venue instruction with its normal SDK, using placeholder amounts for
patched fields, then runs it through `assemble()` and adds patches, fee blocks
and checks.

**Zap-in (SOL → LP).** Watches: `w0` = user's token-X account (swap output),
`w1` = user's SOL wallet (or the wSOL account used for input).

1. Step 0: swap SOL → X (fixed `amount_in`, `min_out` from the quote). Fee
   `{watch w0, mint X}`.
2. Step 1: deposit.
   * The token-X max/amount field is patched with mode 0 from `w0`, `from_step` 0
     (the post-fee delta).
   * The liquidity field is patched with mode 1 from `w0`, `num/den` ≈
     liquidity per unit of X computed from pool state. For DAMM v2, see
     [Open issue: u128 liquidity](#open-issue-u128-liquidity).
   * The SOL-side max stays fixed (the remaining SOL budget).
3. Check: `w1` kind 0 (min) `bound = -sol_budget`, so SOL spent ≤ budget.
   Optionally add a min-liquidity check if the position is a token account.

**Zap-out (LP → SOL).**

1. Step 0: remove liquidity (or `remove_all_liquidity`) with its own thresholds.
2. Step 1: swap X → SOL. `amount_in` is patched with mode 0 from `w0`
   (token-X account), `from_step` 0. Fee `{watch w0', mint SOL/wSOL}` on the
   output account if it is a token account.
3. Check: SOL/wSOL watch kind 0, `bound = min_sol_out`.

**Multi-hop swap (A → B → … → Z).** Watch the output token account of each hop.

* Hop k's `amount_in` = delta of hop k-1's output (mode 0, `watch` = that
  output, `from_step = k-1`).
* Intermediate `min_out` = 0, or a per-hop floor.
* A fee on each hop's output (10 bps per hop). Because the fee settles before
  the next snapshot, hop k swaps exactly the post-fee amount.
* One final check on the last output: kind 0, `bound = min_out`.
* Recursive DBC chains are just more hops, all within the 8-step limit.

### Field offsets (instruction data, after the 8-byte Anchor discriminator)

These offsets were extracted from the venue IDLs. **The FTL server proves each
offset at runtime** by building the instruction with a sentinel amount and
locating it in the built bytes before emitting a patch. Treat this table as a
reference, not a source of truth.

| venue / instruction | field | offset | type |
|---|---|---|---|
| DLMM `swap` / `swap2` | `amount_in` | 8 | u64 |
| | `min_amount_out` | 16 | u64 |
| DLMM `add_liquidity_by_strategy` / `add_liquidity_by_strategy2` | `amount_x` | 8 | u64 |
| | `amount_y` | 16 | u64 |
| DAMM v2 (cp-amm) `swap` | `amount_in` | 8 | u64 |
| | `minimum_amount_out` | 16 | u64 |
| DAMM v2 `swap2` | `amount_0` | 8 | u64 |
| | `amount_1` | 16 | u64 |
| DAMM v2 `add_liquidity` / `remove_liquidity` | `liquidity_delta` | 8 | u128 (agreed: patch low 8 bytes, zero 16..24; see open issue) |
| | `token_a_amount_threshold` | 24 | u64 |
| | `token_b_amount_threshold` | 32 | u64 |
| DAMM v2 `remove_all_liquidity` | `token_a_amount_threshold` | 8 | u64 |
| | `token_b_amount_threshold` | 16 | u64 |

### Open issue: u128 liquidity

The agreed convention is to patch a u128 field in its low 8 bytes and have the
client zero the high 8 bytes. That caps a patched `liquidity_delta` at
`2^64 - 1`, and **real DAMM v2 liquidity is far above that cap.**

DAMM v2 liquidity is Q64-scaled. In `@meteora-ag/cp-amm-sdk`,
`getLiquidityDeltaFromAmountB = (amountB << 128) / (sqrtPrice - sqrtMin)`.
That means `liquidity_delta` is the textbook `L` times `2^64`, which is about
`2^64 × sqrt(x·y)` for a full-range position. For example, with a 1 SOL side
at a price of 1e-3 lamports per unit, `liquidity_delta` = 583337271166399672595555247978
(99 bits). That figure was computed with the SDK's own constants. Even a
1-lamport side would not fit in 64 bits.

Under the current convention, every realistic DAMM v2 `add_liquidity` patch
fails with `PatchOverflow` (4). The transaction reverts safely, but the zap
cannot execute.

There is a resolution that needs no program change, and it is pending the
lead's approval. Patch the **high** 8 bytes instead: `offset` = field + 8, i.e.
16 for `liquidity_delta` at 8, with mode 1, where `num/den` is the textbook `L`
per unit of the patched token. Zero the low 8 bytes. The written
`liquidity_delta` is then `floor(delta × num / den) × 2^64`. For the example
above that is 31622776834 × 2^64, which rounds down by less than 1 part in 3e10.
The deposit's token thresholds still bound what is spent.

The other fix would be a u128 write mode in the program, which is a layout
change. DLMM deposits (`amount_x` / `amount_y` are u64) and all swap paths are
unaffected.

## Client

`client/encode.ts` is dependency-free TypeScript over `Uint8Array`. It uses
erasable syntax only, so Node runs it directly.

* `encodeCompose(spec)` produces the instruction data above. It applies the
  same structural validation as the program.
* `assemble(instructions, user, feeMints)` returns:
  * the outer account list. The user comes first, as signer and writable. Then
    each program id and account follows in first-seen order, deduplicated with
    flags OR-ed. Then, for each fee mint: the mint, its token program, and the
    fee ATA (writable). Then the ATA program and System program, and finally the
    fee recipient (readonly).
  * the index-rewritten steps, plus `indexOf`, `feeAtas` and `fee(watch, mint)`.
* base58, sha256 and `findProgramAddress` are included, so fee ATAs are derived
  locally. They are cross-checked against Rust vectors.

`client/fixtures/compose-fixtures.json` holds the shared layout fixture (the
same bytes as `FIXTURE` in `src/layout.rs`), an `assemble()` golden, and fee-ATA
vectors, each with the description that produced it.
