# Script definitions and button actions

Observed on FileMaker Pro's installed arm64 FMEngine (SHA-256
`e594909ee57d4bc7e412a18450f38fab8efc16bdfbe19419f70b4eaeefa672cf`),
with a caller-supplied Contacts file used as a read-only cross-check.
No native source or client code is incorporated in the implementation.

## Catalog

Root 17 is the script catalog. Definitions are under `17/5/<id>`.
Key 16 is the masked-SCSU name; key 4 is the assembled step list;
`5/<step-key>` holds step parameter leaves. Folder and separator entries
have no step list. The Contacts file distinguishes these through bit 0 of
its key `0xf0` catalog flags; this interpretation has only been checked on
this file and unknown metadata is retained.

`Draco::ScriptStep::Read` at `0x4f38d0` reads 6 opaque reference bytes,
then 14 opaque reference bytes, then a big-endian signed 16-bit opcode,
a big-endian 16-bit value, then a big-endian 32-bit flags word. DWARF
(FileMaker Server 26.0.3 `libFMEngine.so`) names the five fields
`mDataKey`, `mReserved`, `mStepID`, `mStepValue` and `mStepFlags`
(`ScriptStep::StepFlags`); the crate's `ScriptStep` names them
`data_key`, `reserved`, `opcode`, `value` (u16) and `flags` (u32). Field
meanings, flag bits and every parameter location:
[script-steps.md](script-steps.md).
`ScriptStep::Write` at `0x4f3a18` writes those fields in the same order.
Each descriptor is 28 bytes. `IsStepEnabled` at `0x4f3b88` tests the
opcode sign bit; `SetStepEnabled` at `0x4f3b98` negates its absolute value
when disabling. A negative opcode therefore denotes a disabled step.
The implementation preserves both reference regions and every parameter
leaf. `ScriptStepList::Save` at `0x510fb4` writes descriptors in their
current list order through that writer. It does not establish the remote
catalog transaction or lock protocol.

Step keys and member references are `HBKey<5>` values: a length byte, that
many key bytes (`01 <id>` for a one-byte id), then filler that is not part of
the key. The filler is usually `00 be` or `be`, but stale bytes occur
(FMServer_Sample has `02 01 06 00 6f 00`), so only `1 + length` bytes may be
read (script-steps.md 2.1); the crate keeps the filler and ignores it. A field target in the 14 reference bytes is
a `DBFieldSpec`: table key, field key, big-endian repetition. Unsupported
references fail execution rather than becoming guessed targets.

## Layout actions

The repository's `LayoutObject.proto` declares `ScriptStep` fields 1=id,
2=value, 3=flags, 4=reserved. Buttons store the message in field 3 of
`ButtonLayoutObject`, and button groups store it in field 1 of
`GroupLayoutObject`. The Contacts list arrow has opcode 6, value 5,
flags 8 and a reserved local reference to layout 3. The details back
button refers to layout 2. Their option value is not the target id.

The summary field buttons perform script 2. Their object attributes
`17/0000/5/6` contain literal arguments such as `First Name`. Script 2
contains Go to Object (145), then Select All (50). The object-name
calculation is `9b3e`, observed as Get(ScriptParameter). These exact
literal and parameter expressions are supported. Unknown compiled
calculations produce an explicit execution error.

`ScriptRuntimeBase::GetGotoValue` at `0x6850c4` reads the current step's
value (`mStepValue`) at offset `0x16` and dispatches 1=first, 2=last, 3=previous,
4=next, 5=calculated number, 6=stored serial-key number. This supports
record navigation without guessing option bits.

## Limits

FileMaker has no step or call-depth limit (script-runtime.md 2.6), and
neither has `om_scripts::runtime`: script calls are frames on a heap stack,
and a host may choose a step budget per top-level script (the desktop
client uses 1,000,000 steps and 50 ms time slices). Set Field by Name uses
calculation slot 1 for the name and slot 0 for the value, observed in
`ScriptRuntimeBase::DoSetField` at `0x667948`. Calculations are evaluated
by `om-calc`; how the runtime settled the open points of the runtime
specification is in script-runtime.md, "Implementation notes".

Runtime semantics (control flow, errors, variables, triggers,
transactions, server execution) are in [script-runtime.md](script-runtime.md).

What OpenMaker saves: a script's steps through its own authoring
extension ([authoring.md](../authoring.md)), and new, duplicated,
renamed, moved and deleted scripts and the two script options with
FileMaker Pro's transaction below, on OpenMaker Server and on local files
([desktop-client.md](../desktop-client.md), Script Workspace).
Editing a draft never claims a server save.

## The catalog index `[17, 1]` [O: the 66 starter files, 2026-11-13]

Every starter file (6 templates in 11 languages, 330 members) has these
index keys, and no others:

| key | value | meaning |
|---|---|---|
| `00` | key string | last issued member id (schema-path.md 5.1); equal to the highest member id in every starter file |
| `01, <name key> 00 00 00` | key string | name list: one entry per member, the folder end `--` included |
| `03, <u32 position>` | key string | display order, positions 1..n without gaps |
| `04` | u32 | member count, folders and their ends included |
| `07, S` | node | a cache of each member: its name `10` (as the member stores it, chunked or not) and its member flags `f0` (8 bytes). It matches the member's own `10` and `f0` in all 330 members [O]; when FileMaker reads it is not established |
| `08` | 16 bytes | the catalog's digest (`KGlobal`): MD5 over each member's private flags and name, which FileMaker checks when a file opens. A file whose digest does not match the scripts it holds is refused with error 808 ("The access privileges in this file have been damaged or possibly tampered with"). See "The script catalog's digest" below |
| `20` | `04` | in every file; the layout index `[4, 1]` has the same, the value list index `[21, 1]` has `01`. Probably the Manage dialogs' last view order (`KLastViewOrdering`) [I] |
| `d8`, `fc`, `fc01`-`fc03` | | the index's UUID and modification stamp (schema-path.md 8) |

**Name keys do not depend on the file's language.** A name list key is
FileMaker's key of the name in the catalog language (schema-path.md 5.6),
the same in every file: an ASCII name is DUCET 5.2.0's primary weights and
`00 00 00` in all 66 starters; accents add a secondary level ("Añadir
registro relacionado": 27 primary weights, then `00 00 01 01 2f 01 ...
01 00`, `2f` for the tilde), kana a tertiary one, and ideographs and Hangul
syllables key as FileMaker's own weights (`76ee 9332`, the code points, for
`目録`). How the list holds two scripts of one name is not established: no
starter file has two (layouts and fonts of one name are listed under the
key and the member's key segment, schema-path.md 5.6).

**A script member** `[17, 5, S]` in the starter files has `04` and `05`
(scripts only), `10`, `13` = 0, `d8` and `d9` (version-4 UUIDs), `f0` and
the stamp with `fc`; folders and folder ends have no `04`.

**What OpenMaker writes** (`om_data::script_catalog`), all [I where
FileMaker Pro's own records were not compared key by key]:
- **A new script:** `04`, `05` when its steps have data, `10`, `13` = 0,
  a version-4 `d8`, `f0` = 0 and `fc01`-`fc03`, without `d9` and `fc`,
  the keys FileMaker Pro 26 gives the tables and fields it creates
  (schema-path.md 5.8). **A duplicate** is the copy's steps and keys with
  the same changes: a new `d8`, no `d9` or `fc`, a new stamp, and a new
  `d8` for each of its steps that has one (as FileMaker gives a duplicated
  step, script-steps.md, "Implementation notes (Script Workspace)").
  In the index: `00` raised, the name list entry, a `07` entry, `04`
  raised.
- **A rename** changes the member's `10` and stamp, the name list entry
  and `07`. **Include in Menu** and **Run script with full access
  privileges** change bit 3 of `f0` and bit 0 of `f1`, written as 8-byte
  words in the member and in its `07` entry, and the stamp. [O, #914:
  FileMaker Pro 26.0.3 granting full access on a new script wrote `f1` =
  1 in the member and in `07`, and a new `08`. A file with `f1` in the
  member only, and `08` over the cache without it, opened and passed Pro's
  consistency check, but Pro showed no full-access badge and still offered
  Grant Full Access Privileges. What the option does at run time, for a
  restricted account, is not measured.]
- **A delete** empties the member (op13 node state `00`, as FileMaker Pro
  sends it) and removes its name list entry (found by the id it holds),
  its `07` entry and its display position; `04` goes down, `00` stays.
  Perform Script steps, buttons, triggers and privilege set entries
  (`[23, 2, 5, P, 09, 11 S]`, session-identity.md 2.2) that name it are
  left as they are, naming an id no script will have again.
- **A move**, a new member and a delete rewrite the display order whole
  in the Script Workspace's order, positions 1, 2, ...
- Every save writes the catalog's digest `08` for the member cache as it
  leaves it ("The script catalog's digest", below): a save that kept the
  old one would make FileMaker refuse the file after a new script, a
  duplicate, a delete or a rename ([#66](https://github.com/OpenMaker-Dev/openmaker/issues/66)). A save is refused when the
  stored `08` is not the digest OpenMaker computes for the stored cache
  (a file FileMaker already refuses, or flags or names OpenMaker reads
  otherwise). The digest recomputed this way equals the stored `08` of
  all 66 starter files, and the digest of their members too [O]. Every
  save stamps the index; `20` and `d8` are kept. A member's `07` entry
  carries its `f1` when the member has one (an option change, or a copy of
  a script with full access), so the members' digest and the cache's are
  the same, as in Pro's own save (#914). OpenMaker
  writes only ASCII names for now, each under FileMaker's name list key
  (`field_catalog::listed_key`), in any file whatever its language: a
  Create New file's empty catalog and the starters whose names show no
  ASCII key included
  ([#274](https://github.com/OpenMaker-Dev/openmaker/issues/274); [I]:
  FileMaker Pro has not opened such a file since). Deletes, moves and
  option changes write no name. New folders and separators
  are not written: how the name list keys a second `--` or `-` is not
  established.

## The script catalog's digest `[17, 1, 08]` (High: bin and six files, 2026-11-13)

`[17, 1, 08]` holds an MD5 digest of the script catalog's member list.
FileMaker checks it whenever a file opens. A file whose digest does not
match fails with error 808, "Cannot open file because access privileges
are damaged". FileMaker Pro shows that error as "The access privileges
in this file have been damaged or possibly tampered with" (DBEngine
resource `(129, 1315)`).

**The digest** (FMEngine `M_ScriptIndexList::RecalculateCRC`, Mac x86_64
`0x491a36`):
- It is MD5 (Support `Draco::CRCContext` over `FMC_Init`/`FMC_Update`/
  `FMC_Final`, MD5's initial state and round constants).
- It walks the member cache `07, S` in key order. For each entry it
  takes:
  1. its private member flags `KPrivateMemberFlags` (`f1`) as 8 bytes,
     big-endian, or 8 zero bytes when the entry has none;
  2. then its name `KName` (`10`) as UTF-16BE, without a terminator
     (`CRCContext::Update(unistring)` byte-swaps each code unit).
- The stored value is read with `GetMemberData(KGlobal)`, segment `08`.

Recomputed this way, the digest equals the stored `08` of the untouched
English Tasks starter (`2715c72bbedde75330ff774698fd66aa`: nine members,
none with `f1`) [O]. No starter member has `f1`. OpenMaker sets bit 0 of
`f1` for "Run script with full access privileges" (above), and that
changes the digest.

**The check** (`FMScriptCatalog::HasBeenTampered`, `0x4916b0`):
1. With no members, the catalog is not tampered.
2. Otherwise it computes the same MD5 over the members `[17, 5, S]` in
   key order: an 8-byte big-endian value per member, from a catalog
   virtual at vtable offset `0x160` (the member's private flags [I]),
   then its name.
3. It reads `08`. A value of other than 16 bytes reads as 16 zero bytes,
   so a missing `08` fails the check.
4. If the members' digest equals `08`, the catalog passes. If not, it
   computes the cache's digest above, and passes if that equals `08`.
5. Otherwise the catalog has been tampered with.

**The error.** FMEngine `FMSession::FinishFileUserObject` (`0x605348`)
calls `HasBeenTampered` while it sets up the file user, after the
account has signed in. When it returns true, it stores error 808. This is
why FileMaker Pro signs in and writes to the file before it refuses it
([#66](https://github.com/OpenMaker-Dev/openmaker/issues/66)). DBEngine has one other open-time 808: `DBSession::
InternalCreateFileUser` (`0x1baf036`) stores it when the download of the
access catalog `[23]` (`KAccess`) fails (session-identity.md 4).

**When FileMaker rewrites `08`.** `M_ScriptIndexList::Update`, which
keeps the `07` cache in step with a member, and `MixInRecover` (Recover)
call `RecalculateCRC`. `FMScriptCatalog::SetCRC` (`0x491cfa`) writes the
16 bytes. So the digest follows every change to a member's name or
private flags, and every member added or removed [I: the store after
`Update` was not traced instruction by instruction]. A change to steps
alone leaves the names and flags, and so the digest, as they were.

**Observed** on the six stages of [#66](https://github.com/OpenMaker-Dev/openmaker/issues/66) (OpenMaker saves of copies of the
English Tasks starter; `08` kept at its original value in all of them):

| Stage | Scripts' digest equals `08` | FileMaker Pro | `FMDeveloperTool --saveAsXML <file> Admin ""` |
|---|---|---|---|
| A, no change | yes | opens | opens |
| B, a new script | no | refused | `(808): Cannot open file because of damaged access privileges.` |
| C, a duplicate | no | refused | 808 |
| D, a delete | no | refused | 808 |
| E, a step added | yes | opens | opens |
| F, a rename | no | refused | 808 |

`FMDeveloperTool --checkConsistency` finds 0 bad blocks in all six. A
tool that opens the file with an account (`--saveAsXML`, and so
presumably `--copy`) runs this check, so FMDeveloperTool is an oracle for
a fix, without FileMaker Pro.

**Other catalogs.** The same kind of check, `HasBeenTampered` over a
`ComputeCRC`, exists in DBEngine for the privilege sets
(`DBPrivilegesCatalog`), extended privileges
(`DBPrivilegeExtensionsCatalog`), accounts (`DBUserAccountCatalog`) and
external authentication files (`DBAuthFileCatalog`), all in `[23]`
(session-identity.md 4). The only one outside `[23]` is the script
catalog's: no layout, value list, table, field, relationship or custom
menu catalog has a `HasBeenTampered` or a `SetCRC` in DBEngine, FMEngine
or the app (symbol tables) [O for the symbols; that no check works
under another name is inferred].

**Not established:** what `0x160` returns for a member: private flags is
inferred from the `07` cache's use of `KPrivateMemberFlags`. When `f1` is
in the member but not in `07`, Pro's Script Workspace shows the script
without full access [O, #914], so its display reads the cache or depends
on the digest [I: which of the two is not established]. Also not
established: the digest of a file whose names are not plain ASCII (the
names are hashed as they are stored, decoded to UTF-16).

## Saving scripts to a host (live capture, 2026-10-29)

FileMaker Pro 26.0.3 (macOS) edited scripts in the Script Workspace of a
file hosted by OpenMaker Server, which recorded the session (`om-server
--record`; recording kept git-ignored, it holds the client's license key).
The maintainer created a script, saved an edit to it, created a second
script, deleted it and created a third. Only the client's requests are FileMaker evidence:
the replies were OpenMaker Server's, and FileMaker Pro accepted them.
Separately, FileMaker Server's native script engine read and ran scripts
from the saved file through `tools/fms-script-harness` in its network-less
container. That checks saved-file interoperability, not FileMaker Server's
network replies.
Paths are catalog 17 (`11`): a script `S` is `[17, 5, S]` and the catalog
index is `[17, 1]`. The ops are schema-path.md's (op60 4.1, op61 4.2, op62
4.3, op82 3.1, op13 3.3); the script edit is a catalog edit like Manage
Database's, two nodes at a time.

**Opening a script** (selecting it in the workspace): op60 on `[17, 5, S]`,
lock type 9, with "want mod count" set (argument = the file id), under a
transaction of its own. A script being created is locked the same way
before its node exists.

**Saving** (`⌘S`, or creating a script), in this order:

1. op60 `[17, 1]`, type 9, want mod count, a second transaction.
2. op82 HostTimeStamp: the host's time for the stamp below.
3. op61 `[17, 5, S]` from type 9 to `0a` (the opening transaction).
4. op13 to `[17, 5, S]`, node state `02`: the script's whole subtree
   (name, steps, parameters; scripts.md "Catalog"). It carries a stamp
   under `fc`: `fc 01` the client's user name, `fc 02` the account name
   (masked SCSU), `fc 03` the host timestamp from step 2 as a key string
   (`08` and 8 bytes in this capture; shorter before 02:46:40,
   schema-path.md 8.1).
5. op62 `[17, 5, S]`, type `0a`, attention `0x2`.
6. op61 `[17, 1]` from 9 to `0a` (the second transaction).
7. op13 to `[17, 1]`, node state `02`: the whole catalog index (every
   script's entry, display order, folders; file-metadata.md), with the
   same `fc` stamp. `[17, 1]` is a protected index (schema-path.md 3.3):
   FileMaker Server copies it only when it holds `fc` and `KSerNum`.
8. op62 `[17, 1]`, type `0a`, attention `0x2`.
9. op60 `[17, 5, S]` type 9 again: the script stays locked while it is
   open.

The two `0a` releases with attention `0x2` are what tell the other clients
(schema-path.md 9). The new scripts used ids 5, 6 and 7 in order, with 7
allocated after 6 was deleted. Deleted id 6 was not reused in this capture;
the general allocation rule is not established by these three creations.

**Deleting a script:** op60 `[17, 5, S]` and `[17, 1]` (type 9), op61
`[17, 5, S]` to `0a`, then op13 to `[17, 5, S]` with node state `00` and an
empty payload, op62, and the index saved as in steps 6-8 without the
script's entry.

**Leaving a script** (selecting another, closing the workspace): op62
`[17, 5, S]`, type 9, attention 0.

OpenMaker's Script Workspace sends the same sequences, without the locks
held while a script is open (it takes `[17, 1]` and each changed or
deleted member just before the save), and then reads the catalog back
(`om-provider` `scripts.rs`).

**Open:**

- The keys of FileMaker Pro's own new script record and of the index it
  uploaded (whether it writes `d9`, `13` and `f0` for a new script, and
  what it does to `08`).
- FileMaker Server's handling of an op13 with node state `00` is not
  observed. OpenMaker Server stores an empty node at `[17, 5, S]`, which
  FileMaker Server's engine ignores (the index no longer names it);
  OpenMaker's script reader skips an empty member. Whether node state
  `00` removes the node can be checked on the lab server with a
  disposable file: op13 state `00` to an existing node, then op86
  NodeExists on it.
- Saving a script in another folder, a rename, a move between folders and
  a duplicate were not in this capture.
