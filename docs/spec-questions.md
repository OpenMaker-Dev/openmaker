# Spec questions

> **Historical.** New spec questions are GitHub issues labeled `needs-re`
> ([AGENTS.md](../AGENTS.md#6-issues)); the answer goes into the spec in
> `docs/re/` and the issue links it. The questions below were answered that
> way before issues were used.

The clean-room boundary between implementation and reverse engineering runs
through this file. The OpenMaker implementation reads only `docs/re/` (plus
observed network traffic and the live lab server). When a spec is missing,
ambiguous, or contradicted by observed traffic, the implementer adds a
question here; the reverse-engineering side answers by updating the specs in
`docs/re/` and recording the answer below.

Format: `### Qn - <short title>` with **Asked by**, **Needed for**, the
question, and later **Answer** (with the spec section that now covers it).

## Open

(none yet)

## Answered

### Q2 - op80 Login obscuring key + nonce (so a clean client auths first try)

**Asked by** protocol/login implementer.
**Needed for** building the `op80` Login request so the server accepts the
credentials on the first attempt (account-lockout makes trial-and-error costly).

What is the exact credential-obscuring keystream, the `PasswordHash` that keys it,
the `~file[0x3ac] + file[0xa64]` nonce (and where the server gets its matching
`file[0xa64]`), and the Login request field order / which fields are obscured?

**Answer** (network-protocol.md sections 4.3 and 5.4, **validated end-to-end**
against capture `2026-10-27-s03-direct/conn-000`):

- **Keystream** per `PushEncrypt` field = `PBKDF2-HMAC-SHA1(key = PasswordHash
  (20 bytes), salt = connKey (16 bytes), iter = 1, dklen = field length)`, XORed
  over the SCSU+XOR0x5A field bytes. **Per-field restarted** (each field restarts
  PBKDF2 at dklen = its own length, so all fields reuse the same keystream prefix -
  proven by a 6-byte shared obscured prefix across two capture fields). The salt is
  the Connect-reply connKey (`conn+0x284` Linux / `conn+0x1f4` Mac), confirmed from
  `RPO_i::Connect`'s key-LCG storing each byte into both the reply and the
  connection's `+0x284` 16-byte slot.
- **PasswordHash** = `PBKDF2-HMAC-SHA1(pass = peppermix(NumToString(nonce)),
  salt = connKey[0:10], iter = 1, dklen = 20)`. `peppermix`: per UTF-16 code unit
  `i`, emit `BE16(cu[i] XOR ((pepper[(2i) mod 30]<<8) | pepper[(2i+1) mod 30]))`.
  Pepper = 30 bytes at `.rodata` nm 0x2e6d6d0 = `"FileMaker, Inc."` + NUL +
  `ca 3c 23 90 a5 5b 71 74 b6 1d f5 b2 6d 00`. `HashDecompose` is identity for the
  ASCII decimal string; SHA1 -> 20 bytes.
- **Nonce** = `(~file[0x3ac] + file[0xa64]) & 0xFFFFFFFF`, formatted as an unsigned
  decimal string. `file[0x3ac]` = Open-reply nonce (5.6.4 pop #3, server-supplied).
  `file[0xa64]` = **`mInternalCreation`, a file-intrinsic u32** (HBAM key
  `Concat(KStatus, KModified)`, big-endian), NOT login-time `time()` and NOT sent
  on the wire; the server reads it from its hosted file and the client reads it from
  the file blocks it downloads (`op90`, before Login). This resolves the
  "how does the server know the client's half" ambiguity: neither side transmits it;
  both read the same persisted value from the file.
- **Login field order**: env strings (system drive/prefs/docs/desktop/FileMaker
  paths) `Push` plaintext, then the credential block `PushEncrypt` (privilege
  extension, account name, user name, password - **the only obscured fields**),
  then more env strings (system version, printer, app language, file path, remote
  path) plaintext, then scalars (`u16 serverCPU`, `bool hasPrivExt`, `u16 version=8`,
  `loginType` byte, `u32 file-serial`). Reply per 5.6.5 (`errorCode` u16 tail: 0 ok,
  0xd0 change-required, 0x20d0 external).
- **Validation**: reimplementing this chain and XOR-ing the real `op80` credential
  fields yields valid SCSU (account field fully printable; PBKDF2 avalanche pins
  every input exactly). Confirmed both by calling the shipped `libSupport.so`
  crypto and by a from-scratch reimplementation. No credential/key bytes recorded.

### Q1 - Perform reply byte layout (how to parse server responses)

**Asked by** network/transport implementer.
**Needed for** parsing every `Perform` reply: Open nonce, Login error code,
Download/QueryProgram payloads.

How is the `Perform` reply marshalled - where is the boolean return, the inout
sequence, and the out sequence; which one carries the response; is the response a
20-byte-header RCTransaction like the request; and how are success/errors
encoded?

**Answer** (network-protocol.md section 5.6, pinned against capture
`2026-10-27-s03-direct/conn-000` + DWARF):

- `Perform`'s wire return is **`void`** (not boolean). The reply body (CDR LE) is
  exactly two `sequence<octet>` in IDL order: inout `parms` then out
  `backchannel`. No return octet.
- The **inout `parms`** sequence carries the response. The **out `backchannel`**
  sequence is the 0xFE server-CPU/idle piggyback (section 6), ignorable by app
  logic.
- The response is **NOT** a 20-byte-header RCTransaction. The server builds it
  with `ResetPerformPush` (content cursor = 0, no header), so the inout sequence
  is a **pure LIFO result stack**. The client pops in reverse of the server's
  push order, so the **status/error `u16` is the last field = the tail of the
  sequence, popped first** (0 = success for every op).
- arg3 is `maxreturn` (max reply size, `0x14..0x200000`), not the request length.
- Large replies use GIOP 1.2 fragmentation (reassemble Reply + Fragment messages
  by `request_id`).
- Per-op pops: op78 Open yields `u16 status`, `serverMsg`, then the per-file
  nonce `file[0x3ac]` (BE `u32`, pop #3), plus more (5.6.4); op80 Login yields
  `u16 errorCode` (tail; 0 ok, 0xd0 change-required, 0x20d0 external-auth),
  `resultKey`, `version` (5.6.5); op90 Download = HBAM packed-slot bytes +
  trailing `u16 lockError` (5.6.6); op86 QueryProgram = HBAM packed-slot records +
  trailing `u16 status`, `03 02 <u16>` error-slot on failure (5.6.7).
