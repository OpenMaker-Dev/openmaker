# Backups: what FileMaker Server writes

FileMaker Server backs up its hosted files on backup schedules and on
demand. It keeps a number of backups per schedule and lets the administrator
preserve some of them. The Admin Console's Backups tab shows them
([admin-console.md](admin-console.md) 4.3, with the schedule form in 4.5).
This document records what a backup is on disk, how a run proceeds and what it
logs. OpenMaker Server's console is described in
[../admin-console.md](../admin-console.md).

## Sources and provenance

- **The lab server's backup folder, read-only, 2026-11-11.** This is
  FileMaker Server 26.0.3 on Linux, with its default "FMS" schedule (daily at
  midnight, keep 7) and seven backups present. They were examined over SSH
  with `ls -la`, `find -maxdepth 5`, `stat` (sizes, modes, link counts,
  inodes and times) and `od`/`cat` of the seven `backup.config` files.
  Nothing was changed, no database was copied, and no `fmsadmin` or Admin API
  call was made.
- **The lab server's `Event.log`.** Only the lines of the scheduled runs of
  2026-10-29, 2026-10-30, 2026-11-10 and 2026-11-11 were read.
- **The console's string table** (`en.json`, admin-console.md sources) and the
  **Admin API Reference** shipped with the server (the `/schedules/backup`,
  `/server/backuppath`, `/server/cancelbackup` and
  `/server/config/parallelbackup` operations).

- **FileMaker Server's code, read statically.** FileMaker Server 26.0.3 for
  Ubuntu: `fmserverd` and `libFMSLib.so`, analysed in Ghidra with their
  DWARF, and the Mac `Support` x86_64 slice for the file helpers. The
  functions are named where they are used below; `nm` addresses:
  `EngineMgr::EngineBackup` 0x32ec60, `BackupData::Preflight` 0x31e3a0,
  `BackupData::CloseSet` 0x327cb0, `BackupData::VerifyAndOrCloneSet`
  0x32a600 (fmserverd); `FMS::RemoteContainerMgr::IsBackupRC` 0x6c8f00,
  `GetRCRootPath` 0x6c7460, `RCFileSizeOrHardLinkFunctor::operator()`
  0x6c9680, `RCFileCopyFunctor::operator()` 0x6c99c0,
  `FMS::BackupFolder::RemoveExcess` 0x4a6cd0 (libFMSLib);
  `File::PlatformMakeHardLinkIfPossible` 0x182f78,
  `File::PlatformModificationTimeCompare` 0x182236 (Mac Support). Event
  texts are from libFMSLib's message table.

The raw listings are in `re/work/backups/`, which is git-ignored because they
name real hosted files. Here a hosted file is `Sample` or `test project`, and
a subfolder of the database folder is `Sub`.

Confidence: **(obs)** observed on the lab server, **(doc)** stated by the
console's strings or the Admin API Reference, **(bin)** read in FileMaker
Server's code, **(inf)** inferred.

## 1. Where backups go

- The default backup folder is `Data/Backups/` under the install root. On
  Linux that is `filelinux:/opt/FileMaker/FileMaker Server/Data/Backups/`
  (obs). It is shown in FileMaker path syntax by Configuration > Folders
  ("Default Backup Folder Path", with Change) and by `GET /server/backuppath`
  (`{"backupPath": ...}`) (doc).
- Each backup schedule has its own target folder (`backupTarget`). The
  console's Create Schedule form pre-fills it with the default backup folder
  (doc, admin-console.md 4.5).
- The lab server's `Data/Backups/` folder is mode 755. It holds only the backup
  folders (obs).

## 2. One backup

### 2.1 The folder

A run writes one folder in the schedule's target folder:

```text
<schedule name>_YYYY-MM-DD_HHMM/
  backup.config
  Databases/
    test project.fmp12
    RC_Data_FMS/test project/...     (only when the file has container data)
    Sub/
      Sample.fmp12
      RC_Data_FMS/Sample/...
```

- **Name.** The name is the schedule's name, then the date and the hour and
  minute at which the run started, in the server's local time
  (`FMS_2026-11-11_0000` for the midnight run) (obs).
- **While it runs**, the folder is
  `<schedule name>_YYYY-MM-DD_HHMM_InProgress`. Event 774 names it (section
  4). The finished backup has the name without the suffix, so the folder is
  renamed when the run completes (obs: the log names `_InProgress` and the
  folder on disk does not have it). A backup that is cancelled or fails "may
  leave incomplete files in destination" (doc: the string
  `BACKUP_SCHEDULE_ABORTED`). Settings > Keep Cancelled Backups keeps such a
  backup (doc).
- **`Databases/`** mirrors the default database folder (`Data/Databases/`).
  Files directly in that folder are at its top, and files in its subfolders
  are in the same subfolders (obs: 118 hosted files in the top folder and
  five subfolders, 118 copies in the same places).
- **Container data** stored externally goes beside each file's copy, as it
  sits beside the hosted file: `RC_Data_FMS/<file name>/` in the folder of
  the copy, with the same subfolders and file names below it (bin:
  `EngineBackup` walks the hosted file's `RC_Data_FMS/<file name>/`
  (`RemoteContainerMgr::GetRCRootPath`) and the copy functors re-base each
  file's path, taken relative to the folder that holds `RC_Data_FMS`, on the
  folder of the file's copy). The lab's container folders hold no files, and
  its backups have no `RC_Data_FMS` folder at all (obs), so a folder is
  created only when there is a file to put in it (inf).
- **Container data of other folders.** A file in an additional database
  folder that has its own remote container folder gets its container data
  backed up only when that folder's back-up option is on; files in the
  default database folder always do (bin: `RemoteContainerMgr::IsBackupRC`
  looks the file's database root up in the server's map of remote container
  folders and returns that entry's back-up flag, true without an entry; the
  event texts "Separate remote container folder [%1] enabled: %2" and "...
  enabled, without backups: %2"; the Admin API keys
  `backupDatabaseRoot1_RC` and `backupDatabaseRoot3_RC`). Where such a
  file's container data sits in the backup was not traced.
- **No other entries** (obs).
- **Ownership and modes.** The server's user and group own everything
  (`fmserver:fmsadmin` on Linux). Folders are 775 and files 664. The hosted
  files are 664 too (obs).

### 2.2 `backup.config`

This is a small text file of `Key=Value` lines, each ended by a line feed.
Every backup had the same four keys in this order (obs, 7 of 7 files, 102
bytes each):

```text
Label=Auto-backup
TimeStamp=2026-11-11 06:00:00
IsPreserved=false
ID=7F463F4CF60E4591BA1237B8B95A49B3
```

| Key | Value |
|---|---|
| `Label` | the label the Backups tab shows: `Auto-backup` for a scheduled run, `On-demand` for Back Up Now (doc); the administrator can edit it ("Edit Label", one label for all the selected backups) (doc) |
| `TimeStamp` | when the run started, `YYYY-MM-DD HH:MM:SS` in **UTC**. The `0000` backups of a server six hours behind UTC say `06:00:00` (obs) |
| `IsPreserved` | `false` for the seven scheduled backups (obs); `true` for a preserved one (inf: no preserved backup existed to look at) |
| `ID` | 32 uppercase hex digits: a random version-4 UUID without dashes. The version nibble is `4` and the variant bits are `10` in all seven (obs) |

The backup's time and its preserved state are in this file, so it is the
only observed record of a backup besides its folder name. Whether the server also keeps
a list elsewhere is not known (section 7).

### 2.3 Unchanged files are hard links

A file that has not changed since the schedule's previous backup is not
copied again. The new backup's entry for it is a **hard link** to the copy in
the previous backup (obs):

- In each of the seven backups, all 118 database copies have link count 7.
  The seven entries of one file share one inode.
- The run logs event 758 for each such file: `Backup of "<file>" skipped
  because no changes were made since last backup; created hard link to last
  backup file "<path of the previous backup's copy>".` (obs: 118 lines on
  2026-11-10 and 2026-11-11, 116 on 2026-10-30, 5 on 2026-10-29).
- So the backups share their unchanged files' disk space. `du` counts about
  54 GB for each backup and about 54 GB for all seven together (obs).
- A copy's modification time is when the backup that first copied that
  version finished it, not the hosted file's. For example, a file last
  changed on the hosted side on 2026-10-28 has copies dated 2026-10-29
  00:01:18, in the 2026-10-29 backup and the six later ones linked to it
  (obs).
- **Unchanged** means, against the newest earlier backup in the schedule's
  backup folder that holds a copy of the file (the backups sorted by the time
  in their names, the one being written skipped): the hosted file has the
  copy's size, the engine has no unwritten changes for it (`HBFile::mIsDirty`
  clear), and its modification time is not later than the copy's (bin:
  `BackupData::Preflight`, `File::PlatformModificationTimeCompare`). The
  copy's time is set when that backup finished it, after the pause that
  touches the hosted file (section 3), so a file nobody wrote to compares
  as unchanged.

**Container files are linked one by one**, whether the database file changed
or not (bin: `BackupData::Preflight` with
`RCFileSizeOrHardLinkFunctor`):

- For each file under the hosted file's `RC_Data_FMS/<file name>/`, the
  same path in the earlier backup's container folder is linked into the new
  backup when it is a file (not a folder) of the same size whose
  modification time is not earlier than the hosted file's
  (`File::MakeHardLinkIfPossible`). Otherwise its size is added to the space
  the run needs.
- After the database files are copied, every container file is copied
  unless its destination already exists and is not older
  (`RCFileCopyFunctor` with `File::CopyIfNewer`), which skips the linked
  ones. So a new or changed container file is copied, an unchanged one is a
  link, and a file removed from the hosted folder is not in the new backup.
- Nothing is logged for a linked container file. If linking a file's
  container data fails, its database link is removed again and the file is
  copied. Failures log `Backup of "%1" aborted; error (%2) creating remote
  container folder "%3".` or `Backup of "%1" incomplete; error (%2) copying
  remote container file "%3".` (bin: message table).

Retention deletes only the old folder's names. A hard-linked copy lives on
as long as a later backup links it (inf from link counts: deleting the
oldest backup lowers the count).

## 3. How a run proceeds

The 2026-10-29 run copied 113 changed files and linked 5. The 2026-10-30
run copied 2 and linked 116. The 2026-11-11 run copied none (obs, event log):

1. The run decides which files and container files are unchanged and links
   them (section 2.3; bin: `BackupData::Preflight`). Then it copies each
   changed file while the file stays open and in use, and after each file
   its container files that are not linked (bin: `EngineMgr::EngineBackup`).
   This took 78 s for the 113 files on 2026-10-29 (event 774 at 00:00:00.255,
   event 769 at 00:01:18.057). Container files are copied before the pause,
   so one written between its copy and the pause keeps its earlier version in
   the backup (inf: the pause step has no container work).
2. Then it pauses the files (event 769, "attempting to pause up to 118
   database(s)"). Every copy is reported finished within 5 ms (events 460 at
   00:01:18.060-062), and every copy's modification time is 45 to 65 ms
   after event 769. So the files are paused only at the end, to bring each
   copy up to date with what changed while it was copied (inf from this
   timing).
3. On 2026-10-30, the two changed hosted files have modification times at
   the moment of event 769, 20 to 35 ms before their copies' (obs). The pause
   writes to the hosted file (inf: it flushes the file's cache before the
   copy is brought up to date).
4. The `_InProgress` folder is renamed to its final name (section 2.1).
5. Retention runs after the backup is complete (section 5), and the schedule
   then reports completion.

Clients see nothing of a backup but the pause, which a file's requests wait
on (admin-console.md 9: a paused file holds its clients' requests). How long
the pause lasts for a busy file was not measured.

## 4. What the event log says

A scheduled run logs these lines in this order, all at level Information
(obs). Each line is `<time>\tInformation\t<id>\t<server name>\t<text>`.

| Id | Text |
|---|---|
| 148 | `Schedule "<schedule>" running.` |
| 774 | `Starting to back up <n> database(s) to folder "<target>/<schedule>_YYYY-MM-DD_HHMM_InProgress/".` |
| 458 | `Starting to back up database "<file>", current status: Normal` (each changed file, before copying it) |
| 758 | `Backup of "<file>" skipped because no changes were made since last backup; created hard link to last backup file "<previous copy>".` (each unchanged file) |
| 769 | `Backup attempting to pause up to <n> database(s)...` |
| 460 | `Backup of "<file>" completed, current status: Normal` (each changed file) |
| 682 | `Deleted backup folder "<target>/<old backup>/".` (each folder retention deleted) |
| 150 | `Schedule "<schedule>" completed.` |
| 126 | `Schedule "<schedule>" scheduled for 11/12/2026 12:00 AM.` (the next run) |

- `<file>` is the file's name without its extension.
- Paths are in FileMaker path syntax, and folders end with `/`.
- `<n>` counts every file the schedule chooses, changed or not (118 on each
  day).

## 5. Retention and preserved backups

- **Keep count.** A schedule keeps its last *n* backups (`maxBackups`, "Number
  of Backups to Keep"). After a run, the oldest backups past *n* are deleted.
  The count includes the backup just made: with 7, the 2026-11-11 run deleted
  `FMS_2026-11-04_0000` and left 2026-11-05 to 2026-11-11 (obs). A deleted
  backup's whole folder goes, `RC_Data_FMS` included; container files that
  later backups link live on there, as database copies do (bin:
  `FMS::BackupFolder::RemoveExcess` deletes the oldest folders past the
  count and has no step for container data).
- **Preserved backups** are never deleted automatically (doc, the Backups
  tab's text: "Preserved backups are prevented from automatic deletion"). The
  administrator preserves backups from the Auto-Backup and On-Demand view and
  deletes preserved ones from the Preserved view, after "Are you sure you want
  to delete the selected backups?" (doc). Whether preserved backups count
  toward the keep count is not known (inf: they do not, as they are exempt
  from deletion).
- **On-demand backups** of the default "FMS" schedule are preserved
  automatically; those of other schedules are not (doc, the Preserved
  column's help).
- **Deleting a schedule** does not delete its backups (doc).

## 6. Schedules and settings

- **The default schedule.** A fresh install has one backup schedule, "FMS":
  all databases, daily at midnight, keep 7, in the default backup folder. The
  Backups tab shows it ("Repeat: Daily at Midnight", "Keep: 7"), but
  Configuration > Schedules does not list it (obs, admin-console.md 4.3 and
  4.5).
- **A backup schedule's fields** (`POST /schedules/backup`, doc):
  - `name`: 1 to 100 characters, not only whitespace. The console requires "a
    valid folder name, with no characters or words reserved by the operating
    system", because the name begins each backup's folder name.
  - `backupType.resourceType`: `ALL_DB`, `DB_IN_FOLDER` or `SINGLE_DB`, with
    `resource` holding a database folder path (the console adds an optional
    subfolder) or a database name.
  - `backupType.backupTarget`: the target folder.
  - `backupType.maxBackups`: 0 to 99, 1 when omitted. The console says "Enter
    a value from 0 to 99". What 0 does is not documented.
  - `backupType.verify`: a consistency check of each copy. A failure is
    reported as "Consistency check failed on backup of database <name>".
    It checks the database copies only, not the container files (bin:
    `BackupData::VerifyAndOrCloneSet` checks each copy with `DBDamagedFile`
    and has no container step).
  - `backupType.clone` and `cloneOnly`: also, or only, write a clone of each
    file without its records. A clone gets no container data (bin: the
    container copy targets the backup copy's folder only).
  - `enabled` (false when omitted) and the timing types shared by every
    schedule.
- **Settings** (doc):
  - Parallel Backup (`/server/config/parallelbackup`) lets backups of
    unrelated file groups, and several backup schedules, run at once. It is
    off by default.
  - Progressive Backup keeps a separate folder brought up to date at an
    interval (`/server/progressivebackuppath`). It keeps container data by
    hard links too (bin, partly traced): when a change log is flushed, the
    container files it refers to are linked into the log's container folder
    (`EngineDatabase::FlushRedo` with `RCFileHardLinkFunctor`), and its error
    events name container folders it moves and deletes ("Remote container
    directory could not be moved from %2 to %3", "Obsolete remote container
    directory could not be deleted"). With a standby server, closing a backup
    also links each full copy and its container files into a `Copies_FMS`
    folder under the standby's progressive folder (`BackupData::CloseSet`).
    The progressive folder's layout was not traced.
  - Cancel (`POST /server/cancelbackup`, `{"cancel": true}`) stops the
    running backup.
- **No API lists backups.** The Admin API has no path that returns the
  backups or preserves one. The Backups tab uses the console's private API
  (doc: none of the 112 paths does).

## 7. Unknowns

- Container data in a backup (2.1, 2.3) is from bin. To see it: with the
  maintainer's approval of a probe file (lab rules), host a probe copy of
  the lab test file with one container field stored externally
  (containers.md, "External storage: still open"), insert a file, run a
  backup schedule of only that file twice with a change in between, and
  list the two backups read-only (`find`, `stat` link counts), then delete
  the probe file, its schedule, its backups and its container folder.
- Where files in the secure folder and the additional database folders go
  (the server's strings name an additional database root folder for
  backups), and where the container data of a file whose remote container
  folder is separate goes.
- Whether a changed file's copy starts from the previous backup's copy.
- The progressive backup folder's layout, with and without container data.
- FileMaker Server 26.1.1 (announced in Claris's 26.1 launch webinar, not
  released on 2026-11-19): automatic switchover to a standby server, behind a virtual IP
  address, with progressive backups sent to the standby over SSH. FileMaker
  Pro 26.1.1's shared engine already carries pieces of it [O: strings and
  symbols]: Windows Failover Clustering messages (cluster access point
  address, virtual IP address, file share witness, a "FileMaker Server
  failover role", the Windows account for cluster setup), a standby
  keystore (`KeyStorageRequestor::RekeyKeyStore`,
  `ApplyRekeyedKeyStore`, the HTTP headers `X-FMS-Standby-Keystore` and
  `X-FMS-Upload-Request-Id`), a license key field `Automatic_Failover`, and
  libssh2 behind libcurl. How the standby is set up and fed on Linux, and
  what changes in the backup folders, is unknown until it ships.
- What `maxBackups` 0 means.
- Whether preserved backups count toward the keep count, and what
  `IsPreserved=true` looks like.
- What happens when a run's folder name already exists: two runs of one
  schedule in the same minute, such as Back Up Now right after a scheduled
  run.
- Whether Run Now on Configuration > Schedules labels its backup
  `On-demand` like Back Up Now.
- What a cancelled backup leaves with Keep Cancelled Backups on (its name and
  `backup.config`).
- How long the pause lasts for a file that clients are writing to.
