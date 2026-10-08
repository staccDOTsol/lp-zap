// lp-zap client encoder. Plain TypeScript over Uint8Array, no dependencies
// (erasable syntax only, so Node runs it directly: `node --test client/`).
//
// Two layers:
//   encodeCompose(spec)  -> the `compose` instruction data (byte layout in README.md)
//   assemble(ixs, user, feeMints) -> the outer account list + index-rewritten steps
// plus base58, sha256 and findProgramAddress so fee ATAs can be derived here.

// ------------------------------------------------------------------ constants

export const PROGRAM_ID = "BHYw1FAWPriW9Gh7BG49X4UVe96CDjaxFrFFUtGSQmRx";
export const FEE_RECIPIENT = "331nEBz4i3XjyaUHVyHnpw9xBoW7D6P1qMPnUPd76Mth";
export const FEE_BPS = 10n;
export const TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
export const TOKEN_2022_PROGRAM = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
export const ATA_PROGRAM = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
export const SYSTEM_PROGRAM = "11111111111111111111111111111111";

export const TAG_COMPOSE = 0;
export const MAX_WATCH = 8;
export const MAX_STEPS = 8;
export const MAX_STEP_ACCOUNTS = 48;
export const MAX_PATCHES = 8;
export const MAX_STEP_DATA = 1024;
export const MAX_CHECKS = 16;

export const MODE_DELTA = 0; // value = delta
export const MODE_SCALE = 1; // value = delta * num / den (floor)
export const MODE_SCALE_SUB = 2; // value = saturating(delta * num / den - sub)
export const MODE_MIN = 3; // value = min(delta, num)
export const CHECK_MIN = 0; // final delta >= bound
export const CHECK_MAX = 1; // final delta <= bound

export const ERRORS: Record<number, string> = {
  0: "BadLayout",
  1: "IndexOutOfRange",
  2: "UnsupportedBalanceOwner",
  3: "NegativeDelta",
  4: "PatchOverflow",
  5: "DivideByZero",
  6: "CheckFailed",
  7: "Reentrancy",
  8: "LimitExceeded",
  9: "ProgramNotExecutable",
  10: "FeeAtaMismatch",
  11: "FeeMintMismatch",
  12: "FeeProgramMismatch",
  13: "FeeRecipientMissing",
  14: "UserNotSigner",
};

// ---------------------------------------------------------------- layout types

export interface Patch {
  /** Byte offset of the u64 field inside the step data (offset + 8 <= data length). */
  offset: number;
  /** Watch slot (index into `watch`, not an account index). */
  watch: number;
  /** Delta is measured from the snapshot before this step to the snapshot before the patched step. */
  fromStep: number;
  mode: number;
  num?: number; // u32
  den?: number; // u32
  sub?: bigint; // u64
}

export interface Fee {
  /** Watch slot of the token account whose per-step gain is charged. */
  watch: number;
  /** Outer account indices. */
  feeAta: number;
  mint: number;
  tokenProgram: number;
  ataProgram: number;
  systemProgram: number;
}

export interface Step {
  /** Outer account index of the program to invoke. */
  program: number;
  /** Outer account indices, in the callee's account order. */
  accounts: number[];
  data: Uint8Array;
  patches?: Patch[];
  fee?: Fee;
}

export interface Check {
  watch: number;
  kind: number;
  /** i64: final balance minus balance before step 0. */
  bound: bigint;
}

export interface ComposeSpec {
  /** Outer account indices of watched accounts (token accounts or System-owned wallets). */
  watch: number[];
  steps: Step[];
  checks?: Check[];
}

// ------------------------------------------------------------------- encoding

class Writer {
  private b: number[] = [];
  u8(v: number): void {
    if (!Number.isInteger(v) || v < 0 || v > 0xff) throw new RangeError(`u8 out of range: ${v}`);
    this.b.push(v);
  }
  u16(v: number): void {
    if (!Number.isInteger(v) || v < 0 || v > 0xffff) throw new RangeError(`u16 out of range: ${v}`);
    this.b.push(v & 0xff, v >>> 8);
  }
  u32(v: number): void {
    if (!Number.isInteger(v) || v < 0 || v > 0xffffffff) throw new RangeError(`u32 out of range: ${v}`);
    this.b.push(v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, v >>> 24);
  }
  u64(v: bigint): void {
    if (v < 0n || v > 0xffffffffffffffffn) throw new RangeError(`u64 out of range: ${v}`);
    for (let i = 0n; i < 8n; i++) this.b.push(Number((v >> (8n * i)) & 0xffn));
  }
  i64(v: bigint): void {
    if (v < -(1n << 63n) || v >= 1n << 63n) throw new RangeError(`i64 out of range: ${v}`);
    this.u64(BigInt.asUintN(64, v));
  }
  bytes(d: Uint8Array | number[]): void {
    for (const x of d) this.u8(x);
  }
  done(): Uint8Array {
    return Uint8Array.from(this.b);
  }
}

function limit(n: number, max: number, what: string): void {
  if (n > max) throw new RangeError(`${what}: ${n} > ${max}`);
}

/** Encode `compose` instruction data. Validates the same structure the program does. */
export function encodeCompose(spec: ComposeSpec): Uint8Array {
  const w = new Writer();
  const checks = spec.checks ?? [];
  limit(spec.watch.length, MAX_WATCH, "watch");
  limit(spec.steps.length, MAX_STEPS, "steps");
  limit(checks.length, MAX_CHECKS, "checks");
  w.u8(TAG_COMPOSE);
  w.u8(spec.watch.length);
  w.bytes(spec.watch);
  w.u8(spec.steps.length);
  spec.steps.forEach((s, i) => {
    const patches = s.patches ?? [];
    limit(s.accounts.length, MAX_STEP_ACCOUNTS, `step ${i} accounts`);
    limit(s.data.length, MAX_STEP_DATA, `step ${i} data`);
    limit(patches.length, MAX_PATCHES, `step ${i} patches`);
    w.u8(s.program);
    w.u8(s.accounts.length);
    w.bytes(s.accounts);
    w.u16(s.data.length);
    w.bytes(s.data);
    w.u8(patches.length);
    for (const p of patches) {
      if (p.offset + 8 > s.data.length) throw new RangeError(`step ${i}: patch offset ${p.offset} past data`);
      if (p.watch >= spec.watch.length) throw new RangeError(`step ${i}: patch watch ${p.watch}`);
      if (p.fromStep >= i) throw new RangeError(`step ${i}: fromStep ${p.fromStep} must be < ${i}`);
      if (p.mode > MODE_MIN) throw new RangeError(`step ${i}: mode ${p.mode}`);
      if ((p.mode === MODE_SCALE || p.mode === MODE_SCALE_SUB) && !p.den) throw new RangeError(`step ${i}: den 0`);
      w.u16(p.offset);
      w.u8(p.watch);
      w.u8(p.fromStep);
      w.u8(p.mode);
      w.u32(p.num ?? 0);
      w.u32(p.den ?? 0);
      w.u64(p.sub ?? 0n);
    }
    if (!s.fee) {
      w.u8(0);
    } else {
      const f = s.fee;
      if (f.watch >= spec.watch.length) throw new RangeError(`step ${i}: fee watch ${f.watch}`);
      w.u8(1);
      w.bytes([f.watch, f.feeAta, f.mint, f.tokenProgram, f.ataProgram, f.systemProgram]);
    }
  });
  w.u8(checks.length);
  for (const c of checks) {
    if (c.watch >= spec.watch.length) throw new RangeError(`check watch ${c.watch}`);
    if (c.kind > CHECK_MAX) throw new RangeError(`check kind ${c.kind}`);
    w.u8(c.watch);
    w.u8(c.kind);
    w.i64(c.bound);
  }
  return w.done();
}

/** Fee charged on a per-step gain (floor of 10 bps), as the program computes it. */
export function feeFor(delta: bigint): bigint {
  return (delta * FEE_BPS) / 10_000n;
}

/** Little-endian u64 at `offset` in `data` (in place). */
export function writeU64(data: Uint8Array, offset: number, v: bigint): void {
  for (let i = 0; i < 8; i++) data[offset + i] = Number((v >> BigInt(8 * i)) & 0xffn);
}

// ----------------------------------------------------------------- assembling

export interface AccountMeta {
  pubkey: string; // base58
  isSigner: boolean;
  isWritable: boolean;
}

export interface Instruction {
  programId: string; // base58
  keys: AccountMeta[];
  data: Uint8Array;
}

export interface FeeMint {
  mint: string;
  /** Token program that owns the mint (SPL Token or Token-2022). */
  tokenProgram: string;
}

export interface Assembled {
  /** Outer account list for the `compose` instruction: user first (signer, writable). */
  accounts: AccountMeta[];
  /** One entry per input instruction, indices rewritten against `accounts`. */
  steps: { program: number; accounts: number[]; data: Uint8Array }[];
  /** Outer index of a pubkey (throws if absent). */
  indexOf(pubkey: string): number;
  /** Fee ATA (fee recipient's canonical ATA) per fee mint. */
  feeAtas: Record<string, string>;
  /** `Fee` block for charging the gain of watch slot `watch` in `mint`. */
  fee(watch: number, mint: string): Fee;
}

/**
 * Build the outer account list and index-rewritten steps for `compose`.
 *
 * - `user` is account 0, signer and writable (it pays any fee-ATA rent and is
 *   the fee transfer authority).
 * - Every instruction's program id and accounts are added in first-seen order,
 *   deduplicated by pubkey with signer/writable flags OR-ed together.
 * - For each fee mint: the mint and its token program (readonly), the fee
 *   recipient's ATA (writable), the ATA program and System program (readonly).
 * - The fee recipient wallet is appended readonly (the program looks it up by
 *   key; the ATA program needs it as the ATA owner).
 */
export function assemble(instructions: Instruction[], user: string, feeMints: FeeMint[] = []): Assembled {
  const accounts: AccountMeta[] = [];
  const at = new Map<string, number>();
  const add = (pubkey: string, isSigner: boolean, isWritable: boolean): number => {
    decodeBase58(pubkey, 32); // validate
    const i = at.get(pubkey);
    if (i !== undefined) {
      accounts[i].isSigner ||= isSigner;
      accounts[i].isWritable ||= isWritable;
      return i;
    }
    if (accounts.length === 256) throw new RangeError("more than 256 outer accounts");
    accounts.push({ pubkey, isSigner, isWritable });
    at.set(pubkey, accounts.length - 1);
    return accounts.length - 1;
  };
  add(user, true, true);
  limit(instructions.length, MAX_STEPS, "steps");
  const steps = instructions.map((ix, i) => {
    if (ix.programId === PROGRAM_ID) throw new Error(`step ${i}: lp-zap cannot invoke itself`);
    limit(ix.keys.length, MAX_STEP_ACCOUNTS, `step ${i} accounts`);
    limit(ix.data.length, MAX_STEP_DATA, `step ${i} data`);
    const program = add(ix.programId, false, false);
    const idx = ix.keys.map((k) => add(k.pubkey, k.isSigner, k.isWritable));
    return { program, accounts: idx, data: Uint8Array.from(ix.data) };
  });
  const feeAtas: Record<string, string> = {};
  const feeTokenProgram: Record<string, string> = {};
  if (feeMints.length > 0) {
    for (const f of feeMints) {
      if (f.tokenProgram !== TOKEN_PROGRAM && f.tokenProgram !== TOKEN_2022_PROGRAM) {
        throw new Error(`fee mint ${f.mint}: unsupported token program ${f.tokenProgram}`);
      }
      add(f.mint, false, false);
      add(f.tokenProgram, false, false);
      feeAtas[f.mint] = feeAtaAddress(f.mint, f.tokenProgram);
      feeTokenProgram[f.mint] = f.tokenProgram;
      add(feeAtas[f.mint], false, true);
    }
    add(ATA_PROGRAM, false, false);
    add(SYSTEM_PROGRAM, false, false);
    add(FEE_RECIPIENT, false, false);
  }
  const indexOf = (pubkey: string): number => {
    const i = at.get(pubkey);
    if (i === undefined) throw new Error(`account ${pubkey} not in the outer list`);
    return i;
  };
  return {
    accounts,
    steps,
    indexOf,
    feeAtas,
    fee(watch: number, mint: string): Fee {
      const tp = feeTokenProgram[mint];
      if (!tp) throw new Error(`mint ${mint} was not passed in feeMints`);
      return {
        watch,
        feeAta: indexOf(feeAtas[mint]),
        mint: indexOf(mint),
        tokenProgram: indexOf(tp),
        ataProgram: indexOf(ATA_PROGRAM),
        systemProgram: indexOf(SYSTEM_PROGRAM),
      };
    },
  };
}

/** The fee recipient's canonical associated token account for `mint`. */
export function feeAtaAddress(mint: string, tokenProgram: string): string {
  return associatedTokenAddress(FEE_RECIPIENT, mint, tokenProgram);
}

export function associatedTokenAddress(owner: string, mint: string, tokenProgram: string): string {
  const [a] = findProgramAddress(
    [decodeBase58(owner, 32), decodeBase58(tokenProgram, 32), decodeBase58(mint, 32)],
    decodeBase58(ATA_PROGRAM, 32),
  );
  return encodeBase58(a);
}

// --------------------------------------------------------------------- base58

const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

export function encodeBase58(b: Uint8Array): string {
  let n = 0n;
  for (const x of b) n = (n << 8n) | BigInt(x);
  let s = "";
  while (n > 0n) {
    s = B58[Number(n % 58n)] + s;
    n /= 58n;
  }
  for (const x of b) {
    if (x !== 0) break;
    s = "1" + s;
  }
  return s;
}

export function decodeBase58(s: string, expectLen?: number): Uint8Array {
  let n = 0n;
  for (const c of s) {
    const v = B58.indexOf(c);
    if (v < 0) throw new Error(`invalid base58: ${s}`);
    n = n * 58n + BigInt(v);
  }
  const out: number[] = [];
  while (n > 0n) {
    out.unshift(Number(n & 0xffn));
    n >>= 8n;
  }
  for (const c of s) {
    if (c !== "1") break;
    out.unshift(0);
  }
  if (expectLen !== undefined && out.length !== expectLen) {
    throw new Error(`base58 ${s}: ${out.length} bytes, expected ${expectLen}`);
  }
  return Uint8Array.from(out);
}

// --------------------------------------------------------------------- sha256

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

export function sha256(msg: Uint8Array): Uint8Array {
  const l = msg.length;
  const padded = new Uint8Array(((l + 9 + 63) >> 6) << 6);
  padded.set(msg);
  padded[l] = 0x80;
  const bits = BigInt(l) * 8n;
  for (let i = 0; i < 8; i++) padded[padded.length - 1 - i] = Number((bits >> BigInt(8 * i)) & 0xffn);
  const h = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  const w = new Uint32Array(64);
  const rotr = (x: number, n: number): number => (x >>> n) | (x << (32 - n));
  for (let o = 0; o < padded.length; o += 64) {
    for (let i = 0; i < 16; i++) {
      const j = o + 4 * i;
      w[i] = (padded[j] << 24) | (padded[j + 1] << 16) | (padded[j + 2] << 8) | padded[j + 3];
    }
    for (let i = 16; i < 64; i++) {
      const s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >>> 3);
      const s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
    }
    let [a, b, c, d, e, f, g, hh] = h;
    for (let i = 0; i < 64; i++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + S1 + ch + K[i] + w[i]) | 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) | 0;
      hh = g;
      g = f;
      f = e;
      e = (d + t1) | 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) | 0;
    }
    h[0] += a;
    h[1] += b;
    h[2] += c;
    h[3] += d;
    h[4] += e;
    h[5] += f;
    h[6] += g;
    h[7] += hh;
  }
  const out = new Uint8Array(32);
  for (let i = 0; i < 8; i++) {
    out[4 * i] = h[i] >>> 24;
    out[4 * i + 1] = (h[i] >>> 16) & 0xff;
    out[4 * i + 2] = (h[i] >>> 8) & 0xff;
    out[4 * i + 3] = h[i] & 0xff;
  }
  return out;
}

// -------------------------------------------------------- program addresses

const P = (1n << 255n) - 19n;
const D = (-121665n * modInv(121666n)) % P;
const SQRT_M1 = modPow(2n, (P - 1n) / 4n, P);

function mod(a: bigint): bigint {
  const r = a % P;
  return r < 0n ? r + P : r;
}
function modPow(b: bigint, e: bigint, m: bigint): bigint {
  let r = 1n;
  b %= m;
  while (e > 0n) {
    if (e & 1n) r = (r * b) % m;
    b = (b * b) % m;
    e >>= 1n;
  }
  return r;
}
function modInv(a: bigint): bigint {
  return modPow(((a % P) + P) % P, P - 2n, P);
}

/** True if the 32 bytes decompress to a point on ed25519 (what `create_program_address` rejects). */
export function isOnCurve(b: Uint8Array): boolean {
  const bytes = Uint8Array.from(b);
  bytes[31] &= 0x7f;
  let y = 0n;
  for (let i = 31; i >= 0; i--) y = (y << 8n) | BigInt(bytes[i]);
  if (y >= P) return false;
  const y2 = mod(y * y);
  const u = mod(y2 - 1n);
  const v = mod(D * y2 + 1n);
  // x = u v^3 (u v^7)^((p-5)/8)
  const v3 = mod(v * v * v);
  let x = mod(u * v3 * modPow(mod(u * v3 * v3 * v), (P - 5n) / 8n, P));
  const vx2 = mod(v * x * x);
  if (vx2 === u) return true;
  if (vx2 === mod(-u)) {
    x = mod(x * SQRT_M1);
    return true;
  }
  return false;
}

const PDA_MARKER = new TextEncoder().encode("ProgramDerivedAddress");

export function createProgramAddress(seeds: Uint8Array[], programId: Uint8Array): Uint8Array {
  let len = 0;
  for (const s of seeds) {
    if (s.length > 32) throw new RangeError("seed longer than 32 bytes");
    len += s.length;
  }
  const buf = new Uint8Array(len + 32 + PDA_MARKER.length);
  let o = 0;
  for (const s of seeds) {
    buf.set(s, o);
    o += s.length;
  }
  buf.set(programId, o);
  buf.set(PDA_MARKER, o + 32);
  const h = sha256(buf);
  if (isOnCurve(h)) throw new Error("on curve");
  return h;
}

export function findProgramAddress(seeds: Uint8Array[], programId: Uint8Array): [Uint8Array, number] {
  for (let bump = 255; bump >= 0; bump--) {
    try {
      return [createProgramAddress([...seeds, Uint8Array.of(bump)], programId), bump];
    } catch (e) {
      if ((e as Error).message !== "on curve") throw e;
    }
  }
  throw new Error("no viable bump");
}
