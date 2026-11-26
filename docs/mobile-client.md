# OpenMaker Go

OpenMaker Go opens FileMaker apps on iPhone, iPad and Android tablets and
phones. Its Launch Center, sign-in, toolbars, menus and keyboard are the
platform's own (SwiftUI on Apple devices, Jetpack Compose on Android). The
file's layouts are drawn and operated by the desktop client's window
(`om_gui::OpenMakerApp`), so a layout looks and behaves as it does on the
desktop. What FileMaker Go does is in [go-client.md](re/go-client.md).

## Opening files

- **My Apps** lists the `.fmp12` files in the app's documents. Import
  copies one in: the copy is made under a temporary name and linked in
  under its own, so the list never shows half a file and a file already
  there of that name is kept. On iOS the documents are also in the Files
  app. A `.fmp12` file opened with OpenMaker Go from Files, Mail or
  AirDrop is imported so, into My Apps, where a tap opens it; one that is
  already in My Apps opens at once, unless another file is open (it declares
  FileMaker Pro's type identifier, `com.filemaker.doc.fmp12`, read from
  FileMaker Pro 26.1.1 for macOS; FileMaker Go's own declaration is not
  documented). Local edits go to that copy.
- **Hosts** lists the saved hosts, by name or by address (with the port
  when it is not 5003), as FileMaker Go's Hosts tab does. Add Host (+)
  takes a host name or IP address, a port (5003; go-client.md records
  only the name or address) and an optional name, checked as the
  desktop's Add Host checks them (`om_gui::host_address`). A host opens to the files it
  lists, with a search box, and pulled down (or Refresh on Android) lists
  them again; Open by Name opens a file it does not list (`.fmp12` may be
  left out). Swiping a host (iOS), or its menu or a long press, edits or
  removes it; removing forgets only the saved entry and its certificate
  authority. Before the Hosts tab had a list it kept one host and the
  certificate authorities it was given: that host and those hosts make
  up the list. Local hosts (Bonjour) are not listed. A private
  certificate authority (PEM) can be saved for a host and port; it is
  used for that host only, its address matched in any case, as hosts
  are (those the old Hosts tab saved under several spellings of one
  address are all used, and Edit Host shows them all). A save never
  removes one the editor did not show: Add Host, or editing a host onto
  another saved host, adds the ones already saved there. The certificate
  is verified as the desktop client verifies it.
- **Favorites** and **Recent** keep references to local and hosted files;
  opening one signs in again. Rows can be removed, and a local file
  deleted after a confirmation.
- **Sign-in** takes the account, password and, for a host that requires
  one, a FileMaker client license. The account and password last as long
  as the open file and are not stored. A refused account (FileMaker error
  212 or 213), or a host that wants a license it was not sent or refuses
  the one sent (0x507c), keeps the sign-in open to try again. A local
  file opens without one, signed in by the file's own settings as
  FileMaker Go opens it, and asks only when the file needs an account; a
  hosted file always asks.
- **Settings** (the Launch Center's gear) keeps one FileMaker client
  license, as the desktop's FileMaker License dialog does: a hosted
  sign-in whose license is left empty sends it. Android stores it
  encrypted under an AES key kept in the Android Keystore; iOS keeps it
  in the Keychain for this device only, readable after the first unlock.
  A copy carried to another device is not readable there and has to be
  entered again. The setting is OpenMaker's own: how FileMaker Go deals
  with a host's client licensing is not recorded in go-client.md.

## Using a file

The chrome's menus are FileMaker's own, recorded from the window's menus
as the macOS menu bar records them (`menu_record.rs`), so their rows read,
enable and run exactly as the desktop's:

- the **layout menu**, with the file's folders and separators;
- **Records** in Browse mode and **Requests** in Find mode (new, duplicate
  and delete, Go to Record, Show All, Show Omitted Only, Omit, Modify Last
  Find, Sort, Revert and the rest);
- **View**: Browse and Find mode, View as Form, List and Table;
- **Scripts**: the scripts the file puts in the Scripts menu.

The chrome's own controls (`om_gui::ChromeCommand`) are the record arrows
and count (tap it to go to a record by number), Find (New Find and Quick
Find), Find mode's Search, Omit and Cancel, Done and Files. Each is
enabled by the same rule as the desktop command it runs. Two fingers swiped
left or right go to the next or previous record (FileMaker Go's gesture).

A tap on the layout is a click; a pan scrolls what is under the finger
and glides on after it lifts, unless it starts on a handle (a Table view
column's divider, anything that shows a resize pointer there), which it
drags. Typing goes through the platform keyboard and its input methods
(marked text included) to the field. Select All, Copy and Cut, from a
hardware keyboard (Command, or Ctrl on Android), the edit menu or the
keyboard's own commands, and Undo and Redo from a hardware keyboard, act
on the field's text, and a paste is the field's paste, not typing. A
hardware keyboard's Shift reaches keystroke triggers with the key.
TalkBack's and VoiceOver's scroll gestures move what is under the focused
element (a portal's rows, the layout) by most of its own size, up and
down or sideways. The editing
bar above the keyboard has the previous and next field and Done, which
saves the record as a tap outside every field does. In Find mode the
keyboard's Search key performs the find. Hardware keyboards' Tab, arrow,
Return, Space, Escape, Home, End and Page keys reach the layout (Space
also selects a checkbox or radio button item), the numeric
keypad's Enter as FileMaker's Enter (not Return), and Command-Return
(Ctrl-Return on Android) as itself.

## How a frame is made

Each open file is a `Document` (mobile/core) whose window runs on a thread
of its own. The app sends it input; the thread runs egui's pass for the
window only when input arrives or the window asks to be drawn again
(egui's repaint requests, including those of the data worker's threads),
taking all the input that arrived meanwhile in one pass. A command or menu
choice gets a pass of its own, after the field has read everything typed
before it, and an input method's composition is committed as it shows
before any command runs: a command may leave the field, and what the field
had not read would be lost. It hands the app
egui's tessellated triangles, texture changes and clip rectangles at the
device's pixel density, and the app's Metal (iOS) or OpenGL ES 3 (Android)
painter draws them as egui's own painters do (premultiplied colors blended
without converting to linear light). Texture changes not yet drawn are
merged into the next frame, never dropped. The frame also carries the
chrome's state, menus, accessibility elements, copied text and URLs to
open. [mobile-frames.md](performance/mobile-frames.md) has the timings.

Two things differ from the desktop's event loop, both OpenMaker Go's own
choices:

- A command or menu row the chrome runs is followed by another pass, as a
  click's input is on the desktop: what it changes over the next frames
  (Find mode's editors) is drawn without waiting for the next touch.
- While the window waits on a script, a save or a page, it is drawn every
  50 ms. On the desktop, pointer moves draw the window that often while the
  user works, so a step that takes effect on the frame after a reply is
  never left waiting; here it would wait for the next touch.

## Keeping what was typed

- **Leaving the foreground** saves the record's changes, as Done does, as
  soon as the window lets it (also once an edit's record lock arrives).
  FileMaker Go keeps an open file's state when the app leaves the
  foreground; what it does with a record's changes is not documented, so
  saving them is OpenMaker's choice. On iOS the app keeps running in the
  background until the save is over (`Document::is_saving`), for
  up to 25 seconds.
- **Closing** a file from the app (Files, or the app letting go of the
  document, as Android does when its activity ends) runs the input the
  app had sent, saves the record's changes, and closes the file once they
  are saved. Close waits for an edit that is still waiting for its record:
  File > Close would cancel that edit. A dialog the save raises (a
  validation failure) keeps the file open for the user to answer. A close
  an OnWindowClose trigger refuses, or that a lost connection stops, leaves
  the file open; Files again runs the trigger again, as each request to
  close a window does in FileMaker, but the app letting go of the document
  after a refused close does not ask again (a save asked for since still
  runs). Files closes the file it was tapped for: when a script has
  brought another file forward since the app last drew, the tap does
  nothing. Letting go also closes each file a script opened that is left
  open, shown or hidden, with its triggers.
- **Windows behind the front one.** A record a script left with changes in
  another window (it set a field there, then selected this one) is saved
  too, in its own window, when the app leaves the foreground or closes.
  Each window's save goes out without waiting for the host to answer the
  ones before it, so that a save the host refuses (a validation failure,
  whose alert waits for the user) keeps no other window's from going.
- **A save that keeps nothing.** Leaving the foreground or closing asks
  for each record's save once. When it ends with the changes still there
  (an OnRecordCommit trigger kept the record, or the save failed and said
  so), the file stays open with them; the trigger is not run again until
  the user or the app asks again.
- **Changes a window held when its connection was lost** are saved only
  after a reconnect (below), and those a reconnect finds changed on the
  host wait for Keep or Discard My Changes; text the host refused the
  record to (it changed or was locked before the edit was accepted) waits
  for Discard Input. Until then Files is unavailable and the app's own
  close leaves the file open, while other windows' changes are still
  saved; the lost
  banner's Close File discards them when the user chooses it there, as on
  the desktop. Changes a reconnect found unchanged on the host save as
  usual, also when the app leaves the foreground.
- **Password fields** (a dialog's) get the keyboard's secure entry.
- On Android the document belongs to a view model, so rotating the device
  or attaching a keyboard keeps the file open.

## A lost connection

Android closes a backgrounded app's network connections: on Android 17 a
Galaxy Tab's firewall blocked the app about 5 seconds after it left the
foreground, and its connection to the host was gone when it came back. A
network change drops connections on any device. When a hosted file's
connection is lost while the app is in the foreground, or it comes back
to find it lost, the file signs in again by itself, once per loss, with
the account, password and license it was opened with: the lost banner's
Reconnect, with its password already typed. The password goes only to
the host, port and account it was typed for: a file a script opened on
another host or with another account gets Reconnect's dialog to type its
own. Unsaved changes stay in
their windows and are checked against the host's records as Reconnect
checks them; a refusal stays in Reconnect's dialog for the user. On the
tablet, a hosted production-like file's window kept its record and could
be used at once, and its related records were back about 18 seconds later (Reconnect
reads the file's catalogs again first).

FileMaker Go is suspended in the background and resumes where it was
(go-client.md); what it does when the host has dropped it is not
documented, so reconnecting by itself is OpenMaker Go's own choice.

## Limits

- **Not compared with FileMaker Go.** Layout scale, fonts and the chrome's
  look were not measured against a running FileMaker Go ([#667](https://github.com/OpenMaker-Dev/openmaker/issues/667)).
- **SVG pictures and button icons** show egui's error mark. Browse draws
  SVG only in a capped worker process, since a hostile SVG can abort the
  process that draws it (`svg_loader.rs`), and the apps cannot start one
  ([#658](https://github.com/OpenMaker-Dev/openmaker/issues/658)).
- **Web viewers** show a note: the apps embed none ([#659](https://github.com/OpenMaker-Dev/openmaker/issues/659)).
- **Printing, Save/Send Records As, Export and Import** are not in the
  menus: they need the platform's share sheet and print dialog ([#660](https://github.com/OpenMaker-Dev/openmaker/issues/660)).
- **No window list.** FileMaker Go's window title adds, scrolls through
  and closes a file's windows; OpenMaker Go shows the front window only
  ([#664](https://github.com/OpenMaker-Dev/openmaker/issues/664)).
- **The editing bar** has no Insert menu (date, time, user name, barcode)
  and no Revert Record, which FileMaker Go's has ([#665](https://github.com/OpenMaker-Dev/openmaker/issues/665)).
- **Unverified certificates.** FileMaker Go offers to connect to a host
  whose certificate it cannot verify ("Always Permit Connection"); OpenMaker
  Go refuses unless the host's certificate authority is saved for it
  ([#661](https://github.com/OpenMaker-Dev/openmaker/issues/661)).
- **Keyboards.** The field's touch keyboard type and FileMaker Go's
  Auto-Correction setting are not applied: the keyboard is plain text with
  autocorrection off ([#662](https://github.com/OpenMaker-Dev/openmaker/issues/662)). Selecting text in a field by touch
  (with handles) is not offered: a field's text is selected with a
  hardware keyboard or Select All.
- **Screen readers** get the visible fields, labels and buttons, but no
  checked state for checkboxes and radio buttons ([#668](https://github.com/OpenMaker-Dev/openmaker/issues/668)), and
  rectangles that miss where a card or Form zoom moved the layout
  ([#669](https://github.com/OpenMaker-Dev/openmaker/issues/669)).
- **The device's own features**: camera, barcode scanning, signature,
  location and other Get functions answer as on the desktop ([#666](https://github.com/OpenMaker-Dev/openmaker/issues/666)).
- **Hosted files in the background** keep no connection on Android, and
  the host may disconnect an idle client: coming back reconnects (above),
  which takes longer than a connection kept would: related records came
  back about 18 seconds later ([#657](https://github.com/OpenMaker-Dev/openmaker/issues/657)).
- **Records a close trigger changed.** A close that stops at a window or
  card an OnWindowClose trigger opened is taken for a refused close: when
  the app lets go of the file, a record the trigger changed there is not
  saved ([#682](https://github.com/OpenMaker-Dev/openmaker/issues/682)); nor is one it changed while the app left the
  foreground during the close ([#683](https://github.com/OpenMaker-Dev/openmaker/issues/683)).
- **An app ended while the save is held** (a validation failure shown
  when the app left the foreground, a trigger that kept the record, or a
  lost connection, then iOS or Android ending it) loses the record's
  changes: nothing keeps a draft outside the window ([#656](https://github.com/OpenMaker-Dev/openmaker/issues/656)).
- **Saves and closes are inferred.** The engine asks the window to save
  each record and close the file, and reads `om-gui`'s state to learn how
  each ended; a command in `om-gui` that reports each outcome would
  replace that ([#655](https://github.com/OpenMaker-Dev/openmaker/issues/655)).
- **Cards** take the chrome's commands and menus, as a card takes the menu
  bar's on the desktop; their look on a phone was not checked.
- The apps are built for ARM64 only. iPhone was tried on the iOS
  Simulator, Android on a Galaxy Tab S10 FE (Android 17); iPad, real
  Apple devices, Android phones and a rotated Android screen were not.

Building and testing: [mobile/README.md](../mobile/README.md).
