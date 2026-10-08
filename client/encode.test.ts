// node --test client/encode.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import {
  ATA_PROGRAM,
  FEE_RECIPIENT,
  MODE_DELTA,
  MODE_SCALE,
  PROGRAM_ID,
  SYSTEM_PROGRAM,
  TOKEN_2022_PROGRAM,
  TOKEN_PROGRAM,
  assemble,
  decodeBase58,
  encodeBase58,
  encodeCompose,
  feeAtaAddress,
  feeFor,
  sha256,
  type ComposeSpec,
  type Instruction,
} from "./encode.ts";

const here = new URL(".", import.meta.url);
const hex = (b: Uint8Array): string => Buffer.from(b).toString("hex");

/** Same bytes as `FIXTURE` in src/layout.rs. */
const FIXTURE = Uint8Array.from([
  0x00, 0x02, 0x02, 0x05, 0x02,
  // step 0
  0x06, 0x03, 0x01, 0x02, 0x03, 0x09, 0x00, 0x03, 0x10, 0x27, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
  // step 1
  0x06, 0x03, 0x02, 0x04, 0x01, 0x09, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01,
  0x00, 0x00, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
  0x00, 0x00, 0x01, 0x00, 0x07, 0x08, 0x06, 0x09, 0x0a,
  // checks
  0x01, 0x01, 0x00, 0xfb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
]);

/** The description in src/layout.rs, as a spec. */
const FIXTURE_SPEC: ComposeSpec = {
  watch: [2, 5],
  steps: [
    { program: 6, accounts: [1, 2, 3], data: Uint8Array.from([3, 0x10, 0x27, 0, 0, 0, 0, 0, 0]) },
    {
      program: 6,
      accounts: [2, 4, 1],
      data: Uint8Array.from([3, 0, 0, 0, 0, 0, 0, 0, 0]),
      patches: [{ offset: 1, watch: 0, fromStep: 0, mode: MODE_SCALE, num: 3, den: 4, sub: 0n }],
      fee: { watch: 0, feeAta: 7, mint: 8, tokenProgram: 6, ataProgram: 9, systemProgram: 10 },
    },
  ],
  checks: [{ watch: 1, kind: 0, bound: -5n }],
};

const fixtures = JSON.parse(readFileSync(new URL("fixtures/compose-fixtures.json", here), "utf8"));

test("encodeCompose reproduces the Rust FIXTURE", () => {
  assert.equal(hex(encodeCompose(FIXTURE_SPEC)), hex(FIXTURE));
});

test("FIXTURE matches the literal in src/layout.rs", () => {
  const src = readFileSync(new URL("../src/layout.rs", here), "utf8");
  const m = src.match(/pub const FIXTURE: &\[u8\] = &\[([\s\S]*?)\];/);
  assert.ok(m, "FIXTURE literal not found");
  const body = m[1].replace(/\/\/[^\n]*/g, "");
  const bytes = body.match(/0x[0-9a-fA-F]{2}/g)!.map((x) => parseInt(x, 16));
  assert.equal(hex(Uint8Array.from(bytes)), hex(FIXTURE));
});

test("compose-fixtures.json carries the same bytes", () => {
  const f = fixtures.fixtures.find((x: { name: string }) => x.name === "layout");
  assert.equal(f.hex, hex(FIXTURE));
  assert.deepEqual(f.bytes, Array.from(FIXTURE));
  assert.equal(fixtures.programId, PROGRAM_ID);
  assert.equal(fixtures.feeRecipient, FEE_RECIPIENT);
});

test("encoder refuses what the program refuses", () => {
  const one = { program: 1, accounts: [0], data: new Uint8Array(9) };
  assert.throws(() => encodeCompose({ watch: new Array(9).fill(0), steps: [] }));
  assert.throws(() => encodeCompose({ watch: [], steps: new Array(9).fill(one) }));
  assert.throws(() => encodeCompose({ watch: [], steps: [{ ...one, data: new Uint8Array(1025) }] }));
  assert.throws(() => encodeCompose({ watch: [], steps: [{ ...one, accounts: new Array(49).fill(0) }] }));
  const patched = (p: object) => ({
    watch: [0],
    steps: [one, { ...one, patches: [{ offset: 1, watch: 0, fromStep: 0, mode: MODE_DELTA, ...p }] }],
  });
  assert.doesNotThrow(() => encodeCompose(patched({})));
  assert.throws(() => encodeCompose(patched({ offset: 2 })));
  assert.throws(() => encodeCompose(patched({ fromStep: 1 })));
  assert.throws(() => encodeCompose(patched({ watch: 1 })));
  assert.throws(() => encodeCompose(patched({ mode: 4 })));
  assert.throws(() => encodeCompose(patched({ mode: MODE_SCALE, num: 1, den: 0 })));
  assert.throws(() => encodeCompose({ watch: [0], steps: [], checks: [{ watch: 0, kind: 2, bound: 0n }] }));
  assert.throws(() => encodeCompose({ watch: [0], steps: [], checks: [{ watch: 0, kind: 0, bound: 1n << 63n }] }));
});

test("sha256 matches node:crypto", () => {
  for (const n of [0, 1, 55, 56, 63, 64, 65, 119, 200]) {
    const m = Uint8Array.from({ length: n }, (_, i) => (i * 31 + 7) & 0xff);
    assert.equal(hex(sha256(m)), createHash("sha256").update(m).digest("hex"));
  }
});

test("base58 round-trips", () => {
  for (const k of [PROGRAM_ID, FEE_RECIPIENT, SYSTEM_PROGRAM, TOKEN_PROGRAM, ATA_PROGRAM]) {
    assert.equal(encodeBase58(decodeBase58(k, 32)), k);
  }
  assert.equal(hex(decodeBase58(SYSTEM_PROGRAM, 32)), "00".repeat(32));
});

test("fee ATA derivation matches the Rust vectors (tests/compose.rs fee_ata_vectors)", () => {
  for (const v of fixtures.feeAtaVectors) {
    assert.equal(feeAtaAddress(v.mint, v.tokenProgram), v.feeAta);
  }
  assert.equal(
    feeAtaAddress("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v", TOKEN_PROGRAM),
    "BgfAes9oQmYS6sa2LefzNwdkAA1mwTq9iDZxVDhaxKDZ",
  );
  assert.equal(
    feeAtaAddress("2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo", TOKEN_2022_PROGRAM),
    "2WZ6DPdnxEVWDaWDxgWPKZEBGQ1Rs2fbrDXYTuBRyk9s",
  );
});

test("feeFor is floor(10 bps)", () => {
  assert.equal(feeFor(999n), 0n);
  assert.equal(feeFor(1_000n), 1n);
  assert.equal(feeFor(1_000_000n), 1_000n);
});

// Deterministic assemble() golden (also in compose-fixtures.json for the FTL server).
const USER = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";
const A = "4k3Dyjzvzp8eMZWUXbBCjEvwSkkk59S5iCNLY3QrkX6R";
const B = "7xKXtg2CW87d97TXJSDpbD5jBkheTqA83TZRuJosgAsU";
const C = "HN7cABqLq46Es1jh92dQQisAq662SmxELLLsHHe4YWrH";
const USDC = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const transfer = (src: string, dst: string, amount: number): Instruction => ({
  programId: TOKEN_PROGRAM,
  keys: [
    { pubkey: src, isSigner: false, isWritable: true },
    { pubkey: dst, isSigner: false, isWritable: true },
    { pubkey: USER, isSigner: true, isWritable: false },
  ],
  data: Uint8Array.from([3, amount & 0xff, (amount >> 8) & 0xff, 0, 0, 0, 0, 0, 0]),
});

function assembleGolden() {
  const a = assemble([transfer(A, B, 10_000), transfer(B, C, 0)], USER, [{ mint: USDC, tokenProgram: TOKEN_PROGRAM }]);
  const watchB = a.indexOf(B);
  const spec: ComposeSpec = {
    watch: [watchB],
    steps: [
      { ...a.steps[0], fee: a.fee(0, USDC) },
      { ...a.steps[1], patches: [{ offset: 1, watch: 0, fromStep: 0, mode: MODE_DELTA }] },
    ],
    checks: [{ watch: 0, kind: 0, bound: 0n }],
  };
  return { a, spec, data: encodeCompose(spec) };
}

test("assemble: user first, dedupe with OR-ed flags, fee accounts, recipient last", () => {
  const { a, data } = assembleGolden();
  const feeAta = feeAtaAddress(USDC, TOKEN_PROGRAM);
  assert.deepEqual(
    a.accounts.map((x) => [x.pubkey, x.isSigner, x.isWritable]),
    [
      [USER, true, true],
      [TOKEN_PROGRAM, false, false],
      [A, false, true],
      [B, false, true],
      [C, false, true],
      [USDC, false, false],
      [feeAta, false, true],
      [ATA_PROGRAM, false, false],
      [SYSTEM_PROGRAM, false, false],
      [FEE_RECIPIENT, false, false],
    ],
  );
  assert.deepEqual(a.steps[0].accounts, [2, 3, 0]);
  assert.deepEqual(a.steps[1].accounts, [3, 4, 0]);
  assert.equal(a.steps[0].program, 1);
  assert.deepEqual(a.fee(0, USDC), { watch: 0, feeAta: 6, mint: 5, tokenProgram: 1, ataProgram: 7, systemProgram: 8 });
  const g = fixtures.fixtures.find((x: { name: string }) => x.name === "assemble");
  assert.equal(hex(data), g.hex);
  assert.deepEqual(
    a.accounts,
    g.accounts.map((x: { pubkey: string; isSigner: boolean; isWritable: boolean }) => x),
  );
});

test("assemble: flags OR-ed across steps; self-invocation refused; unknown fee token program refused", () => {
  const ro: Instruction = { programId: TOKEN_PROGRAM, keys: [{ pubkey: A, isSigner: false, isWritable: false }], data: new Uint8Array(0) };
  const rw: Instruction = { programId: TOKEN_PROGRAM, keys: [{ pubkey: A, isSigner: true, isWritable: true }], data: new Uint8Array(0) };
  const a = assemble([ro, rw], USER);
  assert.deepEqual(a.accounts[a.indexOf(A)], { pubkey: A, isSigner: true, isWritable: true });
  assert.equal(a.accounts.length, 3); // no fee mints: no fee accounts, no recipient
  assert.throws(() => assemble([{ ...ro, programId: PROGRAM_ID }], USER));
  assert.throws(() => assemble([ro], USER, [{ mint: USDC, tokenProgram: SYSTEM_PROGRAM }]));
  assert.throws(() => assemble([ro], "not-base58-0OIl"));
});
