# Local files: opening a `.fmp12` file without a server

FileMaker Pro opens a file on the computer it runs on (File > Open) as well
as hosted ones. This note records what OpenMaker reads to do the same, what
it refuses because it cannot yet read or enforce it, and how OpenMaker
Client opens local files without FileMaker, a Claris license or a separately
running server. Evidence levels: [O] observed in files or sessions, [I]
inferred; "from bin" marks what is read from FileMaker's code without
running it.

## 1. What opening a local file needs

- The file's contents: the HBAM7 key tree (file-format.md). Encrypted files
  (EAR, file-format.md 6) do not decode and are refused.
- Its accounts, their passwords, whether each may log in (active, a
  FileMaker-file account, no password change due: 2.2) and what its
  privilege set allows: a local file is protected by its own accounts
  exactly as a hosted one is. Being on this computer grants no account.
- Nothing licensed: no Claris license (install code), client identity or
  FileMaker installation. A local file's session sends none.

### 1.1 Related-file failures in FileMaker Pro [O, local copies]

In FileMaker Pro 26.0.3, a disposable opening file with a plain Contact
List layout opened without an alert in each of these cases. The first
switch to Contact Details then used a field from a related file:

| Related source | First use of the related field | After Cancel, when offered |
|---|---|---|
| Present, but requiring its own sign-in | Pro showed that file's sign-in dialog | The local layout drew; the related field showed `<File Missing>`. Returning to the layout did not ask again in that session. |
| File absent | Pro showed a Not Found alert, then a picker to locate the file | The local layout drew; the related field showed `<File Missing>`. Returning to the layout did not ask again in that session. |
| Sole path `fmnet:/127.0.0.1/Name.fmp12`, with the file not hosted on the loopback listener | Pro showed an unencrypted-connection warning; after Connect, a Not Found alert and locate-file picker | After Cancel, the local layout drew with `<File Missing>` in the related field. Returning to the layout did not ask again in that session. |
| File present and openable, but its target table deleted | Pro showed no alert or picker | The local layout drew; the related field showed `<Table Missing>`. Manage Database's graph showed `<Table Missing>` in the related occurrence too. |

These are observations on separate local copies, not a rule for every
external source or a hosted file. In the loopback check, no separate
server was running; FileMaker Pro itself listened on port 5003. The
checks are recorded on
#760,
including the absent-file
and missing-table
copies and the loopback `fmnet:` path.
A hosted file with several simultaneous related-file problems and a data
source naming a different host remain unmeasured.

## 2. Accounts `[23, 1, 5, A]`

### 2.1 Stored passwords `06`

Each account record keeps its password hash at key `06`, 31 to 62 bytes
[O; layout from `DBPassword::Write`/`Read`, Linux `libDBEngine.so` nm
`0x120d6e0`/`0x120d570`]:

| Bytes | Content |
|---|---|
| 1 | format version `01` (`DBPassword` `kPasswordVersion` = `kHMACSHA1Version` = 1) |
| 10 | salt |
| 20 | `ComputePasswordHash(password, salt, PEPPER, 1 iteration, SHA1)`, 20 bytes (network-protocol.md 4.3, 4.4) |
| n | check bytes: `n` = the hash's second byte (byte 12 of the value) `& 0x1f`, 0 to 31; byte `31 + i` is byte `11 + i` XOR byte `1 + i`, for `i` from 0 to `n - 1`, read from the value as it is written (from `i` = 20 on, earlier check bytes enter) |

FileMaker writes `31 + n` bytes. It reads a value of 31 bytes, or of 32 to 62
bytes whose length is `31 + n` and whose check bytes hold; any other value, or
a version byte other than 1, is unreadable and no typed password matches it
(`DBPassword::Read`, `DBUserAccount::MatchPasswordData`: error 212). The salt
of a new password comes from a seeded generator (`DBPassword::Set`,
`NewSalt`).

The first byte is a format version, not a hash algorithm. The inspected
`DBPassword` reader and writer accept and write only 1, and the enumerated
Linux engine calls of `ComputePasswordHash` use SHA-1 (network-protocol.md
4.4). No SHA-2 account form was found in the inspected builds/callers or
sampled files. Support exposes SHA-256/384/512 as selectors 2-4; Mac callers
were not enumerated and Windows was not compared.

Observed [O], read-only, in every account of the 66 FileMaker Pro 26.0.3
starter files (six solutions, eleven languages) and of 14 FileMaker-written
lab files (the s33 fixtures, the test project and others; 163 accounts in
all): the layout and check bytes hold (10 values are 31 bytes, their `n`
being 0); each starter Admin's hash is of the empty password; the test
project's full access account, whose non-empty ASCII password is known (not
recorded here), matches it in all three copies examined; and every [Guest]'s
hash is of its own name text,
`[` U+E002 `]` (below). Non-ASCII text goes through `Unicode::HashDecompose`
first (network-protocol.md 4.4), measured by calling FileMaker's own library.

Observed [O] in a copy of the English Contacts starter in which FileMaker
Pro 26.0.3 (macOS) added four [Full Access] accounts whose passwords were
pasted as NFC text: a Latin one with `ä` and `ö`, a Greek one with `ό`,
a Hangul one and one ending in U+1F511 (a surrogate pair). Pro admitted
each at sign-in (#211, comment 6874981316). Each stored hash is
`HashDecompose` of the password's code units as pasted, with the pepper:
not of the code units without `HashDecompose` (the Latin and Greek ones
differ), and not of the Unicode NFD form (the Greek one, whose `ό` FileMaker
decomposes to `ο` U+030D, and the Hangul one, whose syllables FileMaker
keeps, differ). So Pro decomposes nothing before `HashDecompose` [O, NFC
input]. Whether it first normalizes text typed in another form (to NFC,
say, which leaves NFC input as it is) is not observed.

OpenMaker hashes a password's code units as typed after `HashDecompose`
(`om_crypto::decompose`, FileMaker's table from the catalog), which
reproduces those four stored values and the library's vectors. It
normalizes nothing either: a form whose FileMaker decomposition differs
from the stored password's (the Greek one typed as NFD, `ο` U+0301; the
Hangul one typed as jamo) is refused (212), while forms FileMaker
decomposes alike (`é`, `e` U+0301) match. Should FileMaker Pro normalize
other forms to NFC first, it would admit such an NFD Greek password where
OpenMaker refuses it [I]. A new password that hashes as the current one is
unchanged (209), whichever form it is typed in [I: the spec does not say
how `SetPassword` compares]. It reads `06` as
FileMaker does, the length and check bytes included, and writes it with
them (`om_crypto::account::StoredPassword`): the English Contacts
starter's Admin value, 30 check bytes after the hash, and the four values
above come out byte for byte from their salts and passwords.

[Guest] (account 1 of the templates) stores its name `16` (hex `10`) as a
node with children `0` and `1`, not a value: `1` is the text `[` U+E002 `]`
(SCSU `5b 1b 68 82 5d` before the XOR), a private-use placeholder for the
localized "[Guest]", and the account's password hash is of that same text
[O: every file above]. OpenMaker reads the name as missing, so no typed name
matches it.

### 2.2 Kind `02` and status `0a`

Whether an account may log in is read from two keys, decoded from FileMaker
Pro 26.0.3's DBEngine [O: static analysis of the x86_64 slice;
`DBUserAccount::TestFMPassword`, `GetStatusInfo`, `IsEnabled`,
`IsForceExpire`, `SetEnabled`, `SetForceExpire`, `SetPassword`,
`DBDateTimeKey::Get`/`Set`, `DBFixPtKey::Get`; FileMaker was not run]:

- **Kind `02`**: one byte, the account's type; absent reads as 0 (the reader
  zeroes its result first). 0 is an account of the file itself, whose
  password the file stores; any other value is authenticated by another
  service. FileMaker checks a typed password only against kind 0, and skips
  other kinds when it matches a typed name. The values (DWARF
  `DBUserAccount::UserType`, libDBEngine.so) are 1 for an external server
  group and `provider << 3 | 2` for an OAuth identity, plus 1 for a group:
  Amazon 18, Google 26, Microsoft Azure AD 34 and 35, Claris ID 42 and 43,
  Apple 50 and 51 (`kAppleConnect`), AD FS 58 and 59, custom OAuth 66 and 67.
- **Status `0a`**: a timestamp, the time the account's password was last
  set (2.4 says when FileMaker stamps it),
  whose **microseconds carry the account's flags**: bit 19 (524288) marks
  an inactive account and bit 18 (262144) one whose password must be
  changed at its next login ("User must change password on next login").
  The value is a timestamp index key (query-program.md 7.3) after a byte
  counting the rest: the date's three bytes `0x800000 | year << 10 | month
  << 6 | day`, then the number key of the seconds since midnight, fraction
  included (`80`, a count of integer digits, then each digit as a nibble
  holding `digit + 1`, a zero nibble ending them). An account without the
  key reads as 1/1/1024 at midnight: active, no change due.

FileMaker Pro refuses a typed password for a FileMaker-file account with
**210** (user account is inactive) when bit 19 is set, before it checks the
password; **212** when the password is wrong (also for any other kind); and
**211** (password has expired) when bit 18 is set, or when the account's
privilege set has passwords changed every N days (set key `0c`, a
big-endian u16, absent or 0 for never) and N days have passed since the
status's date (its own [Guest] login, below, skips both). FileMaker Pro
then asks for a new password [I].

Observed [O]:

| `0a` | Decoded | Account |
|---|---|---|
| `05 90 00 41 80 00` | 1/1/1024 0:00:00.000000: active | every account of the s33 privilege fixture (calc-functions.md 9), each of which logged in with error 0, `Get ( AccountType )` "FileMaker File" |
| `08 90 00 41 80 00 63 53 99` | 1/1/1024 0:00:00.524288: inactive | [Guest] (record `01`) of every starter solution |
| `08 9f 91 03 80 05 xx xx xx` | 4/3/2020 11:17:10 to 11:24:22 (per file), no fraction: active | Admin (record `02`) of every starter solution |

The starter solutions checked are all 330 shipped with FileMaker Pro
19.6.3, 20.3.2, 21.1.3, 22.0.6 and 26.0.3 (six solutions in eleven
languages each, read-only copies): no account has `02`, and every [Full
Access] set lacks `0c`. Not observed in any file: a status with bit 18
set, a kind other than 0 or a set with `0c`; those forms are known from
the code above only. The account's 16-byte `04` is a checksum of the
record (2.4).

[Guest] is the account with record key `01` (FileMaker's `KIndex`, which
its Guest check reads); FileMaker opens a file as [Guest] by a login of its
own, never by a typed password.

OpenMaker admits an account to a file, local or hosted by OpenMaker
Server, only when, after its stored password matched (2.1), its kind is 0,
it is not [Guest], it is active, no password change is due (bit 18 clear
and, under a set with `0c`, fewer elapsed calendar days than that since the
status's date), and its set is one OpenMaker enforces (2.3; OpenMaker
Server, the three built-in sets only). It refuses an inactive
account with 210 and a password due a change with 211, as FileMaker does
(the desktop then asks for a new one, below); [Guest] and other kinds as a
wrong password (212); and a kind, status or `0c` it cannot read (another
length, a non-canonical key, a date or time out of range, more than six
fraction digits) with 3. Each refusal leaves the file waiting for another
account, before any session exists.

### 2.3 Privilege set `0b`: the three built-in sets

`0b` is a key string naming the account's set under `[23, 2, 5]`
(session-identity.md 1). OpenMaker enforces the limits of FileMaker's three
built-in sets and of the custom sets below (session-identity.md 2.5), so a
local file opens only with an account in one of them, and a file hosted by
OpenMaker Server only with an account in a built-in set: the server does
not yet hold a client that sends its own requests to every member of a
custom set (its scripts, view-only fields, value lists and container
uploads), and refuses one with 9. The built-in sets are verified from
their keys and members, whatever their names
(`om_data::host::security::builtin_set`); the name `[23, 2, 5, S, 10]`
need only be readable, and the window shows it as stored:
- [Full Access]: the account's reference is `01 01` (member `01`) and its
  record privileges `[23, 2, 5, 01, 02]` are `55 03` [O for each part, in
  the starter solutions and the s33 fixture].
- [Data Entry Only] (member `02`) and [Read-Only Access] (member `03`):
  every member that grants or limits access exactly as the starter
  solutions store it (session-identity.md 2.7): record access `02`, the
  layout, script and value list defaults under `09` and no other `09`
  entry, other privileges `0a`, and no table entry `05`.

A custom set opens when OpenMaker enforces every member it has
(session-identity.md 2.5): each table's create, edit and delete yes, no
or limited by a calculation, its records viewable, viewable only where a
calculation is true, or never, every field modifiable, view only or
without access, layouts,
scripts and value lists with any access, the records through each layout
it may see modifiable, view only or without access (#707), printing and
exporting allowed or not, and any menu commands. Other custom sets (a
default limited by a calculation), a set
`01`-`03` whose members are not those session-identity.md 2.7 lists
(FileMaker's rules for one are not known, #729) and unreadable sets are
refused with 9 after a correct password. Whether FileMaker Pro also requires the
`fmapp` extended privilege to open a local file is unmeasured: the starter
solutions grant it to no set (session-identity.md 2.6).

FileMaker Pro 26.0.3 made two blank files under its German UI with the
same built-in set keys and checked access members but localized names:
`[Voller Zugriff]`, `[Nur Dateneingabe]` and `[Nur Lesezugriff]` [O:
session-identity.md 2.7, [#254](https://github.com/OpenMaker-Dev/openmaker/issues/254)]. OpenMaker verifies sets `01`-`03` by
their keys and members, whatever their names, so it reads these as the
built-in sets (#873; checked on both files, read-only, without a
sign-in).

### 2.4 How FileMaker writes an account

Evidence: FileMaker's engine code, read statically [from bin: Linux
`libDBEngine.so` 26.0.3 with DWARF, nm addresses below; Mac x86_64
`DBEngine` and `FMEngine` for the client paths], and the checksum below
reproduced [O] for every account of 80 FileMaker-written files: the 66
FileMaker Pro 26.0.3 starter files and 14 lab files (the s33 fixtures,
the test project and others), 163 accounts. A local FileMaker Pro account
write sequence is observed below.

**Members.** An account record `[23, 1, 5, A]` holds:

| Key | Content |
|---|---|
| `02` | kind (2.2), one byte; absent in every observed FileMaker-file account |
| `03` | description, text (`KDescription`); absent in every observed file |
| `04` | checksum, 16 bytes (`KCount` used as `CRC`; below) |
| `06` | password (2.1) |
| `0a` | status: timestamp and flags (2.2) |
| `0b` | privilege set key (2.3) |
| `10` (16) | name; for [Guest] a node whose child `1` is the text `[` U+E002 `]` |
| `d8`, `fc01`, `fc03` | the member UUID and modification stamps every catalog member has (schema-path.md, catalog keys and section 8) |

**Checksum `04`** (`DBUserAccount::ComputeCRC`, nm `0x120dbc0`;
`DBFileUser::InitSecureCRC`, nm `0xf31a20`). MD5 (Support's `CRCContext`
is an MD5: its initial state and round constants are MD5's) of, in this
order:

1. When the file's version `[2, 5]` (big-endian u16) is above 10, which it
   is in every observed file (1208 in the 26.0.3 starter files): the
   16-byte value of `[23, 4, 1, 10]` (the file access catalog's UUID,
   `DBAuthFileCatalog::GetUUID`) when it exists and is 16 bytes long, then
   the file's `mInternalCreation` `[2, 254]`, 4 bytes as stored.
2. The account's key as a one-segment path key: its length plus 1, its
   length, the key bytes (`02 01 02` for account `02`).
3. The kind `02`, one byte, 0 when absent.
4. The name as UTF-16 code units, big-endian, no length.
5. The first 31 bytes of `06`: version, salt, hash (not the bytes after).
6. The description `03` as UTF-16 code units, big-endian (nothing when
   absent).
7. The status `0a` as a path key: one byte holding the stored value's
   length, then the stored value. When absent, the key of 1/1/1024 at
   midnight, `06 05 90 00 41 80 00` [from bin; no observed account lacks
   `0a`].
8. The privilege set key `0b` the same way (`02 01 01` for `01 01`); when
   absent, `02 01 00` [from bin; never observed].

Vector, the English Tasks starter file's account `02` ("Admin"): MD5 of
`1a40e6a143b54e24be9884c5323d1a41` `dadd00d6` `020102` `00`
`00410064006d0069006e`
`01bf821c8e1e120aeda6413ac7b8a64b4ded8d15bbeeaa8bf89f02c6e7709b`
`09089f91038005521730` `020101` is `fd7ee9c1858689de75a95ebf99b187da`, its
stored `04`.

FileMaker recomputes `04` whenever it commits an account
(`DBUserAccount::DoCommitProcessing`, nm `0x12124c0`). When FileMaker Pro
opens a file (`DBSession::InternalCreateFileUser`, Mac x86_64 `0x1bae6a6`)
it recomputes the checksum of every account (`DBUserAccountCatalog::
HasBeenTampered`, nm `0x1215510`), and of the privilege set, extended
privilege and file access catalogs, and refuses the file with **808**
("Cannot open file because access privileges are damaged") when one
differs. So a writer must store a correct `04`, or FileMaker Pro will not
open the file; the conditions under which the open runs this check are not
fully decoded.

**What sets each member** (`DBUserAccount`, nm in parentheses). Each setter
but `SetPassword` does nothing unless the caller may manage the account
(`HasSufficientAccess`, `0x120e5b0`: full access, or a privilege set flag
that allows managing accounts and an account outside [Full Access]):

- `SetPassword(text, …)` (`0x1210000`), FileMaker-file accounts only (206
  otherwise): more than 100 grapheme clusters gives 11. A caller that may
  manage the account skips the next checks; any other caller needs its
  privilege set to allow changing its own password (206 otherwise), a
  password at least the set's minimum length (set member `0d`, u16;
  **208**, the minimum travelling with the error) and one different from
  the current password (**209**). It
  then writes `06` with a new random salt (`SetPasswordData`, `0x1210790`)
  and `0a`: today's date and the current time to the second
  (`DateTime::Now`), keeping the old status's flags in the microseconds,
  and clearing bit 18 when the account is the one signed in to the file
  (so changing one's own password ends "must change password").
- `SetEnabled(b)` (`0x1211430`): bit 19 of `0a` becomes `!b`; date, time
  and the other bits stay (an account without `0a` gets 1/1/1024).
- `SetForceExpire(b)` (`0x1211600`): bit 18 of `0a` becomes `b`; the rest
  stays.
- `SetUserType` (`0x1210a90`): `02`. `SetDescription` (`0x1210b20`): `03`.
  `SetPrivilegesKey` (`0x12117d0`): `0b` (assigning [Full Access] needs
  full access). The name is the catalog member's `10` and name list
  (schema-path.md 5.6).
- Commit (`DoCommitProcessing`): for [Guest] (key `01`) `06` becomes the
  hash of its name text `[` U+E002 `]` (the stored hashes of every observed
  [Guest] match); for an account changed from another kind to a
  FileMaker-file account, the hash of the empty password; for any other
  kind, the hash of the account's own name. Then `04`.

**Native local writes [O].** On 2026-11-13, FileMaker Pro 26.0.3 on macOS
changed a disposable copy of the English Contacts starter solution. The
file was closed and copied after each action; the byte-level account
catalog dumps remain private under `re/work/issue-148/` (issue [#12](https://github.com/OpenMaker-Dev/openmaker/issues/12)).
Manage Security added a FileMaker-file account with [Data Entry Only]:
its kind `02` was absent, and the new member had `04`, `06`, `0a`, `0b`,
`10`, UUID and modification stamps. Its index entries appeared too.
Disabling and re-enabling it changed only status bit 19, the checksum and
modification stamps; the password-set second stayed fixed, and both
`0a` and `04` returned to their earlier bytes on re-enable. Resetting
its password changed the salt and hash, stamped `0a` to the current
second and recomputed `04`; the password check bytes followed 2.1.
Setting "must change password" changed only status bit 18, the checksum
and stamps. After File Options' auto-login was turned off separately,
signing in as that account showed a required-change prompt, accepted a
new password and opened the file. This changed the salt and hash,
advanced the password-set second, cleared bit 18 and recomputed `04`.
Deleting the account removed its member and index entries. An independent
recalculation of `04` matched at every saved stage. These are UI and
sign-in observations; the account script steps below were not run.
Changing the starter's Admin password from empty through File > Change
Password also created an empty subtree at `[23, 1, 5, 02, 5d, 0000, 11,
18]` [O: both the key-tree scan and `om-hbam` read it; the same subtree
is present in another FileMaker-written full-access account]. Its
purpose and whether another password change needs to update it are not
established; it is not part of the `04` calculation above.

**Script steps** (Mac x86_64 FMEngine `ScriptRuntimeBase`):

| Step | Writes | Notes |
|---|---|---|
| Add Account (`DoAddAccount`, `0x681002`) | a new member: `02` (`SetUserType`), `10`, `06` and `0a` (kind 0 only), `0b`, bit 18 of `0a` when "User must change password" is set, `04` | needs full access (9); an empty name or the [Full Access] set gives 5 |
| Delete Account (`DoDeleteAccount`, `0x6815c6`) | removes the member | needs full access (9) |
| Reset Account Password (`DoResetAccount`, `0x6818b0`) | `06`, `0a` (password stamp, then bit 18 as the step's option says), `04` | needs full access (9) |
| Enable Account (`DoEnableAccount`, `0x681cde`) | bit 19 of `0a`, `04` | needs full access (9) |
| Change Password (`DoChangePassword`, `0x6b21be`) | `06`, `0a` (bit 18 cleared), `04` | the signed-in account only; 3 for [Guest] or another kind; both passwords cut to 100 code units; the old one must test 0 or 211 |

**Native hosted writes [O].** The `s148-hosted-account-writes-20261113`
private relay capture recorded FileMaker Pro 26.0.3 changing a disposable
Contacts copy on FileMaker Server: add a FileMaker-file account, disable it,
re-enable it, reset its password, then delete it. Each of the five saves
took an op60 bundle lock on `[23, 1]`, upgraded it with op61, sent op86
UploadMultiple into `[23, 1, 5]`, uploaded the full index `[23, 1, 1]`
with op13, and released the lock with op62. The first four op86 requests
carried the changed member under the source spot; deletion sent that
member key in the delete list and no source member. All calls received
successful transport replies, and FileMaker Pro reported the saves.
After the file was closed and removed from the host, its final account
catalog had no probe member or index entry; only the index counter,
modification count and timestamp differed from the staged copy. The
remaining Admin account's `04` matched the independent checksum rule.
This directly confirms the catalog transfer path for Manage Security
actions. The script steps above call the same catalog commit machinery
[I: from the client code], but their requests were not captured.

A password changed at sign-in takes another path: Login with a new
password, which FileMaker Server writes and checksums itself
(network-protocol.md 5.4) [from bin; not captured on the host]. Re-Login
uses login type 2 on the open connection [from bin; not captured]. Closing
and reopening the hosted file instead sent a fresh Open and a normal
login type 0 [O: same relay capture].

**OpenMaker.** A local file's host sets an account's own password the way
the sign-in change above does (`Hosted::change_password`, reached through
`LocalHost::change_password`): `06` with a new random salt and its check
bytes, `0a` stamped to the second in this computer's local time [I] with
bit 18 cleared and the other flags kept, and `04` again
(`om_server::accounts::checksum`, which reproduces the vector above and the
English Contacts starter's Admin and [Guest]), in one commit saved before
it returns. An inactive FileMaker-file account is refused with 210 before
its old password is tested, as FileMaker's check gives it; an account that may not manage itself
gets `SetPassword`'s checks (206, 208, 209); more than 100 characters is
11. A new password outside ASCII is hashed after `HashDecompose` (2.1), as
FileMaker Pro's own accounts are. It does not create the empty `5d`
subtree. Before it rewrites an account, it recomputes the stored `04`
from the account's current members; when the two differ nothing is
written (3 for a script step; OpenMaker's choice): FileMaker Pro would
refuse a file whose checksum OpenMaker got wrong (808), so an account it
does not read as FileMaker does is left alone. An account with no `04`
is given one.

OpenMaker Server's scripts run Change Password through the same
operation, as `DoChangePassword` does. The target is the account the
session logged in with, found by its record key so that a rename since
cannot redirect it. Both passwords are cut to 100 UTF-16 code units, and
[Guest] or an account of another kind gives 3. A step running with full
access counts as a caller that may manage the account, so it skips
`SetPassword`'s set checks [I: Claris documents that a script granted
full access may change a password the set does not let the account
change]. The step's dialog is not shown on a server, so with it the step
gives 3 [I: FileMaker Server's handling is not measured]. While it
writes, the script's session holds a write lock on `[23, 1]`, as Manage
Security does, and another user's lock gives 310 [I: Reset Account
Password's observed code]. The commit stamps the account and the
catalog's index `[23, 1, 1]` (schema-path.md 8.1), so a client's cached
copy of the catalog shows as stale, and other clients are told the
catalog changed.

Reset Account Password and Enable Account run through
`Hosted::reset_account_password` and `Hosted::enable_account`, under the
same lock, stamps, notification and checksum check, writing the members
the table above lists. A session without full access gets 9. Reset
Account Password takes only a FileMaker-file account (206) and a password
of at most 100 characters (11); its status is stamped as Change
Password's is. Its "Expire password" option gives 3 before anything
changes: the account could then sign in to a hosted file only with a new
password, which OpenMaker Server does not take at login yet
([#248](https://github.com/OpenMaker-Dev/openmaker/issues/248)). Enable
Account changes only bit 19; an account already in that state is not
written and no client is told of a change. It takes the file's own
accounts and external server groups (kinds 0 and 1) and gives 3 for an
OAuth or Claris ID account, and it gives 3 rather than make a [Full
Access] account inactive [I: Claris documents both refusals for Enable
Account; their codes are not observed]. Both find the account by name,
ignoring case; a name no account has gives 213 [I: FileMaker's code for
a name it does not find is not observed], and [Guest], whose name is not
text, is never found. A new status that would not read back as written
(flags added to a fraction of a second already held, past the six digits
the value keeps) is not written (3).

When a local file's Login is refused with 211, the desktop asks for a new
password in a Change Password dialog (New Password, Confirm New Password)
in the sign-in dialog's place [I: OpenMaker's wording; FileMaker Pro's
prompt was seen to accept a new password and open the file, above, but its
text is not recorded]. A confirmation that does not match is caught there;
OK sends the new password (`DataProvider::change_due_password`, which keeps
the account and the password the Login just refused, erased when dropped),
and the file opens with it. A refusal (206, 208, 209, 11) keeps the
dialog up with its reason; Cancel closes the waiting file. A hosted file
does not ask: there the new password travels in the Login
(network-protocol.md 5.4), which OpenMaker does not send ([#8](https://github.com/OpenMaker-Dev/openmaker/issues/8)).

File > Change Password… changes the password of the account a local file
is open with through the same host operation: the dialog asks for the old
password, the new one and its confirmation [I: OpenMaker's wording;
FileMaker Pro's dialog is not measured], a mismatch is caught there, and
a refusal (212 for a wrong old password, 206, 208, 209, 11) keeps it up
with its reason. The new password is then the session's, for the files its
scripts open later [I]. A file on a host does not offer it yet ([#248](https://github.com/OpenMaker-Dev/openmaker/issues/248)).

Unknowns: the account script steps above and hosted password change at
sign-in were not run live, so their errors for a missing account, for an
account catalog another user has locked (other than Reset Account
Password's 310) and for [Guest] are not observed; login type 2 was not captured; whether the
password stamp's time is local or UTC is not measured; the empty `5d`
subtree noted above is not explained. The checksums for privilege sets,
extended privileges and file access entries are specified separately in
session-identity.md 4.

## 3. The account catalog's header `[23, 1, 1]`

FileMaker Pro reads `[23, 1, 1]` before Login (network-protocol.md 5.6.9b).
It is the member data of the account catalog itself
(`DBUserAccountCatalog`, the file user's `mUserAccountCatalog`). In every
starter solution [O]: `00` = `01 02` (account 2's key), `04` = 2, `06`
empty, `16` = "Admin", `24` = "0" (text), `252` = `01 02`.

### 3.1 File Options > Open: "Log in using" and sign-in options

Three of these keys (decimal, as above) hold the Open tab of File Options
[O: files FileMaker wrote, below; from bin for the code paths]:

| Key | Constant | Holds |
|---|---|---|
| `16` (`0x10`) | `KName` | "Log in using": the account name. Empty when the option is off; the Guest account's stored name `[` U+E002 `]` (section 1 of file-metadata.md) for "Guest Account" |
| `06` | `KRequests` | the password, as text: masked like every stored text (SCSU, each byte XOR `0x5A`; file-format.md 3.5), **not hashed**. Empty for an empty password and when the option is off; the Guest name again for "Guest Account" |
| `24` (`0x18`) | `KDefaults` | a decimal number as text: `0x1` set when "Allow stored credentials for authentication" is **off**, `0x2` "Require iOS or iPadOS passcode", `0x4` "Show sign-in fields even when OAuth or AD FS is enabled"; FileMaker keeps any other bits |

So "Log in using" is on exactly when `16` is not empty, and the starter
solutions log in as Admin with the empty password (Claris documents that
FileMaker Pro opens such a file without asking, unless Shift (Windows) or
Option (macOS) is held, and asks again when the stored password is wrong;
it ignores the option for files hosted in FileMaker Server's secure
folder or FileMaker Cloud and for OAuth accounts:
[Setting file options](https://help.claris.com/en/pro-help/content/file-options.html)).
An account name or password is stored like any other text: as a plain
value (`[synthetic non-ASCII password]` below), or as the node of file-format.md 3.6 (the Guest
name below).

Evidence:

- **The dialog** (FileMaker Pro x86_64): `CDocPrefModel`'s constructor
  (0x10023eec2) reads `KName`, `KRequests` and `KDefaults` from the file's
  account catalog with `DBCatalogCommon::GetMemberData`, and
  `CDocPrefModel::WriteDirtyPrefs` (0x10024086c) writes them back with the
  catalog's text setter; `KDefaults` is rebuilt from three checkboxes
  (`CDocOpenClosePrefPage::SaveItems`, 0x10022dec6; tags 105-107 of
  `Base.lproj/IDD_PREFS_DOCUMENT.nib`). For "Guest Account" `SaveItems`
  stores the Guest name as both account and password. For "Account Name
  and Password" it first checks that the account exists, is active and
  that the password logs in (`DBUserAccountCatalog::LoginValid`). The
  password box shows a 20-character placeholder for a stored password,
  and saving the placeholder keeps the stored one.
- **The engine** (FileMaker Server `libDBEngine.so`, DWARF):
  `DBAuthenticationInfo::SetToPredefined` (0x1140680) makes the Guest
  login an account and a password that are both the Guest name, the pair
  the dialog stores. `DBSession::AllowAutoLoginAfterRestore` (0x11483f0)
  compares a session's account and password with `KName` and `KRequests`
  (or the Guest name) before logging in again without asking after a
  restore.
- **Save as XML** (FMEngine x86_64,
  `DynamicTemplateXML_Director::CreateMetadataXML_Login` 0x18c566 and
  `ProcessMetaDataXML_Login` 0x19f72e, `CreateMetadataXML_ShowSignInFields`
  0x18c29e) writes the keys as `Metadata` `Login` `type` -1 (off), 0
  (Guest) or 1 (`AccountName`, and `Password` when not empty),
  `SavePassword keychain requireMobile` and `ShowSignInFields enable`.
- **Files FileMaker wrote** [O, 2026-11-11, oracle harness]: FileMaker
  Server's `FMDeveloperTool --saveAsXML` of the Contacts starter solution
  writes `Login type="1"` with `AccountName` Admin and no `Password`,
  `SavePassword keychain="True" requireMobile="False"` and
  `ShowSignInFields enable="False"` for the values above. `FMUpgradeTool
  --generateDBFile` built three files from that XML with only these
  elements changed:

  | Save as XML | `16` | `06` | `24` |
  |---|---|---|---|
  | `type="1"`, Admin, password `[synthetic non-ASCII password]`, keychain False, requireMobile True, sign-in fields on | Admin | `[masked SCSU bytes omitted]` (round-trips to the same synthetic non-ASCII password) | "7" |
  | `type="0"` | `[` U+E002 `]` | `[` U+E002 `]` | "0" |
  | `type="-1"`, keychain True, requireMobile True | empty | empty | "2" |

Not followed in the code: where FileMaker Pro tries the stored login when
a file opens, and what it shows when the account no longer exists or is
inactive [U]; the behavior above is Claris's description.

OpenMaker opens a local file as that description says: with no account
given, it logs in once as `16` with the `06` password (an absent `06`
being the empty one), and opens the file without asking when that is
admitted, as the starter solutions open (their copies open with no
dialog). When it is refused, or the option is off, it asks, suggesting the
`16` account (section 6). Holding Shift or Option (Alt) when choosing the
file asks anyway. "Guest Account" (`16` the Guest name) asks too:
OpenMaker does not log in as [Guest] ([#19](https://github.com/OpenMaker-Dev/openmaker/issues/19)).

### 3.2 Files FileMaker Server does not host

Claris documents that FileMaker Server by default opens no file with
"insecure" accounts: a [Guest] account using the [Full Access] set, a
[Full Access] account with an empty password, or a [Full Access] account
whose password the file stores for "Log in using" (3.1); FileMaker Pro
does not upload such a file either. `fmsadmin set serverconfig
securefilesonly=false` and a restart allow them in the default and
additional database folders, never in the secure folder; the Admin
Console calls the setting "Host password-protected databases only"
([Uploading database files using FileMaker Pro](https://help.claris.com/en/server-help/content/hostdb-upload-pro.html)).
New files and the starter solutions are insecure: their Admin has [Full
Access] and an empty password (2.1).

The check [C: `DBUserAccountCatalog::IsInsecure`, Mac DBEngine 26.0.3
`0x1c03be0`, read statically; FileMaker was not run] walks the accounts
and stops at the first that makes the file insecure. An account counts
only when it is enabled (an inactive account, 2.2, never does) and its set
is [Full Access] (record `01`, `KIndex`) or one whose other privileges
include `kCanModifyAccounts`, "Manage accounts that don't have Full
Access" (`0x0010` of `0a`, session-identity.md 2.3). Such an account makes
the file insecure when:

- it is [Guest] (record `01`);
- its password is empty: FileMaker's typed-password check
  (`DBUserAccount::TestFMPassword`) answers 0 or 211 (a password change
  due) [I: that the password tried is the empty one is Claris's
  description; the call's argument was not read];
- or it is the account File Options log in as (its name is `16` of 3.1,
  case not counted) and the password they store (`06`) passes the same
  check. A stored password that is not the account's does not count.

FileMaker Server 26.0.3's message table (`libFMSLib.so`, server-logs.md 4)
has two messages for it [O]: 1017 (Error, databases) `SECURITY: Database
"%1" cannot be opened because it is insecure. Require Password-Protected
Databases is enabled.` and 1038 (Warning, databases) `SECURITY: Database
"%1" contains one or more insecure accounts using the Full Access
privilege set. Because "Host password-protected databases only" is
enabled, these types of accounts cannot be used to log in.` FileMaker
Pro's upload refusal (`libDBEngine.so`) is "File cannot be uploaded
because it does not have the required password protection." The lab
server (FileMaker Server 26.0.3) left a generated file whose full access
account had no password OPENING and then CLOSED, with no Admin API error
[O, one probe, 2026-10-29].

Unknown: when FileMaker Server writes 1038 and keeps the file open with
the insecure accounts refused instead of not opening it
(`ServerHostSession::IsInsecureFullAccessAllowed` and
`EngineDatabase::IsInsecure`, in `fmserverd` and `libDBEngine.so`, were
not read).

OpenMaker Server applies the check above when it opens a file, at startup
or when the administrator opens it again, before anything is written: an
insecure file is not hosted, raises an alert and writes 1017, unless the
server runs with `--allow-insecure-files` (`ServerConfig::insecure_files`).
An account whose status it cannot read counts as active; an account of
another kind (2.2) has no password to try. It never writes 1038. A
desktop's private host for a local file does not check: FileMaker Pro
opens such a file itself.

### 3.3 Save a Copy as

File > Save a Copy As and the Save a Copy as step (37) write a copy of
the open file. Claris documents four kinds, the step's value
(script-steps.md, step values): "copy of current file" (1), "compacted
copy (smaller)" (2, "removing blank space"), "clone (no records)" (3: the
tables, layouts, scripts, page setup and field definitions, no data) and
"self-contained copy (single file)" (4: container data stored as a
reference or externally, open or secure, embedded in the copy). A copy or
compacted copy does not include externally stored container data. The
command and the step are not supported in files hosted by FileMaker Server
or FileMaker Cloud. A step with no output file shows the Save dialog; its
other options are Create folders, "Automatically open file" and "Create
email with file as attachment" (`kOpenAutomatically`, `kCreateEmail`,
script-steps.md)
([Saving and copying files](https://help.claris.com/en/pro-help/content/saving-copying-files.html),
[Save a Copy as](https://help.claris.com/en/pro-help/content/save-a-copy-as.html)).
Claris also says an open transaction is committed first (script-runtime.md
8.5).

Observed:

- Under a custom set with exporting off, FileMaker Pro 26.0.3 opens the
  step's chooser, and Save fails with the alert that the file could not be
  created on this disk; the step returns 800 (session-identity.md 2.3) [O].
- FileMaker's compacted copies (`FMDeveloperTool --copyCompress`) keep
  every key and value and are packed bulk rebuilds (hbam-writer.md 9.3)
  [O].

Unknown: whether a copy of the current file is byte for byte the file
(its page 0 SCN, hbam-writer.md 5.1); the default name the Save dialog
proposes; what happens when the output file exists or is open (replaced,
refused, which error); the error a hosted file's step returns; whether the
window's uncommitted record is committed first; the copy's permissions;
what a clone resets (serial numbers, record ids, the saved found sets).

## 4. Implementation notes (OpenMaker)

OpenMaker Client opens a local file through a private host embedded in the
client, reusing OpenMaker Server's engine (`om_server::LocalHost`) rather
than a second reader and writer.

- **Opening and locking.** The file's path is resolved (links followed) and
  the file opened through one descriptor, which is then locked (`flock`):
  exclusively when the file can be written, shared when it cannot. The lock
  only counts while that descriptor is still the file at the path: one
  opened just before another OpenMaker renamed a new copy over the file
  holds the old file, whose lock is free once that save is done, so the
  open checks the descriptor's device and inode against the path after
  locking and opens again when they differ; a file that keeps changing, or
  is locked by another window or process, is refused with FileMaker's error
  300. Its bytes are read through the locked descriptor and decoded; a file
  that is not HBAM7 is refused with 807, one that does not decode (damaged
  or encrypted) with 805. Nothing is written before an admitted login.
- **Read-only files.** A file is opened read-only when it cannot be written
  safely: this user or its file system may not write it, its folder does not
  allow the new copy a save writes, it has other names (hard links, which
  the renamed copy would leave with old contents), it cannot be locked here
  (Windows; file systems without `flock`), or an entry already has the name
  of a save's copy (`<file>.omsave`). The window shows why; the provider
  refuses changes before sending them (FileMaker's error 6, also for the
  deletions scripts and portals make), the host refuses every write request
  (record creation, uploads, commits, serials, schema and container
  changes), and the store never writes the file.
- **Serving.** The decoded file is served on an ephemeral loopback port
  (127.0.0.1 only, never a fixed port) with a certificate generated for this
  host, and the host accepts only the client certificate of the provider that
  opened it: no other process completes a TLS handshake. Login checks the
  stored password (2.1), then the account's kind and status (2.2) and set
  (2.3): wrong credentials, [Guest] and other kinds are refused with 212,
  an inactive account with 210, a password due a change with 211, settings
  OpenMaker cannot read with 3 and an unsupported set with 9, each leaving
  the file waiting for another account. The session's Connect carries an
  empty install code. The host
  publishes no file in OpenMaker's file list (`om_protocol::hosted_files`).
  Its client's session is never closed for silence, since the client closes
  the host (below); its other connection limits are OpenMaker Server's
  (network-protocol.md 6.2), against anything else on the computer that
  connects to the port.
- **Writing.** Commits are saved as OpenMaker Server saves them: a complete
  new copy is written beside the file, flushed, given the file's
  permissions, locked, and renamed over it; a failed save takes the commit
  back. The copy is created only where nothing is (exclusively): an entry of
  that name another program or an earlier crash left, a link included, stops
  the save untouched, and a failed save removes only the copy it made. Until
  it has the file's permissions the copy is owner-only (created 0600,
  whatever the umask), so other users cannot read the database through it.
  Before renaming, the save checks that its locked descriptor is still the
  file at the path with no other names, so a file another program replaced
  or linked is not saved over. The lock moves to the new copy before the
  rename. The rename gives the file a new inode; ownership, ACLs and other
  metadata beyond the permission bits are not carried over.
- **Save a Copy as.** The host writes a copy of the current file or a
  compacted copy (3.3) to a new file only.
  - *Contents.* A copy of the current file is the file's bytes as the last
    save left them, read by position through a second descriptor of the
    one the host holds locked, so the version copied is the one the host
    has, whatever is renamed over the path meanwhile. A compacted copy is
    the key tree written as a new packed file (hbam-writer.md 7.2). So is a
    copy while changes a failed save left wait in memory, or of a file the
    host could not lock.
  - *Writing.* The copy is written and flushed under a hidden name of its
    own (`.omcopy-<process>-<n>`) in the output folder. It is then renamed
    to the output name with a rename that refuses to replace
    (`RENAME_NOREPLACE` on Linux, `RENAME_EXCL` on macOS), or, on file
    systems without one, hard-linked there and the staged name removed.
    An entry at the output path (a file, the open file, a link) refuses
    the copy and is left untouched. So does a name ending in `.omsave` in
    any case, the name the file's saves use for their new copy. A failure
    removes only the staged file; a crash can leave one beside, never a
    partial copy at the output name.
  - *Permissions.* The copy holds the password verifiers, so it is created
    owner-only (0600 on Unix), whatever the file's own permissions or the
    folder's group [I: OpenMaker's choice; FileMaker's is unmeasured]. ACLs
    the folder passes on to new files still apply.
  - *The desktop's step.* Before committing or creating anything, it
    fails as a step OpenMaker does not support (error 3; the desktop stops
    the script with its "not supported here yet" alert) for clones and
    self-contained copies, a step without an output file (the Save dialog
    of File > Save a Copy As is not built), "Automatically open file",
    "Create email", an open transaction and a hosted file [I]
    ([#63](https://github.com/OpenMaker-Dev/openmaker/issues/63)). Otherwise
    it returns 800 when the account's set denies exporting, commits the
    window's record, as Export Records does [I], and returns 800 when the
    copy cannot be created, an existing file included [I].
- **Recovery.** Deferred field work an earlier stop left in the file
  (schema-path.md) is finished in memory when the file opens and saved only
  once an admitted account logs in to a writable file: a wrong password,
  a refused account, Cancel or a failed open leave the file as it was.
  OpenMaker Server's own files are still recovered as it starts.
- **Closing.** Closing the file disconnects its session and gives up the
  session's hold on the host (below, "Related files and other files"). When
  no other session of the window's files has the file open, the host stops,
  closes every connection it accepted (one still in its TLS handshake
  included; a handshake has 30 seconds in all anyway), waits for its threads
  and reports whether the lock and port were released; a host that could not
  release the file in time (a server script that did not stop) is logged
  as such, and the file stays in use until it does. Quitting closes every
  file first; a process that dies releases them with its descriptors.
- **Related files and other files.** A local file's related files (the
  files its occurrences show tables of, file-metadata.md 1.1) and the files
  its scripts open (Open File, Perform Script and Go to Related Record of
  another file, docs/desktop-files.md) are other local files, opened by the same
  rules as File > Open:
  - *Where.* A data source's alternatives are tried in their order; only a
    relative `file` alternative with a plain name (stored `("file", "",
    Name)`, calculated `file:Name` or `Name`; `.fmp12` added when absent)
    is followed, and it names the file of that name in the folder of the
    file that names it, links followed (a related file's own related files
    are beside it). Related files take the first alternative that names a
    file there; a script's open tries each in turn, keeping the last error
    when none opens (missing: 100). Other forms are skipped: folders
    (`file:../Name`, `file:Sub/Name`), volumes and absolute paths
    (`filemac:`, `filewin:`, `filelinux:`), `fmnet` (a local file never
    contacts a server) and other schemes; an Open File whose every
    alternative is skipped is unsupported, and occurrences whose source has
    no followed alternative stay unresolved with a warning.
  - *One host per file, shared.* The files one window has open (the file
    the user opened, the files its scripts open and every related file any
    of them reads) form a family. Each file of a family has one private
    host (one engine, lock and port), and every session of the family that
    opens the file shares it, each with a connection and login of its own:
    a related file a script then opens, or a file that is both related and
    opened, is not refused as in use. A file is told apart by its path with
    links followed: another name or link for an open file is that file (a
    script's open finds its session already open; a cycle of related files
    ends there). Another window or process finds the family's files in use
    (300). Two different files with the same name are never open in one
    family: the second is refused with 802 [I: FileMaker Pro opens one file
    of a name at a time], so a name means one file to calculations, scripts
    and global fields.
  - *Accounts.* Each file is opened with the account and password the
    first file was opened with, and that file's own accounts must admit it
    (2). A file that refused the account and password is not sent them
    again until the first file is opened again: a later open of it, by a
    script or another file's relationships, fails at once with the same
    refusal (network-protocol.md 5.5). The first file keeps the account and
    password (erased when dropped) while it is open, to open the files
    beside it its scripts open later; a reconnect (below) does not keep the
    password, so after one a script's open of a new file fails with 802
    until the file is opened again. No license is read.
    - *FileMaker Pro* [O: a FileMaker Pro 26.0.3 run on a local pair, a
      file relating to one whose accounts differ, [#60](https://github.com/OpenMaker-Dev/openmaker/issues/60)'s comment
      https://github.com/OpenMaker-Dev/openmaker/issues/60;
      a hosted pair is unmeasured]: it does not ask at open. It asks when a layout first needs the related file (a field
      of one of its occurrences), whether the user or a script's Go to
      Layout shows the layout, and before the window draws when the opening
      layout needs it: a dialog titled `Open “Name”` with the text
      `Sign in to open “Name”.`, Account Name (filled first with the Mac
      user's name) and Password, a "Save password in Keychain Access"
      checkbox (off), Sign in and Cancel.
      Cancel leaves the layout drawn with `<File Missing>` in the related
      field, Go to Layout's error is still 0, and showing the layout again
      does not ask again in the session. An Open File of that file asks
      again; its Cancel gives 1, a Cancel after one wrong password 212 (the
      dialog stays up after the wrong password, with no alert under Set
      Error Capture On), and the file's own account opens it (0), after
      which the related field shows its value. Unmeasured: other steps
      that first need the file, the same without error capture, a hosted
      pair.
    - *OpenMaker*: a related file that refused is asked about as FileMaker
      Pro asks, once a window's layout needs it (a field or portal of one
      of its occurrences, whether the user or a script's Go to Layout
      shows the layout; the step's result does not depend on the answer),
      one dialog at a time, each file once a session: `LoginDialog` with
      FileMaker Pro's title and text, without the Keychain checkbox, its
      Account Name filled with the user's name (what `Get ( UserName )`
      answers). Sign in sends one Login with the account typed
      (`DataProvider::sign_in_related`), none for a pair the file refused
      before; a refusal keeps the dialog up with its reason, for another
      try (each one Login) or Cancel [I: that the layout's dialog stays up
      as the Open File step's does]. A file it opens joins the calculation
      schema after the others, which keep their ids, and its fields show
      their values at once. A Reconnect keeps no password, so after one
      such a file refuses the first account again and is asked about again
      when a layout needs it [I: OpenMaker's choice]. Cancel leaves `<File
      Missing>` in the file's fields; no alert names a file the window asks
      about. An Open File step of a file that refused asks the same way,
      also after a Cancel: Cancel gives 1, Cancel after a refused answer
      that refusal's code (212), and the file's own account opens it (0) as
      the step's "Open hidden" says, and signs in to it for the first
      file's relationships too (one more Login, of the pair the file just
      admitted). With no one to answer (the window closing, the connection
      lost) the step fails at once with the remembered refusal, sending no
      Login [I: OpenMaker's choice]. Go to Related Record into the file
      asks the same way, as Open File does (`tests/related_files.rs`
      `go_to_related_record_into_a_file_that_refused_the_account_asks_for_another`).
      Other steps that first need the file (Perform Script in it, an
      external script trigger, Set Field through a relationship) fail with
      the refusal (212) without asking
      ([#60](https://github.com/OpenMaker-Dev/openmaker/issues/60)):
      FileMaker Pro's behavior there is unmeasured.
  - *Lifetime.* Each session holds the host of its file; closing a session
    (Close File, the window, the file that opened it) gives up only that
    hold, and the host stops when the last one goes: closing the first file
    leaves a file its script opened working, and a related file stays open
    as long as a file reading it does. Close File refuses a file another
    open file's relationships read (3), as for hosted files.
  - *Writes and read-only files.* Every write goes through the session of
    the file that owns the table, and is saved to that file as above; a
    read-only file's writes are refused (6) whichever window makes them.
  - *Reconnect.* A local session reconnected after it was lost keeps its
    host, and the hosts of the related files it reads stay open through the
    reconnect: the same engines, locks and ports, with no moment another
    process could take the files. A file a script opened reconnects at its
    own path, independently.
- **License and session values.** The license store is never read for a
  local file. `Get ( HostApplicationVersion )` is empty, `Get ( FilePath )`
  the `file:` path, `Get ( HostName )` this computer's name and
  `Get ( ConnectionState )` 0. The login dialog's password, the request
  carrying it and the provider's copies are erased when dropped; transport
  buffers are not claimed to be.

## 5. Connecting to OpenMaker Server without a license

OpenMaker Server does not read the Connect's install code. OpenMaker Client
sends an empty one unless the user saved a license and checked Use saved
license; a credential store that cannot be read sends none rather than
refusing to connect (a server that requires a license then refuses, and the
error says the saved one was not sent). Claris-hosted FileMaker Server keeps
its own requirements.

## 6. Refused or not supported yet

- Accounts, local and hosted by OpenMaker Server: inactive accounts,
  a password due a change in a hosted file (a local file's is changed in
  the desktop's Change Password dialog, 2.4;
  [#8](https://github.com/OpenMaker-Dev/openmaker/issues/8)), [Guest],
  external (Claris ID, OAuth, external server) accounts, account
  settings in forms it cannot read (2.2) and every privilege set but the
  three verified built-in ones and, for a local file, the custom sets it
  enforces (2.3).
- File Options' "Log in using" with the Guest account (3.1): OpenMaker
  asks, since it does not log in as [Guest]
  ([#19](https://github.com/OpenMaker-Dev/openmaker/issues/19)).
- Related and other files from a local file (4): only plain names in the
  same folder are followed. Folder-relative, absolute (`filemac:`,
  `filewin:`, `filelinux:`), `fmnet` and other alternatives are skipped
  (their occurrences stay unresolved, with a warning); two files of the same name at
  once; a file another OpenMaker window or process has open (300). How
  FileMaker Pro orders and caches alternatives, and what it does on a
  name collision, is not compared.
- Writing files that are hard-linked, on Windows, or on file systems without
  `flock`; encrypted (EAR) files; conversion of older formats, Recover, Save
  a Copy As's dialog, clones and self-contained copies (4, "Save a Copy
  as") and sharing a local file with other clients.
- Native comparison: nothing here was compared with FileMaker Pro opening
  the same file. Copies of all 330 starter solutions of 2.2 open through
  `om_server::LocalHost` as Admin with the empty password, a wrong password
  and "[Guest]" refused with 212, each copy left byte for byte as it was
  (one read-only probe, not a test in the repository); none was written by
  OpenMaker under these rules.
