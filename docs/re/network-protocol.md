# FileMaker Pro client/server network protocol

How FileMaker Pro 26 talks to FileMaker Server 26 over port 5003, to the depth
needed for a clean-room client to open a hosted file. Covers transport, TLS,
the CORBA/GIOP framing, the FileMaker request envelope, the opcode catalog, the
message layouts for connect/list-hosts/login/open, data encodings, the
redirect/reconnect behavior, and the experiments that still need live traffic.

Observations, inferences, and unknowns are marked. Confidence is High / Medium /
Low. Evidence is cited as symbol names, binary addresses (Mac x86_64 `DBEngine`
slice unless noted `Linux`), struct field names/offsets from FMS 26.0.3 Linux
DWARF, string offsets, and capture paths under `re/captures/`. Client-to-server
capture bodies are sensitive (license key, MAC addresses, credentials); this doc
describes those fields by position, length, and type only.

## 0. Layer stack

```
TCP 5003
  TLS 1.3 (mutual: server requires a client certificate)
    CORBA GIOP 1.2, little-endian, over omniORB 4.3.4
      object key \xffomniINSPOA\xffFMPOA\x00FMRPO, interface RCFMP12 (IDL:RCFMP12:1.0)
      6 operations: Connect, Reconnect, Disconnect, Perform, Tickle, Notify
        operation bodies carry a CORBA sequence<octet> (OctetSeq)
          = the FileMaker "RCTransaction": 20-byte header + LIFO body
            fields: big-endian ints; strings SCSU-compressed then XOR 0x5A;
                    credential fields additionally PBKDF2/AES-obscured
              opcode (header byte 0) selects a transaction class (Login, Open, ...)
```

One sentence: FileMaker tunnels a bespoke binary RPC (the `RCTransaction`) inside
a CORBA `sequence<octet>`, carried by omniORB GIOP over mutually-authenticated
TLS. CORBA is a thin envelope (confirms README F3); the real protocol is the
`RCTransaction` opcode set.

## 1. Transport and TLS

Observations (High unless noted):

- TCP, default port 5003. `Draco::RCNetworkStack::sServerPort` initializes to
  `5003` (DBEngine `.data` 0x257fcec; `GetRemoteServerPort`/`SetRemoteServerPort`
  at 0x1d4478c / 0x1d5682c).
- TLS 1.3 only. Live probe: 5003 completes a TLS 1.3 handshake
  (`TLS_AES_256_GCM_SHA384`), refuses TLS 1.2 (`handshake failure`) and lower.
  Client cipher config (DBEngine `Draco::OpenSSL::kDefaultClientCiphers` 0x2584040
  and `kDefaultClientCipherSuites` 0x2584048) is copied from the server defaults:
  TLS 1.3 suites `TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:
  TLS_AES_128_GCM_SHA256`, plus an ECDHE/AES-GCM TLS 1.2 list with
  `!aNULL:!eNULL:!EXPORT:!DES:!RC4:!MD5:!PSK`. Set with `SSL_CTX_set_cipher_list`
  + `SSL_CTX_set_ciphersuites` in `RCNetworkStack::ConstructORB`.
- Mutual TLS. Port 5003 requires a client certificate: with none, the server
  sends TLS 1.3 alert 116 `certificate_required` right after the client
  Finished (live probe, and relay captures `re/captures/*s01*`). The
  CertificateRequest acceptable-CA list is `FileMaker Root Authority` (2013),
  `Claris Root Authority` (2020), then ~25 public roots (ISRG/Let's Encrypt,
  DigiCert/GeoTrust/thawte/VeriSign/Symantec, Entrust, COMODO/Sectigo, Go Daddy,
  AddTrust). A throwaway self-signed client cert is accepted (lead's
  `tools/probe/giop_probe.py`), so the server requests but does not hard-verify
  the client chain for a plain client. Inference (Medium): the client cert gates
  reachability/DoS, not identity; account identity is established later by Login.
- The client presents SNI = the dialed hostname (capture `server_hostname`:
  the capture relay's DNS name), not the server's canonical name. No ALPN is
  negotiated (README). HTTP/2 is not used on 5003.
- Server certificate verification (client side): `Draco::OpenSSL::VerifyCert`
  (0x158d12a) via `CertVerifyCallback` (0x158d068), installed with
  `SSL_CTX_set_cert_verify_callback` and the omni `sslContext`
  (`update_verify_mode`). It runs `X509_verify_cert`, then checks expiry
  (`Certificate::IsExpired` -> err 0x5096) and hostname
  (`Certificate::VerifyHostName` -> err 0x664 on mismatch), mapping OpenSSL
  errors to FileMaker codes 0x5096/0x5097/0x5098.
- "Connect anyway". On verification failure the client can prompt and continue:
  the `IDD_CERTIFICATE_ALERT_DLOG` dialog ("An SSL certificate encrypts data ...
  The certificate for this host is invalid and it can't be verified. Do you want
  to connect anyway?", with "Always permit connection to this host"). A
  per-host override map lives in `sCertVerificationCallback` /
  `sConnectionMap` keyed by host name. `RCNetworkStack::ShouldVerifySSL`
  (0x1d4e240) returns false for AppType 3, so one application type skips
  verification entirely (Inference Medium: a server/engine context, not the
  interactive client).
- Client certificate for the engine side: `FMSSLContext::SetContext(ca, key,
  keypw)` (0x1542ff0) takes a CA file plus key/keypassword. For the interactive
  client, `DBEngineController::SetSSLContext` (0x194b1b4) passes the regenerated
  root PEM bundle as CA and empty key/keypw, i.e. no client key is installed by
  default. `SSL_CTX_use_certificate`/`use_PrivateKey` appear only on the gRPC/tsi
  path, not the RCFMP client path. Inference (Medium): stock FileMaker Pro does
  not send a client cert, which is why bare probes without a throwaway cert are
  rejected; a client-cert story for OpenMaker needs a cert chaining to a CA the
  server accepts, or the server configured to request-but-not-require.
- Trust store: `Draco::FMCertificateStore` (Support) builds a PEM store
  (`root.pem`, `server.pem`, `certifiedroot.pem`; strings at DBEngine 0x220120a)
  seeded from the bundled `FileMaker Root Authority` (2013) and `Claris Root
  Authority` (2020) self-signed roots (embedded in `Support`, and the Mac
  CertificateRequest list) plus the OS machine root CAs
  (`AppendWithMachineRootCA`). `RegenerateDefaultPEMFiles` can also create a
  self-signed RSA-2048 cert (`FMCertificateStore::CreateSelfSignPEMFile`, subject
  "Claris Self Signed Certificate (Not for Production Use)", 1-year validity) for
  the server role.

### Other open ports on the test host (banners only; High)

- 80: nginx, 301 -> https.
- 443: nginx, HTTP/2. Serves the FileMaker Data API
  (`/fmi/data/vLatest/productInfo` returns `{"name":"FileMaker Data API
  Engine",...}`) and the Admin/WebDirect surfaces. `/fms/ws` returns 502 (the
  WebSocket GIOP tunnel backend is reachable only from the engine).
- 8443: Caddy, HTTP/2, 200 (Admin Console / WebDirect front).
- 3000: an HTTP service (`X-Content-Type-Options: nosniff`, 404 on `/`), not TLS.
- 2399: FileMaker ODBC/JDBC listener (`fmxdbc_listener`), not TLS on probe.
- 5013: open, no banner (fmsased/helper channel).

The desktop RCFMP client uses only 5003 for the hosted-file session. Port 443 is
referenced by the client only as a GIOP-over-HTTP/WebSocket tunnel fallback (see
section 2) and for Data API / Claris ID / OAuth web flows, not the normal path.
Bonjour/mDNS host discovery exists: service type `_fmpro-rcfmp12._tcp` (DBEngine
0x2245194), `Draco::BonjourBrowser`, and `HostInfo::CreateTXTRecord` /
`DecodeTXTRecord` (TXT keys `ver=`, `app=`, `exe=`, `i=`, `n=`, `noIPv4`,
`noIPv6`). Used for local host browsing, not for the remote lab server.

## 2. CORBA / GIOP framing

Observations (High):

- omniORB 4.3.4 with the SSL, HTTP(WebSocket), and ZIOP transports shipped
  (README F1; `libomnisslTP`, `libomnihttpTP`, `libomniZIOP`). The ORB is
  configured programmatically, not from a file.
- GIOP 1.2, little-endian (capture: every message header is `GIOP\x01\x02\x01
  <type> <size-LE>`). Request/Reply bodies are standard CDR.
- Single interface `RCFMP12`, repository id `IDL:RCFMP12:1.0` (README F2).
  Server object key (constant in every request): bytes
  `\xffomniINSPOA\xffFMPOA\x00FMRPO` (built in `RCNetworkStack::Initialize`,
  0x1d4eb92, from POA names `omniINSPOA` -> `FMPOA`, object id `FMRPO`).
- Six operations on `RCFMP12`, confirmed by DWARF (Linux `libDBEngine.so`) and
  the omniidl call descriptors `_0RL_cd_07aabdf5dad7309d_*` in DBEngine. IDL
  (mangled-name + capture + DWARF; return types where DWARF is explicit):

  | op | idx | signature (CORBA) | oneway | notes |
  |----|-----|-------------------|--------|-------|
  | `Connect`   | 0x00 | `unsigned long Connect(in RCFMP12 cb, in string, in string, inout sequence<octet>, out string)` | no | returns the connection id |
  | `Tickle`    | 0x20 | `Tickle(in unsigned long, in string, inout sequence<octet>)` | no | keepalive, client -> server (6.1) |
  | `Disconnect`| 0x40 | `Disconnect(in unsigned long, in string)` | yes (no reply body path) | |
  | `Perform`   | 0x60 | `void Perform(in unsigned long, in string, in unsigned long maxreturn, inout sequence<octet>, out sequence<octet>)` | no | carries the RCTransaction; see 5.6 |
  | `Notify`    | 0x80 | `Notify(in unsigned long, in string, in sequence<octet>, out sequence<octet>)` | no | server->client callback |
  | `Reconnect` | 0xa0 | `boolean Reconnect(in unsigned long, in RCFMP12, in string)` | no | no caller found in inspected artifacts; inspected servants answer FALSE (6.3) |

  (The C++ wrappers return `Draco::DBError`; the on-wire CORBA return for
  `Connect` is `unsigned long` = the connection id. `Perform`'s wire return is
  **`void`**, not boolean - the DWARF prototype of both `_objref_RCFMP12::Perform`
  and `RPO_i::Perform` is `void Perform(...)`, and the capture reply body carries
  no return octet (section 5.6.1); the earlier "boolean" was an unverified guess.
  `Reconnect`'s boolean is from DWARF and its call descriptor, 6.3.)

- Arguments common to all data ops: `arg1 = unsigned long` connection id (the
  value `Connect` returned), `arg2 = string` (an empty/session string in
  observed traffic). `Perform`'s third argument is DWARF-named `maxreturn`: the
  **maximum reply size** the client will accept, NOT the request length (server
  `RPO_i::Perform`, nm `0x14eee00`, rejects `maxreturn < 0x14` or `> 0x200000`,
  resizes the inout OctetSeq to `maxreturn` to hold the reply, then trims it to
  the actual response length). The request length is the inout OctetSeq length on
  the request. The "opcode" is byte 0 of that OctetSeq (section 4), not an
  argument. See 5.6.2. High.

- Bidirectional GIOP. `_is_a` (and the ORB config, section below) carry a
  BiDirIIOP service context (id 5). In capture `conn-001` the `_is_a` request's
  service context lists the client's own callback endpoints: each client IP
  (the LAN IPv4 plus a VPN interface's IPv4 and IPv6) with a listen port,
  e.g. `192.0.2.19:0x138b`, `198.51.100.96:0x138b`, `[2001:db8::1]:0x138b`
  (addresses replaced; the shapes and the port are as captured). This is
  the back-channel: the server calls `Notify`/`Disconnect` back to the
  client over the same connection (see section 6, back-channel).

- Client ORB configuration, set in `RCNetworkStack::ConstructORB` (0x1d4c47a)
  and `Draco::ORBConfig::SetDefaultConfig` (0x158fa96) via programmatic key/value
  pairs (High, from string references):
  - `clientTransportRule = "* http,ssl,tcp,bidir"` (default) /
    `"localhost unix,ssl,tcp,http"`; with TLS off, `"* ssl,tcp,bidir"`.
  - `serverTransportRule = "* tcp,http,bidir"` / `"* tcp,bidir"`.
  - `offerBiDirectionalGIOP = 1`, `acceptBiDirectionalGIOP = 1`.
  - `oneCallPerConnection = 0`, `maxGIOPConnectionPerServer = 10`,
    `threadPerConnectionPolicy = 0/1`, `maxServerThreadPerConnection = 10`,
    `maxServerThreadPoolSize = 10`, `supportPerThreadTimeOut = 1`,
    `endPointPublish = "all(ipv4),all(ipv6)"`, in/outConScanPeriod set.
  - ORB id `omniORB4`, client call timeout set per-call
    (`omniORB::setClientThreadCallTimeout(30000)` around `Tickle`).
  - corbaloc built by `Draco::BuildCorbalocString` (0x158f6e2): for TCP,
    `corbaloc:ssliop:1.2@<host>:<port>/<objkey>` (and `iiop:1.2@` when SSL off,
    `omniunix:/tmp/fmp12-socket:,` for localhost). For the HTTP tunnel,
    `giop:http:wss://<host>/fms/ws#...` with the object key
    `/%ffomniINSPOA%ffFMPOA%00FMRPO` URL-escaped
    (`RCNetworkStack::BindRemoteConnectionBlocking`, 0x1d5107c). This is the
    source of the `wss://.../fms/ws` string and the only use of 443 by the RCFMP
    client: GIOP tunneled in WebSocket frames (subprotocol `giop.omniorb.net`,
    stock omniORB). Observed sessions used direct ssliop on 5003, so the tunnel
    is a fallback.

- ZIOP (compression) transport is linked but not observed on the wire; no ZIOP
  service context appears in captures. Unknown whether the server ever enables
  it. Low priority.

## 3. Connection handshake sequence

Order on every new connection (capture `conn-001`/`conn-002`, client side
`RCNetworkStack::BindRemoteConnectionBlocking`):

1. `_is_a("IDL:RCFMP12:1.0")`, request_id 2, with the BiDirIIOP service context
   (id 5) advertising the client callback endpoints. Server reply: boolean TRUE.
   This both type-checks the object and registers the back-channel.
2. `Connect`, request_id 4. Request body: the marshalled client callback object
   reference (CORBA object ref = type-id `IDL:RCFMP12:1.0` + IIOP profile(s)
   with the client's IPs and the callback port 0x138b, plus the object key),
   then two strings (empty in observed traffic), then the `inout sequence<octet>`
   machine-info payload (section 5). Reply: `NO_EXCEPTION`, return value = the
   connection id (`unsigned long`, e.g. `0x5db9006e`), plus the inout OctetSeq
   carrying the server identity payload (section 5), plus the out string.
3. The client stores the connection id and the server identity, then either
   keeps the connection for work or (host-browse/identity probe) immediately
   `Disconnect`s, request_id 6: body = connection id `unsigned long` + a trailing
   byte. See section 7 and `redirect-answer.md` for why the probe connection is
   torn down and the work happens on a separately-dialed connection.

The connection id returned by `Connect` is generated server-side
(`RPO_i::Connect`, 0x1d402d2): a 16-byte random `mConnectionKey` is filled from a
linear-congruential generator (`x = x*0x1dcd65 + 0x12e3a181`, top byte of each
step), and the id = `(next LCG top byte) + LE32(connectionKey[0..3])`, retried
until unique in `RCConnectionList`. The 16-byte key and the id both feed later
credential obscuring (section 4).

## 4. The RCTransaction envelope (inside the OctetSeq)

`Perform`'s OctetSeq is a `Draco::RCTransaction` buffer (class in
`RemoteSupport.h`). Layout (High, from `RCTransaction::ResetCallPush` 0x1d5af7a,
the receive ctor 0x1d59e5c, and the Push/Pop primitives):

### 4.1 Header (20 bytes, big-endian), written by ResetCallPush at offset 0

Field names are from DWARF `RCOpHeader` (size 0x14). Confirmed against the
captured op78, op80 and op90 headers.

| off | size | field | notes |
|-----|------|-------|-------|
| 0 | 1 | `opcode` | transaction class selector (section 5 catalog) |
| 1 | 1 | `taskid` | 0 in observed traffic |
| 2 | 2 | `transidhi` (u16 BE) | high 16 bits of the transaction id |
| 4 | 4 | `transidlo` (u32 BE) | low 32 bits; an increasing per-connection counter |
| 8 | 4 | `sessionid` (u32 BE) | the connection's session id; 0 before a file is open |
| 12 | 4 | `fileid` (u32 BE) | the open file's id = Open reply pop #5 (5.6.4); 0 before Open |
| 16 | 4 | `idletime` (u32 BE) | client idle seconds; 0 is fine |

The write cursor (obj+0x58) is set to 20 after the header. An earlier version of
this table read bytes 12..19 as an 8-byte target key. That form belongs to
`SetTargetKey`, used by record/lock ops; other ops carry `fileid` and `idletime`.

### 4.2 Body: a LIFO stack of typed values

- The body is appended forward by `Push*` (cursor obj+0x58 at offset 0x20 buffer
  obj+0x20, capacity obj+0x2c), and consumed from the end by `Pop*`
  (`RCTransaction::Pop(void*,uint)` at 0x1d5b178 decrements the cursor). So the
  receiver pops fields in reverse of the Push order. Important for decoding: the
  reply fields come off the top of the stack.
- Integers are big-endian and packed (unaligned): `Push(int)` byte-swaps and
  stores 4 bytes (0x1d5b27e); `Push(short)`/`Push(unsigned short)` swap 2;
  `Push(long long)` swaps 8; `Push(bool)`/`Push(unsigned char)` store 1;
  `Push(char16_t)` swaps 2.
- Strings (`unistring`/UTF-16) via `Push(unistring, maxLen)` (0x1d5b420): the
  text is SCSU-compressed (`SCSUCompressFromUnistring` -> `SCSUCompress`), then a
  4-byte big-endian byte count is appended after the compressed bytes. FileMaker's
  SCSU codec XORs every emitted byte with 0x5A (`SCSUCompress::BaseState`/
  `Emit2AndGotoBase`/`DoNextByte` at Support 0x1eb532.., `SCSUExpand::NextByte`
  at 0x1eca84). So on the wire a string field = `[SCSU bytes, each ^0x5A][u32 BE
  length]`. This is the "XOR 0x5A obfuscation" seen in captures; it is the string
  codec, not a separate layer. For ASCII, SCSU length == character count.
- A second string form (Pascal-style) `Push(uchar const*)` (0x1d5b392): writes
  the bytes then a trailing 1-byte length. Used where length < 256.
- `sequence<octet>` string fields in the Connect/identity payload use the form
  `[1-byte length (plain)][SCSU bytes ^0x5A]` (static `PushUnistring`/
  `PopUnistring`, 0x1d5debc / 0x1d40f1c). Confirmed against the capture: the
  reply field holding the server's six-character name is preceded by a plain
  0x06 length byte.

### 4.3 Credential obscuring (inside the body, on top of TLS)

This is the exact, implementable recipe. **Confirmed end-to-end against capture
`2026-10-27-s03-direct/conn-000` (see "Validation" below): a clean reimplementation
de-obscures the real Login credential block to valid SCSU.** Addresses are the
Linux `libDBEngine.so` / `libSupport.so` (Ghidra image base = nm + 0x100000);
Mac VM addresses in parentheses. The algorithm is identical on both; only struct
offsets differ (Mac `file[0x3ac]`/`file[0xa64]`/`conn+0x1f4` == Linux
`file[0x304]`/`file[0xa1c]`/`conn+0x284`).

- `PushEncrypt(PasswordHash, ...)` (nm 0x15170a0 unistring / 0x1517270 void*)
  first `Push`es the value (SCSU + XOR 0x5A as in 4.2), then calls
  `Encrypt_ObscureBytes` over exactly those just-written bytes.
- **Keystream (the obscuring XOR).** `Encrypt_ObscureBytes(buf, len, salt, saltlen,
  PasswordHash)` (libSupport nm 0xe7450) computes
  `keystream = PKCS5_PBKDF2_HMAC_SHA1(pass = PasswordHash.data (20 bytes),
  passlen = 0x14, salt = connKey, saltlen = 16, iter = 1, dklen = len)` and XORs
  the field bytes with it. `PushEncrypt` passes `salt = *(this+0x30) + 0x284`,
  `saltlen = 16` (Mac: `+0x1f4`).
  - **salt = the 16-byte connection key** (`connseq[0:16]` from the Connect reply,
    section 5.2). Confirmed from `RPO_i::Connect` (nm 0x14ea200): the per-connection
    key LCG (`x = x*0x1dcd65 + 0x12e3a181`, top byte each step; section 3) writes
    each generated byte BOTH to the reply key buffer AND to `rsp+0x70`, which is
    copied as the 16-byte `CRC` arg into `RCConnection+0x284` by
    `RCConnection::RCConnection(uint, CRC const&)` (nm 0x150f890, store at +0x284).
    So `conn+0x284 == conn+0x1f4 (Mac) == connKey`.
  - **Per-field restart (critical).** Each `PushEncrypt` field is obscured by its
    own `Encrypt_ObscureBytes` call with `dklen = that field's SCSU byte length`.
    PBKDF2 output for dklen=N is the first N bytes of the same stream, so every
    field reuses the SAME keystream prefix - the keystream does NOT advance across
    the credential block. (Proven in the capture: two obscured login fields of 10
    and 19 bytes share an identical 6-byte obscured prefix.)
- **`PasswordHash` (the PBKDF2 key above).** Derived per-login, NOT from the
  account password. In `RCLogin::Call` (nm 0xd6a5f0, Mac 0x186316e):
  `ComputePasswordHash(text = NumToString(nonce), salt = connKey (first 10 bytes),
  saltlen = 10, pepper, pepperlen = 0x1e, iter = 1, out, algo = 1 (SHA1))`
  (libSupport `PasswordHash::ComputePasswordHash` nm 0xe5430; the `DBPassword`
  wrapper nm 0x120d450 hardcodes the pepper/iter/algo). Note the PBKDF2 **salt here
  is the SAME connKey, first 10 bytes** (`conn+0x284`/`conn+0x1f4`, saltlen 10),
  not a file field. Internals of `ComputePasswordHash`:
  1. `text` is UTF-16; `Unicode::HashDecompose` (4.4; identity for an ASCII
     decimal string) yields the code units.
  2. **Pepper mix** (loop at libSupport 0xe54b0): for each code unit `i`, the
     password buffer gets 2 big-endian bytes
     `BE16( codeunit[i] XOR ((pepper[(2i) mod 30] << 8) | pepper[(2i+1) mod 30]) )`.
     So `passlen = 2 * len(text)`.
  3. `keystream_key = PKCS5_PBKDF2_HMAC(pass = that buffer, passlen,
     salt = connKey[0:10], saltlen = 10, iter = 1, md = SHA1, keylen = 20)`.
  - The **pepper** is the fixed 30 bytes at `.rodata` (nm 0x2e6d6d0): ASCII
    `"FileMaker, Inc."` + NUL, then
    `ca 3c 23 90 a5 5b 71 74 b6 1d f5 b2 6d 00`. (Algorithm selector 1, SHA1,
    and a 20-byte output; the other selectors and lengths are in 4.4.)
- **The nonce** = `(~file[0x3ac] + file[0xa64]) & 0xFFFFFFFF`, both u32, bitwise
  NOT of the first, 32-bit add with wraparound, then formatted by
  `NumToString<unsigned int>` (nm 0x8feb70) as a plain **unsigned decimal string**
  (no grouping/sign/hex). Confirmed: `RCLogin::Call` does `esi = ~u32[file+0x3ac];
  esi += u32[file+0xa64]`; `RCLogin::Perform` (nm 0xd66c00, Mac 0x186134c)
  recomputes the identical value from its own `NetHostFileUser` file. The two
  halves:
  - `file[0x3ac]` (= `file[0x304]` Linux) = the **server-supplied per-file nonce**
    returned by `Open` as pop #3 (section 5.6.4), a big-endian u32 read into the
    native value via `Pop(unsigned int&)`.
  - `file[0xa64]` (= `file[0xa1c]` Linux) = **`mInternalCreation`, a file-intrinsic
    u32**, not a login-time `time()` value. It is stored in the file's HBAM at key
    path `[2, 254]` (`Concat(KStatus, KModified)`) as a 4-byte big-endian value.
    It is set once when a file is created (`CreateFileUser`, same key) and read
    back on every open, so files made from a template inherit the template's value.
    Both sides read it from the file: the server from its hosted copy, the client
    from the first `op90` Download reply after Open, which carries the `[2]`
    subtree. It is not a separate protocol field.

**Validation (against `conn-000`, no secrets disclosed).** connKey and `file[0x3ac]`
are taken from the Connect reply and Open reply in the capture. Computing the
recipe above (nonce `= (~0x..5666 + mInternalCreation) & 0xFFFFFFFF`; PasswordHash
= PBKDF2-SHA1(peppermix(decimal), connKey[0:10], 1, 20); keystream = PBKDF2-SHA1(
PasswordHash, connKey, 1, fieldlen)) and XOR-ing the real `op80` Login credential
fields yields valid SCSU: both obscured credential fields decode to fully
printable text. (Correction: the shorter field is the password and the longer
one is the account name; an earlier version had them swapped.) Using each file's
own `[2, 254]` value, all 19 Logins in the capture decode cleanly, while the same
value plus one does not. An earlier value given here for the captured file was a
false positive from a brute-force search; the real one is the file's `[2, 254]`
value. A live Login to the test file with this recipe succeeded (error 0). Verified two ways: (a) calling the shipped
`libSupport.so` `ComputePasswordHash` + `Encrypt_ObscureBytes` directly, and (b) a
from-scratch pure reimplementation of the pepper-mix + PBKDF2 chain; both produce
the same de-obscured bytes. Scratch at `re/work/login/` (git-ignored). No
credential/key bytes are recorded here.

Net: the account name, password, and privilege extension travel as `PushEncrypt`
fields = SCSU + XOR 0x5A + a per-field PBKDF2-HMAC-SHA1 keystream XOR keyed by the
per-connection key, all inside TLS 1.3. The plaintext password is never sent.
Answer to question (d): yes, credentials travel in the `Login` `Perform` after
`Connect`/`Open`, protected by TLS plus this per-connection obscuring.

### 4.4 `PasswordHash::ComputePasswordHash` (observation: FileMaker's own library)

The one hash behind the Login keystream (4.3), every stored account password
(local-files.md 2.1) and file encryption keys. Support exports it as
`Draco::PasswordHash::ComputePasswordHash(unistring text, salt, salt length,
pepper, pepper length, iterations, PasswordHash& out, HashAlgorithm)` (Mac
x86_64 Support nm `0x13e974`, arm64 `0x130510`; Linux `libSupport.so` DWARF:
`PasswordHash` is `{mData, mHashSize}`, 16 bytes; `HashAlgorithm` is `kSHA1` 1,
`kSHA256` 2, `kSHA384` 3, `kSHA512` 4). For text `T`:

1. **Decompose.** `Unicode::HashDecompose(out, T, true)` (x86_64 Support nm
   `0x21815b`) works on UTF-16 code units, one at a time:
   - each code unit is replaced by its full decomposition from FileMaker's
     own table, [catalogs/password-hash-decompose.tsv](catalogs/password-hash-decompose.tsv)
     (987 BMP code units decompose; a unit absent from the table, or listed
     as itself, stays);
   - combining marks are never reordered: `a` U+0301 U+0323 and `a` U+0323
     U+0301 stay different;
   - when the first code unit is one of FileMaker's 552 combining marks (the
     table's `mark` column), U+0020 is put before the result;
   - tested surrogate pairs pass through unchanged; pass-through of lone
     surrogates is inferred, not measured.

   It is not NFD or NFKD and folds no case: compatibility characters
   (U+FB01, U+FF21, U+00B2, U+00B5, U+00A0) stay; Hangul syllables (11,172) and CJK
   compatibility ideographs (460) stay, although NFD decomposes them; 54 code
   units decompose differently from Unicode 16 NFD (Greek tonos becomes
   U+030D, not U+0301; U+1EC7 becomes `e` U+0302 U+0323, NFD `e` U+0323
   U+0302; U+1FF4 omega with oxia and ypogegrammeni starts with omicron
   U+03BF; Cyrillic schwa, barred o, ae and Abkhasian dze become their Latin
   letters; Thai U+0E33, Lao U+0EB3, Bengali U+09B0, Gurmukhi U+0A5C, Oriya
   U+0B5F and five Tibetan vowel signs decompose where NFD keeps them or
   orders the parts differently); 62 more that NFD
   decomposes stay (for example U+0218 S with comma below, Arabic letters
   with hamza or madda, Balinese tedung forms, arrows with a stroke). The
   mark set is Unicode's nonzero combining classes less 156 later marks
   (U+08CA-U+08FF, U+1AB0-U+1ACE, U+1DE7-U+1DFC and others). ASCII is
   unchanged.
2. **Pepper.** With a pepper, code unit `i` becomes the two big-endian bytes
   of `unit XOR (pepper[2i mod n] << 8 | pepper[(2i + 1) mod n])` (4.3). With
   no pepper, the code units are used in the machine's byte order
   (little-endian on x86-64 and arm64).
3. **PBKDF2.** `PKCS5_PBKDF2_HMAC(those bytes, salt, iterations, digest,
   out.mHashSize)`: digest SHA-256 for selector 2, SHA-384 for 3, SHA-512 for
   4 and SHA-1 for any other value (0, 1, 5 and 6 were run). The output length
   is the caller's `PasswordHash` size, not the digest size: a default
   `PasswordHash` is 20 bytes, so selector 2 with it gives the first 20 bytes
   of PBKDF2-HMAC-SHA256. The result equals standard PBKDF2 (RFC 8018) for
   every selector, iteration count (1, 2, 1000, 20000) and length (20 to 100
   bytes) measured.

`ComputePasswordHashRawBytes` (nm `0x13eb58`) is plain PBKDF2-HMAC-SHA1 over
raw bytes: no decomposition, no pepper.

**Callers** (from bin: all 18 call sites in Linux `libDBEngine.so` 26.0.3
and the one in `libFMSLib.so`, a license id; `libFMEngine.so`, `libSupport.so`
and the server daemons have none). Every one passes selector 1, SHA-1.
Account passwords (`DBPassword::Set`,
`Verify`, `ComputePasswordHash`; `DBUserAccount::MatchPasswordData`), Login
(`RCLogin::Call`, `Perform`) and ODBC connect pass the 30-byte pepper and 1
iteration into a 20-byte `PasswordHash`. The host's TXT records
(`HostInfo`) pass no pepper and 1 iteration; file encryption
(`HBEncryption*`) passes no pepper and 18,000 (key) or 20,000 (checksum)
iterations. The SHA-2 selectors exist in Support but none of these
enumerated callers uses them; the inspected account paths use SHA-1
(local-files.md 2.1). The Mac client's own call sites were not enumerated
and Windows was not compared; the Mac Login and accounts use the same
`DBPassword` code.

**Evidence.** A scratch harness (outside the repository) loaded the arm64
slice of FileMaker Pro 26.0.3's own `Support.framework` and called
`HashDecompose` and `ComputePasswordHash` 78,869 times: non-surrogate BMP
code units individually, 15,006 pairs of combining marks after `a`, and
400 strings, selectors
0-6, lengths and iteration counts. The rules above, with the catalog table,
reproduce every one of those hashes. Test vectors (text as UTF-16 code units,
salt, iterations, selector, length, decomposed units, hash):
[catalogs/password-hash-vectors.tsv](catalogs/password-hash-vectors.tsv).
For four non-ASCII account passwords pasted as NFC text into FileMaker Pro
26.0.3 (macOS), the stored hash is this chain over the pasted code units:
nothing decomposes them first (local-files.md 2.1). Whether Pro normalizes
text typed in another form (to NFC, say) is not measured; NFC input cannot
show it. Not measured: a lone surrogate (inferred to pass
through like a pair), and whether FileMaker Server's and FileMaker Pro for
Windows' libraries hold the same table (only the arm64 Mac library was
run).

## 5. Opcode catalog and message layouts

### 5.1 Opcode catalog (High)

Transactions register via `RCNetworkStack::RegisterTransactionClass(opcode,
factory, name)` and `RegisterTransactionNameOnly(opcode, name)` in
`DBEngineController::Start` (Mac 0x194c712; recovered with
`tools/re-network/rc_registry.py`). The Linux 26.0.3 build registers the same
opcode numbers (verified from `DBEngineController::Start` at Linux 0xed3d70). Byte
0 of the RCTransaction selects the class:

| op | name | class |
|----|------|-------|
| 13 | Upload To Host | RCUploadToHost |
| 14 | Upload List To Host Temp Location | RCUploadListToHostTempLocation |
| 15 | Download From Host | RCDownloadFromHost |
| 20-23 | ODBC: Get DSN List / Create ODBC Connection / ODBC Connection Command / ODBC Query | FMODBC::RCGetDSNList / RCODBCConnect / RCODBCCommand / RCODBCQuery |
| 30 | Perform Script on Server | (name-only) |
| 60-66 | Lock / Upgrade / Unlock / Request Notification / Kill All Locks / Lock Finished | DBRemoteLock / DBRemoteUpgrade / DBRemoteUnlock / DBRemoteRequestNotification / DBRemoteKillAllLocks / DBRemoteLockFinished |
| 70 | Host Commit | RCCommitRecords |
| 71 | Host Create Record | RCCreateRecord |
| 72 | Host Get Serial | RCSerialize |
| 73 | Host Update Table | RCUpdateTable |
| 75 | ConnectToServer | RCConnectToServer |
| 77 | GetFileList | RCGetFileList |
| 78 | Open File | RCOpen |
| 80 | Login | RCLogin |
| 81 | Logout | RCLogout |
| 82 | HostTimeStamp | RCHostTimeStamp |
| 83 | GetFileSize | RCGetFileSize |
| 84 | GetGuestCount | RCGetCalcGuestCount |
| 85 | Build Index | RCBuildIndex |
| 86 | Query Program | DBRemoteQueryProgram |
| 87 | Adjust Reference Count | RCAdjustReferenceCount |
| 88 | Upload Binary Data | RCUploadBinaryData |
| 89 | Compare Mod Counts | RCCompareModCounts |
| 90 | Download With Lock | RCDownloadWithLock |
| 91 | Upload With Lock | RCUploadWithLock |
| 92 | Download List | RCDownloadList |
| 93 | Download From Temp File | RCDownloadFromTemp |
| 95 | Download File | RCDownloadFile |
| 96 | Download Thumbnail | RCDownloadThumbnail |
| 97 | Verify Container | RCVerifyContainer |
| 98 | Get Container URL | RCGetContainerURL |
| 99 | Transfer Container | RCTransferContainer |
| 102 | Change Session Name | RCChangeSessionName |
| 103 | TransactInfo | RCDBTranInfo |
| 104 | Send AppleId Mail on Server | (name-only) |
| 105 | Send PSOS result to client | RCSendPSOSRetToClient |

OpenMaker Server also answers op240, OpenMaker's own script authoring
extension ([authoring.md](../authoring.md)); it is not a FileMaker opcode,
and OpenMaker sends it only to hosts that claim
`IDL:OpenMaker/Authoring:1.0` with `_is_a`.

op 83 GetFileSize has a name only: no recorded session (s01 to s06)
contains it, so its request and reply are not specified. Whether
FileMaker Pro sends it for `Get ( FileSize )` of a hosted file is open;
calc-functions.md 6.2 O7 is the probe that settles it. OpenMaker Server
answers it with status 3, "not supported".

Negative opcodes exist for lock/notify conflict paths (`DBRemoteNotify` op -64,
`DBRemoteNotifyConflict` op -67, `Cancel Query` op -101) dispatched on the
back-channel. The schema download (tables, fields, relationships, layouts,
scripts, value lists, accounts/privileges) is not a single opcode: the client
`Download File`/`Download List`/`Download With Lock` transactions stream the
on-disk `.fmp12` structures (the HBAM slot format, section 5.5), and the full
schema is parsed client-side from those pages (ties to the file-format and
layout workstreams).

### 5.2 Connect request and reply - exact byte layout

This is the CORBA `Connect` body (the `op`/opcode of section 5.1 is **not** used;
that is only for `Perform`). Pinned byte-for-byte against capture
`2026-10-27-s03-direct/conn-000` (Request/Reply `request_id=4`), the request
builder `PushMachineInfo` (Linux `libDBEngine.so` nm `0x14ec780`), the server
servant `RPO_i::Connect` (nm `0x14ea200`), the client reply parser inside
`RCNetworkStack::BindRemoteConnectionBlocking` (nm `0x1506850`, parse loop
`+0x1b10..+0x1d9a`), the framing primitive `PopUnistring` (nm `0x14ebec0`), and
`PushUnistring` (nm `0x151aa60`). (Ghidra's image base for this `.so` is
`+0x100000`, so the Ghidra addresses are these nm values plus `0x100000`.)
Reproduce with `re/work/network/decode_connect.py` / `decode_request.py`.
Confidence High unless noted. Sensitive values (license key, MACs, server id,
`authID`) are given by offset/length/type only.

**`PopUnistring` framing (confirmed, nm `0x14ebec0`).** A string field is
`[1 plain length byte n][n bytes SCSU, each XOR 0x5A]`; `PopUnistring(seq, off)`
reads `n = seq[off]`, SCSU-expands `seq[off+1 .. off+1+n]`, and returns
`off+1+n`. For ASCII, `n` == character count. The length byte is NOT XOR'd; the
payload bytes are. This is the only string form in either Connect OctetSeq. The
earlier "naive leading-u8 parse desyncs" artifact was from the four **raw scalar
bytes** interleaved between the string fields (below): a leading-u8 walk that
treats those as string headers loses sync at the server id.

#### CORBA framing (GIOP 1.2, CDR little-endian)

Request in-params (op `Connect(in RCFMP12 cb, in string, in string, inout
sequence<octet>, out string)`), in marshalling order:

1. **Client callback object reference** (IOR): CDR string `type_id`
   (`"IDL:RCFMP12:1.0\0"`, len 16) + `sequence<TaggedProfile>` with **count = 1**:
   one `TAG_INTERNET_IOP` profile (tag 0), encapsulated length `0xbc`, body =
   byte-order(1)+pad, IIOP version `1.2`, host string (a client IP, e.g.
   `192.0.2.19`), `u16` port = the client callback port (`0x138b` observed),
   object key seq (23 bytes `\xffomniINSPOA\xffFMPOA\x00FMRPO`), then a
   `sequence<TaggedComponent>` **count = 4** carrying the client's other listen
   endpoints (the VPN interface's v4/v6 etc.). So exactly **one** IIOP profile; the extra
   addresses ride as components, not extra profiles.
2. **string** arg1 - empty (`len=1`, just the NUL). DWARF names it `unused1`.
3. **string** arg2 - empty (`len=1`). DWARF `unused2`.
4. **inout sequence<octet>** `parms` = the machine-info payload (table below):
   `u32 LE` length + bytes.

Reply (status `NO_EXCEPTION`), in order:

1. **`ULong` return value = connection id** (`u32 LE`; conn-000 `0x463b59ad`).
   This is what `Perform`/`Tickle`/`Disconnect` echo as arg1 (verified: the next
   `Perform` carries `ad 59 3b 46`).
2. **inout sequence<octet>** `parms` = server-identity payload: `u32 LE` length
   (conn-000 = 133) + bytes (table below).
3. **out string** `sessKey` - empty in the capture (`len=1`). DWARF
   `_CORBA_String_out`.

There is no `u32 LE` "connection id echo" and no `u32 LE` "payload length" inside
the OctetSeq; those two words the old spec listed were the CORBA return value and
the CDR sequence-length prefix, i.e. part of the GIOP/CDR framing, not the
OctetSeq contents.

#### Request OctetSeq (machine info) - byte layout

Gathered by `PushMachineInfo`, pushed as 2 bytes, 4 strings, 2 bytes, 3 strings.
conn-000 payload = 365 bytes.

| off | size | field | framing | meaning / evidence | conn-000 |
|-----|------|-------|---------|--------------------|----------|
| 0x00 | 1 | `exeType` | raw u8 | `ApplicationInfo::GetExecutableType` (+1) | `0x10` |
| 0x01 | 1 | `appType` | raw u8 | `GetApplicationType` | `0x01` |
| 0x02 | 1+L | `machineName` | PopUnistring | `RCNetworkStack::GetHostName` (client host name) | L=21 |
| +prev | 1+L | `installCode` | PopUnistring | FileMaker **license key** (`GetInstallCode`) | L=41, redacted |
| +prev | 1+L | `versionString` | PopUnistring | `GetVersionString` | `"Pro 26.0.3"` (L=10) |
| +prev | 1+L | `macAddr` | PopUnistring | `GetMACAddresses`, CR-separated NIC list | L=250, redacted |
| +prev | 1 | `systemLanguage` | raw u8 | `Locale::GetTypeFromNonLocalizedLanguage` (+1) | `0x15` |
| +prev | 1 | `protocolVersion` | raw u8 | client protocol version; server requires `> 0x1a` | `0x20` |
| +prev | 1+L | `persistentID` | PopUnistring | `Machine::GetPersistentID` (32-hex machine id) | L=32, redacted |
| +prev | 1+L | `webServiceSettings` | PopUnistring | `GetWebServicesSettings` | empty |
| +prev | 1+L | `standbyAddress` | PopUnistring | HA standby addr | empty |

Server read order in `RPO_i::Connect`: `parms[1]` (appType), four `PopUnistring`,
then `parms[uVar9+1]` as the protocol version (checked `0x1a < v`, else
`LogUnsupportedClient`), then three `PopUnistring`. `parms[uVar9]`
(`systemLanguage`, the first of the two mid-bytes) is not range-checked.

#### Reply OctetSeq (server identity) - byte layout

Built by `RPO_i::Connect`; parsed client-side in `BindRemoteConnectionBlocking`.
Same structural shape as the request (16-byte key, then 2 bytes, 4 strings, 2
bytes, 3 strings), so the two directions share one layout with different fields
populated. conn-000 payload = 133 bytes (`0x85`), fully accounted for below.

| off | size | field | framing | meaning / evidence | conn-000 |
|-----|------|-------|---------|--------------------|----------|
| 0x00 | 16 | `connectionKey` | raw 16 bytes | **random LCG key**, becomes `RCConnection.mConnectionKey` (the PBKDF2 obscuring salt, section 4.3). **Not a hash** (see below). | redacted |
| 0x10 | 1 | byte A | raw u8 | server role/exe byte (mirrors request `exeType` slot); client stores to local `0x40` | `0x22` |
| 0x11 | 1 | byte B | raw u8 | server role/app byte (mirrors request `appType` slot); client stores to local `0x3c` | `0x03` |
| 0x12 | 1+6 | `remoteName` | PopUnistring | server canonical name -> `mRemoteName` | the lab server's 6-character name |
| 0x19 | 1+0 | (empty) | PopUnistring | empty (installCode slot; server returns none) | L=0 |
| 0x1a | 1+13 | `versionString` | PopUnistring | -> `mVersionString` | `"Server 26.0.3"` |
| 0x28 | 1+0 | (empty) | PopUnistring | empty (macAddr slot) | L=0 |
| 0x29 | 1 | flag byte | raw u8 | mirrors request `systemLanguage` slot; client stores to local `0xc` | `0x15` |
| 0x2a | 1 | `protocolVersion` | raw u8 | server protocol version -> `mProtocolVersion`; client stores to local `0x28` | `0x20` |
| 0x2b | 1+32 | `installCode` | PopUnistring | server 32-hex id -> `mInstallCode` | L=32, redacted |
| 0x4c | 1+55 | `webServiceSettings` | PopUnistring | `port=...;authID=...` -> `mWebServiceSettings` | L=55 |
| 0x84 | 1+0 | `standbyAddress` | PopUnistring | empty -> `mStandbyAddress` | L=0 |

Resolving the specific questions:

- **The 18-byte binary prefix (0x00-0x11).** It is **16-byte `connectionKey` +
  two raw bytes (0x10, 0x11)**, NOT `conn-id(4) + key(16) + 2`. There is no
  connection id inside the OctetSeq; the connection id is the CORBA return value
  (above). Bytes `0x10`/`0x11` (conn-000 `0x22 0x03`) are two scalar bytes the
  client reads individually into locals `0x40`/`0x3c` right before the first
  `PopUnistring`; they occupy the structural slot that holds `exeType`/`appType`
  in the request direction. The client parser does not range-check them.
- **Bytes between the version string and the server id.** After `versionString`
  (field 3) comes the empty field 4 at `0x28`, then the two raw bytes `0x29`
  (flag, conn-000 `0x15`) and `0x2a` (`protocolVersion`, conn-000 `0x20`), then
  the `installCode` string at `0x2b`. So the spec's "one field + 2 bytes" = the
  empty field 4 followed by `[flag][protocolVersion]`. The protocol-version byte
  the server gates on (`< 0x1b` rejected) is at wire offset `0x2a`; the observed
  value for 26.0.3 is `0x20`, not the 0x1a range guessed earlier.
- **Leading-u8 `PopUnistring` throughout?** Yes for every string field, but the
  payload is strings **interleaved with 4 raw scalar bytes** (`0x10`, `0x11`,
  `0x29`, `0x2a`) plus the 16-byte key prefix - that mix is what defeated the
  naive all-strings walk.
- **Reply string field order, mapped.** `remoteName` (the server's name), empty, 
  `versionString` ("Server 26.0.3"), empty, `installCode` (32-hex server id),
  `webServiceSettings`, `standbyAddress` (empty). The server id sits in field 5,
  after the two raw bytes - not immediately after the version string.

**Is the leading word a hash/MAC? No (answer to experiment 3).** `RPO_i::Connect`
fills `parms[0..15]` with a linear-congruential stream
(`mSeed = mSeed*0x1dcd65 + 0x12e3a181`, taking the top byte each step), i.e. a
random per-connection key, not a digest of the payload. The connection id (the
CORBA return) is then `(next LCG top byte) + LE32(key[0..3])`, retried until
unique in `RCConnectionList`. The capture matches exactly: key[0..3] =
`6e 59 3b 46` (LE `0x463b596e`), return value `0x463b59ad` = `0x463b596e + 0x3f`.
So the 16 leading bytes are the connection key echoed to the client (which needs
it as the credential-obscuring salt), and the id is derived from it - neither is
a MAC over the body.

Meaning of the `port=/clientPort=/ssl=/clientSSL=/authID=` string (answer to
question (c)): parsed by `RCConnection::ParseWebServicesSettings` (Linux nm
`0x1513d50`; DWARF `(settings, int32* port, bool* isSsl, unistring* portKeyword,
unistring* sslKeyword)`, tokenizing on `;` then `=`), Mac `0x1d5967c`.
`port`/`ssl` = the server's web-services (Data API / WebDirect / tunnel) endpoint
and whether it is TLS; `clientPort`/`clientSSL` = what this client should use for
the HTTP/WebSocket GIOP tunnel fallback (443/TLS). `authID` = an opaque session
/ web-auth token the client carries into the web-services and OAuth flows
(`GetRemoteWebAuthProviderList`, `WebServiceRequestor`), not into the 5003 RCFMP
path. The client uses 443 only when it falls back to the `giop:http:wss://
<host>/fms/ws` tunnel; in the observed sessions it used direct ssliop on 5003 and
did not contact 443.

#### What a clean client must reproduce for `Connect` to succeed

- Precede `Connect` with the `_is_a("IDL:RCFMP12:1.0")` request (request_id 2)
  carrying the BiDirIIOP service context that advertises the client's callback
  endpoints (section 3); then send `Connect` (request_id 4).
- Marshal a **well-formed client callback IOR** (one IIOP 1.2 profile, type-id
  `IDL:RCFMP12:1.0`, the object key `\xffomniINSPOA\xffFMPOA\x00FMRPO`, a
  reachable host/port, extra endpoints as components). The server stores
  `replyto` into the new `RCConnection` and **does not call it or validate it at
  `Connect` time** - it is exercised only later when the server pushes a
  `Tickle`/`Notify`/`Disconnect` over the back-channel (`RPO_i::Connect` passes
  `replyto` only to `LogUnsupportedClient` on the reject path, otherwise just
  retains it). So a malformed/unroutable callback ref still lets `Connect` return
  a connection id; it breaks later server-initiated messages, not the handshake.
- Send both string args empty, then the machine-info OctetSeq with the exact
  framing above. The `protocolVersion` byte (request offset = the second mid-byte)
  must be `> 0x1a`; `0x20` is what 26.0.3 sends. `exeType`/`appType` are taken
  from `ApplicationInfo`; the license key, host name, MAC list, and persistent id
  are informational to the server at this step (licensing/account checks happen at
  `Login`/`Open`, not observed to block `Connect`).
- From the reply, the client MUST keep: the CORBA return value (connection id) for
  arg1 of every later `Perform`/`Tickle`/`Disconnect`, and the 16-byte
  `connectionKey` for the PBKDF2 credential-obscuring salt (section 4.3). The
  `authID` in `webServiceSettings` is for the 443 web-services/OAuth path only,
  not the 5003 RCFMP session.

#### FileMaker Pro 26.1.1

[O code and strings, FileMaker Pro 26.1.1 Mac DBEngine x86_64 (SHA-256
`096a1994...`); FileMaker Server 26.1.1 is not released, so nothing here
was seen on the wire.]

- `PushMachineInfo` (0x1d56c86) builds the same request: protocol version
  byte `0x20`, the same leading bytes, and `GetVersionString` now
  `Pro 26.1.1`.
- `RCFMP12` is unchanged: the servant `RPO_i` and the client stub keep the
  six operations and their signatures, and DBEngine has no new host
  request class. Whether existing Perform operations changed their bodies
  was not checked [U].
- New client-certificate code in the shared engine: `TrustStoreManager`
  (`VerifyClientCert`, `VerifyClientCertUsingStoreCache`,
  `GenerateNewKeyAndClientCertRequest`, `ReadUniversalClientPemFile`,
  `GetCustomOIDsFromPemCert`, `sPersistent_ID_key`),
  `OpenSSL::VerifyCertAgainstEmbeddedRootCA`, `DBClientCertListManager`
  (an encrypted host-to-certificate list, `clientCertlist.txt`), the subject
  `/CN=FileMakerClient`, and log lines that `BindRemoteConnectionBlocking`
  found no client certificate or one whose persistent id differs from the
  machine's, that Connect failed because a client certificate could not be
  verified, and that one expired or was revoked. FileMaker Pro has a
  matching settings page (`CClientCertificateListPage`: host address and
  certificate file) and its About box shows the machine's persistent id.
  [I] A host can now demand a client certificate tied to the Connect
  `persistentID`; in 26.0 any self-signed certificate passes (F10, F11).
- `RCNetworkStack::IsADTAllowed` and `SetADTConnectionsEnabled` gate
  connections of the agent interface (client-ui.md, FileMaker Pro 26.1.1),
  with the message "Unable to connect. This host doesn't allow ADK
  connections." [O: strings]. How such a connection identifies itself is
  [U].

Unknowns, until FileMaker Server 26.1.1 can be captured: whether it
requires client certificates and how they are issued; the ADK switch's
default; whether it accepts protocol version `0x20` clients as before.

### 5.3 ConnectToServer (op 75), GetFileList (op 77)

`RCConnectToServer::Call` (0x185abfa) push order (High): `ResetCallPush`, then
`PushEncrypt(password-blob)`, `PushEncrypt(privilege-extension)`,
`PushEncrypt(username)`, `PushEncrypt(account-name)`, `Push(host string)`,
`Push(share string)`, `Push(u16 authFlags @0x18a)`, `Push(u16 version=8)`,
`Send`. Reply pops: `u16 error`, `u16 shareType`, and (error 0x20d0) an encrypted
NT-auth blob. op75 is not observed.

#### GetFileList (op 77)

The host-browse that fills the Hosts and Open Remote File dialogs. Sources:
the client's `RCGetFileList::Call`, `CallPushAddition`, `CallPopAddition`,
`RawGetRemoteFileList` and constructor (Mac DBEngine `0x1859c00`,
`0x185dec2`, `0x185ee38`, `0x1859510`, `0x186504c`); the server's
`RCGetFileList::PerformPopAddition`, `PerformOperation`,
`PerformPushAddition`, `PushFile` and `PushFileThumbnails` (Linux nm
`0xd60ab0`, `0xd60f90`, `0xd621d0`, `0xd5da80`, `0xd60040`),
`FileOpenPush::operator()` (nm `0xd5c540`) and `DBFileUser::SetShareable`
(nm `0xf27b90`); the DWARF enums of `RCGetFileList`, `FileOpenPush`,
`DBFile` and `DBPrivilegeExtensions`. Observed: two anonymous requests to
the lab FileMaker Server on 2026-11-11, one with share type `fmapp` and one
with an empty share type; and FileMaker Pro's own anonymous request and
nonempty reply through a private relay capture on 2026-11-14. Confidence
High unless marked.

op77 is an `RCUploadDownload` transaction (5.6.6c): the request carries the
shared block with no payload, and the list comes back as a download page.

**Request**, push order after the 20-byte header (opcode `0x4d`; the
header's session and file ids are not checked, `ValidTransaction` accepts
any):

```
[ u32 N 0 ][ u64 perform 0 ][ u64 cursor 0 ][ i16 depth -1 ][ u8 copy-key length 0 ]
[ NT blob, L bytes, obscured ][ u16 L ]
[ privilege extension: SCSU bytes, obscured ][ u32 length ]
[ password: SCSU bytes, obscured ][ u32 length ]
[ account name: SCSU bytes, obscured ][ u32 length ]
[ share type: SCSU bytes ][ u32 length ]
[ u16 flags ][ u16 version 8 ]
```

| field | meaning |
|-------|---------|
| share type | the extended privilege the caller lists files for. FileMaker Pro's Hosts window sent `fmapp` [O: `s134-host-list-20261114/conn-000`] |
| flags | `RCGetFileList` bits: 1 `kIgnoreHiddenBit`, set when the caller's share type ends with U+E007 (the client strips that character); 4 `kDAPIGetDBList`, a Data API listing |
| version | 8 (`mClientVersion`) |
| credentials | account name, password, privilege extension and an external-authentication blob; all empty for an anonymous list, which makes them 14 zero bytes (`u16 0` and three `u32 0`) |

**Obscuring.** The credential fields are obscured as Login's are (4.3),
with a fixed key: keystream = PBKDF2-HMAC-SHA1(password = 20 bytes of
`0x63`, salt = the 16-byte connection key, 1 iteration, length = the
field's byte count), restarted for each field and XORed over its SCSU bytes
(not its length word) and over the NT blob. `0x63` is the DWARF constant
`RCGetFileList::kGetFileEncryptSalt`; the client's constructor and the
server's transaction factory both fill the transaction's `PasswordHash`
with it. A credentialed op77 therefore needs only the Connect reply, no
opened file (from bin; not run live, because where the host filters, a
credentialed list signs in to every candidate file, below).

**Native Hosts-window request [O].** In FileMaker Pro 26.0.3, selecting a
favorite host in File > Hosts through the relay sent `_is_a`, Connect,
one Perform carrying op77, and Disconnect. No file was opened and no
Login was sent. The op77 transaction was 70 bytes: a 20-byte header,
23-byte empty download shared block, 14 zero bytes for the anonymous
credential fields, `fmapp` as five SCSU/XOR bytes plus its u32 length,
flags 0 and version 8. The bytes after the header equal the `fmapp`
request vector below. No dialed host string appears in this transaction.
The reply had error 0, a nonempty file tree and the server's six-byte
name; FileMaker Pro displayed the returned file entries. Decoding that
native tree showed each entry's `01`, `07`, `10` and `1818` children,
as in the reply layout below. The request body after its header matched
the earlier anonymous `fmapp` probe vector; variation between two native
Hosts-window requests has not been measured. File names, paths and
counts are withheld [O: private
`re/captures/s134-host-list-20261114/conn-000`].

**Reply**, push order after the slot bytes:

```
[ list tree, N ][ u32 N ][ u64 perform ][ u64 cursor ][ i16 depth ][ u8 copy-key length 0 ]
[ NT blob, obscured ][ u16 length ]          only when the error is 0x20d0 (external authentication continues)
[ u16 count ][ host name: SCSU bytes ][ u32 length ][ u16 error ]
```

`count` is the number of files in the tree, `host name` the server's
(`RCNetworkStack::GetHostName`), `error`
0 or a code below. The client pops error, host name and count, then the
blob for 0x20d0, then the page.

**The list tree.** One node per listed file, keyed by a serial key in
listing order (`01`, `02`, ...):

| key | value |
|-----|-------|
| `01` | `u32` BE, the host's id for the file, the id op78 returns (5.6.4, pop #5) |
| `07` | text, the path of the folder holding the file on the host, its components separated by U+E000; a chunked text node (`00` header, `01` piece, file-format.md 3.6). FileMaker Pro does not read it |
| `10` | text, the file name with its `.fmp12` extension |
| `1818` | `18`: a copy of the file's document preferences `[2, 18]` (file-metadata.md 3). `01` large, `02` small, `03` mini thumbnail when the host has one: `04` 32 hex digits, `4d41494e` ("MAIN") a URL |

For a client listing files and preparing to open one, `10` supplies its
display and Open name, `01` identifies the host's file id, and `07` preserves
its host folder path. `1818` adds optional display metadata. FileMaker Pro's
observed path construction below uses `10` and the host address; it does not
read `07`.

FileMaker Pro (from bin) takes each serial-keyed node, reads the name
(`10`), makes an `fmnet:` path of it with the host's address, reads the
thumbnails with the `[1818, 18]` preferences, and keeps the list sorted by
the name's collation key.

**Observed** (anonymous lists, 2026-11-11; names, paths and inventory
counts withheld): share type `fmapp` returned a subset of the files
listed with an empty share type, both with error 0 and the server's host
name. Every entry had `01`, `07`, `10` and `[1818, 18]`; some had
thumbnails. A returned entry's file id matched the file already open in
the session. The request bytes for `fmapp` after the header are below;
the reply tail uses symbolic placeholders for lab-derived identifiers,
sizes and counts:

```
00000000 00x16 ffff 00                    N 0, handles 0, depth -1, empty copy key
0000 00000000 00000000 00000000           NT blob, privilege extension, password, account: empty
3c373b2a2a 00000005                       "fmapp" (SCSU, XOR 0x5A) and its length
0000 0008                                 flags 0, version 8

80 20 01 80 03 01 <file-id:u32> 20 07 80 ...  a file entry, then its folder
... <N:u32> 00x16 0000 00                    handles 0, depth 0
<count:u16> <host-bytes> <host-length:u32> 0000  count, host name, length, error 0
```

**Which files are listed** (`FileOpenPush::operator()` and
`RCGetFileList::PerformOperation`, from bin):

1. Only files the host has open and shared (`HBFile::mSharingFlag` set and
   a network host file user).
2. With a share type, only files where that extended privilege is enabled
   for some privilege set (`DBPrivilegeExtensionsCatalog::IsEnabled`).
   Then, unless flags bit 1 is set, the hide bits of `DBFile::mFlags`:
   for `fmapp` (`kBuiltInExtensions[2]`, `kPeerToPeerNetworking`) a file
   with any of `hHideFromPeerToPeer` 1, `hPreventPeerToPeer` 2 or
   `hPeerToPeerOff` 4 is left out; for `fmwebdirect` (`[0]`) one with
   `kHideFromIWP` 8. Other share types check no bit. An empty share type
   skips both checks, so every shared file is listed (observed: the
   unrestricted list included files excluded from the `fmapp` list).
3. When the host requires sign-in for the list
   (`DBUserAccountCatalog::sRequiresLoginForFileList`): a request without
   an account name gets error 18 (`kLoginRequired`) and no list; with one,
   the host signs in to each candidate file with it
   (`RCLogin::LoginWithLockoutCheck`, so failures count toward lockouts)
   and lists the files where that succeeds (0, or 211 password expired).
   If none is listed and some failed, the error is 214 when every failure
   was a lockout and 9 otherwise (Apple ID accounts: `0x5141` without a
   passcode, `0x5144` with one). Without that setting the credentials are
   not checked and every client gets the same list.
4. Flags bit 4 (Data API listing) with the guest account, where sign-in is
   required and the guest account is denied: error 18.
5. Block New Users (6.5): error 1638 for WebDirect and custom web
   clients; for other clients that are not connected as guests yet, 1638
   when their protocol version is above `0x18`, otherwise 812.

`sRequiresLoginForFileList` is by inference the Admin Console's "Filter
Databases" (admin-console.md): fmserverd imports its setter
`DBUserAccountCatalog::SetRequiresLoginForFileList` and has a preference
key `kFileDisplayFilter`. The lab server listed its files to an anonymous
request on 2026-11-11, so no such filter was in effect then; its setting
was not read.

**Hidden files.** When a file is shared, the host copies two bits of the
file's document preferences `[2, 18]` (file-metadata.md 3.2) into
`DBFile::mFlags` (`DBFileUser::SetShareable`, from bin): option `0x0080`
(`prefHideSharing`) becomes `hHideFromPeerToPeer`, and `0x1000`
(`prefWebHideSharing`) becomes `kHideFromIWP`. So "Don't display in Open
Remote File dialog" is `[2, 18]` bit `0x80`. Observed: files without the
bit in their `[1818, 18]` copy exactly matched the `fmapp` entries; the
unrestricted list also included files with the bit. Inventory counts are
withheld. Where
`hPreventPeerToPeer` and `hPeerToPeerOff` come from is not traced (by
inference the `fmapp` privilege's sharing state, `kAllOn`, `kSpecified`,
`kAllOff`).

**Earlier probes.** The two 31-byte replies of 2026-10-29
(`re/work/hostlist/`) answered OpenMaker's request of the time, which had
no shared block. Their host name is empty, so by inference the server did
not run the operation, and the field read there as a count held no count.

#### Implementation notes (GetFileList)

- OpenMaker's client (`om_protocol::build_file_list`) sends the anonymous
  request above, byte for byte the native Hosts window's after the header
  (share type `fmapp`, flags 0, version 8). It reads the reply's error,
  host name and count and the list tree (`parse_file_list_reply`), and
  asks for each later page by sending the request again with the reply's
  resume block, as for a download (`Session::get_file_list`); the tree is
  the pages' slots in order, at most 64 MiB of them, and the count, host
  name and error are the first page's. `om-provider` names each file by
  its entry's `10` and refuses a tree whose names fall short of the
  count.
  The desktop's Hosts dialog asks op77 of any host that does not claim
  OpenMaker's own listing extension.
- OpenMaker Server answers op77 with the files it publishes, the same files
  its own listing extension lists (`docs/hosted-file-list.md`): those open
  or paused, in hosting order. Each entry carries only `01`, the id Open
  returns, and `10`, the name. That FileMaker Pro accepts entries without
  `07` and `[1818, 18]` is inferred: it reads only `10` and the thumbnails
  (above). No sign-in is asked for, as with no "Filter Databases". The
  share type, the flags and the hide bits filter nothing. Block New Users
  refuses a new user with 1638, whatever its protocol version. The list is
  paged within the request's `maxreturn` as a download is (5.6.6c): every
  page carries the list's count, and a `maxreturn` too small for the
  reply's other fields and one whole chunk of the list is refused with a
  CORBA system exception rather than answered past it.
- Not settled: a credentialed list from a host that filters, and how
  FileMaker Server pages a long list: whether its later pages carry the
  count, and what it does with a `maxreturn` too small for one entry.
  FileMaker Pro's `maxreturn` for op77 is not recorded.

### 5.4 Login (op 80)

`RCLogin::Call` (nm 0xd6a5f0, Mac 0x186316e). The obscuring key and nonce are the
make-or-break; both are pinned in section 4.3 and **validated against the
`conn-000` capture** (a clean reimplementation de-obscures the real credential
block). The field framing below is confirmed against the same capture's `op80`
request body (GIOP request_id 14) and the server pop order in `RCLogin::Perform`.

Before the first `PushEncrypt`, `RCLogin::Call` computes the per-login
`PasswordHash` (section 4.3): `nonce = (~file[0x3ac] + file[0xa64]) & 0xFFFFFFFF`
(both u32); `text = NumToString(nonce)` (unsigned decimal);
`PasswordHash = ComputePasswordHash(text, salt = connKey[0:10], pepper, iter=1,
SHA1)`. `file[0x3ac]` is the Open-reply nonce (5.6.4 pop #3); `file[0xa64]` is the
file's `mInternalCreation` read from the downloaded file (4.3).

Body layout, front to back (header first; the server pops from the tail), from
`RCLogin::Call` (Mac x86_64 `0x186316e`) and `RCLogin::Perform` (Mac
`0x186134c`) [from bin]. A "str" is plain `[SCSU ^0x5A][u32 BE len]` (4.2);
"enc-str" is the same, with the data bytes then obscured by the per-field
keystream (4.3); "enc-bytes" is raw bytes then a u16 BE length, the bytes
obscured the same way. Confirmed byte-exact: an implementation rebuilds both
captured official-client Logins with no bytes left over, and a live Login with
it succeeded.

| # | type | field | value for a clean client |
|---|------|-------|--------------------------|
| 1 | u32 BE | callback file-user id | 0 (only used to route server script callbacks). The client pushes it when the host's version is above 8; the server pops it when field 10 is above 7 |
| 2 | u16 BE | client flags | 0. Bit 0 is `kConsumeLicense`: this login takes a connection license. Pushed when the host's version is 8 or more; popped when field 10 is above 6 |
| 3 | enc-str | new password | present only when field 4 is 1 |
| 4 | u8 | has-new-password | 0 |
| 5-9 | str x5 | system drive, preferences, documents, desktop, FileMaker paths | any absolute paths |
| 10 | u16 BE | version | 8 |
| 11 | enc-bytes | external authentication data (`mNTAuthentication`) | empty: `00 00` |
| 12 | enc-str | privilege extension | empty: `00 00 00 00` |
| 13 | enc-str | **password** | the account password |
| 14 | enc-str | **account name** | the account name |
| 15-17 | str x3 | system version, printer, app language | e.g. "26.6.2", " ", "English" |
| 18 | str | file path | `fmnet:/<host>/<file>.fmp12` |
| 19 | str | session name | any display label |
| 20 | u8 | login type | 0 normal, 1 test only (`kTestOnlyLogin`), 2 re-login (`kReLogin`) |
| 21 | u32 BE | target file id | the Open reply's file id (5.6.4) |

The five enc fields are obscured, each with its own keystream restart; the
empty ones obscure nothing. None of the plaintext strings gate authentication;
the server only records them for its client list. (Corrections: an earlier
version of this table had a different field order and an "optional guest
count", which is field 1; it also showed fields 11 and 12 as one
"privilege extension length", leaving out four bytes.)

Observed [O] in all 59 Logins of captures s03, s06 (FileMaker Pro 26.0.3
against OpenMaker Server) and s07 (against the lab FileMaker Server), parsed
from the tail without decoding credentials: login type 0, has-new-password 0,
fields 11 and 12 empty, version 8, client flags 0, both leading fields
present, and nothing left over. In all 11 opens of the test file in s06 and
s07 that were followed by a second Login, the first Login carried the account
name "Admin" (that file's `[23, 1, 1]` `16`, local-files.md 3) and an empty
password and was refused with 212, before the account the user typed or
FileMaker Pro remembered (the two obscured names share a keystream: their
XOR, with the second name known, gives the first; no credential was decoded).

Server `RCLogin::Perform` (nm 0xd66c00, Mac 0x186134c) pops in reverse: the target
`file-serial` u32 (`GetNetHostFileUserPtr` -> host file), the `loginType` byte,
five strings, **recomputes the identical `PasswordHash` from its own host file's
`~file[0x304] + file[0xa1c]`** (nm 0xd671c2), `PopEncrypt`s account name and
password (then `memset`-zeroes the password), builds a `DBAuthenticationInfo` via
`SetToUserNamePassword`, `PopEncrypt`s the privilege extension (an empty one
becomes a default) and the external data (`SetNTAuthentication`, then
`ExtractOAuth`), pops the version, the five paths, has-new-password and, when
set, the new password, then the flags and callback id. It logs in through
`RCLogin::LoginWithLockoutCheck` and pushes the reply (5.6.5).

**Login forms** [from bin unless marked]:

- **[Guest].** `DBAuthenticationInfo::SetToPredefined(kGuest)` (Mac
  `0x1b9bdcc`) sets both the account name and the password to
  `sGuestAccountName`, the text `[` U+E002 `]`, the name FileMaker stores
  for [Guest], whose stored hash is of that same text (local-files.md 2.1),
  so the server's ordinary check accepts it; login type 0. FileMaker Pro
  builds it in `DBSession::InternalCreateFileUser` (Mac `0x1bae6a6`). Not
  captured: no recorded open fell back to [Guest].
- **A new password.** The login dialog (`DBSession::UILogin`, Mac
  `0x1bab944`) passes a new password into `RCLogin::Login`, which sends it
  as field 3 with field 4 set to 1; FileMaker Pro asks for one after a 211
  (password expired or must be changed) [I: the dialog's trigger was not
  decoded]. The server accepts the change when the credentials give 0 or
  211 and the login type is 0 or 2: it opens the account, calls
  `DBUserAccount::SetPassword(new password, true)` and commits on the host,
  which so writes `06`, `0a` and `04` itself (local-files.md 2).
  `SetPassword`'s refusals come back as the Login's error: 208 with the
  minimum length (5.6.5), 209, 206. Not captured.
- **Re-Login.** `DBFileUser::Relogin` (Mac `0x1a191a6`) sends login type 2
  on the open file's connection with the new credentials and the session's
  privilege extension; on success FileMaker Pro takes the new account's
  privileges and drops its cached `[T, KAccess]` subtrees. Not captured.
  In `s148-hosted-account-writes-20261113`, the maintainer found no
  File > Re-Login menu item in FileMaker Pro 26.0.3. Closing and reopening
  the hosted file sent a second Open and Login on the same connection;
  both Login requests had type 0 [O: request framing, no credentials
  decoded]. This does not exercise login type 2.
- **Test only.** Login type 1 checks credentials; `RCLogin::Call` then
  does not mark the file logged in. Which feature sends it is not known.
- **External server accounts** (kind 1, local-files.md 2.2). The client's
  security token travels in field 11. When the server needs another round
  it answers **0x20d0** (8400, not in FileMaker's error table) with its
  continuation token, and the client stores that as its next field 11
  (`SetNTAuthentication`) and logs in again. Not captured.
- **OAuth, Claris ID and Sign in with Apple** (kinds 18 to 67).
  `SetOAuthInfo` and `SetAppleCredentialsInfo` (Mac `0x1b9c3cc`,
  `0x1b9d980`) put an ASCII text into field 11 instead: a scheme name
  (`soAuth` or `sAppleID`, static strings whose values were not read) and
  `;`-separated parts (the request id of the HTTPS step before,
  `FMSession::GetOAuthLoginURLAndRequestID`; an identifier; the Claris ID
  token after `sFMIDTokenScheme`; an Apple passcode after
  `sAppleIDPasscodeScheme`; a paired e-mail address). The server splits it
  on `;` into 3 to 8 parts (`ExtractOAuth`, Mac `0x1b9cbd0`). After such a
  login succeeds the reply carries the account name (5.6.5), which the
  client keeps as its user name. Not captured.

Account-lockout handling is in
`RCLogin::LoginWithLockoutCheck` (nm 0xd5cc10 / Mac 0x185bfe0). Its
thresholds, how long a lockout lasts, whether it counts by account or by
client, and the error a locked-out login receives are not measured.
FileMaker's error table has 214, "Too many login attempts" (DBEngine string
resource 159, `catalogs/error-codes.csv`); that a lockout answers with it is
inferred from its name only. OpenMaker Server's own limits are in
`crates/om-server/src/login_limits.rs`.

**The second nonce half (`file[0xa64]`).** It is the file's `mInternalCreation`
(4.3), the HBAM value at key `[2, 254]`. Both sides read it from the file. A clean
client must (1) send the first `op90` Download after Open, (2) read `[2, 254]` from
that reply's slot data, and (3) use it as the second nonce half. If it is wrong the
keystream is wrong and the server rejects the credentials, so it must be exact on
the first try.

Auth types (`DBAuthenticationInfo`, 0x1b9c0e0; DWARF `AuthTypes`: 0 none, 1
[Guest], 2 security bypass, 3 other, 4 OAuth, 5 Sign in with Apple): local
account (`SetToUserNamePassword`), external/NT (`SetNTAuthentication`), Claris
ID/OAuth (`SetOAuthInfo`/`ExtractOAuth`, provider list from
`GetRemoteWebAuthProviderList`), Sign in with Apple
(`SetAppleCredentialsInfo`). OAuth/Claris ID credentials are obtained out of band
over HTTPS (`FMSession::GetOAuthLoginURLAndRequestID`, FMEngine) and the
resulting token is what travels in field 11 (above); on Windows the external
helper is `ZAuth.exe` (README; not present on Mac/Linux).

### 5.5 Open (op 78) and record transactions

`RCOpen::Call` (0x185eefe) push order (High): `ResetCallPush`,
`Push(u64 file @0xc08)`, `Push(file-path string)`, `Push(string @file+0x40)`,
`Push(string @file+0x28)`, `Push(u16 version=8)`, `Send`. Reply pops: `u16
status`; on success: `string serverMsg`, `u32 file[0x3ac]` (the server per-file
nonce half used for login key derivation, section 4.3), `bool file[0xc]`, `int
file[0xaa0]`, `u16 file[0xaa4]`, then (status 0) a byte, a string into
`file[0x170]`, a u32 flag, and a `u64 file[0xc08]` session value. The status
names below are from the FileMaker 26 Linux `Draco::Error::Codes` DWARF enum
(`Support/Errors.h`); the observation column distinguishes wire captures
from those names. A status value alone does not imply that a message string
follows it.

| status | FileMaker name | Open observation |
|--------|----------------|------------------|
| `0x4fb3` | `kSpecification` | Not identified in a live Open reply; the older "cannot open" gloss had no evidence. |
| `0x4fb5` | `kFileNotFound` | FileMaker Server answered this when the requested file name was not hosted (observed in the desktop; no relay capture). Whether a hosted but closed file answers the same is unknown. |
| `0x507c` | `kInstallCodeConflict` | An absent or invalid license key was refused at Open without a message string in the captured reply (F26). |
| `0x0666` | `kHostBlockNewUsersEnabled` | Block New Users refusal captured in 5.6.4. |
| `0x0004` | `kUnknownCommand` | No observed Open reply; it is not "file missing". |

Record/data transactions reuse the LIFO body with the HBAM slot codec:
`RCUploadDownload::Call` (0x1d27a1c) frames `PushPtr`/`PushLen` + a packed slot
buffer produced by `HBAMSlot::S_SlotPack` (0x15f1678) and unpacked by
`S_SlotUnpack` (0x15f1d44). `RCCommitRecords` (op 70), `RCCreateRecord` (op 71),
`RCSerialize` (op 72, serial-number allocation), `RCUpdateTable` (op 73),
`DBRemoteLock`/`Unlock`/`RequestNotification` (ops 60-66, record locking),
`DBRemoteQueryProgram` (op 86, finds/queries), `RCHostTimeStamp` (op 82,
keepalive/clock), `RCChangeSessionName` (op 102). The record/field value encoding
is the on-disk HBAM page/slot format (field-format and layout workstreams own it);
over the wire it is carried verbatim as the packed slot buffer, so field values
(integers, text, containers, record ids) are HBAM-encoded, not re-encoded for the
network.

The `RCOpen::Call` reply-pop order above is now confirmed byte-exact against the
capture (section 5.6.4): the `u32 file[0x3ac]` nonce is the third field popped
(after `u16 status` and the `serverMsg` string), a big-endian u32.

#### The design catalogs read at open

FileMaker Pro 26 does not download a hosted file's whole layout catalog
`[4]` when it opens the file. In s03 it read the catalog's index `[4, 1]`
(the layouts' names, display order, and for each layout its
`{tableAliasKey, menuSetKey}` definition, kind and view flags;
layout-model.md 10.6) and the member `[4, 5, L]` of the layout it showed
first. It read each other member when that layout was first shown, and
never read `[4]` whole. It read the theme catalog `[6]` and the script
catalog `[17]` selectively as well: each one's index (`[6, 1]`, `[17, 1]`),
then one theme member and one script member. The theme member, `[6, 5, 1]`,
was read whole, as one op90 of its key, before the first window. Another
theme's member came much later in the session. It read the font catalog
`[25]` whole (observed: the op90 key paths of s03, classified outside this
repository, #1018) (High for which keys were read; Medium for when each
read happened relative to drawing; that the later theme read followed
showing a layout of that theme is inferred).

OpenMaker's window reads the index of a hosted file's layout catalog at
open, and each layout's member the first time something reads that
layout (desktop-client.md, "Opening files", #1019) [implementation]. Its
calculation schema lists the scripts from the script catalog's index
`[17, 1]`, and a script that runs is read alone, its member `[17, 5, S]`
(#1052, #1053). The whole script catalog is read when the Scripts menu
is first opened. The theme catalog's index is read at open, and a theme's
member `[6, 5, T]` whole the first time a layout naming it is decoded
(#1129; layout-model.md 7.5).

#### Related files

A file's table occurrences can show tables of other hosted files, named by
its external data sources (file-metadata.md 1.1). FileMaker Pro opens such
a file in the session it already has, on the same connection: s03
conn-000 holds one `Connect` and 19 files opened in it, each with its own
Open, op63 (client file ids -4, -6, ... -38, then -45; write-path.md 6.1)
and Login, and each later closed with an op62 Unlock of its root
(write-path.md 4.3) (High). In all 19, the op63's client file id is
`-(its transidlo + 2)` and the Open and Login headers carry session id 1
with each file's own `fileid`; the op90 downloads' `transidlo` rises
across files (High, [#229](https://github.com/OpenMaker-Dev/openmaker/issues/229)). How FileMaker Pro tickles such a session (6.1)
is not recorded.

When FileMaker Pro opens a related file: the solution of s03 names 63
files through its data sources, and over that whole 105-second session
Pro opened 19 of them (19 Opens, 19 Logins, 592 requests in all). It left
out files OpenMaker opened at the root's open, and opened one OpenMaker
never reached, presumably through a script (#983). So Pro opens a related
file when something first needs it, not every file at the root's open
(Medium). The Open times, mapped coarsely from the capture's event log but
in order, show two groups. 16 of the 19 opened within about 11 s of the
root's open. The other 3 opened about 55 to 79 s in, after user actions.
Whether all 16 opened before the first window was drawn is not known. The
window's size was not recorded. Its zoom was probably 150%: the startup
script that sets it and fits the window ran, since its script member was
read. One of the 3 later files is read on the first layout only by the
conditional formats of 17 objects right of x 1,093 pt, on a layout about
1,970 pt wide. Offline, over the recorded layout and schema, the files
that layout's objects read give exactly those 16 when every object's data
counts (fields, merge fields, portals, value lists' fields) but its
formulas count only for objects inside a view at the top left. That held
for every view tried, from 600 × 400 to 1,092 × 600 pt. Counting every
object's formulas adds that file. Counting only objects in view misses
files Pro opened, except near 1,092 pt wide (#1120). So Pro evaluates a
formula that only changes how an object looks when it draws the object,
and opens its file then (Medium: inferred from which files opened in one
capture; no evaluation was observed). A file on another host is
opened the same way on one connection to that host: FileMaker Pro 26.0.3
opened a file there and that file's own related file on the same
connection, with the same account (OpenMaker Server as both hosts,
2026-11-17; private capture) (High for OpenMaker Server; FileMaker Server
unmeasured).

**What FileMaker Pro reads when it opens a related file** [O: the key paths
of every request in s03, relayed with no names, values or payloads,
2026-11-17; the copy of the solution is older than the one OpenMaker is
measured on]:
- **Each of its 18 related opens:** Open, `[2]` (before Login, as for the
  opened file, 5.6.9b), op63 and Login. Then:
  - targeted reads of the account's own security entries (`[23, 1, …]`,
    two requests) and `[23, 4]`;
  - the schema `[3]`, the data sources `[32]`, the value lists `[33]`
    and `[64]`;
  - the script and theme catalogs' indexes `[17, 1]` and `[6, 1]`;
  - the index and three members of `[23, 3]`, and `[65]`.

  That is 17 requests per file, one after another on the root's
  connection. The opened file's own open read the same, with its layout,
  theme, font and script members besides (above).
- **Read once:** `[2]` was read once per file, before Login, in all 19
  files, and never again in the session.
- **Read when first needed:**
  - **A table's field catalog `[T, 3]`.** None at any file's open. In 17
    of the 19 files, the catalog of one or two tables came later, each just
    before that file's first query on it. The first came about half a
    second after the last of the 16 early opens. Two files read none.
  - **Custom functions `[16]`.** Read for one related file only, after its
    open. Never for the opened file.
  - **Fonts `[25]`.** Read at the opened file's open. Of the related files,
    2 of 18 read them, a minute or more later.

High for which keys each open read. Medium for why each later read came
when it did: that is inferred from its position in the session.

OpenMaker's window opens a hosted file's related files when its layouts
and calculations first read them (desktop-client.md, "Opening files",
#983) [implementation]; other callers of its provider open them all when
the calculation schema is first built. Either way they open as FileMaker
Pro opens them, on the connection of the file the user opened
[implementation, [#229](https://github.com/OpenMaker-Dev/openmaker/issues/229)]: each
has a handle of its own on it (`om_session::Session::share`), with its own
`Open` naming that host, op63 (client file id `-(transidlo + 2)`) and
`Login` (5.4) with the same account, and no host lookup, TLS handshake or
`Connect` of its own. A file is opened on a connection once: another
reader of it there (another file's related file, a file a script opens)
for the same account and password holds that open, with no Open, op63 or
Login of its own, takes its own copy of each change notification for it,
and the op62 that closes it goes when its last holder lets go (#1011). A
reader with another account opens it on a connection of its own, since a
host keeps one entry per file on a connection and a second Open there
replaces the first (OpenMaker Server does; FileMaker Server's answer is
unmeasured). The connection's transaction counters are one, and
its Tickle carries one record, session 1, while any file is open, as
FileMaker Pro's did with two files open (6.1). A related
file of a file opened from this computer is opened through its own host in
the file's family, on a connection of its own. It keeps the account and password,
the password zeroed when dropped, from the user's open only until the
related files are opened (the first calculation schema) or the file is
closed. A file on the opened file's host first reached when the schema is
built again (a data source's next path, after another host turned out not
to have its file) opens with the account the client keeps for the file's
scripts' opens until a reconnect (#978) [I: OpenMaker's choice; when
FileMaker Pro asks for such a file is unmeasured]. A data source's paths are tried in their order, up to 16, each
on the host and port it names (file-metadata.md 1.1): a relative path on
those of the file whose data source it is, an `fmnet` path on its own.
FileMaker Pro 26 does not ask first for a file on another host or port
than the opened file's: it sends that host a Login (5.4) with the account
and password the first file was opened with, then one with an empty
password, and asks the user for an account once both are refused; a file
the second host's file names by a relative path then opens in the session
already open on that host, with the account that host accepted (observed:
op80 Logins of a two-host capture, classified outside this repository;
#991). Which account the empty-password Login names, and what it stands
for, are not recorded here.

OpenMaker sends such a file, on a verified connection to its host and
port ([#272](https://github.com/OpenMaker-Dev/openmaker/issues/272)): an account that host admitted in this session (typed for one
of its files, or the opened file's), else the opened file's account once
for that host. After any of that host's files refused it, none is sent
it again, and the search ends there until the user signs in to that file
with an account of its host's, which opens it and that host's other files
the data sources name. A host's files after the first open in the first
one's session there, related files and the files a script opens alike
(#1011). A host whose certificate is not trusted gets no
Login. No empty-password Login is sent [I: OpenMaker's choice, until what
it stands for is known]. Otherwise:
- An Open answered 4 ("file missing", above) or 0x4fb5 moves on to the
  next path.
- The first file the host has, or a file already open (the opened file
  included), is the data source's file.
- A host that could not be reached (no connection: host lookup, connect
  or TLS, before any Open or Login) ends the search for now, not for the
  session ([#229](https://github.com/OpenMaker-Dev/openmaker/issues/229)): the data source waits, its warning given, and its paths
  are tried again once 30 seconds have passed, when a read next wants one
  of its occurrences (on demand) or the schema is built again. No Login
  went there, so an account offered there once (#1010) may be offered when
  it is reached [I: OpenMaker's choice; whether and when FileMaker Pro
  tries a host again is unmeasured].
- Any other failure ends the search, the account refused included. That
  file is there, so it gets no second Login: the refusal is remembered for
  the session (below), whichever data sources list the file.

What is observed:
- FileMaker Server answered 0x4fb5 (FileMaker's `kFileNotFound`) to the
  Opens of related files it does not host (#739). OpenMaker Server answers
  4.
- Claris documents that FileMaker Pro searches a data source's paths in
  their order and uses the first file it finds.

What is inferred: which answers Pro treats as "not found" for that search,
and that a refused account stops it there. Pro's handling of either was
not measured. A related file that cannot be opened is tried once per open
and reported. A file whose Login was refused is not sent that account and
password again in the session, by any path: the related files of the
first file, a file a script opens (Open File, Perform Script in another
file, Go to Related Record), that file's own related files, a Reconnect.
It fails at once with the refusal it gave, without a connection, so a host
that counts failed Logins sees one per file per session. Opening the first
file again starts a new session. A Login refused because the host held
back the client's address (214) judged no credentials and is tried again
[I: OpenMaker's choice]. When a window's layout first needs a file that
refused, the window asks for another account for it, as FileMaker Pro does
for a local file (local-files.md 4, *Accounts*): each answer is one Login,
and a new pair is remembered as the first was. At most 128 Opens are made
(failed ones and each path tried included): one at a time on the opened
file's connection, which carries one request at a time ([#237](https://github.com/OpenMaker-Dev/openmaker/issues/237)), and 4 at a
time on connections of their own. A data source naming a file already open needs no Open and counts
toward no limit; discovery queues at most 4096 data sources. The alert
retains at most 20 lines, including a count of omitted problems.
Closing the file's last window signals cancellation as Close is queued,
stopping further batches and stopping a file's next operation after the
current network operation returns. Credentials and related sessions are
then dropped. Closing another window keeps the file's hydration and
sessions alive; its surviving windows share warning history until reopen.
A connection has one keepalive (6.1), whatever files it carries. Losing it
loses every file on it and is shown as losing the host (6.3); on this
computer, a related file's own connection can be lost alone. A refused
Open or Login, or a failed read of one file, closes that file alone (op62)
and leaves the connection and the other files open. The client-only ids
the client gives a related file's members (calc-values.md 13) never go on
the wire: its downloads, record lists, global values and finds name the
file's own table ids, under its own `fileid`. Nothing is written to a related file:
layouts showing its tables, finds through them and changes to their
records are refused (error 3).

The window lists these problems in one alert after the file opens. That
is OpenMaker's choice: what FileMaker Pro shows for them is unmeasured
(#741). Each line says which file it is about:
- A related file is named with the data source and the file that name
  it.
- A data source is named with its file and by its name, never by its id.
  One without a name that reads as one is named by up to three of the
  occurrences that use it, which FileMaker Pro's relationships graph
  shows ([#233](https://github.com/OpenMaker-Dev/openmaker/issues/233)). A name past 100 characters is cut.
- The host's Open answers and Login errors are in words, with the status
  or FileMaker error number only as detail.
- An occurrence whose table its related file does not have is named by
  the occurrence. No cause is given: the file as the host sent it has no
  definition of that table, and why is not known.

A data source whose file path list does not decode or reassemble is said
to have one OpenMaker cannot read. One whose paths are all forms OpenMaker
does not follow is said to name no file on the file's host. A data source the
occurrences use but the file's data sources `[32, 5]` do not hold, as the
host sent them, is said to be missing from them. One they hold with
neither a name nor a path list is said to have neither. A name that is
empty or does not decode is said to be so. Why any of these happens is not
known ([#233](https://github.com/OpenMaker-Dev/openmaker/issues/233), file-metadata.md U11). The same line reached twice (one file
named by two data sources) is listed once.

### 5.6 Perform reply layout

Pinned against capture `2026-10-27-s03-direct/conn-000` (request_ids 6=op78,
8=op90, 14=op80, 456=op86), the omniORB client stub `_objref_RCFMP12::Perform`
(nm `0x14e8250`), the server servant `RPO_i::Perform` (nm `0x14eee00`),
`RCNetworkStack::DispatchTransaction` (nm `0x14ef060`), the two reset primitives
`RCTransaction::ResetCallPush` (nm `0x1516340`) / `ResetPerformPush`
(nm `0x15163e0`), the 5-arg receive ctor (nm `0x1514bd0`), and the per-op
server/client handlers listed below. (Ghidra image base = nm + `0x100000`.)
Decoders: `re/work/network/decode_perform_reply.py` (reassembles GIOP fragments,
splits the two reply sequences) and `pop_result.py` (LIFO pop of a result stack);
saved decompiles in `re/work/network/declist_reply_out/`. Sensitive values
(the per-file nonce, session/incarnation words, license key) are given by
offset/length/type only. Confidence High unless noted.

#### 5.6.1 CORBA reply body - the return value is void, two sequences follow

DWARF prototype (from the stub and servant, both agree):

```
void Perform(this, ULong connectionid, char *sessKey, ULong maxreturn,
             OctetSeq *parms /*inout*/, OctetSeq_out backchannel /*out*/)
```

**Correction to earlier inference:** `Perform`'s on-wire CORBA **return type is
`void`, not `boolean`** (DWARF `void Perform`; confirmed by the capture - no
return octet precedes the sequences). The old "boolean Perform" in section 2 and
README F21 was an unverified guess; success/error is carried inside the reply
payload instead (5.6.3). Per CDR a void return marshals nothing, so the reply
body (status `NO_EXCEPTION`, GIOP 1.2, CDR little-endian) is exactly two
`sequence<octet>` in IDL order:

1. **inout `parms`** - `u32 LE` length + bytes = the **response** (5.6.3).
2. **out `backchannel`** - `u32 LE` length + bytes = the back-channel piggyback
   (5.6.3b). DWARF names this out parameter `backchannel`.

Each sequence length is a 4-aligned `u32 LE` relative to the GIOP message start;
a pad byte can sit between the two sequences (the Login reply has one).
FileMaker Server does not zero these pad bytes (reply 10 in conn-000 has
`59 5b`), so a receiver must ignore their values; OpenMaker writes zeros. Worked
example, Open reply (request_id 6, unfragmented, body at message offset `0x18`,
51 bytes):

| body off | bytes | meaning |
|----------|-------|---------|
| `0x00` | `28 00 00 00` | inout `parms` length = 40 |
| `0x04`..`0x2b` | 40 bytes | response result stack (5.6.4) |
| `0x2c` | `03 00 00 00` | out `backchannel` length = 3 |
| `0x30`..`0x32` | `fe 09 9e` | back-channel (0xFE server-CPU report) |

There is no leading boolean and no trailing field; `4 + 40 + 4 + 3 = 51` accounts
for the whole body. The earlier two empirical guesses failed because they
(a) expected a boolean return octet first, and (b) expected the response to be a
full 20-byte-header RCTransaction - neither is present.

**Large replies may fragment.** A GIOP 1.2 reply can set the "fragment
follows" flag (bit 1, flags `0x03`) and continue in `Fragment` messages (type 7,
flags `0x01`) carrying the same `request_id`. Reassemble by concatenating the
Reply body with each Fragment's bytes after its 12-byte GIOP header + 4-byte
fragment `request_id`, then parse the two sequences. **Correction:** there is
no 26 KB threshold; when and how FileMaker Server fragments, and why a client
need not, is in 5.6.6c ("GIOP framing of large replies").

#### 5.6.2 arg3 is `maxreturn`, not the request length

**Correction:** DWARF names `Perform`'s third argument `maxreturn`; it is the
**maximum reply size the client will accept**, not the request transaction
length. `RPO_i::Perform` rejects `maxreturn < 0x14` or `> 0x200000`, then:
`parms.length(maxreturn)` grows the inout buffer to that size, runs
`DispatchTransaction(conn, maxreturn, bufptr, contentlen)` in place (where
`contentlen` = the incoming `parms` length = the request, bounds-checked
separately to `<= 0x200000`), and finally `parms.length(responseLen)` trims the
inout sequence to the actual response length `DispatchTransaction` returns. The
request RCTransaction length is therefore the **inout sequence length on the
request**, independent of `maxreturn`. Capture: op78 req `maxreturn`=2334 with an
87-byte request; op90 req `maxreturn`=1048876 with a 46-byte request. (This
supersedes section 5.1's "`len` = byte length of the RCTransaction".)

#### 5.6.3 The response is a header-less result stack; status is the last field

Request and reply use the **same buffer but different framing**:

- **Request** `parms` (client, `ResetCallPush`): a 20-byte header **+** LIFO body.
  `ResetCallPush` writes the header and sets the write cursor to `0x14`. DWARF
  `RCOpHeader` field names (refines section 4.1): `opcode`(0), `taskid`(1),
  `transidhi`(2-3, `u16` BE), `transidlo`(4-7, BE), `sessionid`(8-11, BE),
  `fileid`(12-15, BE), `idletime`(16-19, BE). (So section 4.1's "target file id"
  at off 8 is `sessionid`; the file id is at off 12.) The receive ctor
  (nm `0x1514bd0`) parses these same fields back, big-endian via a pshufb swap,
  and sets the Pop cursor to the content length (the stack top).
- **Response** `parms` (server, `ResetPerformPush`): **no header at all** -
  `ResetPerformPush` just sets the content cursor to `0`, and the op's `Perform`
  pushes reply fields from offset 0. So the inout sequence that comes back is the
  **pure LIFO result stack**. Proof: the Login reply (5.6.5) is 7 bytes, smaller
  than a 20-byte header, and decodes cleanly as three stacked fields.

Because it is a LIFO stack, the client pops reply fields in reverse of the
server's push order, so **the last field the server pushes (the status/error
code) sits at the very END of the sequence and is popped first.** Every observed
reply ends with a `u16` = 0 success indicator:

| op | last field (seq tail) | 0 means |
|----|-----------------------|---------|
| 78 Open | `u16 status` | opened; named statuses, their sources and observations are in 5.5 |
| 80 Login | `u16 errorCode` | logged in (0xd0 change-required, 0x20d0 external/NT) |
| 90 DownloadWithLock | `u16 lockError` (`RCDownloadWithLock::CallPopAddition`, nm `0x110e770`) | lock+download ok |
| 86 QueryProgram | `u16 status` | query ok |

Integers, strings, and keys in the result stack use the same encodings as the
request body (section 4.2 / 9): big-endian packed ints; `Push(unistring)` strings
= `[SCSU bytes ^0x5A][u32 BE length]` (length on top, popped first); `Push(uchar
const*)` keys = `[bytes][u8 length]` (bytes not XOR'd).

#### 5.6.3b The out `backchannel` sequence

`RPO_i::Perform` fills the out sequence from `RCConnection::PackBackChannel`
(nm `0x14ed9a0`), independent of the op. It is the same piggyback described in
section 6: first byte `0xFE` = server-CPU-load report (`0xFE`, a CPU byte, then a
short random/status payload via `Random_Bytes`). Observed values (all start
`0xFE`): `fe 09 9e`, `fe 09 4f`, `fe 1a`, `fe 1a e2 1e`, `fe 09 59 5b 34`. It is
2-5 bytes in these captures and is not part of the op's response; a clean client
reads it for CPU/idle accounting (section 6) and otherwise ignores it. It may be
empty when there is nothing to report (inference; not seen empty here).

#### 5.6.4 op78 Open (RCOpen) reply - fully decoded

`RCOpen::Call` (nm `0xd62980`, Mac `0x185eefe`) pops; `RCOpen::Perform`
(nm `0xd63230`) pushes. Result stack = 40 bytes in conn-000 id 6, consumed
exactly (cursor reaches 0). Offsets are within the result stack (= reply body
offset minus 4):

| pop # | stack off | type | field | conn-000 value |
|-------|-----------|------|-------|----------------|
| 1 | `0x26` | `u16` BE | `status` | `0x0000` (opened) |
| 2 | `0x1c` (len@`0x22`) | string | `serverMsg` | SCSU len 6 = the server's name |
| 3 | `0x18` | `u32` BE | **`file[0x3ac]` per-file login nonce** | 4 bytes, redacted |
| 4 | `0x17` | `bool` | `file[0xc]` | 0 |
| 5 | `0x13` | `int` BE | `file[0xaa0]` | 132 |
| 6 | `0x11` | `u16` BE | `file[0xaa4]` | 9 |
| 7 | `0x10` | byte | (status-0 marker) | 0 |
| 8 | `0x0c` (len) | string | `file[0x170]` | empty (len 0) |
| 9 | `0x08` | `u32` BE | flag | `0x00a10000` |
| 10 | `0x00` | `u64` BE | `file[0xc08]` session/incarnation | 8 bytes, redacted |

**The per-file nonce `file[0x3ac]` is pop #3: the big-endian `u32` immediately
below the `serverMsg` string.** It feeds the login key derivation (section 4.3,
`~file[0x3ac] + file[0xa64]`). A clean client reads status first; if 0, reads
`serverMsg` then this u32 and keeps it for the Login that follows.

Pop #5 (`file[0xaa0]`) is the host's id for the open file: the
administrator's messages name the file by it (6.5). s07 got `0x165` at two
sign-ins while the host kept the file open, and `0x168` after the host had
closed and opened it again.

**Refused open.** Under Block New Users (6.5) FileMaker Server pushes the
same 40 bytes with status 1638 (`0x0666`, `kHostBlockNewUsersEnabled` in
`Draco::Error::Codes`), the bool at `0x17` set and the ids, flag
and nonce zero. s07 conn-001 request 6, with the server's name as
`server`:

```
00000000 00000000 00000000 00000000    session 0, flag 0, empty file[0x170]
00 0009 00000000 01 00000000           marker, 9, file id 0, bool 1, nonce 0
293f282c3f28 00000006 0666             serverMsg (XOR 0x5A), its length, status 1638
```

#### 5.6.5 op80 Login (RCLogin) reply

`RCLogin::Call` (nm `0xd6a5f0`) pops; `RCLogin::Perform` (nm `0xd66c00`) pushes.
Result stack = 7 bytes in conn-000 id 14, consumed exactly:

| pop # | stack off | type | field | conn-000 |
|-------|-----------|------|-------|----------|
| 1 | `0x05` | `u16` BE | `errorCode` | `0x0000` (success) |
| 2 | `0x02` (u8 len@`0x04`) | key | `resultKey` (`Push(uchar const*)`) | len 2 = `01 03` |
| 3 | `0x00` | `u16` BE | `version` | `0x0009` (=9) |

FileMaker Server 26.0.3 answered FileMaker Pro 26.0.3's Login to the lab
test file in capture s154 (login type 0, the file's [Full Access] account
with its own password) the same way: `00 09 01 02 02 00 00`, key `01 02`
for that account's record `02` [O].

FileMaker Server pushes `version`, `resultKey` and `errorCode` on every path
(`RCLogin::Perform`), so a refused Login is 5 bytes: `00 09`, an empty key
`00`, the error (observed [O]: the four 212 replies in s07 are each
`00 09 00 00 d4`). OpenMaker Server answers its refusals in this form. Before them, deepest, it pushes one more
field for three cases, and `RCLogin::Call` pops it after `version`
[from bin]:

| `errorCode` | extra field | client |
|---|---|---|
| 0, after an OAuth or Sign in with Apple login | str: the account name | keeps it as the session's user name, with an empty password |
| `0xd0` (208, "Password does not contain enough characters") | u16 BE: the minimum length (`DBMinPassLengthError`) | reports 208 with the minimum |
| `0x20d0` (8400) | enc-bytes: the external authentication continuation token | stores it as the next Login's field 11 (`SetNTAuthentication`) |

So `0xd0` is the answer to a new password that is too short, not "password
change required": an expired password or one that must be changed answers 211
(local-files.md 2.2). `resultKey` is the account's key (session-identity.md 1;
2 bytes here), not the account secret.

#### 5.6.6a op90 DownloadWithLock request body (observed)

Derived from traffic only: all 359 op90 requests in conn-000 parse as

```
[ 22 bytes: 00 x20, ff ff ][ 00 ][ key path: segments as [u8 len][bytes] ][ u8 path length ]
```

The trailing byte equals the encoded byte length of the key path in every request.
The header carries the open file's `sessionid` and `fileid` (4.1); `maxreturn` was
`0x10012c` throughout. Example, the first op90 after Open requests key path `[2]`:
body tail `00 01 02 02`. Meaning of the fixed 22-byte prefix is not established;
send it as observed.

**Shared framing (op90, op92, op86).** The 22-byte prefix is the empty case of
a framing the three upload/download ops share. In push order:

```
[ payload, N bytes ][ u32 BE N ][ 16 bytes, zero ][ u16 0xffff ][ u8 0 ]  then per op:
  op90: [ key path ][ u8 path length ]                (N = 0)
  op92: [ key path ][ u8 path length ][ u8 flag ]     (payload = key list, 5.6.6b)
  op86: (nothing more)                                (payload = query program, 5.6.7)
```

So op90's `00 x20, ff ff` is `u32 0` + 16 zero bytes + `ff ff`. Verified by
rebuilding every op90 and op92 request in conn-000 from its parsed fields
(`capture_server.rs`); the header `idletime` (4.1) is the only difference.

**Correction (2026-10-28, see 5.6.6c):** the fixed bytes are not opaque. The 16
zero bytes are two `u64` handles (`perform` 0, `cursor` 0), `0xffff` is an
`i16 depth` of -1 ("start a transfer") and the `u8 0` is the length of an empty
`copy key`. A follow-up request for the next page of a large reply echoes the
nonzero values the server returned in the same fields (5.6.6c).

#### 5.6.6 op90 DownloadWithLock (RCUploadDownload / RCDownloadWithLock) reply

Base `RCUploadDownload::Perform` (nm `0x1105160`) writes the downloaded HBAM
page/slot bytes into the response buffer verbatim (the on-disk packed-slot
format; `RCUploadDownload::CallPopAddition` and `PerformPushAddition`,
nm `0xf55be0`/`0xf55bf0`, are empty - no extra wire framing). The subclass
`RCDownloadWithLock::CallPopAddition` (nm `0x110e770`) then pops a trailing
`u16 mLockError`. The result stack is:

```
[ HBAM packed-slot bytes, N ][ u32 BE N ][ 22 bytes ][ u16 BE lockError  <- tail ]
```

So a clean client reads the trailing `u16` lock status, reads `u32 N` from the
start of the trailer, and hands the leading N bytes to the HBAM slot decoder
(`om_hbam::SlotReader`). The first such reply after Open carries the `[2]` subtree,
which holds `mInternalCreation` at `[2, 254]` (needed for Login, 4.3/5.4).

Observed across the 359 op90 replies in conn-000 (High; checked by
`crates/om-protocol/tests/capture_server.rs`):

| replies | N | 22-byte block | meaning |
|---------|---|---------------|---------|
| 356 | > 0 | `00 x20`, `00 02` | the subtree, as below |
| 2 | 0 | `00 x16`, `ff ff`, `00 x4` | no such key (both for path `[0x8001, 8]`) |
| 1 | - | (other shape) | see "Other reply shape" below |

**Correction (2026-10-28, see 5.6.6c):** the earlier text called the 22-byte
block unexplained. It is `[u64 perform][u64 cursor][i16 depth][u8 copy-key
length 0][i16 inline-value length][u8 node state]`, and the full reply is
`[slots N][u32 N][block][u16 lockError]` (with an inline value between the
copy-key length and its own length when node state is 1). Node state is
`HBPosition::NodeState`: 0 missing, 1 terminal (value inline), 2 non-terminal
(slot stream). So `00 x20, 00 02` = perform 0, cursor 0, depth 0 (stream
complete), empty key, no inline value, node state 2; `00 x16, ff ff, 00 x4` =
depth -1 (no stream), node state 0. An existing but empty list is not "no such
key": it comes back as the 2 bytes `80 40` with the `00 02` block.

**The slot stream is one bracketed list relative to the requested path.** It
starts with `0x80` (opens the requested node's list) and ends with the `0x40`
that closes it. Each child, in key order, is a leaf chunk, or a push chunk
followed by its own `0x80 ... 0x40` list (file-format.md 3.6). Unlike the leaf
chain on disk, the stream never re-pushes a path at a page boundary. Every
chunk uses the smallest code for its key and value sizes (`minimal_code`), and
an empty leaf value uses value class 0 (code `0x00` for a 1-byte key), not a
zero-length class-6 value. Re-encoding the decoded subtree under these rules
reproduces all 356 data replies byte for byte, so a server can generate the
stream from a file's key tree (`om_hbam::Node::to_wire`).

**Other reply shape (resolved 2026-10-28, see 5.6.6c).** The reply to request
568 (path `[0x1f, 5, 0x58, "SVG "]`, 856 bytes) is
`[ 00 x20, ff ff, 00 ][ raw value bytes (an SVG document) ][ 03 3c 01 ][ u16 0 ]`.
**Correction:** this is not the request's prefix echoed. It is the normal
trailer for a target key that holds a value instead of a list: N 0, perform 0,
cursor 0, depth -1, empty copy key, the 828-byte value, its `i16` length
`03 3c` and node state `01` (terminal). The neighbouring request 562 for
`[0x1f, 5, 0x51, "SVG "]` returned the slot form because that key is a node.
The shape depends only on the key's node state in the file.

**Oversized subtrees (resolved 2026-10-28):** FileMaker Server pages them; see
5.6.6c.

#### 5.6.6b op92 DownloadList request and reply (observed)

Fetches selected children of one key path in a single round trip; the official
client uses it to fetch records by id (`[table, 5]` with record keys) and
asset entries under `[0x1f, 5, x]`. 13 exchanges in conn-000, all verified.

Request: the shared framing (5.6.6a) with the payload = a **key list**: a
bracketed slot list (`0x80`, one empty leaf per key in key order, `0x40`),
e.g. `80 00 01 00 02 ... 00 19 40` for records 1..25 or `80 08 81 9a ... 40`
for 2-byte record keys. Then the parent path, its length, and a flag byte:
`0` for the 9 record lists, `1` for the 5 `[0x1f, 5, x]` lists. The flag's
meaning is not established. `maxreturn` was `0x10012b`.

Reply result stack:

```
[ slots, N ][ u32 BE N ][ 19 bytes, zero ][ u16 BE status <- tail ]
```

The slots are the parent node restricted to the requested children **that
exist**, in the Download wire form (5.6.6): every record list returned all of
its keys; each flag-1 list asked for 48 keys and got 5 to 7. Re-encoding the
decoded parent reproduces all 13 replies. What FileMaker Server returns for a
missing parent is not observed; OpenMaker Server returns `80 40`.

OpenMaker also uses this existing selected-child operation for an
authenticated related file's root catalogs: parent `[]`, keys `[3]`
(tables), `[16]` (custom functions), `[32]` (data sources) and `[33]`
(value lists), flag `0`. A joined file without a retained login challenge
also selects `[2]`; otherwise the pre-Login `[2]` reply is reused.
The response is split back into the same per-catalog subtrees before
schema decoding. Table field catalogs and the account's narrow privilege
paths are fetched separately. This application of the generic sibling
selection semantics is inferred, not an observed native catalog request;
the framing and paging are the existing op92 format above. Synthetic
OpenMaker Server round trips check the selection. Native root-catalog
selection remains unverified; no new batch transaction is defined.
OpenMaker Server rejects the root parent with lock error `200` for
accounts without Full Access (`connection.rs`, `readable` /
`credential_path`), even when the selected children are readable. On that
specific denial the client keeps the guard and downloads the same catalogs
by their individual paths, checking cancellation before each request. It
does not repeat Open or Login. Synthetic Read-Only, Data Entry and custom
account fixtures exercise that fallback and retain narrow privilege reads.

**Correction (2026-10-28, see 5.6.6c):** the 19 zero bytes are `[u64 perform]
[u64 cursor][i16 depth][u8 copy-key length]` (all 0: stream complete), and
`status` is the lock error (100 = file missing). The flag selects the locking
(`RCDownloadList::PerformOperation`): `1` locks the parent path once for the
whole list, `0` locks, copies and unlocks each requested child in turn.

#### 5.6.6c Large replies and paging

Sources: the server's shared `RCUploadDownload::Perform` (Linux nm `0x1105160`),
`HBAMSlot::S_SlotPack` / `S_SlotUnpack` (nm `0x1597e80` / `0x1598b00`),
`RCConnection::StorePositionAllocation` / `IsStoredPosition` /
`FreePositionAllocation` (nm `0x1510c60` / `0x1510dd0` / `0x15111e0`), the op
additions of `RCDownloadFromHost`, `RCDownloadWithLock` and `RCDownloadList`,
`RPO_i::Perform` (nm `0x14eee00`); the client loops `RCUploadDownload::Call` /
`MultiDownload` / `MultiUpload` (Mac `0x1d27a1c` / `0x1d28050` / `0x1d27d90`);
the omniORB 4.3.4 libraries shipped with both. Confirmed live against FileMaker
Server on 2026-10-28 for op90 and op92 (paged downloads and split uploads), and
against all 468 op90/op92/op86 replies in conn-000. Scratch:
`re/work/download/` (`blackbox.txt`, `live2/`, `live3/`, `pageprobe/`).
Confidence High unless marked.

**Summary.** No reply is larger than the client's `maxreturn`, and one reply
carries at most `0xFFF00` (1,048,320) bytes of slot data. A subtree, record
list or found set that does not fit is **paged**: the server keeps a cursor on
the connection, returns a page of whole chunks with a nonzero resume block, and
the client sends follow-up requests echoing that block until it comes back
zero. The same block lets the client upload a payload larger than one request.
`maxreturn` only sizes the reply buffer; it never causes paging for the
official client, which always leaves room for the trailer.

**The resume block.** Every op whose server class uses
`RCUploadDownload::Perform` (by vtable: 13, 14, 15, 70, 77, 86, 88, 89, 90, 91,
92, 93) frames its request and reply with the same fields after the payload. In
push order (the tail is popped first; integers big-endian):

```
request: [ payload, N ][ u32 N ][ u64 perform ][ u64 cursor ][ i16 depth ][ copy key ][ u8 copy-key length ][ op request addition ]
reply:   [ slots,   N ][ u32 N ][ u64 perform ][ u64 cursor ][ i16 depth ][ copy key ][ u8 copy-key length ][ op reply addition ]
```

With an empty copy key the reply's base trailer after the slot bytes is 23
bytes.

| field | request (client to server) | reply (server to client) |
|-------|----------------------------|--------------------------|
| `N` + payload | bytes uploaded with this request: the op92 key list or op86 query program; empty for op90 and for every download continuation | slot bytes in this page |
| `perform` | 0 on the first request, otherwise echoed from the previous reply | handle of the server's source position (op90: the file position at the target key; op92/op86: a temporary node that holds the upload and the operation's result); 0 when nothing is retained |
| `cursor` | 0 on the first request, otherwise echoed | handle of the server's transfer cursor. **Nonzero means call again**; 0 means this is the last reply |
| `depth` | -1 = start a transfer or continue an upload; >= 0 = continue a download (echoed) | -1 = no slot stream (op90 target missing or terminal, or an upload continues); 0 = stream complete; > 0 = more pages, the number of lists open at the cut |
| copy key | empty, or echoed while uploading | empty, except while uploading: the key path (relative to the transfer root) where the server stopped unpacking |

The copy key is encoded like the op90 key path: segments as `[u8 len][bytes]`,
then the `u8` total length. The handles are 8-byte server-private values
(FileMaker Server sends its own heap addresses, `00 00 7e 0f xx xx xx xx`
live); a client treats them as opaque and echoes them unchanged. The server honours only
handles it issued on the same connection and keeps them until the transfer
completes (or, by inference, the connection closes).

**Per-op additions.**

| op | request addition (after the block) | reply addition (after the block) | reply trailer |
|----|------------------------------------|----------------------------------|---------------|
| 90 | `[key path][u8 path length]` | `[value, V][i16 V][u8 node state][u16 lockError]` | 28 + V bytes |
| 92 | `[key path][u8 path length][u8 flag]` | `[u16 status]` | 25 bytes |
| 86 | none | none | 23 bytes |
| 89 | `[u32 host file id]` | none | 23 bytes (5.6.11) |

op90 node state (`HBPosition::NodeState`): `0` kMissing (no such key, or the
file is not valid; N 0, depth -1; FileMaker Pro deletes its local copy of the
key), `1` kTerminal (the key holds a value, not a list: the value comes back
inline as V raw bytes with N 0 and depth -1; FileMaker Pro stores it as the
key's value), `2` kNonTerminal (the key is a node: the slot stream, paged when
needed, with V 0). The server reads a terminal value into a 4096-byte window
(derived; the largest observed is 828 bytes). `lockError` is the result of the
lock op90 takes on the target key on the first request (100 = file missing);
intermediate pages carry 0 and the last page carries the result of releasing
it (derived; 0 in every live exchange). The data is returned even when the
lock fails (derived). op92's `status` is its lock error (5.6.6b); no op92 lock
spans pages.

**Correction to 5.6.7:** op86 has no status field. Its reply ends with the
copy-key length byte; the "`[u32 N][17 bytes, zero][u16 status]`" there is
`[u32 N]` + perform 0 + cursor 0 + depth 0 + copy-key length 0 (all 96 replies
in conn-000). Query errors travel only in the slot data (`03 02 <u16>`).
`RCUploadDownload`'s own push/pop additions are empty and op86 does not
override them.

**Server algorithm.** For one request, after popping the op's request
addition and the block:

1. **Validate.** If the cursor is nonzero but not issued on this connection,
   the copy key is malformed or the file is not open, answer an empty last page
   (N 0, perform 0, cursor 0, depth 0) plus the op's addition (derived from
   code; not exercised live).
2. **Continue a download** (cursor known, depth >= 0): go to step 6.
3. **Start a transfer** (cursor 0): create the source position (op90: look up
   the target key and record its node state; op92/op86: a new temporary node)
   and a cursor on it, and issue handles for both.
4. **Unpack the payload** (N > 0) into the cursor's node, starting at the copy
   key. If the payload's outermost list is still open, more upload follows:
   reply N 0, both handles, depth -1 and the copy key where unpacking stopped,
   plus the op's reply addition, without running the operation. A later part
   (cursor known, depth -1) is unpacked the same way, continuing from the
   echoed copy key. Unpacking ignores `0x80`; `0x40` closes one level, and the
   `0x40` that closes the outermost list completes the upload.
5. **Run the operation** (op90: lock the target and, if node state is not 2,
   set depth -1; op92: copy the requested children that exist; op86: execute
   the query), point the cursor at the result and set depth 0.
6. **Pack a page** of at most `min(maxreturn, 0xFFF00)` bytes (the response
   starts empty). Copy whole chunks from the cursor while the next one fits;
   never split a chunk (each on-page slot record is one chunk, 5.6.6). Depth
   starts at 1 inside the requested node's list, each push chunk (`0x20..0x3F`
   on the wire) adds 1 and each `0x40` subtracts 1; reaching 0 completes the
   stream. A page may end at any chunk boundary at any depth.
7. **Finish or retain.** If the stream is complete or depth is -1, free the
   cursor and the source position, set both handles to 0 and run the op's
   after-download step (op90 releases its lock). Otherwise keep both and return
   their handles.
8. **Push the trailer:** `N`, perform, cursor, depth, the copy key (always
   empty on a download reply), its length, then the op's reply addition. Each
   push is written only if it fits in `maxreturn`; no room is reserved for the
   trailer (see "`maxreturn` truncates" below).

**Client algorithm (FileMaker Pro).**

1. Send the first request with perform 0, cursor 0, depth -1, an empty copy key
   and the payload. The payload is packed as whole chunks up to `0xFFF00` bytes;
   a larger payload is uploaded in parts first (below).
2. For each reply, pop the op's reply addition, then require the remaining
   result stack to be 23 to `0x100000` bytes (otherwise the call fails with
   error `0xffff`), then pop the copy key, depth, cursor, perform and `N`.
3. If **cursor is 0**, this is the last reply: unpack its `N` slot bytes and
   stop. Depth is not consulted.
4. Otherwise unpack the page in the background and send a continuation with a
   new transaction id: `N` 0 (no payload), perform, cursor, depth and copy key
   **echoed exactly**, then the same request addition as the first request
   (op90: the same key path; op92: the same parent path and flag; op86:
   nothing). The continuation reuses the transaction's buffer size, so its
   `maxreturn` is the same (inference; FileMaker Server accepts any).
5. Stop on any transport error or nonzero op error.

Each page is unpacked into the same destination with the key path carried
across page boundaries, so the slot bytes of all pages concatenate to exactly
the stream one reply would have carried (verified live, below).

Uploading a large payload (`MultiUpload`): the client sends it in parts of up
to `0xFFF00` bytes of whole chunks, each with depth -1. The first part has
perform 0 and cursor 0; each later part echoes perform, cursor and copy key
from the previous reply. Every part but the last gets an `N` 0, depth -1 reply
with the handles; the reply to the part that closes the payload's outermost
list is the first download page.

**Constants.**

| constant | value | source |
|----------|-------|--------|
| slot bytes per reply | at most `min(maxreturn, 0xFFF00)`; 1,048,320 for the official client | server packer |
| payload bytes per request | at most `0xFFF00` | client packer |
| `maxreturn` op90 / op92 / op86 | `0x10012c` / `0x10012b` / `0x10002a`; leaves 556 / 555 / 298 bytes above `0xFFF00` for the trailer | every captured request; the client never sends less for these ops |
| `maxreturn` and request length accepted | `0x14` to `0x200000`; outside this, or for an unknown connection id, CORBA `BAD_PARAM` | `RPO_i::Perform` |
| result stack accepted by the client | 23 to `0x100000` bytes after the op's reply addition | `MultiDownload` |
| op90 inline value | up to 4096 bytes (derived) | `RCDownloadFromHost::PerformPushAddition` |

**`maxreturn` truncates; it does not page.** `RPO_i::Perform` sizes the inout
buffer to `maxreturn` and the transaction pushes into it. The packer respects
the limit, but trailer pushes that do not fit are skipped, so a small
`maxreturn` yields whole chunks plus whichever trailer fields happened to fit,
and the cursor stays allocated (and op90's lock held) with no handle returned.
The model (slot bytes = longest whole-chunk prefix within
`min(maxreturn, 0xFFF00)`; then `u32 N`, `u64` perform, `u64` cursor,
`i16` depth, the copy key and its `u8` length, and the op90 addition, each
written only if it fits) reproduces all 34 black-box replies for subtree `[3]`
(2690 slot bytes) byte for byte (`re/work/download/blackbox-model.txt`):

| `maxreturn` | reply bytes | after the slot bytes |
|-------------|-------------|----------------------|
| 256 | 256 | 255 slot bytes, then only the copy-key length `00` |
| 2689 | 2689 | 2689 slot bytes (the final `0x40` does not fit), no trailer |
| 2698 | 2698 | `00 00 0a 82`, depth `00 00`, `00`, node state `02` |
| 2699 | 2699 | `00 00 0a 82`, depth `00 00`, `00`, value length `00 00` |
| 2701 | 2700 | the whole trailer except `lockError` |
| >= 2718 | 2718 | complete |

A server matching FileMaker exactly reproduces this rule; the official client
never depends on it.

**GIOP framing of large replies (corrects 5.6.1).** There is no fixed fragment
size and no 26 KB limit. omniORB 4.3.4 in both the Linux server and the Mac
client keeps its defaults (no FileMaker engine sets them): `giopMaxMsgSize`
2,097,152, `giopBufferSize` 8192, direct-send cut-off 16384 bytes. An octet
sequence at or above the cut-off is sent directly, so every FileMaker Server
reply whose `parms` reaches 16384 bytes (52 of 52 in conn-000) is exactly one
Reply message (flags `0x03`, total length a multiple of 8) carrying the headers
and nearly all of `parms` (up to 612,804 bytes in one message), then one
Fragment (type 7) of 10 to 21 bytes: its request id, the last few `parms`
bytes, alignment padding and the `backchannel` sequence. Smaller replies (up to
15,407 bytes observed) are not fragmented. FileMaker Pro reassembles fragments
but does not need them: its omniORB accepts any message up to 2 MiB, so one
unfragmented Reply for the largest page (`maxreturn` `0x10012c` plus about 100
bytes of GIOP headers) is within its limit (derived from the shipped defaults;
not yet exercised against FileMaker Pro). A clean server may send one
unfragmented Reply.

**Test vectors (live, 2026-10-28, test project, file id `0x166`).**
`PP PP PP PP PP PP PP PP` and `CC CC CC CC CC CC CC CC` stand for the perform
and cursor handles the server returned (exact bytes in
`re/work/download/live2/` and `live3/`). Requests are whole transactions
(20-byte header, 4.1); replies show the bytes after the slot data.

op90 `[3]` in three pages:

```
request 1, maxreturn 96:
  5a 00 0000 0000000b 00000001 00000166 00000000
  00000000 0000000000000000 0000000000000000 ffff 00 0103 02
reply 1 (96 bytes): 68 slot bytes, then
  00000044 PPPPPPPPPPPPPPPP CCCCCCCCCCCCCCCC 0004 00 0000 02 0000
request 2, maxreturn 1664:
  5a 00 0000 0000000c 00000001 00000166 00000000
  00000000 PPPPPPPPPPPPPPPP CCCCCCCCCCCCCCCC 0004 00 0103 02
reply 2 (1664 bytes): 1636 slot bytes, then
  00000664 PPPPPPPPPPPPPPPP CCCCCCCCCCCCCCCC 0004 00 0000 02 0000
request 3, maxreturn 0x10012c: request 2 with transid 0000000d
reply 3 (1014 bytes): 986 slot bytes, then
  000003da 0000000000000000 0000000000000000 0000 00 0000 02 0000
```

The three pages' slot bytes concatenate to the 2690-byte stream of one unpaged
request (trailer `00000a82`, 16 zero bytes, `0000 00 0000 02 0000`). Page 1
ends `... 03 02 80 02` and page 2 begins `1e 21 12 ...`; both cuts are at depth
4 (1 + pushes - pops before the cut).

op92, parent `[3]`, keys `10 | 11 | fb`, flag 0, with the upload split inside
the pushed child `11`:

```
request 1:
  5c 00 0000 00000010 00000001 00000166 00000000
  80 00 10 20 11 80 00 01  00000008 0000000000000000 0000000000000000 ffff 00  0103 02 00
reply 1 (27 bytes):
  00000000 PPPPPPPPPPPPPPPP CCCCCCCCCCCCCCCC ffff 0111 02 0000
request 2:
  5c 00 0000 00000011 00000001 00000166 00000000
  40 00 fb 40  00000004 PPPPPPPPPPPPPPPP CCCCCCCCCCCCCCCC ffff 0111 02  0103 02 00
reply 2: the same 2690 slot bytes an unsplit request returns, then
  00000a82 0000000000000000 0000000000000000 0000 00 0000
```

Reply 1's copy key `01 11` (length `02`) is where the server stopped, inside
child `11`; request 2 echoes it. Splitting the flat key list before its closing
`0x40` instead gives an empty copy key. A paged op92 download behaves like op90
(maxreturn 41: 16 slot bytes, depth 4; the continuation completes it).

**Not yet verified.**

- op86 paging was not exercised live (its found sets here are tiny). It shares
  `RCUploadDownload::Perform` and the client loop with op90 and op92, so the
  mechanism is the same (High, derived).
- The official client's own continuation requests are not in any capture (no
  captured reply exceeded 1 MiB); their layout comes from the Mac client code,
  and FileMaker Server accepted it live.
- An abandoned paged transfer keeps its cursor, source position and op90 lock
  until the connection closes (inference; disconnect cleanup not read).
  Whether FileMaker Server limits how many transfers, or how many bytes, one
  connection holds is unmeasured. OpenMaker Server's limits are its own
  choice (`crates/om-server/src/transfer.rs`).
- The empty-last-page answer to an unknown cursor is derived from code only.
- Behaviour when the file changes between pages was not examined.
- FileMaker Pro accepting one unfragmented ~1 MiB Reply follows from its
  omniORB defaults; serve one from om-server to confirm.

To capture the official client paging on its own, the hosted file needs a key
subtree or a 25-record batch (the client fetches records 25 at a time with
op92) above 1,048,320 bytes of slot data, for example 25 or more records with
about 50 KB of text each, browsed through the capture relay. This spec does not
depend on it.

#### 5.6.7 op86 QueryProgram (DBRemoteQueryProgram) reply

`DBRemoteQueryProgram` subclasses `RCUploadDownload`, so the find result is
returned with the same mechanism: the server (`DBRemoteQueryProgram::Perform
Operation`, nm `0x10b47a0`) runs `DBQueryProgram::Execute` and writes the found
record set as **HBAM packed-slot records** into the response buffer, followed by
the common `u16 status` at the tail. On a query error the server embeds an error
slot built as `03 02 <u16 errorCode BE>` (`HBInt16Key errKey`) via `HBPosition::
Put` before returning. conn-000 samples (ids 456/502/510/546): result stacks of
29-32 bytes beginning `80 20 02 80 ...` (HBAM slot/key framing) and ending
`00 00` (status 0); the `80 20 02 80` head and the `08 xx yy` / `40 40` groups are
HBAM key/position encodings (file-format workstream). The trailer is 23 bytes
in all 96 replies: `[u32 BE N]` then the resume block (5.6.6c) with perform 0,
cursor 0, depth 0 and an empty copy key. **Correction:** op86 has no status
field; an earlier version of this section read the block's last bytes as one.
The request uses the shared framing of 5.6.6a with the query program as
payload, and large found sets page like op90 (5.6.6c).
Depth reached: the **wire
framing is HBAM packed-slots + trailing `u16` status**, with an `03 02 <u16>`
error-slot convention on failure; the internal slot record layout is the HBAM
format documented by the file-format workstream.

**Program and result shape (observed; decoded with the slot reader).** Both
the payload and the reply's slots are bracketed slot trees. Every program in
conn-000 has two top-level keys:

- `[1, 1]`: a fixed-shape header. Leaves `0` = `0c`, `1` = `01 02 01 01`,
  `2` = `01 03 01 02`, `3` = `01 02 01 03` (identical in all samples),
  `4` = the file id (u32, equals the transaction header's `fileid`),
  `5` = `02` + a key such as `80 01` or `80 06` (inference: the table or
  occurrence searched), `7` = 16 bytes that are constant per file (inference:
  a schema or file identity digest).
- `[2]`: the query. `[2, 1, ...]` holds the criteria as a key path ending in a
  leaf, e.g. `[2, 1, 0x819a, 0x19, 01 ...]` (a record key, then a field, then a
  masked value) or `[2, 1, 1, 1, 80 06 ...]` (a six-digit number's key); `[2, 3]` holds leaves such
  as `01 = 00 02 01 01 02 01 01` (inference: request and sort options).

The reply's slots are `[2]` holding one leaf per matching record, keyed by the
record id with no value: `80 20 02 80 08 81 9a 40 40` lists record `0x819a`;
`80 20 02 80 40 40` is an empty found set. A server therefore needs a query
interpreter over the file's records to answer op86; that is a separate
workstream (ROADMAP.md, milestone S2).

#### 5.6.8 What a clean client must do to read any Perform reply

1. Send `Perform(connId, "", maxreturn, requestTxn)` where `requestTxn` is the
   20-byte-header + LIFO request and `maxreturn` (`0x14..0x200000`) bounds the
   reply. Reassemble GIOP fragments for large replies (5.6.1).
2. Split the reply body into two CDR `sequence<octet>`: `parms` (response) then
   `backchannel`. Ignore `backchannel` except for section-6 accounting.
3. Treat `parms` as a **header-less LIFO result stack**: pop the trailing
   `u16` status/error first; if it is a nonzero op-specific error code, stop.
   Otherwise pop the op-specific fields in the order in 5.6.4-5.6.7. Exception:
   op86 has no status field (5.6.7); its tail is the resume block (5.6.6c).
4. For the upload/download ops (5.6.6c), follow a nonzero cursor with
   continuation requests until the cursor comes back 0.

#### 5.6.9 op63 RequestNotification (observed)

Sent once per opened file, after the first op90 and before Login. Body, push
order (17 bytes): `i32 -1`, `u32 1`, `u8 0`, `u32 fileid` (equals the header's),
`i32 handle`. The handle is `-4` for the first file and 2 lower for each later
one in conn-000 (a client-side handle, by inference). The reply is the bare
status `00 00` in all 19 exchanges. Notifications themselves travel on the
back-channel (section 6).

#### 5.6.9b What is readable before Login (observed)

FileMaker Pro downloads two subtrees between Open and Login, and FileMaker
Server serves both with data: `[2]` (the status area holding `mInternalCreation`,
needed for the Login nonce) and `[23, 1, 1]` in the security catalog (key 23 =
`0x17`; conn-000 request 12, 1125 bytes, before Login at request 14). A server
that refuses `[23, 1, 1]` (lock error) lets Login succeed, but FileMaker Pro then
loads the schema and layouts and never draws the layout: the window stays blank
(observed against OpenMaker Server, 2026-10-28).

That `[2]` is the one Pro uses for the session. In s03 every one of the 19
files it opened read `[2]` once, before Login, and never again. The related
files read their `[23, 1, …]` entries after Login (5.5, "Related files";
High). OpenMaker keeps the `[2]` it reads for the challenge as the file's
node for its calculation schema, so it too reads `[2]` once per open.
Only a later rebuild of the schema reads it again, and a file joined on a
shared connection, which does not log in, reads it once after the join
[implementation].

The node's size can differ by a few bytes between the reads before and
after Login. In a recorded OpenMaker open against FileMaker Server (kept
privately), every file's second reply was within 3 bytes of its first.
Which keys differ is unknown. OpenMaker reads only the locale `[2, 0d]`, the document
preferences `[2, 18]` and `[2, 254]` from it.

#### 5.6.10 op62 Unlock (observed)

Sent after a burst of op90 downloads (the "with lock" reads). Request body
(19 bytes in all 19 conn-000 requests): `00 x6, 20 00, 00 x4, u32 fileid` in
bytes 10..13 (equals the header's), `00 00 00 05`. The reply result stack is
exactly the request's 20-byte transaction header (opcode `3e`, same transaction,
session and file ids) with `idletime` 0, so the `u16` a client pops is 0. All 19
replies match (`capture_server.rs`). The body's fields are not decoded further.
FileMaker Pro stops loading a file if Unlock answers with a nonzero status.

#### 5.6.11 op89 CompareModCounts (observed)

FileMaker Pro asks the host which of the catalog entries, records and
library members it has cached are out of date. Sources: the client's
`DBFileUser::GatherModCounts`, `DBCatalog::GatherModCounts`,
`DBTableProxy::ValidateCachedRecordsAndGatherModCounts`,
`DBFileUser::ResyncTempFile`, `ResyncGlobalSharedCatalog` and
`RecursiveNotifyChange`, `DBRecordCursor::SetupRowPos` (Linux
`libDBEngine.so` nm `0xf41b30`, `0xe6eb10`, `0x11a0b00`, `0xf43740`,
`0xf44fe0`, `0xf42e90`, `0xe8c210`); FileMaker Pro's layer
`FMFileUser::GatherModCounts` (Mac FMEngine `0x5fc96c`); the server's
`RCCompareModCounts::PerformPopAddition` and `PerformOperation` (nm
`0xf41f40`, `0xf41fe0`), `RecursiveCompareModCounts` (nm `0xf423c0`) and
`IsLockAllMembersCatalogKey` (nm `0xf43290`). Observed: FileMaker Pro's own
request in capture `2026-10-28-s06-omserver-p` (conn-000, request id 22),
and FileMaker Server's replies in a live run against the lab server's test
project on 2026-11-11 (that request replayed, a request built to exercise
each rule below, and one naming no open file; op89 changes nothing on the
host). Confidence High unless marked.

**When FileMaker Pro sends it** (from bin, Mac DBEngine callers):

- When it creates a file user for a file whose catalogs or records it has
  cached already: `DBSession::InternalCreateFileUser` runs
  `ResyncGlobalSharedCatalog` and `ResyncTempFile`, each one op89 over
  everything cached. In s06-p it came right after Login and four op90
  downloads.
- `DBTransaction::RevertFile` runs `ResyncTempFile`.
- `DBRecordCursor::SetupRowPos`, when it positions on a record of a hosted
  file: one op89 for up to 5,000 cached records around that row that it
  has not validated yet (10,000 in one case the code distinguishes).

**Request.** The upload framing of 5.6.6c, with the tree as payload and a
4-byte addition; opcode `0x59`, the file's `sessionid` and `fileid` in the
header (4.1), `maxreturn` `0x10002e`:

```
[ mod-count tree, N ][ u32 N ][ u64 perform 0 ][ u64 cursor 0 ][ i16 depth -1 ][ u8 copy-key length 0 ][ u32 host file id ]
```

The host file id is the client's `DBFile::mRemoteSerialNumber`, the id the
Open reply gave (5.6.4, pop #5): the lab server answered a request naming
its id for the test file and ignored one naming another (below). The tree
is a bracketed slot list in the download wire form (5.6.6), keyed like the
file, holding the client's cached values:

| key | value |
|-----|-------|
| `[cat, 1]` | the catalog index's mod count as cached, the value of `[cat, 1, fc]` (schema-path.md 8.1), e.g. `01 03`; empty if it has none |
| `[cat, 5, M]` | member M's `fc` value; empty if it has none |
| `[1f, 5, M]`, `[T, 1f, 5, M]` | a library member's 16-byte MD5, its `04` key (containers.md 2.2) |
| `[T, 5, R]` | a cached record's `fc` (write-path.md 5.4); empty if it has none |

`DBFileUser::GatherModCounts` lists the catalogs of tables `[3, 16]`,
occurrences `[3, 17]`, relationships `[3, fb]`, custom functions,
accounts, privilege sets, extended privileges, authorized files, value
lists, file references, fonts and base directories, then for each base
table its field catalog `[T, 3]`, its cached records `[T, 5]` and its
library `[T, 1f]`; FileMaker Pro adds layouts, scripts, menu sets, menus,
themes and the file library `[1f]`. A catalog is listed with its index
count even when no member of it is cached. The captured request (806
bytes, a 759-byte tree) lists 17 top-level keys, among them records
`[8002, 5, 1..3]` and eight file library MD5s. Key segments in this
section are the hex wire bytes, except the established `[3, 16]` and
`[3, 17]` (wire `10` and `11`).

**What the server does** (from bin; each rule observed live):

1. It looks up the open file whose id is the addition. If there is none
   it compares nothing and the tree comes back unchanged (live: a request
   naming `fffffff0` got its 135-byte tree back byte for byte).
2. It walks the tree. For each leaf it looks the same key up in the file.
   - Present: it compares the cached value with the file's, both padded
     with zeros to 16 bytes: for a library member (`[1f, 5, M]`, or
     `[T, 1f, 5, M]` with T a table key, first byte `0x80`-`0xCF`) the
     member's `04` MD5, and a cached value that is not 16 bytes long is
     always different; for anything else the key's `fc` value, a missing
     `fc` reading as zeros, so an empty cached value matches a key without
     a count. Equal: the leaf is removed. Different: its value becomes
     `01 01`.
   - Absent: a record `[T, 5, R]` is removed; any other leaf's value
     becomes `01 00`.
3. A node whose leaves were all removed is removed. A node left with a
   changed leaf is replaced by the single leaf `01 01` at its key when it
   is a catalog FileMaker locks as a whole: tables `[3, 16]`, occurrences
   `[3, 17]`, relationships `[3, fb]`, authorized files, or a table's field
   catalog `[T, 3]`.

`01 01` and `01 00` are the key strings of the engine's `KOne` and
`KUnknown`. The server also sets `RCConnection::mSkipExtraAuthCheck` when
the client's protocol version is below `0x1c` (a side effect; 26.0.3
clients send `0x20`).

**Reply.** The remaining tree as one download page with no op addition
(5.6.6c, 23-byte trailer, depth 0, both handles 0):

```
[ tree, N ][ u32 N ][ u64 0 ][ u64 0 ][ i16 0 ][ u8 0 ]
```

A request whose every entry is current presumably returns the empty list
`80 40` (derived; not observed). A larger tree pages like any download
(5.6.6c; not observed).

**Vectors** (live, 2026-11-11; test project, host file id `0x168`). The
request body after the header, then the reply:

```
80 20 03 80 20 10 80 02 01 01ff 20 05 80 0a 8002 01ff 40 40 40     [3,16,1] = 01 ff; [3,16,5,8002] = 01 ff
20 1f 80 20 05 80 06 01 10 f39253342337dbf938d1861c394dd29f       [1f,5,1] = the member's MD5
   06 02 10 00..00 (16) 06 03 0f 00..00 (15) 40 40                 [1f,5,2] = wrong MD5; [1f,5,3] = 15 bytes
20 21 80 02 01 01ff 20 05 80 02 7f 0101 40 40                      [21,1] = 01 ff; [21,5,7f] (no such member)
28 8002 80 20 03 80 02 01 01ff 40                                  [8002,3,1] = 01 ff
   20 05 80 02 01 0102 00 02 02 03 0107 0a 9fff 0101 40 40 40      records 1, 2 (empty), 3 and 9fff (absent)
00000087 00x16 ffff 00 00000168

80 20 03 80 02 10 0101 40                                          [3,16] collapsed to 01 01
20 1f 80 20 05 80 02 02 0101 02 03 0101 40 40                      library members 2 and 3 changed; 1 current
20 21 80 02 01 0101 20 05 80 02 7f 0100 40 40                      index count changed; member 7f unknown
28 8002 80 02 03 0101 20 05 80 02 01 0101 02 03 0101 40 40 40      [8002,3] collapsed; records 1 and 3 changed
0000003f 00x16 0000 00
```

The host held `[3, 16, 1, fc]` = `01 03`, `[21, 1, fc]` = `01 02`,
`[8002, 3, 1, fc]` = `01 02`, `[8002, 5, 1, fc]` = `01 01`, record 2
without `fc`, record 3 without `fc` and no record `9fff`. The replayed
s06-p request (cached from an older copy of the file) got back `[3, 16]`
and `[3, 17]` collapsed, the layout index `[4, 1]` changed, the indexes
`[10, 1]` and `[20, 1]` (custom functions, file references) unknown, records
`[8002, 5, 1]` and `[8002, 5, 3]` changed, `[8002, 1f, 5, 1]` unknown and
`[8006, 3]` collapsed.

**What FileMaker Pro does with the reply** (from bin,
`RecursiveNotifyChange`; Low on the effects, not observed): it reads the
tree back and, for each leaf, raises a local change notification (the
attention codes of write-path.md 6.3): a whole-catalog leaf, attention
`0x202` (catalog, modify) on that key; a record `[T, 5, R]`, a record
notification `0x200` (modify) on its table; any other leaf `0x202`, or
`0x402` (catalog, delete) when the value is `01 00`. If any of them was a
schema key it also raises attention 2 on the whole file. These make it
drop or refetch its cached copies; nothing more is sent to the host for
op89 itself.

**OpenMaker Server** answers op89 with status 3 (a 2-byte reply), as for
every opcode it does not handle. FileMaker Pro needs a 23-byte trailer
(5.6.6c), so by inference its call fails and it keeps its cache as it is
without validating it, which s06-p is consistent with (it went on
downloading). A server that cannot compare can echo the tree unchanged:
FileMaker Pro then treats every entry as changed and refetches it
(inference from `RecursiveNotifyChange`).

## 6. Back-channel, keepalive and dropped connections

Server-initiated messages ride the bidirectional GIOP connection as `Notify`
(op 0x80) calls, and Perform Script on Server callback results as `Perform`
(op105) calls, targeting the client callback object (write-path.md 6.4,
script-runtime.md 11.4). The client packs outbound status in
`RCConnection::PackBackChannel` (0x1d41f2c) and decodes inbound in
`ProcessBackChannel` (0x1d439b2):

- First byte `0xFE` = server-CPU-load report (`PushBackServerCPU`): `0xFE`, a
  CPU-usage byte, then 0-3 random bytes.
- `ff 00` = a list of `{u32 BE message code, u16 BE length, length bytes}`
  notification records (write-path.md 6.4 corrects an earlier reading of
  this form as a session/idle table).
- Messages are dispatched via `sBackChannelCallback` (engine events: schema
  change, record-modified notifications `kDBRemoteNotify`, conflict
  `kDBRemoteNotifyConflict`, forced disconnect). The administrator's
  messages, requests to close and forced disconnects are in 6.5.

**Correction (captures, 2026-10-31):** the `{u32 BE sessionId, u32 BE
secondsIdle, u16 0}` records this section used to place in the `0xFF`
back-channel form are the payload of the client's `Tickle` (6.1). `Tickle`
is observed only client -> server; no recording holds a server-initiated
`Tickle`. The keepalive is not a 250 ms timer. The 250 ms gate in the code
(`RCConnectionList::TickleAConnection`, 0x1d493be) makes a quiet connection
with queued messages eligible for a push when no callback is active; it is
not a guaranteed delivery time (write-path.md 6.4). The same pass picks a
connection for `RCConnection::HandleTimeout`
(0x1d49644) only after 60 seconds or more without a request (6.1).

### 6.1 Keepalive: `Tickle` (observed)

Evidence: 21 Tickles in 7 connections of FileMaker Pro 26.0.3 against
OpenMaker Server, recorded by the server (`re/captures/2026-10-28-s06-omserver-c`,
`-d`, `-e`, `-n`, `-o`, `-p`, `-q`, each `conn-001` or `conn-000`; these
recordings carry message order but no timestamps). FileMaker Server
sessions s01-s03 hold no Tickle: s03's longest pause between requests was
37.5 s (timestamps in `events.jsonl`), below the interval. Against
FileMaker Server with timestamps: 9 Tickles in `2026-11-11-s07-admin-actions`
(FileMaker Pro 26.0.3 through the relay, conn-000). Two more Tickles with
two independently opened files on one connection were recorded through the
relay in private capture `s137-hosted-privileges-20261114/conn-000`.

Request (High). `Tickle(in ulong connectionid, in string sessKey, inout
sequence<octet> parms)`, response expected, no service contexts, object key
`FMRPO`. Example, s06-o conn-001 request id 252 (98 bytes, byte for byte):

```
47494f50 01020100 56000000          GIOP 1.2 LE Request, 86-byte body
fc000000 03000000 0000 0000         request id 252, response expected, KeyAddr
17000000 ff6f6d6e69494e53504f41ff464d504f4100464d52504f 00
                                    object key (23 bytes), pad
07000000 5469636b6c6500 00          operation "Tickle", pad
00000000 00000000                   no service contexts, pad to 8
539e6859                            arg1: connection id (Connect's return)
01000000 00 000000                  arg2: empty string (length 1, NUL), pad
0a000000                            parms: 10 bytes
00000001 00000099 0000              {session 1, idle 153 s, 0}
```

- arg1 is the connection id `Connect` returned, as for `Perform` (High).
- arg2 is the empty string, as for `Perform` (High).
- `parms` holds one 10-byte record for the connection's session: `u32 BE`
  session id (1, the file session id of `Login`, 5.4), `u32 BE` the user's
  idle seconds, `u16 BE` 0. High for one open file (every recording has
  exactly one record, always session 1 and trailing 0). With two files
  opened independently and signed in on one connection, FileMaker Pro sent
  one record, not one per file: `(1, 112, 0)` and `(1, 173, 0)` in two
  Tickles 60.8 s apart ([#229](https://github.com/OpenMaker-Dev/openmaker/issues/229), private capture
  `s137-hosted-privileges-20261114/conn-000`). Not measured: files opened
  through a relationship and sessions with different ids.
- The idle seconds are the same counter as the RCTransaction header's
  `idletime` (4.1): seconds since the user's last action, not since the
  last request. In s06-n, op86 requests sent after the user stopped carry
  `idletime` 68, 69, 70 ... and the Tickle 60 s after the last of them reports
  the continuing count (79, 140); after the user acts, the next requests
  carry 4, 1, 0 again (High).
- Padding bytes are not initialized by FileMaker Pro (`43 46 4d`, `ff ff ff`
  and `00 00 00` observed in the same positions); a receiver ignores them.
- Request ids: Tickles take the next id of the connection's own sequence
  (even ids, step 2), shared with `Perform` and `Disconnect` (s06-d: Performs
  ... 40, 42, 44, then Tickles 46, 48, 50, 52) (High).

Reply. OpenMaker Server answers NO_EXCEPTION with the inout sequence empty
(28 bytes: `47494f50 01020101 10000000 <id> 00000000 00000000 00000000`),
and FileMaker Pro accepted it (it kept tickling every 60 s and went on with
`Perform`s; s06-d, s06-p) (High). FileMaker Server answers with the
inout sequence holding its CPU report, `fe <cpu> <0-3 random bytes>`, 2 to
5 bytes (s07: 9 of 9 replies, for example `fe 09 ff 8c f8` and `fe 0e`)
(High). A client accepts either and may read a non-empty sequence with the
back-channel forms above.

Cadence (Medium-High; derived from the idle seconds, since s06 has no
timestamps):

- Between consecutive Tickles of one quiet period the reported idle time
  grows by 60 or 61 s in all 13 pairs (s06-d 65 -> 126 -> 187 -> 248; s06-p
  61 -> 122 -> 182 -> 243 -> 304; s06-o 153 -> 213 -> 274 -> 334; s06-e
  159 -> 220; s06-n 79 -> 140).
- The first Tickle follows the last request by 60-61 s in 8 of the 9 cases
  where that request carries an idle time (s06-c 4 -> 65, s06-d 5 -> 65,
  s06-e 99 -> 159, s06-n 5 -> 66 and 19 -> 79, s06-o 92 -> 153, s06-p 1 ->
  61; s06-n 71 -> 144 with untimed lock requests between). One exception:
  s06-q request 856 came 16 s after op86 request 854 (idle 2 -> 18); its
  trigger is unknown.
- So: FileMaker Pro never left a connection with an open file silent for
  more than about 61 s. A Tickle went out about 60 s after the last request
  and every 60 s after that while nothing else was sent. The s07 wall-clock
  times agree: consecutive Tickles came 60.1 to 60.9 s apart, and one came
  60.3 s after the last request (an op80). FileMaker Pro also tickled a
  file it had opened (op78) but not yet signed in to, while its sign-in
  dialog was up.
- In flight: FileMaker Pro does overlap them. In s07 its op71 (request id
  84) waited 53 s for a reply that FileMaker Server held while the file was
  paused (6.5), and FileMaker Pro sent Tickle 86 at 38 s into that wait;
  FileMaker Server answered the Tickle at once and the op71 after the file
  resumed. So a server must read and answer a Tickle while a `Perform` of
  the same connection is outstanding. The 30 s `setClientThreadCallTimeout`
  around `Tickle` (section 2) bounds the wait for its reply.
- Connections with no file open (the identity probes) closed before any
  Tickle was due; whether FileMaker Pro tickles them is Unknown.

OpenMaker's client (om-session) follows the conservative reading: while a
file is open it sends a Tickle when no request has gone out for 60 s, and
every 60 s after that. The Tickle uses the next request id and holds the
connection for the whole exchange, so it never interleaves with a
`Perform`. A Tickle whose reply has not come 30 s after it was sent loses
the connection, however many calls of its own the host sends meanwhile
(they are answered, but do not extend the wait) and however slowly its
bytes arrive (every socket read under TLS waits only for the time left). It stops when the session
disconnects. It reports one record
`{1, idle, 0}`. OpenMaker has no input-idle counter in the session layer, so
"idle" is the seconds since the session's last request other than a
Tickle. Request headers keep `idletime` 0 (4.1). This is OpenMaker's
choice; a FileMaker Server set to disconnect idle users may therefore count
a user who types for a long time without committing as idle.

### 6.2 Idle connections are closed by the server (observed)

- s02 conn-001 (FileMaker Server via the relay, timestamps): after the
  probe's `Connect` and `Disconnect` (t = 3.007 s) the client left the GIOP
  connection open and silent. At t = 147.522 s, 144.5 s later, FileMaker
  Server sent a GIOP **CloseConnection** (the 12-byte header alone, message
  type 5) and the connection ended (High).
- s02 conn-002: the same kind of silent connection was closed by FileMaker
  Pro itself about 117 s after its last message, before the server did
  (relay log; no CloseConnection) (High).
- Live, 2026-10-31: OpenMaker's GUI, signed in to a file on FileMaker
  Server, sent nothing for about 4-5 hours (it had no keepalive). Its next
  request failed with "server closed the connection", which is om-session's
  report of a received GIOP CloseConnection, and the request after that with
  `Broken pipe`. FileMaker Pro, idle for the same time against the same
  server, stayed connected (Medium: one observation, no capture).
- So FileMaker Server closes a GIOP connection that has carried nothing for
  some time (by inference omniORB's connection scan, `inConScanPeriod`,
  section 2): 144.5 s without a session. A connection whose session
  ended for silence (below) was still open 75 s later; when it closes is
  not measured.
- **The session ends after 125 s of silence** (observed; mechanism from
  bin). The engine's background thread runs
  `RCConnectionList::TickleAConnection` (Linux nm `0x14fbc30`) on a timer
  message; for a host-side connection not heard from for more than
  60,000 + 65,000 ms (or for more than the engine setting `connTickSec`,
  when that is set) it calls `RCConnection::HandleTimeout` (nm
  `0x14fbf40`), which drops the connection and posts engine event
  `kConnectionTickleFail` (51; `EngineEventMsg`, HBEngine.h). Live against
  the lab server on 2026-11-11 (test project, read-only): a signed-in
  session left silent for 100 s was answered; left silent for 200 s, its
  next `Perform` got a GIOP SYSTEM_EXCEPTION reply on the still-open GIOP
  connection (by 5.6.6c an unknown connection id is `BAD_PARAM`). Event.log
  logged `Warning 30 ... Client "<name>" no longer responding; connection
  closed. (51)` 125.4 s after that session's last request; the same held
  in a second run. Access.log logged no "closing database" or "closing a
  connection" for it (a session that sends `Disconnect` gets both), and the
  server listed no client afterwards. The session's record lock was gone
  with it (6.3). FileMaker Pro tickles after 60 s of silence (6.1), well
  inside the limit.

OpenMaker Server closes silent connections by default: it sends
CloseConnection and ends the session once a connection has received
nothing for its limit. The time counts from the end of the last exchange,
when the server's reply went out, so a request the server takes long to
answer (a Perform Script on Server that waits for its script) does not use
up the client's allowance. The limits are OpenMaker's choice
(`ConnectionLimits` in om-server):

- Without a session (before `Connect`, or after `Disconnect`): 145 s,
  about when FileMaker Server closed s02 conn-001 (144.5 s). FileMaker Pro
  closed such a connection of its own after about 117 s (s02 conn-002), so
  it still closes first.
- With a session: an hour. A client with a file open tickles after 60 s
  of silence (6.1), so it never comes near; the margin is for a client
  that holds a session with no file open, which may not tickle (FileMaker
  Pro's behavior there is Unknown; OpenMaker's client does not). FileMaker
  Server ends a silent session after 125 s (above), so a vanished client
  holds its files and locks far longer on OpenMaker Server ([#173](https://github.com/OpenMaker-Dev/openmaker/issues/173)).

`om-server --idle-seconds` sets the limit for sessions or turns it off (a
connection without a session is then still closed after 145 s, or after the
limit for sessions if that is shorter). A private host for a local file
([local-files.md](local-files.md)) never closes its owner's session for
silence. OpenMaker Server also caps the connections it serves at once, in
all and from one address, gives the TLS handshake one deadline, and ends a
connection whose client takes nothing it writes for 2 minutes; whether and
where FileMaker Server limits these is not measured.

### 6.3 Reconnect and file reopening in the inspected builds

Sources: the servant `RPO_i::Reconnect` and stub `_objref_RCFMP12::Reconnect`
(Linux `libDBEngine.so` nm `0x14ed0f0`, `0x14e9080`; Mac DBEngine
`0x1d4195e`, `0x1d3c504`) and the call descriptor
`_0RL_cd_07aabdf5dad7309d_a0000000` (nm `0x14e8e30`-`0x14e9040`); the client
`DBSession::ReconnectFile` and `CheckReauthenticateSetting` (Mac DBEngine
`0x1ba5f06`, `0x1ba0e14`; Linux nm `0x1152fb0`, `0x114a540`), FMEngine's
`FMSession::ReconnectDeadFiles`, `ReconnectFile`, `OnAppWake`,
`CheckFilesAndAuthentication` and `FMFileUser::ShouldReconnectFile`,
`AddSleepTime`, `OnReconnect` (Mac FMEngine `0x6078dc`, `0x60744c`,
`0x60b54c`, `0x607f1c`, `0x5fc9fc`, `0x5f72f2`, `0x5f730e`; names and the
`ReconnectState` and `AuthStatus` enums from Linux `libFMEngine.so` DWARF).

**The operation** (from bin, High). DWARF gives
`Boolean Reconnect(ULong connectionid, RCFMP12_ptr replyto, char *sessKey)`:
the request marshals the connection id (`u32`), the client's callback
object reference (an IOR, as in `Connect`, 5.2) and a string; the reply is
one CDR boolean octet. The inspected server servant returns FALSE
without reading its arguments, as does the inspected Mac FileMaker Pro
servant (REA evidence `ev_e51148343defd209aa5634fa59613e97fd5806e1913bdc4a93447801fcb6a5aa`).
No caller was found in the inspected artifacts: Mac DBEngine has no
reference to `_objref_RCFMP12::Reconnect`, and `libDBEngine.so` has no PLT
entry for it. None of the 30 recorded connections used `Reconnect`.
These findings do not establish usage in other platforms, builds or
client paths. The inspected servants do not restore a session through
this operation; the inspected client recovery path opens the file afresh
(below).

**What the server keeps after a drop.** The inspected Reconnect handlers
do not restore the session. Treating a dropped transport like a silent
one is inference: both stop the requests that keep `mLastHeardFrom`
current. Under that inference, the session, with its open
files, sign-in and locks, stays until 125 s of silence and is then
dropped (6.2). Observed 2026-11-11: session A locked record id `9fff` of
the test project's table `8002` (op60, type 9; no such record, so nothing
changed); session B's lock was refused with 8003 naming A; A then sent
nothing; FileMaker Server logged A's drop 125.4 s after A's last request,
and B's lock 145 s after A's was granted. The connection's paged downloads
(5.6.6c) go with the session (inference).

**What FileMaker Pro does** (from bin; not observed, the drop test needs
FileMaker Pro on the capture Mac):

1. A file whose connection is lost becomes a dead file, with a
   `ReconnectState` of `kIsConnected` 0, `kIsReconnecting` 1,
   `kCannotReconnect` 2 or `kFileNotFound` 3, which its windows follow.
2. `FMSession::ReconnectDeadFiles` tries again at most every 2 s, and only
   when the platform reports a network and some dead file is not
   `kCannotReconnect`; it reconnects on a background thread. Its callers
   are in the application and not traced; by inference `OnAppWake` (after
   the computer sleeps) is one.
3. For each file, `DBSession::ReconnectFile` opens it afresh: a new
   connection and Open (`_is_a`, `Connect`, op78), a download of the
   file's creation value `[2, 254]` (4.3), which must match the one it
   had (otherwise error `0x4fb4` and no reconnect), op63
   RequestNotification, then Login (op80) with the account and password
   it kept. If the host refuses them (212, 213) it asks the user; a
   sign-in to a different account (anything but the same name with
   another password and the same privilege set) is logged out (op81) and
   fails with 212. Errors that allow it send it to the host's standby
   addresses (`ParseStandbySettings`). An open transaction remembers its
   file locks for this (`DBTransaction::SaveForReconnect`,
   `RememberFileLocks`); how it takes them again is not traced.
4. **Additional wake-policy re-authentication**
   (`CheckFilesAndAuthentication`, after a wake): this policy requests a
   password if all three conditions hold. Prompts for refused retained
   credentials in step 3 are independent of this policy:
   - the file has an extended privilege whose name starts with
     `fmreauthenticate` (compared without case), enabled for the account's
     privilege set: `DBSession::CheckReauthenticateSetting` takes the
     number at the end of the name as the minutes allowed away, the
     smallest when several match, at most 10080 (7 days), and 0 or less as
     10 seconds;
   - the file was away (since the connection was lost, time asleep
     included; `AddSleepTime`, `OnReconnect`) at least that long;
   - the account did not sign in during the last 5 minutes
     (`kReauthExpiredMinutes` = 5; only a sign-in made after the file went
     away counts).
   Otherwise this wake policy does not force a password prompt. Recovery
   may still ask when retained credentials are refused (step 3); absence
   of `fmreauthenticate` does not prevent that prompt. The hibernation
   path (`DBSession::SaveForHibernation`, by its name FileMaker Go's)
   reads the same setting.

**OpenMaker.** OpenMaker does not send `Reconnect`. OpenMaker Server answers
it with BAD_OPERATION, like every operation it does not handle, where
the inspected FileMaker Server servant answers FALSE. No caller was
found in the inspected artifacts and no recorded connection used it.
When the transport fails, or the server sends CloseConnection, OpenMaker's client
marks the session lost. Later requests fail at once without touching the
socket, and the desktop client shows that the connection to the host was
lost and keeps unsaved edits on screen. Reconnecting means opening the file
again (Connect, Open, Login): the desktop client's **Reconnect…** does so
when the user asks, with the account the file was opened with and a
password typed again ([recovery.md](../recovery.md), "Reconnecting"); it
does not reconnect by itself as FileMaker Pro does ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)).

### 6.4 Disconnect

`Disconnect(connId, string)`: no reply body path (section 2). FileMaker
Server answered it with an empty NO_EXCEPTION reply in s02, then left the
GIOP connection to idle out (6.2).

### 6.5 Administrator actions: messages, closing, disconnecting, pausing, blocking (observed)

Evidence: `re/captures/2026-11-11-s07-admin-actions` (git-ignored).
FileMaker Pro 26.0.3 opened the lab test file on the lab FileMaker Server
26.0.3 through the timestamping relay. Each action was sent with the Admin
API (admin-console.md 8) at a logged time, and the client's dialogs were
read from its accessibility tree every 0.2 to 5 s. Times are intervals
between relay timestamps. The vectors show the lab server's 6-character
name as `server`. Confidence is High unless noted.

**Delivery.** Each message is one `Notify` call on the client's own GIOP
connection, as for record notifications (write-path.md 6.4, "Call"):

- Before its first call of a session, FileMaker Server sends a GIOP
  LocateRequest for the client's callback object key
  `\xffomniINSPOA\xffFMPOA\0FMRPO`, which FileMaker Pro answers with a
  LocateReply, OBJECT_HERE. The session's later calls come without one.
  In all three sessions the `Notify` reached the relay after the client's
  LocateReply had passed it, within 3 ms of the LocateRequest: by
  inference the server waits for the reply before it calls.
- The server's request ids are odd and count per GIOP connection, across
  sessions, from 3 (both connections): conn-001 has 3, 5, 7, 9 in one
  session and 11, 13, 15 in the next.
- The body is the session's connection id (`u32`, as in `Tickle`), the
  empty string, then `parms`: `ff 00` and one record `{u32 BE code, u16 BE
  length, DBMesg}` (write-path.md 6.3, 6.4). Every call held one record.
- FileMaker Pro answers within 2 ms, before it shows anything: NO_EXCEPTION,
  with its own CPU report in the out sequence, `fe <cpu> <0-3 random
  bytes>` (7 of 7 calls).

OpenMaker Server sends these messages this way, and holds a paused file's
requests while it answers everything else
([../admin-console.md](../admin-console.md), "Administration in
om-server").

**The DBMesg** has the header of write-path.md 6.3 (send-to session 1,
reply-to 0, the code, 4 items, the offset of the last length word) and four
items, each followed by its `u16 BE` length:

| item | type | content |
|---|---|---|
| 1 | `i32` BE | the file. With `kDBAttnHostSerial` in the code: the host's file id, as op78 returns it (5.6.4, pop #5) and op63 sends it. Without it: the client file id the session registered with op63 (write-path.md 6.1) |
| 2 | UTF-16BE | the administrator's message, no terminator |
| 3 | UTF-16BE | the server's name (`remoteName`, 5.2) |
| 4 | `u32` BE | 30 in the requests to close (by inference the client's countdown, below), 0 otherwise |

The codes, with FileMaker Server's names for them (`DBAttnCode` constants in
the DWARF of `libDBEngine.so`, `DBConstants.h`):

| code | constants | sent for |
|---|---|---|
| `0x0810` | `kDBAttnHostSerial` \| `kDBAttnMessage` | Send Message (`POST /clients/{id}/message`) |
| `0x0850` | `kDBAttnHostSerial` \| `kDBAttnCloseAll` | Disconnect (`DELETE /clients/{id}`): close your files |
| `0x0040` | `kDBAttnCloseFile` | Close (`PATCH /databases/{id}`, `CLOSED`): close this file |
| `0x0870` | `kDBAttnHostSerial` \| `kDBAttnDisconnectAll` | either one's grace time ran out |

Send Message, conn-000 request 5, `parms` (160 bytes) for the text
"OpenMaker capture 1: a test message from the Admin API":

```
ff000000 08100098 00000001 00000000    ff 00; code 0x810, 152 bytes; send-to 1, reply-to 0
00000810 00040096 00000165 0004004f    code; 4 items, last length word at 150; item 1 0x165 (4); "O"
00700065 006e004d 0061006b 00650072    item 2: 54 UTF-16 units ...
00200063 00610070 00740075 00720065
00200031 003a0020 00610020 00740065
00730074 0020006d 00650073 00730061
00670065 00200066 0072006f 006d0020
00740068 00650020 00410064 006d0069
006e0020 00410050 0049006c 00730065    ... "API", length 108; item 3 "se...
00720076 00650072 000c0000 00000004    ...rver", length 12; item 4 0, length 4
```

The others differ only in the code, items 1, 2 and 4 and the lengths:
Disconnect (conn-000 request 7) `00000850`, item 1 `00000165`, item 4
`0000001e`; Close (conn-001 request 5) `00000040`, item 1 `fffffff7` (-9,
that session's op63 client file id), item 4 `0000001e`; the forced notice
(conn-001 requests 9 and 15) `00000870`, item 1 the host file id
(`00000165`, `00000168`), item 4 0.

**What FileMaker Pro does.**

- `0x0810`: a dialog "Message from “<server name>”" with the text, a
  Close button and "(This dialog will be dismissed in N seconds.)". It closed
  itself about 30 s later (Medium: polled every 20 s). Nothing more is
  sent.
- `0x0850`, `0x0040`: a "Close File" dialog: "Files to close" (the file
  name with its extension), "Messages" (the text), "(Will attempt
  automatic close in 30 seconds.)" counting down in steps of 5, and the
  buttons Cancel and Close Now. It starts at 30 whatever the grace time
  (40 s, 60 s and Close's default alike). At zero, 30.1 s after the notice,
  FileMaker Pro closes the file: one op62 Unlock of the file root (type 5,
  attention `0x2000`, txn 0; write-path.md 4.3), the release it sends when
  a user closes a file, and no Logout or `Disconnect`. FileMaker Server
  then lists no client. Cancel sends nothing. Close Now was not tried.
- `0x0870`: at once op81 Logout for the file (the transaction header
  alone, its file id) and `Disconnect`; then the alert "File has been
  forcefully disconnected by the host. All affected windows will be
  closed. [From Administrator: “<text>”]" with OK.

**When FileMaker Server forces.**

- Disconnect with `graceTime` 40, Cancel pressed: no second notice;
  `0x0870` 39.0 s after the `0x0850` (39.4 s after the Admin API call).
- Disconnect with `graceTime` 60, the client closing its file when its
  countdown ended (30.1 s): no `0x0870`. 90.1 s after the Admin API call
  FileMaker Server called `Disconnect(connectionid, "")` on the client's
  callback object (conn-000 request 9, response expected, the session's
  connection id), and FileMaker Pro answered NO_EXCEPTION with an empty
  body. The connection stayed open: the client later sent new `_is_a` and
  `Connect` requests on it. Whether that call comes 30 s after the grace
  time or 60 s after the client closed its file is not established (one
  observation). During the grace time the Admin API listed the client
  with status `NORMAL`.
- Close (the Admin API's `PATCH` takes no grace time), Cancel pressed each
  time: the same `0x0040` notice again 45.0 s after the first, then
  `0x0870` 90.0 s after the first. The file stayed `CLOSING` until the
  client had gone, then `CLOSED`. So Close's default grace time is 90 s.
  Whether a long Disconnect grace time also repeats the notice, and on what
  schedule (every 45 s or halfway), is Unknown.

**Pause** (`PATCH /databases/{id}`, `PAUSED`) sends clients nothing.
FileMaker Server holds a request on the paused file without answering it:
an op71 waited 53.0 s and was answered 30 ms after `RESUMED`. Meanwhile a
Tickle on the same connection was answered at once (6.1). FileMaker Pro's
window stopped responding (its accessibility tree went empty) and showed no
dialog in those 53 s. Moving to a record it had cached sent nothing. Only
op71 was tried (Medium for other requests).

**Block New Users** sends nothing either. A new client's `_is_a` and
`Connect` succeed as usual: the reply matches a normal one except for the
connection id and key. The op78 Open is refused with status 1638 (5.6.4),
and FileMaker Pro shows "The host is not allowing new connections. Try
again later." (error 1638 in `docs/re/catalogs/error-codes.csv`) with OK.
It sent `Disconnect` 59.9 s later, with the alert still up. An earlier
attempt from the Hosts dialog, while FileMaker Pro also held an unrecorded
direct connection to the server, showed instead two rounds of `_is_a`,
`Connect` and `Disconnect` on the recorded connection and no op78. The
first `Connect` was answered after 3.0 s. Where that client learned of the
refusal is not established. Whether a connected client can still open
another file is not measured.

## 7. Sequence diagrams

Connect / host identity probe (observed over the relay):
```
Client                                   Server (5003, mTLS)
  |-- TCP + TLS1.3 (client cert) ------->|
  |-- GIOP _is_a("IDL:RCFMP12:1.0") ---->|   (+BiDir ctx: client callback IPs:port)
  |<-------------------- Reply TRUE -----|
  |-- GIOP Connect(cb, "", "", octets)-->|   octets = machine info (license/host/MACs)
  |<-- Reply connId + identity octets ---|   identity = name/serverId/version/ws-settings/authID
  |-- GIOP Disconnect(connId) ---------->|   (probe only; torn down)
```

List hosts / open file (on the resolved working connection):
```
Client                                   Server
  |-- Connect (as above) --------------->|
  |-- Perform(connId,"",len, [op77]) --->|   GetFileList -> hosted file list
  |<-- Reply NO_EXCEPTION + out octets --|
  |-- Perform(connId,"",len, [op78]) --->|   Open File -> per-file nonce file[0x3ac]
  |<-- Reply out octets -----------------|
  |-- Perform(connId,"",len, [op80]) --->|   Login (PushEncrypt creds, keyed by session nonce)
  |<-- Reply out octets (err, version) --|
  |-- Perform(... op86/70/71/60 ...) --->|   finds / commits / records / locks
  |<== Notify back-channel calls ========|   change notifications (write-path.md 6.4)
  |-- Tickle(connId,"",{1,idle,0}) ---->|   after 60 s without a request (6.1)
  |<-- Reply (empty inout) --------------|
  |-- Perform(... op81 Logout ...) ----->|
  |-- Disconnect(connId) --------------->|
```

## 8. Redirect / reconnect  (answer (a); full note in re/work/network/redirect-answer.md)

The `Connect` reply carries no alternative address, so there is no
protocol-level redirect. The client identifies the server from the probe
(`remoteName`, `installCode`) and resolves the working address from its own host
records (Favorites/Recents, `fmnet:/<host>/<serverName>/<file>.fmp12`), because
the bare returned name does not resolve. DBEngine dedupes/reuses connections by
`(remoteName, installCode)` in `RCConnectionList::GetAndIncrementIfConnected`
(0x1d486c8), never by dialed address; the host match and re-dial are client-UI
acts (`FavoritesHelper`, `SignInModel`, `OpenRemoteUI`, `XParseURL`).

Relevant `RCConnection` fields (FMS 26.0.3 Linux DWARF, `RemoteSupport.h`):
`mConnectionId` 0x180, `mNeedReconnect` 0x188, `mRemoteName` 0x190,
`mInstallCode` 0x1b0, `mVersionString` 0x1d0, `mProtocolVersion` 0x1f8,
`mWebServiceSettings` 0x220, `mStandbyAddress` 0x240, `mConnectionKey` 0x284
(the 16-byte obscuring salt), `mSkipExtraAuthCheck` 0x970.
`RCNetworkStack` toggles: `mNewConnectionPerBind` 0x76 (force a fresh connection
per bind; default 0 = reuse), `sAlwaysUseTCPProtocol` (force ssliop, skip the
HTTP/wss tunnel), `sServerPort` (5003). `mStandbyAddress` + `SetStandbyAddress`
exist for HA failover and are not populated from this reply.

## 9. Data encoding reference

- Integers in the RCTransaction body: big-endian, packed/unaligned. CORBA/GIOP
  header and CDR fields: little-endian. (Two different endian conventions: GIOP
  CDR is LE as negotiated; the FileMaker payload is BE by hand.)
- Booleans/bytes: 1 byte. char16: 2 bytes BE.
- Strings: SCSU-compressed UTF-16, every byte XOR 0x5A, with either a trailing
  `u32 BE` length (stack `Push(unistring)`) or a leading 1-byte length
  (`PushUnistring` in Connect/identity payloads) or a trailing 1-byte length
  (`Push(uchar const*)`).
- Credential fields: the above string encoding, then XOR with a
  PBKDF2-HMAC-SHA1 keystream keyed by the 16-byte per-connection key.
- Record ids / field values / containers: carried as HBAM packed-slot buffers
  (`HBAMSlot::S_SlotPack`/`Unpack`), i.e. the on-disk `.fmp12` encoding, not a
  distinct wire encoding.
- Password hashing (account auth internals): PBKDF2-HMAC over Unicode-decomposed
  text with a 10-byte salt and a fixed 30-byte pepper beginning "FileMaker,
  Inc."; selectable SHA1/256/384/512.

## 10. Open questions and high-value live experiments

Ranked; each needs a TLS-terminating proxy between an official client (separate
Windows or Mac host) and the server. The proxy must present a client cert the
server accepts (or the server set to request-but-not-require) and log, per
connection: dialed host/IP + SNI, direction-split plaintext, timestamps.

1. Confirm the redirect driver (blocks the next capture): put the proxy in front
   of BOTH the relay and the lab server's address on port 5003 (point the lab
   server's name at the proxy) so the working session is captured. Run three
   variants - recents present; recents cleared; a favorite whose `serverName` differs from the
   reply `remoteName` - to prove whether the key is `remoteName`, `installCode`,
   or the favorites match. (detail in `redirect-answer.md`.)
2. PARTLY RESOLVED (section 5.6, capture `s03/conn-000`): the `Perform` **reply**
   layout is now pinned against real bytes - void return + inout `parms` (a
   header-less LIFO result stack, status at the tail) + out `backchannel`; op78
   Open and op80 Login reply fields decode byte-exact (op78 yields the
   `file[0x3ac]` nonce; op80 the `errorCode`), op90/op86 framing = HBAM
   packed-slots + trailing `u16`. Still open: the **request** body of op70
   `RCCommitRecords` and op71 `RCCreateRecord` against real bytes, and the
   internal HBAM slot-record layout inside op90/op86 payloads (file-format
   workstream). The request-side op80/op78 bodies are confirmed too (headers decode
   per 5.6.3).
3. RESOLVED (section 5.2, capture `s03/conn-000` + `RPO_i::Connect`): the reply's
   leading 16 bytes are NOT a hash/MAC - they are the random LCG connection key,
   echoed so the client can use it as the credential-obscuring salt. The
   connection id (CORBA return) is `(LCG byte) + LE32(key[0..3])`, not a digest.
4. RESOLVED (section 5.2): the exact Connect-request field order and framing are
   pinned from `conn-000` + `PushMachineInfo` + the `RPO_i::Connect` read order
   (2 bytes, 4 strings, 2 bytes, 3 strings; both string args are empty). A second
   diff capture is only nice-to-have confirmation now.
5. Determine whether the server ever enables ZIOP compression or the wss tunnel
   for a desktop client (set `sAlwaysUseTCPProtocol=0` and block 5003 to force
   the 443 tunnel; capture the `giop:http:wss://.../fms/ws` frames).
6. Determine whether a client cert is ever required to be account-bound, or only
   to pass the TLS gate (try a self-signed client cert vs none vs a
   FileMaker/Claris-rooted cert against Login success).
7. Capture a back-channel `Notify` (edit the same record from a second client)
   to confirm the `{msgCode,len,payload}` record-change format and op -64/-67.
8. Keepalive against FileMaker Server (6.1, 6.2): PARTLY RESOLVED by s07
   (the wall-clock interval, FileMaker Server's Tickle reply, and a Tickle
   sent while a `Perform` waited) and by the 2026-11-11 silence runs (a
   silent session ends after 125 s, 6.2). Still open: whether the s06-q
   16 s Tickle has a trigger, and when FileMaker Server closes the GIOP
   connection of a session it ended.
9. Lost connection (6.3): no `Reconnect` caller was found in the inspected
   artifacts, and no recorded connection used it. FileMaker Pro's recovery
   after a dropped connection remains unobserved. Drop the TCP connection under FileMaker Pro (stop
   the relay's upstream socket) while a file is open, once for less than
   125 s and once for longer, and record the next connection (expected:
   `_is_a`, `Connect`, op78, the `[2, 254]` download, op63, op80), whether
   it asks for the password, what the window shows meanwhile, and what
   happens to a record it was editing. With an `fmreauthenticate<N>`
   privilege, sleep the Mac for longer than N minutes to see the prompt.
10. Account lockout (5.4): with a throwaway account of a throwaway file,
    never a shared lab account, fail Login until it stops answering 212 and
    record the count, the error code then, whether the right password is
    refused, whether another client address or another account is affected,
    and how long until a login succeeds again.
11. Login forms not yet captured (5.4): open a throwaway file whose [Guest]
    is active and choose Guest; sign in to an account set to "must change
    password" and give a new password, once too short; run Re-Login; and,
    where the lab server has one, an external-server or OAuth account. Record
    each op80's field lengths and flags, the reply, and for OAuth the HTTPS
    step before it. Also which feature sends login type 1.

## Appendix: scratch artifacts

Under `re/work/network/` (git-ignored): demangled symbol dumps `*.dm.txt`;
`DBEngine.dis` (full disassembly); `rc_registry.tsv` (opcode table);
`DBEngine.stubmap.txt` (PLT->symbol); `decomp_all.c` / `decomp_b2.c` /
`decomp_b3.c` / `decomp_support.c` / `decomp_s2.c` (batch decompilations);
`dwarf_RCConnection.txt` / `dwarf_RCNetworkStack.txt` (Linux DWARF field maps);
`bind.c` (BindRemoteConnectionBlocking); `redirect-answer.md`.
Tooling under `tools/re-network/`: `rc_registry.py`, `fn_strings.py`,
`addr2func.py`, `disrange.sh`, `getfn.sh`, `DecompileList.java`.
