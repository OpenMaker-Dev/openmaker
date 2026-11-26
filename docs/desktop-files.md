# Desktop Open File

Open File (33) opens a hosted database as an independently owned desktop
session. It does not replace the initiating file's provider or turn a
related-table reader into a desktop session.

The public [Claris Open File reference](https://help.claris.com/en/pro-help/content/open-file.html)
states that the following steps execute in the file containing the script,
and that hidden opens still run startup scripts. The
[Perform Script reference](https://help.claris.com/en/pro-help/content/perform-script.html)
defines external calls and case-sensitive data-source/script names. Stored
source ids and path alternatives follow `script-steps.md` 11.8 and
`file-metadata.md` 1.1.

## Supported sources

- Stored `20` source references: one complete counted serial key, resolved
  against the initiating file's `32/5` data-source catalog. Its `82` path
  stream must decode completely before any alternative is used.
- A calculated data-source name, or a CR/LF path list containing plain
  hosted names, `file:Name`, and `fmnet:/host/Name`. An explicit nondefault
  port must match the connected endpoint. Source names compare exactly;
  hosted file identity ignores case and adds `.fmp12` only when absent.
- Relative sources inherit the current endpoint. An explicit `fmnet`
  source naming that endpoint is a file there: a stored one is read as FileMaker Pro 26.0.3
  stores it, the host (and port) between two U+E000 characters
  (file-metadata.md 1.1, [#232](https://github.com/OpenMaker-Dev/openmaker/issues/232)), and a calculated `fmnet:/host/Name` the
  same way. The host is compared as text, any case, never looked up; no
  port means 5003. One naming another host or port is that host's file
  (below). A folder on the host, absolute/local files, other path schemes
  and ODBC are refused. A
  hosted file's related files follow these paths in their order, and an
  `fmnet` path to another host or port too, once the user signs in to
  that file there ([#272](https://github.com/OpenMaker-Dev/openmaker/issues/272); network-protocol.md 5.5).
- A source on another host or port (a stored or calculated `fmnet` path)
  is named to the provider as `fmnet:/host:port/Name`. It is sent, on a
  connection to that host whose certificate is verified first:
  - with a file of that host's open already, through that file, the
    account that host admitted, in that file's session there (what it
    answers stands: a host it cannot reach asks for no account);
  - else an account that host admitted in this session (typed for one of
    its files, or the opening file's, which it admitted), kept while the
    opening file is open, as the opening file's own;
  - else the opening file's account and password, once for that host: a
    refusal by any of its files means none of them is sent them again in
    the session.

  A host whose certificate is not trusted is sent no Login, and the step
  fails without asking. Once the account sent is refused (or none can be
  sent: the opening file reconnected without keeping its password), Open
  File, and a step that first needs such a file through its data source
  (Perform Script in it, an external trigger), asks for an account, as
  Open File does for a file that refused ([#60](https://github.com/OpenMaker-Dev/openmaker/issues/60), #991), and that account
  opens it there, on that host's port; a refusal there is remembered for
  that host. An accepted account that does not open the
  file (none of that name there, no connection) moves on to the source's
  next path. The calling script waits at the prompt with its file,
  window, variables and transaction as they were. Cancel fails the step
  and ends the search: Open File and Perform Script by stored file
  reference with 1, a user cancel, which an open transaction records
  (script-runtime.md 8.4), Perform Script by name (`source::script`) with
  104, as for any source a name does not resolve (13.16) [I: FileMaker
  Pro's codes after Cancel there are unmeasured]. Go to Related Record to
  an occurrence of such a file asks too (below). The new file's session
  context and reconnect endpoint are that host and port, its own relative
  sources stay there, and open files are told apart by host, port and
  name, so `Peer` on two hosts is two files. FileMaker Pro sends that host
  the opening file's account and password, then an empty password, and
  asks once both are refused (network-protocol.md 5.5); OpenMaker sends
  the first and not the empty password [I: what the empty password stands
  for, perhaps File Options' automatic sign-in, is not established].
- Unsupported alternatives are skipped; supported alternatives are
  attempted in order, keeping the last open error if none succeeds. No
  unsupported path is reinterpreted as a same-host file.
- The sole path of a stored local source can be a file global such as
  `$$RelatedPath`.
  Open File reads it when the step runs. A relative `file:` path names a
  sibling and an absolute macOS `filemac:` path names a file on its stated
  volume. An unset variable returns 802 and a missing file returns 100.
  With Set Error Capture on, the script continues without a prompt.
  For a root file's related reads, a source with just one variable path
  waits for first use and keeps its first successful binding for the
  session. The stored source is preserved. See file-metadata.md 1.1
  and [#988](https://github.com/OpenMaker-Dev/openmaker/issues/988).

A new native session inherits the caller's TLS identity, CA configuration,
machine/license configuration, endpoint and account credentials. It opens
in the session the client has on the file's host when one is open there
and not lost (#1011): the opening file's for a file on its host, on
another host a related file's there, and a file opened through a file of
that host its session. FileMaker Pro opened two hosted files of one host,
in two windows, on one connection ([#229](https://github.com/OpenMaker-Dev/openmaker/issues/229)). So a host gets one connection,
one Connect and one license from the client. A file is opened and logged
in to once on it: a desktop file and a related file of the same file,
or two files' common related file, with the same account and password
hold that one open, each with its own copy of its change notifications;
closing one leaves the other its reads, records and locks, and the op62
goes with the last (#1011). A file open there for another account opens
on a connection of its own. The connection ends with its last file, and
a host with no such session gets a connection of its own. Credentials
remain in zeroing storage only for the lifetime of that provider so later
script opens can authenticate. They never enter UI events. A newly opened
file supplies its own schema, script catalog, account/privilege metadata,
record cursors, calculations, globals, windows and transaction state.
Global-field overrides are keyed by owning file and native table, then
mapped into each imported schema. A shared generation expires provider
working sets after another desktop session changes those values. A file
held by a relationship retains its overrides after its desktop window
closes; closing the last holder clears them. Script/frame variables remain
scoped by the runtime's file ids.

## Ownership and execution

The worker keeps an active provider/state pair and a registry of peer
pairs. Switching windows moves the entire pair. File and window ids are
allocated monotonically within an application epoch; replies retain window
ownership even after that window closes. Catalog/schema replies from a
closed window may reach another window of the same file, never another
file. Calculation caches, warning history and unsaved UI records stay with
their owning file/window.

Opening a peer preserves the caller's current record, drafts, found sets,
variables, windows and open transaction. The caller continues. A repeated
open reuses the same desktop session. Open hidden hides that file's
windows; a visible reopen shows them without reconnecting. A visible Open
File requests foreground presentation independently of the caller's script
context. The last explicit Open File, New Window or Select Window request
is applied once after the visible viewports are drawn. Internal trigger or
caller restoration is state-only. After such a request, a modal prompt or a
pause brings the script's own window forward once, so the user answers it
there and Continue goes on in the caller; the script's windows then come
forward as it works in them, as without a request. A later user selection
supersedes the presentation request; ordinary state replies do not raise
the opener again. Hidden opens and implicit background file resolution
request no foreground presentation. Dialogs and cards retain their existing
rules. Native macOS activation and FileMaker foreground parity for this
path remain unverified. Hidden sessions remain owned until closed. A new
window, a prompt, a pause, or Select Window run by a script of that file
can reveal a hidden session's window. Startup FirstWindowOpen/WindowOpen
scripts run once per new desktop session, including hidden opens; opening
an existing session does not rerun them. The default layout and existing
layout/record trigger machinery are retained.

An external Perform Script resolves sources in its calling file, fetches
the target's script, runs it in that target's own window/context and
returns its result to the caller. A file it opens is opened hidden, as
FileMaker Pro opens the source file of a related field (script-runtime.md
9.2; whether Perform Script itself opens it hidden is undocumented, U16).
The runtime remembers the caller's most recent window. Scripts started by
the user over a pause retain the window the user selected. File calculation
metadata and DatabaseNames/WindowNames include owned desktop peers; related
calculation contexts retain their existing owning-file metadata and
container callbacks.

Close File and last-window close release only their owning desktop
session. Other files survive and remain usable. Closing an idle peer while the caller
is paused preserves the caller's pause and continuation. Closing a different file
preflights and commits its windows before removing them; a refusal retains
it. Relationship-held sources are refused with error 3, matching the
restriction in the [Close File reference](https://help.claris.com/en/pro-help/content/close-file.html).
Whether a file holds the one being closed is asked by host, port and name
(a hosted file by `fmnet:/host:port/Name`), so a relationship to a file of
the same name on another host does not hold it, and one through an
`fmnet` path to another host does ([#272](https://github.com/OpenMaker-Dev/openmaker/issues/272)).
Related-reader connections remain independent of desktop ownership.
Connection loss stops affected execution and keeps its UI drafts; queued
close continuations retain the existing loss protections. A close queued
during opening cancels further opening work at network-operation
boundaries, drops the unregistered provider and does not report success.

## Explicit limits and acceptance

A provider must supply at least one accessible layout; files without one
remain refused. A file opened from this computer opens local peers through
hosts its window's files share, by plain names in its folder or by a stored
path variable (local-files.md 4, "Related files and other files"); no chooser,
external-authentication or cross-server opening is added. A file that
refused the session's account and password is not sent them again, until
the first file is opened again (#738). An Open File step of such a file
asks for another account, as FileMaker Pro's does ([#60](https://github.com/OpenMaker-Dev/openmaker/issues/60); local-files.md 4,
*Accounts*): 1 on Cancel, the refusal's code (212) on Cancel after a
refused answer, 0 when the file's own account opens it; with no one to
answer, it fails at once with the refusal. Go to Related Record to an
occurrence whose file is not open yet opens it first (on demand, #983);
when that file refused the account it was sent, or was sent none (another
host), the step asks for an account with the same prompt, which signs the
relationship in, and then goes on in the schema that has the file: Cancel
gives 1, Cancel after a refused answer 212, and with no one to answer the
step fails with 106 as before. Into that file's own layouts, the account
typed opens the file's window too, holding the relationship's open of it
on the connection (#1011): one prompt and one Login. Nothing keeps the
account after the step [I: when FileMaker Pro asks there, and its codes
after Cancel, are unmeasured]. Other steps that open such a file on the
same host fail with the refusal without asking; one on another host asks
(above).
Open File without a specified source is unsupported. Known provider error codes are
preserved; otherwise a failed peer open/catalog read reports 802 with its
provider detail. Unsupported source families halt through the existing
Unsupported mechanism rather than pretending to open a file.

A file participating in an external script stack, or a peer holding an
open script transaction, cannot be closed by a script: error 3 is returned
before saves or disconnection, so suspended callers and their transaction
state are never discarded. Native unwinding of such stacks remains
unmeasured. Only one script transaction can be active across a call stack;
a nested peer Open Transaction, or Commit/Revert while another file is selected, is refused
with 3. The original transaction stays owned by its file, including when
a peer halts the script. The peer's TransactionOpenState is independent.

Tests use synthetic two-file catalogs and verified TLS loopback servers.
They establish ownership, continuation, startup, alternatives, errors,
hidden/repeated opens, account/global separation, dirty records,
transactions, simultaneous windows, close survival and cancellation/loss.
They do not establish native FileMaker acceptance for every event order,
focus transition, credential flow, filename normalization or cross-file
transaction/close case. Those comparisons remain acceptance work on
independent disposable fixtures. Dedicated cybersecurity auditing remains
deferred. `Get ( UserName )` is Settings' User Name
([desktop-client.md](desktop-client.md#settings)).

## Extension points

`OpenFiles` owns provider/state pairs and maps windows to files.
`DataProvider::database_sources` exposes decoded generic source metadata;
`open_peer` creates an independent provider using inherited connection
configuration. The runtime passes the calling file explicitly through
`resolve_file`, `file_named_from`, `open_file_from` and `close_file_from`.
A qualified script name requires a declared source alias; a matching hosted
filename alone is not a data-source declaration. Script ids and source aliases therefore remain local even after explicit
selection of another file's window. Catalog lookup and `window_for_file`
use the registry; `use_window` restores a complete owning context.

Layouts of another file's tables are written by that file
(record-writes.md, "Related records of another file"; scripting.md), and
external layout and object triggers run in the owning file's session
(script-runtime.md, desktop notes); what those paths still refuse is listed
there. Go to Related Record into another file's
layout ("Use external table's layouts") opens or reuses that file through
this registry and the data source's paths (script-runtime.md, Go to Related
Record). The file it reaches must be the one the related records came from,
by host, port and name (`FileIdentity`; by name when the import or the file
does not say), else the step returns 3 before it commits, opens a window or
changes a found set. On that file's current layout with only related
records, no new window and no Match current found set, in a window the file
already shows, the step leaves the caller's script and window where they
are. That window shows the related records on its layout, as FileMaker Pro
26.0.3 does (script-runtime.md); other forms still go to the destination.
Local startup triggers in newly opened files are supported. Select Window
searches owned visible peers when Current file only is off: every file's
names first, current-file matches first, then every file's
`Get ( WindowUUID )`s. From another file it does not find a file
opened hidden. Run by a script of the file whose window it selects (Current
Window, or a name in that file), it shows that window, hidden or not, so a
script performed in a file opened hidden can bring it forward for its
caller (#1313); while a dialog window is open, showing a hidden one fails
with 3, OpenMaker's reading of Claris's rule that opening a document window
then fails (New Window). The two Select Window rules are Claris's
documented ones (script-runtime.md 9.2), not measured, as is matching a
name without case ([#320](https://github.com/OpenMaker-Dev/openmaker/issues/320)). Named Close
Window, Set Window Title and Move/Resize Window remain scoped to the
selected file. Their cross-file options require a separate extension using
the same registry, including independent native collision-precedence and
close semantics. No second registry or catalog implementation is needed for
that work.

Imported-calculation script-variable contexts retain their existing behavior;
this feature does not add propagation of desktop `$$` variables into those
related-only formula contexts. The runtime file ids and imported-file
metadata provide the ownership hooks for that separate extension.
