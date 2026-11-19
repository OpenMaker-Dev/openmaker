# Hosted-file list

The Hosts dialog's **List files** needs a host's published files before
any file opens. The client asks a host without this extension, FileMaker
Server above all, with FileMaker's own op77 GetFileList
(`docs/re/network-protocol.md` 5.3; [desktop client](desktop-client.md),
"Hosts dialog file list"). OpenMaker Server answers op77 with the same
published files, for FileMaker Pro's Hosts window (network-protocol.md 5.3,
"Implementation notes"). OpenMaker Server and the OpenMaker client also use
an extension of their own (`om_protocol::hosted_files`), which the client
asks first. It is not a FileMaker operation and is never sent to a host that
has not claimed it:

1. A connection of its own: TLS verified as for opening a file, `_is_a`
   and `Connect` with an empty install code (a listing opens no file, so
   no host needs a license for it).
2. `_is_a("IDL:OpenMaker/HostedFiles:1.0")` on the server object: CORBA's
   built-in type check. OpenMaker Server answers `true`. A host that does
   not implement the interface answers `false` by CORBA's semantics. A
   FileMaker Server 26.0.3 did (observed 2026-11-02: verified TLS,
   `Connect` without a license, `_is_a`, `Disconnect`; no file opened, no
   account sent). The client then lists that host's files with op77
   (above); only an op77 reply it cannot read reports the host as
   unsupported, never as listing nothing.
3. `OpenMakerListFiles(in unsigned long connectionid, in unsigned long
   version)` (CDR, little-endian): the id `Connect` returned and the
   newest list version the client reads (1). The server answers only a
   connection it handed out. The reply is `unsigned long version`, then
   `sequence<string>` of UTF-8 names as clients open them, at most 10,000
   of at most 1,024 bytes, none empty or holding `/`, `\` or a control
   character, none twice (case-insensitively). The client rejects anything
   else, trailing bytes included, as malformed rather than shortening the
   list.
4. `Disconnect`, whatever the outcome; a cancel stops between steps.

What is listed is the server operator's choice: `Server::listing_file`
(`om-server --list <name>`) or `listing_all_files` (`--list-all`). Files are
unlisted by default because OpenMaker Server does not read a file's own
"Don't display in Open Remote File dialog" setting yet (`[2, 18]` bit
`0x80`, `docs/re/file-metadata.md` 3.2), so it cannot tell which files
their designers hid. An unlisted file still opens by name; a listed one
still needs an account (visibility is not authorization). The list is the
same for every client: FileMaker Server's "Filter Databases" (per-account
lists) needs a credentialed request, which is not implemented, so anyone
who can reach the port sees the listed names. Names only: never paths,
accounts, or files not hosted.

The client's `om_data::HostBrowser` (from `DataProvider::host_browser`)
runs each listing on a thread of its own, apart from the provider and its
open file. `ListingFailure` tells apart an unreachable or untrusted host,
an unsupported one, a malformed or incomplete list, and a cancel; an empty
list is a success. The GUI keeps a listing with the host and port it was
asked of and a token, shows it for that server only, and drops replies to
listings it no longer waits for.
