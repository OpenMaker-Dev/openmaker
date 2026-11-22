//! Reconnecting a file whose connection to the host was lost
//! (docs/recovery.md, "Reconnecting"). The lost banner's Reconnect
//! asks for the password of the account the file was opened with; the
//! worker opens the file again where it was opened, in a fresh session
//! (`DataProvider::reopen`), and reads back the records the file's windows
//! hold unsaved edits of. Each window then finds the record it showed and
//! compares each record read back with what its edits started from: edits
//! the host's copy still matches can be saved as usual; edits of a record
//! the host changed, deleted or could not be read back are held, and
//! nothing saves them until the user keeps or discards them (an unreadable
//! record is read again first). The version a record was read back at is
//! the one its save is checked against (`DataProvider::record_by_id`): a
//! save refused because the host changed the record since holds the edits
//! again and reads the record again. A record write the loss left
//! unanswered is never sent again: the record read back tells what became
//! of it where it can. Once the file's fresh layout list is in, every
//! window of the file takes it and reloads its layout and records.
//!
//! Only the reconnected file's windows change; the session's other files
//! keep their state, lost or not. A failed reconnect leaves the file as it
//! was: lost, with every unsaved edit on screen.
//!
//! An ordinary save goes the same way. A record the window begins unsaved
//! edits of is retained (`DataProvider::retain_record`): its save answers
//! to the version the window showed then, with the values it showed kept
//! beside it, whatever any window reads of the record meanwhile. A save
//! the host refuses because the record changed (FileMaker's 306) or is
//! gone (101) holds the edits and reads the record by id, to be compared
//! as after a reconnect; one refused because another user is editing the
//! record (300, 301) leaves the edits as they are, answering to the same
//! version, for the user to save again or revert. Nothing is sent again by
//! itself. Edits stay their window's: settling or giving up one window's
//! edits releases only that window's version of the record.

use std::collections::{BTreeMap, BTreeSet};

use egui::RichText;
use om_data::{DataError, RecordPage};
use zeroize::Zeroizing;

use super::{Action, OpenMakerApp};
use crate::document::FieldDraft;
use crate::session::Session;
use crate::worker::{ReconnectJob, Reconnected, Request};
use om_data::{LayoutInfo, LayoutMenuEntry};

/// A record's values by field name, an empty value as empty text (as
/// edits hold it).
pub(crate) type Values = BTreeMap<String, String>;

/// What a refused action while edits are held tells the user.
/// Why a window whose save may be partly applied refuses to leave its
/// record (#1006).
pub(super) const UNSETTLED: &str = "This record was saved in some of its files but not all. \
     Check Again, Resume Save or Discard Unsaved Changes in the window's bar first.";

/// Why a window whose new record a reconnect could not make again refuses
/// to leave it (#152).
pub(super) const UNRECREATED: &str = "This window's new record could not be made again on the \
     new connection. Try Again or Discard New Record in the window's bar first.";

pub(super) const HELD: &str = "Keep or discard the unsaved changes the host's copy of the record \
                               no longer matches before going on.";

/// Why a reconnect is not offered while a record keeps changes of its
/// related records, or changes scripts left, uncommitted.
const SCRIPT_CHANGES: &str = crate::scripting::session::CHANGES_UNRECOVERABLE;

/// A record write sent to the host: the window waits for its answer
/// (`DocumentState::pending`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Write {
    Update {
        layout: u32,
        record: i64,
    },
    Delete {
        layout: u32,
        record: i64,
    },
    /// A save or revert of what scripts left, on the worker.
    Scripted,
}

/// Why a record's unsaved edits are held: nothing saves them, and the
/// file's windows run no script, until the user keeps or discards them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    /// The host changed the record since the edits started: Keep My
    /// Changes lets them be saved over its values.
    Changed,
    /// The host deleted the record: the edits cannot be saved.
    Deleted,
    /// The record could not be read back: Check Again reads it; nothing
    /// lets the edits be saved before that succeeds.
    Unreadable,
    /// The layout the edits were made on is no longer in the file.
    LayoutGone,
}

/// A record whose unsaved edits a reconnect checked.
#[derive(Debug)]
pub(crate) struct Checked {
    /// What the window says of it (empty once put away).
    pub note: String,
    /// The host's values last known for the record, by field name (those
    /// the edits were last checked against, or as last received when it
    /// could not be read): checking it again compares the host's values
    /// with these.
    pub basis: Option<Values>,
    pub hold: Option<Hold>,
    /// A mixed UI/script commit was refused for its version. Pending
    /// repetitions/related cells may not appear in a scalar page: reading
    /// equal visible values cannot approve overwriting them automatically.
    whole_record_conflict: bool,
}

/// What a reconnected window tells the user, and the records whose
/// unsaved edits the reconnect checked, by layout and id: each stays until
/// its edits are saved, reverted or discarded.
#[derive(Debug, Default)]
pub(crate) struct Recovery {
    pub notes: Vec<String>,
    pub records: BTreeMap<(u32, i64), Checked>,
    /// The file reconnected (rather than a save being refused).
    pub reconnected: bool,
}

impl Recovery {
    pub(crate) fn holds(&self) -> bool {
        self.records.values().any(|c| c.hold.is_some())
    }

    fn holds_for(&self, hold: Hold) -> bool {
        self.records.values().any(|c| c.hold == Some(hold))
    }

    /// Whether its bar has anything to say or ask.
    pub(crate) fn shows(&self) -> bool {
        !self.notes.is_empty()
            || self
                .records
                .values()
                .any(|c| c.hold.is_some() || !c.note.is_empty())
    }
}

/// What became of a record's edits once checked.
enum Settled {
    /// The window keeps checking them.
    Kept(Checked),
    /// The host holds every edit: they went, and the window says so.
    Applied(String),
}

/// What a save refused because the host changed the record tells the
/// user while it is read again.
const CHANGED: &str = "This record was changed on the host since your unsaved changes began, so \
                       they were not saved. Reading it again to compare…";

/// What a save refused because the host no longer has the record tells
/// the user while it is read again.
const MISSING: &str = "This record could not be found on the host, so your unsaved changes were \
                       not saved. Reading it again…";

/// The bar's heading while a refused save's edits are held.
const ON_HOLD: &str = "Your unsaved changes are on hold.";

/// How the notes name `count` unsaved changes: the subject, the object,
/// their verb and pronoun.
fn phrasing(count: usize) -> (&'static str, &'static str, &'static str, &'static str) {
    if count == 1 {
        ("The unsaved change", "the unsaved change", "was", "it")
    } else {
        ("The unsaved changes", "the unsaved changes", "were", "them")
    }
}

/// The dialog of a reconnect, for the file whose window asked.
pub(crate) struct ReconnectDialog {
    file: u64,
    /// `file (host)` and the account, for the dialog's text.
    target: String,
    account: String,
    password: Zeroizing<String>,
    pending: bool,
    error: Option<String>,
    /// What the file's windows held when the reconnect was sent.
    probes: Vec<Probe>,
}

/// One window of the file as the reconnect found it.
struct Probe {
    window: u32,
    /// The file the window shows.
    file: u64,
    /// Its current record, by layout and id.
    current: Option<(u32, i64)>,
    drafts: Vec<Drafted>,
    /// The record write the loss left unanswered.
    unanswered: Option<Write>,
    /// The window's unsaved edits of stored related records (#152).
    related: Vec<crate::document::RelatedEdit>,
    /// Its record keeps changes to related records uncommitted: edited,
    /// begun or deleted through it.
    related_pending: bool,
    /// Its record keeps changes a reconnect cannot keep
    /// (`unrecoverable_changes`).
    unrecoverable: bool,
    /// A new record of its record session, by layout and the lost
    /// connection's id: not on the host, so not read back; the reconnect
    /// holds it again under a new id (#152).
    new_record: Option<(u32, i64)>,
    table: Option<u64>,
}

/// A record the window holds unsaved edits of.
struct Drafted {
    layout: u32,
    record: i64,
    fields: Vec<FieldDraft>,
    /// The record's values as last received, by field name, when the
    /// window still has them.
    received: Option<Values>,
}

/// A record's values by field name, an empty value as empty text (as edits
/// hold it).
fn values_by_name(page: &RecordPage, index: usize) -> Values {
    page.fields
        .iter()
        .zip(&page.records[index].values)
        .map(|(field, value)| (field.name.clone(), value.clone().unwrap_or_default()))
        .collect()
}

/// What became of a record's unsaved edits, the host's copy read back.
#[derive(Debug, PartialEq, Eq)]
enum Check {
    /// Neither the edited fields nor the others changed on the host: the
    /// edits can be saved as usual.
    Unchanged,
    /// The host's copy holds every edit already (a save the loss left
    /// unanswered reached it): nothing is left to save.
    Applied,
    /// The host changed the record since the edits started.
    Changed,
    Deleted,
    Unreadable(String),
}

fn check(drafted: &Drafted, read: Option<&Result<RecordPage, DataError>>) -> Check {
    let page = match read {
        None => return Check::Unreadable("it was not read".into()),
        Some(Err(error)) => return Check::Unreadable(error.message.clone()),
        Some(Ok(page)) => page,
    };
    let Some(index) = page.records.iter().position(|r| r.id == drafted.record) else {
        return Check::Deleted;
    };
    let now = values_by_name(page, index);
    // Unchanged needs every edited field as it was, applied every edit on
    // the host; anything else (some of both included) is a change.
    let (mut original, mut applied) = (true, true);
    for field in &drafted.fields {
        match now.get(&field.name) {
            Some(value) if *value == field.value => original = false,
            Some(value) if *value == field.original => applied = false,
            _ => (original, applied) = (false, false),
        }
    }
    if applied {
        return Check::Applied;
    }
    if !original {
        return Check::Changed;
    }
    let edited: BTreeSet<&str> = drafted.fields.iter().map(|f| f.name.as_str()).collect();
    let elsewhere = drafted.received.as_ref().is_some_and(|received| {
        received
            .iter()
            .filter(|(name, _)| !edited.contains(name.as_str()))
            .any(|(name, before)| now.get(name) != Some(before))
    });
    if elsewhere {
        Check::Changed
    } else {
        Check::Unchanged
    }
}

/// The record `record` of `layout` as `session` last received it.
fn received(session: &Session, layout: u32, record: i64) -> Option<Values> {
    if session.layout != Some(layout) {
        return None;
    }
    session.loaded_pages().find_map(|page| {
        let index = page.data.records.iter().position(|r| r.id == record)?;
        Some(values_by_name(&page.data, index))
    })
}

/// The stored related records of `edits` made through the records `held`
/// (by layout and id): by base table and id.
fn related_rows(edits: &[crate::document::RelatedEdit], held: &[(u32, i64)]) -> Vec<(u64, i64)> {
    edits
        .iter()
        .filter(|edit| {
            u32::try_from(edit.field.layout)
                .is_ok_and(|layout| held.contains(&(layout, edit.field.parent)))
        })
        .filter_map(|edit| match edit.field.row {
            crate::document::RelatedRow::Record(record) => {
                Some((edit.field.table, i64::try_from(record).ok()?))
            }
            crate::document::RelatedRow::New { .. } => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A related record read back, `None` when the host no longer has it.
type RelatedValues = Result<Option<std::sync::Arc<om_data::host::RecordValues>>, DataError>;

/// The window's edits of related records, each read back on the new
/// session (#152): one the host changed since the edit began (its
/// modification count, or, without one, the edited field's text), no
/// longer has, or that could not be read holds the whole record it was
/// made through, as that record's own edits are held, with Keep My
/// Changes, Check Again or Discard My Changes. Unchanged ones stay, to
/// be saved as usual against the version read back. Pending edits stay
/// as they are.
fn settle_related<'r>(
    schema: Option<&om_data::host::CalcSchema>,
    edits: &[crate::document::RelatedEdit],
    read: impl Fn(u64, i64) -> Option<&'r RelatedValues>,
    recovery: &mut Recovery,
) {
    let today = om_data::host::SessionInfo::for_this_machine().evaluation_date();
    for edit in edits {
        let crate::document::RelatedRow::Record(record) = edit.field.row else {
            continue;
        };
        let Ok(record) = i64::try_from(record) else {
            continue;
        };
        let read = read(edit.field.table, record);
        let held = match read {
            None => Some((Hold::Unreadable, "it was not read".to_owned())),
            Some(Err(error)) => Some((Hold::Unreadable, error.message.clone())),
            Some(Ok(None)) => Some((Hold::Deleted, String::new())),
            Some(Ok(Some(values))) => {
                let changed = match (edit.version, values.mod_count) {
                    (Some(began), Some(now)) => began != now,
                    _ => {
                        let now = schema.map(|schema| {
                            let cx = om_data::calc::Ctx::new(&schema.locale, today);
                            values
                                .get(edit.field.field, 1)
                                .map(|value| value.to_text(&cx).into_string())
                                .unwrap_or_default()
                        });
                        now.as_deref() != Some(edit.original.as_str())
                    }
                };
                changed.then(|| (Hold::Changed, String::new()))
            }
        };
        let Some((hold, why)) = held else {
            continue;
        };
        let Ok(layout) = u32::try_from(edit.field.layout) else {
            continue;
        };
        let note = match hold {
            Hold::Changed => format!(
                "A related record (ID {record}) edited through this record was changed on \
                 the host since your changes to it began. They were not saved and stay in \
                 this window. Keep My Changes lets you save them over the host's values; \
                 Discard My Changes gives up this record's changes."
            ),
            Hold::Deleted => format!(
                "A related record (ID {record}) edited through this record was deleted on \
                 the host. Your changes to it cannot be saved; copy what you need, then \
                 discard this record's changes."
            ),
            _ => format!(
                "A related record (ID {record}) edited through this record could not be \
                 read back from the host to check your changes to it ({why}), so they \
                 cannot be saved yet. Check Again reads this record again; Discard My \
                 Changes gives up its changes."
            ),
        };
        // The strongest hold wins: deleted, then unreadable, then changed.
        let rank = |hold: Option<Hold>| match hold {
            Some(Hold::Deleted) => 3,
            Some(Hold::Unreadable) => 2,
            Some(Hold::Changed) => 1,
            _ => 0,
        };
        let checked = recovery
            .records
            .entry((layout, edit.field.parent))
            .or_insert_with(|| Checked {
                note: String::new(),
                basis: None,
                hold: None,
                whole_record_conflict: true,
            });
        // Its own fields' values cannot approve its related cells.
        checked.whole_record_conflict = true;
        if rank(Some(hold)) > rank(checked.hold) {
            checked.hold = Some(hold);
            checked.note = note;
        }
    }
}

impl OpenMakerApp {
    /// Whether the window's file has edits held after a reconnect.
    pub(super) fn recovery_holds(&self) -> bool {
        let file = self.w.file_id;
        std::iter::once(&self.w)
            .chain(&self.windows)
            .filter(|d| d.file_id == file)
            .any(|d| d.recovery.as_ref().is_some_and(Recovery::holds))
    }

    /// The lost banner's Reconnect: the dialog for the window's file.
    pub(super) fn open_reconnect(&mut self) {
        if self.w.connection_lost.is_none() || self.reconnect.is_some() || self.script.running {
            return;
        }
        let file = self.w.file_id;
        // A user's related edits are read back and checked (#152); what a
        // script left, or a related record deleted, cannot be.
        if std::iter::once(&self.w).chain(&self.windows).any(|d| {
            d.file_id == file
                && d.uncommitted.as_ref().is_some_and(|(_, _, open)| {
                    // A new record in its record session may be carried:
                    // the worker asks the provider (#152).
                    crate::scripting::session::unrecoverable_changes(open, true)
                })
        }) {
            self.error = Some(SCRIPT_CHANGES.into());
            return;
        }
        let Some(session) = &self.w.session else {
            return;
        };
        let stem = om_data::file_refs::file_stem(&session.target.file);
        self.reconnect = Some(ReconnectDialog {
            file,
            target: format!("“{stem}” ({})", session.target.host),
            account: self.w.calcs.account.clone(),
            password: Zeroizing::new(String::new()),
            pending: false,
            error: None,
            probes: Vec::new(),
        });
    }

    /// The reconnect dialog, over the front window.
    pub(super) fn reconnect_dialog(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.reconnect else {
            return;
        };
        let (mut submit, mut cancel) = (false, false);
        egui::Modal::new(egui::Id::unique("reconnect")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Reconnect");
            ui.label(format!(
                "Open {} again as “{}”.",
                dialog.target, dialog.account
            ));
            ui.small(
                "Unsaved changes stay in their windows. Each record they belong to is read \
                 back from the host and compared with what the changes started from before \
                 anything can be saved.",
            );
            ui.add_space(8.0);
            ui.add_enabled_ui(!dialog.pending, |ui| {
                ui.horizontal(|ui| {
                    let label = ui.label("Password");
                    let response = ui
                        .add(
                            egui::TextEdit::singleline(&mut *dialog.password)
                                .password(true)
                                .desired_width(220.0),
                        )
                        .labelled_by(label.id);
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                });
            });
            if let Some(error) = &dialog.error {
                ui.label(RichText::new(error).color(crate::theme::TEXT));
            }
            if dialog.pending {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reconnecting…");
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!dialog.pending, egui::Button::new("Cancel"))
                    .clicked()
                {
                    cancel = true;
                }
                if ui
                    .add_enabled(!dialog.pending, egui::Button::new("Reconnect"))
                    .clicked()
                {
                    submit = true;
                }
            });
        });
        if cancel {
            self.reconnect = None;
        } else if submit {
            self.send_reconnect(ctx);
        }
    }

    /// Send the reconnect, with what the file's windows hold now.
    fn send_reconnect(&mut self, ctx: &egui::Context) {
        let Some(file) = self
            .reconnect
            .as_ref()
            .filter(|d| !d.pending)
            .map(|d| d.file)
        else {
            return;
        };
        // Every lost window: the other files of the lost connection may
        // reopen with this one (#1011), and their windows are read back too.
        let lost: Vec<(u32, u64)> = std::iter::once(&self.w)
            .chain(&self.windows)
            .filter(|d| d.connection_lost.is_some())
            .map(|d| (d.id, d.file_id))
            .collect();
        let Some(window) = lost
            .iter()
            .find(|(_, of)| *of == file)
            .map(|(window, _)| *window)
        else {
            self.reconnect = None;
            return;
        };
        let ids: Vec<u32> = lost.into_iter().map(|(window, _)| window).collect();
        let probes: Vec<Probe> = ids
            .into_iter()
            .filter_map(|id| self.with_window(id, |app| app.probe()))
            .collect();
        // A save the window sent itself, left unanswered by the loss while
        // the window had related changes: whether the host has them cannot
        // be told, so they cannot be kept (#152). Nor can what a script
        // left, or a related record deleted. This file is not reconnected;
        // another file with such changes is not reopened with it, and stays
        // lost. A save the worker made, which commits a record's related
        // changes, is the worker's to judge: it knows whether that commit
        // sent the host any record data, and refuses the reconnect when it
        // may have (`FileState::unrecoverable`).
        let ambiguous = |probe: &Probe| {
            probe.related_pending && matches!(probe.unanswered, Some(Write::Update { .. }))
        };
        let refusal = probes
            .iter()
            .filter(|probe| probe.file == file)
            .find_map(|probe| {
                if ambiguous(probe) {
                    Some(
                        "A save of changes to related records was on its way when the \
                         connection was lost, and whether the host has them cannot be told. \
                         Reconnecting cannot recover them: copy what you need, then close the \
                         file and open it again.",
                    )
                } else {
                    probe.unrecoverable.then_some(SCRIPT_CHANGES)
                }
            });
        if let Some(refusal) = refusal {
            let dialog = self.reconnect.as_mut().expect("checked above");
            dialog.error = Some(refusal.into());
            return;
        }
        let keep_lost: Vec<u64> = probes
            .iter()
            .filter(|probe| probe.file != file && (probe.unrecoverable || ambiguous(probe)))
            .map(|probe| probe.file)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let related: Vec<(u32, u64, i64)> = probes
            .iter()
            .flat_map(|probe| {
                probe
                    .related
                    .iter()
                    .filter_map(|edit| match edit.field.row {
                        crate::document::RelatedRow::Record(record) => {
                            Some((probe.window, edit.field.table, i64::try_from(record).ok()?))
                        }
                        crate::document::RelatedRow::New { .. } => None,
                    })
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut records = BTreeSet::new();
        let mut current = Vec::new();
        for probe in &probes {
            records.extend(
                probe
                    .drafts
                    .iter()
                    .filter(|d| probe.new_record != Some((d.layout, d.record)))
                    .map(|d| (probe.window, d.layout, d.record)),
            );
            if let Some(Write::Update { layout, record } | Write::Delete { layout, record }) =
                probe.unanswered
            {
                records.insert((probe.window, layout, record));
            }
            // A new record's place is asked by its old id; the worker
            // looks for it under its new one (#152).
            if let Some((layout, record)) = probe.current {
                current.push((probe.window, layout, record));
            }
        }
        let dialog = self.reconnect.as_mut().expect("checked above");
        let password = std::mem::replace(&mut dialog.password, Zeroizing::new(String::new()));
        dialog.pending = true;
        dialog.error = None;
        dialog.probes = probes;
        let job = ReconnectJob {
            password,
            use_saved_license: self.hosts.use_saved_license,
            records: records.into_iter().collect(),
            current,
            related,
            keep_lost,
        };
        // Sent as is: a lost window's requests (`request`) go nowhere.
        let request = Request::Reconnect(Box::new(job));
        if let Err(error) = self.worker.send(ctx, self.epoch, window, request) {
            dialog.pending = false;
            dialog.error = Some(error.message);
        }
    }

    /// The window the app works on, as a reconnect finds it.
    fn probe(&self) -> Probe {
        let session = self.w.session.as_ref();
        let drafts = self
            .w
            .document
            .drafts()
            .into_iter()
            .map(|((layout, record), fields)| Drafted {
                layout,
                record,
                fields,
                received: session.and_then(|s| received(s, layout, record)),
            })
            .collect();
        let related = self.w.document.all_related_edits();
        let related_pending = !related.is_empty()
            || self
                .w
                .uncommitted
                .as_ref()
                .is_some_and(|(_, _, open)| !open.related.is_empty());
        let unrecoverable = self.w.uncommitted.as_ref().is_some_and(|(_, _, open)| {
            crate::scripting::session::unrecoverable_changes(open, true)
        });
        let new_record = self
            .w
            .uncommitted
            .as_ref()
            .and_then(|(layout, record, open)| {
                (open.new && open.session && !open.saved).then_some((*layout, *record))
            });
        Probe {
            window: self.w.id,
            file: self.w.file_id,
            related,
            related_pending,
            unrecoverable,
            current: session.and_then(|s| Some((s.layout?, s.current_record()?.1.id))),
            drafts,
            unanswered: self.w.unanswered,
            new_record,
            table: session
                .and_then(crate::calc::layout_occurrence)
                .and_then(|occurrence| {
                    self.w
                        .calcs
                        .schema()?
                        .table_of(occurrence)
                        .map(|table| table.id)
                }),
        }
    }

    /// The worker's answer to a reconnect. A failure keeps the dialog up
    /// with the reason, and the file as lost as it was.
    pub(super) fn reconnected(
        &mut self,
        ctx: &egui::Context,
        result: Result<Reconnected, DataError>,
    ) {
        let Some(dialog) = &mut self.reconnect else {
            return;
        };
        let reconnected = match result {
            Ok(reconnected) => reconnected,
            Err(error) => {
                dialog.pending = false;
                dialog.error = Some(format!("The file was not reconnected: {error}"));
                return;
            }
        };
        let dialog = self.reconnect.take().expect("checked above");
        // Each file open again reads its catalogs from its fresh session,
        // once; a window of a file still lost keeps its banner.
        let mut asked = std::collections::BTreeMap::new();
        for probe in &dialog.probes {
            let Some((_, privileges)) = reconnected
                .recovered
                .iter()
                .find(|(file, _)| *file == probe.file)
            else {
                continue;
            };
            let recovered = self.with_window(probe.window, |app| {
                app.recover(probe, &reconnected, privileges.clone());
                app.w
                    .recovery
                    .as_ref()
                    .map(|r| r.records.keys().copied().collect::<Vec<_>>())
                    .unwrap_or_default()
            });
            if let Some(keys) = recovered {
                asked.entry(probe.file).or_insert(probe.window);
                // Release only this owner's settled observations. A second
                // window may still hold an edit of the same record.
                let released: Vec<_> = reconnected
                    .records
                    .iter()
                    .filter_map(|((window, layout, record), _)| {
                        (*window == probe.window && !keys.contains(&(*layout, *record)))
                            .then_some((*layout, *record))
                    })
                    .collect();
                if !released.is_empty() {
                    self.with_window(probe.window, |app| app.release_records(ctx, released));
                }
            }
        }
        // The file's catalogs, from the fresh session: the layout list
        // reloads every window of the file (`reload_reconnected`).
        for window in asked.into_values() {
            self.with_window(window, |app| {
                app.request(ctx, Request::CalcSchema);
                app.request(ctx, Request::Layouts);
                // A catalog the Scripts menu read is read again; one not
                // read yet waits for the menu (#1053).
                if app.w.scripts_read != super::windows::ScriptsRead::Unread {
                    app.w.scripts_read = super::windows::ScriptsRead::Asked;
                    app.request(ctx, Request::Scripts { workspace: None });
                }
            });
        }
    }

    /// The window the app works on after its file reconnected: it finds
    /// its record and checks its edits. It reloads once the file's fresh
    /// layout list is in (`reload_reconnected`).
    fn recover(
        &mut self,
        probe: &Probe,
        reconnected: &Reconnected,
        privileges: Option<om_data::host::Privileges>,
    ) {
        self.w.connection_lost = None;
        self.w.calcs.host_version = reconnected.host_version.clone().unwrap_or_default();
        self.w.calcs.privileges = privileges;
        self.w.calcs.data_changed();
        self.w.document.clear_layout_cache();
        self.w.reconnected = true;
        let mut recovery = Recovery {
            reconnected: true,
            ..Recovery::default()
        };
        let read = |key: (u32, i64)| {
            reconnected
                .records
                .iter()
                .find(|(k, _)| *k == (probe.window, key.0, key.1))
                .map(|(_, read)| read)
        };
        let position = reconnected
            .positions
            .iter()
            .find(|(window, _)| *window == probe.window);
        if let (Some(session), Some((_, found))) = (&mut self.w.session, position) {
            match found {
                Ok(Some(index)) => session.current = *index,
                Ok(None) => recovery.notes.push(
                    "The record this window showed is no longer in its found set; it may have \
                     been deleted on the host."
                        .into(),
                ),
                // Where it is cannot be told: the window stays where it was.
                Err(_) => {}
            }
        }
        // Related reservations answer to their owning table, even when two files
        // used the same numeric record id. Keep the UI copy in step with the worker.
        if let Some((_, _, open)) = &mut self.w.uncommitted {
            for (window, table, old, new) in &reconnected.recreated {
                if *window != probe.window {
                    continue;
                }
                let Ok(new) = new else { continue };
                for draft in &mut open.related {
                    if draft.table == *table && draft.reserved == Some(*old) {
                        draft.reserved = Some(*new);
                        draft.prepared = None;
                    }
                }
            }
        }
        // The lock an existing record's editing took went with the lost
        // connection, unless the reconnect carried the window's record
        // session (a refused related save's drafts, #152): its edits are
        // saved by the ordinary path, which locks the record again and
        // checks the version they began from.
        if let Some((_, _, open)) = &mut self.w.uncommitted {
            if open.session
                && !open.new
                && open.attachment_field.is_none()
                && open.related_attachments.is_empty()
                && !reconnected.carried.contains(&probe.window)
            {
                open.session = false;
            }
        }
        // The window's new record, held again under a new id or gone (#152).
        let recreated = probe.new_record.and_then(|(layout, old)| {
            let (_, _, _, new) = reconnected
                .recreated
                .iter()
                .find(|(window, table, id, _)| {
                    *window == probe.window && Some(*table) == probe.table && *id == old
                })?;
            Some((layout, old, new.clone()))
        });
        let renumbered = match &recreated {
            Some((_, old, Ok(new))) => Some((*old, *new)),
            _ => None,
        };
        match recreated {
            Some((_, old, Ok(new))) => {
                // A failure kept from an earlier reconnect is over.
                self.w.unrecreated = None;
                self.w.document.renumber(old, new);
                if let Some((_, record, _)) = &mut self.w.uncommitted {
                    if *record == old {
                        *record = new;
                    }
                }
                recovery.notes.push(
                    "This window's new record was not saved yet. It was made again on the new \
                     connection with the values it had (its serial numbers, creation time and \
                     UUID among them): save or revert it as usual."
                        .into(),
                );
            }
            // Kept as it was, frozen, until the user tries again or
            // discards it (`unrecreated_bar`).
            Some((_, _, Err(message))) => self.w.unrecreated = Some(message),
            None => {}
        }
        for drafted in &probe.drafts {
            let key = (drafted.layout, drafted.record);
            // Not on the host: nothing to check it against.
            if probe.new_record == Some(key) {
                continue;
            }
            let current = probe.current == Some(key);
            match self.settle(drafted, read(key), current, false) {
                Settled::Kept(checked) => {
                    recovery.records.insert(key, checked);
                }
                Settled::Applied(note) => recovery.notes.push(note),
            }
        }
        match self.w.unanswered.take() {
            Some(Write::Delete { layout, record }) => {
                recovery.notes.push(match read((layout, record)) {
                    Some(Ok(page)) if page.records.iter().all(|r| r.id != record) => format!(
                        "The record being deleted when the connection was lost (ID {record}) \
                         is gone from the host."
                    ),
                    Some(Ok(_)) => format!(
                        "The record being deleted when the connection was lost (ID {record}) \
                         is still on the host; it was not deleted again."
                    ),
                    _ => format!(
                        "Whether the record being deleted when the connection was lost (ID \
                         {record}) was deleted cannot be told; it was not deleted again."
                    ),
                });
            }
            // A save the worker knows sent nothing keeps its changes on
            // screen, to save again (#152).
            Some(Write::Scripted) if reconnected.unsent.contains(&probe.file) => {}
            Some(Write::Scripted) => recovery.notes.push(
                "A save of changes scripts made was on its way when the connection was lost. \
                 Whether the host has it cannot be told, and it is not sent again."
                    .into(),
            ),
            // The edits it carried were checked above.
            Some(Write::Update { .. }) | None => {}
        }
        // Edits of related records through a new record held again answer
        // to its new id, as its drafts do.
        let related: Vec<crate::document::RelatedEdit> = probe
            .related
            .iter()
            .cloned()
            .map(|mut edit| {
                if let Some((old, new)) = renumbered {
                    if edit.field.parent == old {
                        edit.field.parent = new;
                    }
                }
                edit
            })
            .collect();
        settle_related(
            self.w.calcs.schema().map(AsRef::as_ref),
            &related,
            |table, record| {
                reconnected
                    .related
                    .iter()
                    .find(|(key, _)| *key == (probe.window, table, record))
                    .map(|(_, read)| read)
            },
            &mut recovery,
        );
        // The lost session's versions went with it; the edits checked now
        // answer to the versions read back (`record_by_id`), those settled
        // are released (`reconnected`).
        // A new record answers to no version on the host.
        self.w.retained = probe
            .drafts
            .iter()
            .filter(|d| probe.new_record != Some((d.layout, d.record)))
            .map(|d| {
                let key = (d.layout, d.record);
                let basis = recovery.records.get(&key).and_then(|c| c.basis.clone());
                (key, basis.or_else(|| d.received.clone()))
            })
            .collect();
        self.w.recovery = (recovery.shows() || !recovery.records.is_empty()).then_some(recovery);
    }

    /// What becomes of `drafted`'s edits, the record read back as `read`.
    /// `current`: the record is the window's current one.
    fn settle(
        &mut self,
        drafted: &Drafted,
        read: Option<&Result<RecordPage, DataError>>,
        current: bool,
        whole_record_conflict: bool,
    ) -> Settled {
        let count = drafted.fields.len();
        // Users know records by position; the window's own record needs no
        // number, another one goes by its id.
        let (subject, object) = if current {
            ("This record".to_owned(), "this record".to_owned())
        } else {
            let id = drafted.record;
            (
                format!("The record with ID {id}"),
                format!("the record with ID {id}"),
            )
        };
        let (changes, the_changes, were, them) = phrasing(count);
        let basis = match read {
            Some(Ok(page)) => page
                .records
                .iter()
                .position(|r| r.id == drafted.record)
                .map(|index| values_by_name(page, index)),
            _ => None,
        };
        let checked = check(drafted, read);
        let checked =
            if whole_record_conflict && matches!(&checked, Check::Unchanged | Check::Applied) {
                Check::Changed
            } else {
                checked
            };
        let (note, hold) = match checked {
            Check::Unchanged => (
                format!(
                    "{changes} to {object} {were} checked against the host's copy, which has \
                     not changed: save or revert {them} as usual."
                ),
                None,
            ),
            Check::Applied => {
                self.w.document.clear_record(drafted.layout, drafted.record);
                return Settled::Applied(format!(
                    "The host's copy of {object} already holds {the_changes}; nothing is left \
                     to save."
                ));
            }
            Check::Changed => (
                format!(
                    "{subject} was changed on the host since {the_changes} began. {changes} \
                     {were} not saved and stay in this window. Keep My Changes lets you save \
                     {them} over the host's values; Discard My Changes shows the host's \
                     version."
                ),
                Some(Hold::Changed),
            ),
            Check::Deleted => (
                format!(
                    "{subject} was deleted on the host. {changes} cannot be saved; copy what you \
                     need, then discard {them}."
                ),
                Some(Hold::Deleted),
            ),
            Check::Unreadable(why) => (
                format!(
                    "{subject} could not be read back from the host to check {the_changes} \
                     ({why}), so {them} cannot be saved yet. Check Again reads it; Discard My \
                     Changes gives {them} up."
                ),
                Some(Hold::Unreadable),
            ),
        };
        Settled::Kept(Checked {
            note,
            basis: basis.or_else(|| drafted.received.clone()),
            hold,
            whole_record_conflict,
        })
    }

    /// The file's fresh layout list after a reconnect (`None` when it
    /// could not be read): every window of the file that reconnected takes
    /// it and reloads its layout and records, from the record it showed. A
    /// window whose layout is no longer in the file says so and shows the
    /// first layout; edits made on the layout gone are held.
    pub(super) fn reload_reconnected(
        &mut self,
        ctx: &egui::Context,
        catalog: Option<&(Vec<LayoutInfo>, Vec<LayoutMenuEntry>)>,
    ) {
        let file = self.w.file_id;
        let ids: Vec<u32> = std::iter::once(&self.w)
            .chain(&self.windows)
            .filter(|d| d.file_id == file && d.reconnected)
            .map(|d| d.id)
            .collect();
        for id in ids {
            self.with_window(id, |app| app.reload_window(ctx, catalog));
        }
    }

    fn reload_window(
        &mut self,
        ctx: &egui::Context,
        catalog: Option<&(Vec<LayoutInfo>, Vec<LayoutMenuEntry>)>,
    ) {
        self.w.reconnected = false;
        let Some(session) = self.w.session.as_mut() else {
            return;
        };
        let Some((layouts, menu)) = catalog else {
            let key = session.refresh();
            self.request_layout(ctx, key);
            return;
        };
        let (shown, shown_name) = (session.layout, session.layout_name().map(str::to_owned));
        session.layout_menu = menu.clone();
        let schema = self.w.calcs.schema().map(AsRef::as_ref);
        let key = match session.set_layouts_for(layouts.clone(), schema) {
            None if shown.is_some_and(|id| layouts.iter().any(|l| l.id == id)) => session.refresh(),
            key => {
                let now = session.layout_name().map(str::to_owned);
                let name = shown_name.unwrap_or_default();
                let gone = shown.unwrap_or_default();
                let recovery = self.w.recovery.get_or_insert_with(Recovery::default);
                recovery.reconnected = true;
                recovery.notes.push(match &now {
                    Some(now) => format!(
                        "The layout “{name}” this window showed is no longer in the file; it \
                         shows “{now}” instead."
                    ),
                    None => format!(
                        "The layout “{name}” this window showed is no longer in the file, and \
                         no other layout can be opened."
                    ),
                });
                for ((layout, record), fields) in self.w.document.drafts() {
                    if layout != gone {
                        continue;
                    }
                    let count = fields.len();
                    recovery.records.insert(
                        (layout, record),
                        Checked {
                            note: format!(
                                "{} made on “{name}” to the record with ID {record} cannot be \
                                 shown or saved; copy what you need, then discard {}.",
                                if count == 1 {
                                    "The unsaved change"
                                } else {
                                    "The unsaved changes"
                                },
                                if count == 1 { "it" } else { "them" }
                            ),
                            basis: None,
                            hold: Some(Hold::LayoutGone),
                            whole_record_conflict: false,
                        },
                    );
                }
                key
            }
        };
        self.request_layout(ctx, key);
    }

    /// The bar of a window whose save may be partly applied (#1006), in
    /// place of the unsaved changes bar until the save is settled: what
    /// each file did, and the provider's operations to settle it.
    pub(super) fn unsettled_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(files) = &self.w.unsettled else {
            return;
        };
        use om_data::FileOutcome;
        egui::Panel::top("partly_saved").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("This record was saved in some of its files but not all.")
                        .strong(),
                );
                for file in files {
                    let name = file.file.rsplit('/').next().unwrap_or(&file.file);
                    let name = name.strip_suffix(".fmp12").unwrap_or(name);
                    let state = match file.outcome {
                        FileOutcome::Committed => "saved",
                        FileOutcome::NotSubmitted => "not saved yet",
                        FileOutcome::Unknown => "not known whether it was saved",
                        FileOutcome::NotApplied => "not saved (checked)",
                        FileOutcome::Unresolved => "cannot be checked",
                    };
                    ui.label(format!("{name}: {state}."));
                }
            });
            ui.horizontal(|ui| {
                let idle = !self.w.document.pending;
                let open = |outcome: FileOutcome| {
                    matches!(outcome, FileOutcome::Unknown | FileOutcome::Unresolved)
                };
                let checkable = files.iter().any(|f| f.outcome == FileOutcome::Unknown);
                if ui
                    .add_enabled(idle && checkable, egui::Button::new("Check Again"))
                    .clicked()
                {
                    actions.push(Action::CheckUnsettledSave);
                }
                let resumable = !files.iter().any(|f| open(f.outcome));
                if ui
                    .add_enabled(idle && resumable, egui::Button::new("Resume Save"))
                    .clicked()
                {
                    actions.push(Action::ResumeUnsettledSave);
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Discard Unsaved Changes"))
                    .clicked()
                {
                    actions.push(Action::DiscardUnsettledSave);
                }
            });
            ui.label(
                "Discarding keeps what the files already saved: nothing they saved is undone. \
                 The record cannot be changed until the save is settled.",
            );
        });
    }

    /// Each window's save that may be partly applied as a script left it
    /// (`ScriptEvent::Unsettled`, #1006), in every window of `files`, with
    /// the script's end: a commit the script made in any of them, in any
    /// file, may be partly applied (it leaves its changes on the record),
    /// and Revert Record, or closing, may have settled one. A window that
    /// becomes unsettled is frozen before it takes any typing.
    pub(super) fn unsettled_after_script(
        &mut self,
        ctx: &egui::Context,
        files: &[u64],
        saves: &[(u32, Vec<om_data::FileCommit>)],
    ) {
        let ids: Vec<u32> = std::iter::once(self.w.id)
            .chain(self.windows.iter().map(|d| d.id))
            .collect();
        for id in ids {
            let report = saves
                .iter()
                .find(|(window, _)| *window == id)
                .map(|(_, report)| report.clone());
            self.with_window(id, |app| {
                if files.contains(&app.w.file_id)
                    && app.w.connection_lost.is_none()
                    && (report.is_some() || app.w.unsettled.is_some())
                {
                    app.save_settled(ctx, crate::worker::Settle::Report, Ok(()), report);
                }
            });
        }
    }

    /// The bar of a window whose new record a reconnect could not make
    /// again (#152), in place of the reconnected bar: its values are kept,
    /// nothing was sent, and only the user's choice goes on.
    pub(super) fn unrecreated_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(message) = &self.w.unrecreated else {
            return;
        };
        egui::Panel::top("unrecreated").show(ui, |ui| {
            ui.label(
                RichText::new(
                    "This window's new record was not saved yet, and it could not be made \
                     again on the new connection.",
                )
                .strong(),
            );
            ui.label(message.as_str());
            ui.label(
                "Its values are kept, and nothing was sent. Try again, or discard the new \
                 record.",
            );
            ui.horizontal(|ui| {
                let idle = !self.w.document.pending;
                if ui
                    .add_enabled(idle, egui::Button::new("Try Again"))
                    .clicked()
                {
                    actions.push(Action::RecreateRecord);
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Discard New Record"))
                    .clicked()
                {
                    actions.push(Action::DiscardNewRecord);
                }
            });
        });
    }

    /// Try again to make the window's kept new record (`retry`), or
    /// discard it.
    pub(super) fn recreate(&mut self, ctx: &egui::Context, retry: bool) {
        if self.w.unrecreated.is_none() || self.w.document.pending {
            return;
        }
        self.w.document.pending = true;
        self.request(ctx, Request::Recreate { retry });
    }

    /// The worker's answer to [`Self::recreate`]: the record under its new
    /// id, or discarded, the window editable again; or why it still could
    /// not be made, kept as it was.
    pub(super) fn recreated(
        &mut self,
        ctx: &egui::Context,
        result: Result<Option<(i64, i64, Option<u64>)>, DataError>,
    ) {
        self.w.document.pending = false;
        match result {
            Err(error) => self.w.unrecreated = Some(error.message),
            Ok(made) => {
                match made {
                    Some((old, new, position)) => {
                        self.w.document.renumber(old, new);
                        // Its related records' holds answer to its new id.
                        if let Some(recovery) = &mut self.w.recovery {
                            let moved: Vec<_> = recovery
                                .records
                                .keys()
                                .filter(|(_, record)| *record == old)
                                .copied()
                                .collect();
                            for (layout, record) in moved {
                                if let Some(checked) = recovery.records.remove(&(layout, record)) {
                                    recovery.records.insert((layout, new), checked);
                                }
                            }
                        }
                        if let (Some(session), Some(index)) = (&mut self.w.session, position) {
                            session.current = index;
                        }
                        if let Some((_, record, _)) = &mut self.w.uncommitted {
                            if *record == old {
                                *record = new;
                            }
                        }
                    }
                    None => {
                        if let Some((layout, record, _)) = self.w.uncommitted.take() {
                            self.w.document.clear_record(layout, record);
                        }
                    }
                }
                self.w.unrecreated = None;
                self.refresh_capabilities(ctx);
                self.w.calcs.data_changed();
                let key = self.w.session.as_mut().and_then(Session::refresh);
                self.request_page(ctx, key);
            }
        }
    }

    /// Ask the provider to settle the window's partly applied save.
    pub(super) fn settle_save(&mut self, ctx: &egui::Context, settle: crate::worker::Settle) {
        if self.w.unsettled.is_none() || self.w.document.pending {
            return;
        }
        self.w.document.pending = true;
        self.request(ctx, Request::SettleSave(settle));
    }

    /// The provider's answer to [`Self::settle_save`] (or to the query
    /// after a script): the save's report now. A save no longer unsettled
    /// was applied in full or discarded, so the window's changes are no
    /// longer to save: they go, and the record is read again, in every
    /// window, since what it saved can show in any (a related file's
    /// windows too).
    pub(super) fn save_settled(
        &mut self,
        ctx: &egui::Context,
        settle: crate::worker::Settle,
        result: Result<(), DataError>,
        report: Option<Vec<om_data::FileCommit>>,
    ) {
        if settle != crate::worker::Settle::Report {
            self.w.document.pending = false;
        }
        let was = self.w.unsettled.is_some();
        if let Err(error) = &result {
            if settle != crate::worker::Settle::Report {
                self.error = Some(error.message.clone());
            }
        }
        self.w.unsettled = report;
        if self.w.unsettled.is_some() && !was {
            // Frozen: its cells read-only, the record read again with the
            // save's part shown (`shown_transactions`).
            self.refresh_capabilities(ctx);
            self.w.calcs.data_changed();
            let key = self.w.session.as_mut().and_then(Session::refresh);
            self.request_page(ctx, key);
        }
        if was && self.w.unsettled.is_none() {
            if let Some(session) = &self.w.session {
                self.w.document.revert(session);
            }
            self.w.uncommitted = None;
            self.refresh_capabilities(ctx);
            self.w.calcs.data_changed();
            let key = self.w.session.as_mut().and_then(Session::refresh);
            self.request_page(ctx, key);
            let current = self.w.id;
            let others: Vec<u32> = self
                .windows
                .iter()
                .map(|d| d.id)
                .filter(|id| *id != current)
                .collect();
            for id in others {
                self.with_window(id, |app| {
                    // A lost window keeps what it shows: it can read nothing.
                    if app.w.connection_lost.is_some() {
                        return;
                    }
                    app.w.calcs.data_changed();
                    let key = app.w.session.as_mut().and_then(Session::refresh);
                    app.request_page(ctx, key);
                });
            }
        }
    }

    /// Read the shown layout's record capabilities again: a window frozen
    /// by a partly applied save edits nothing until it is settled.
    pub(super) fn refresh_capabilities(&mut self, ctx: &egui::Context) {
        if let Some(layout_id) = self.w.session.as_ref().and_then(|s| s.layout) {
            self.request(ctx, Request::RecordCapabilities { layout_id });
        }
    }

    /// The bar of a reconnected window, in place of the unsaved changes
    /// bar while it has something to say or ask.
    pub(super) fn recovery_bar(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(recovery) = &self.w.recovery else {
            return;
        };
        let heading = if recovery.reconnected {
            Some("Reconnected to the host.")
        } else {
            recovery.holds().then_some(ON_HOLD)
        };
        egui::Panel::top("connection_recovery").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if let Some(heading) = heading {
                    ui.label(RichText::new(heading).strong());
                }
                let notes = recovery.records.values().map(|c| &c.note);
                for note in recovery.notes.iter().chain(notes) {
                    if !note.is_empty() {
                        ui.label(note);
                    }
                }
            });
            ui.horizontal(|ui| {
                let idle = !self.w.document.pending;
                if recovery.holds() {
                    if recovery.holds_for(Hold::Changed)
                        && ui
                            .add_enabled(idle, egui::Button::new("Keep My Changes"))
                            .clicked()
                    {
                        actions.push(Action::KeepRecoveredEdits);
                    }
                    if recovery.holds_for(Hold::Unreadable)
                        && ui
                            .add_enabled(idle, egui::Button::new("Check Again"))
                            .clicked()
                    {
                        actions.push(Action::CheckRecoveredEdits);
                    }
                    if ui
                        .add_enabled(idle, egui::Button::new("Discard My Changes"))
                        .clicked()
                    {
                        actions.push(Action::DiscardRecoveredEdits);
                    }
                    return;
                }
                if self.record_modified() {
                    if ui
                        .add_enabled(idle, egui::Button::new("Save Record"))
                        .clicked()
                    {
                        actions.push(Action::CommitRecord);
                    }
                    if ui
                        .add_enabled(idle, egui::Button::new("Revert Record"))
                        .clicked()
                    {
                        actions.push(Action::RevertRecord);
                    }
                }
                if ui.add_enabled(idle, egui::Button::new("OK")).clicked() {
                    actions.push(Action::DismissRecovery);
                }
            });
        });
    }

    /// Keep My Changes: edits of records the host changed may be saved,
    /// over the host's values of the fields they change, as long as the
    /// host still holds the values they were checked against. Edits of a
    /// deleted or unreadable record stay held.
    pub(super) fn keep_recovered_edits(&mut self) {
        let Some(recovery) = &mut self.w.recovery else {
            return;
        };
        for checked in recovery.records.values_mut() {
            if checked.hold == Some(Hold::Changed) {
                checked.hold = None;
                checked.whole_record_conflict = false;
                checked.note = "Your unsaved changes are kept. Saving them replaces the host's \
                                values of the fields you changed."
                    .into();
            }
        }
    }

    /// Check Again: the records that could not be read back are read again.
    pub(super) fn check_recovered_edits(&mut self, ctx: &egui::Context) {
        let Some(recovery) = &mut self.w.recovery else {
            return;
        };
        let keys: Vec<(u32, i64)> = recovery
            .records
            .iter()
            .filter(|(_, c)| c.hold == Some(Hold::Unreadable))
            .map(|(key, _)| *key)
            .collect();
        if !keys.is_empty() {
            // The related records edited through them too: a record held
            // for one is checked again only once that one is read (#152).
            let related = related_rows(&self.w.document.all_related_edits(), &keys);
            self.request(
                ctx,
                Request::Recheck {
                    records: keys,
                    related,
                },
            );
        }
    }

    /// Records read again (Check Again, or after a refused save): each is
    /// checked as after the reconnect, against the window's edits now.
    pub(super) fn rechecked(
        &mut self,
        ctx: &egui::Context,
        reads: Vec<((u32, i64), Result<RecordPage, DataError>)>,
        related: Vec<crate::worker::RelatedRead>,
    ) {
        let drafts = self.w.document.drafts();
        let current = self
            .w
            .session
            .as_ref()
            .and_then(|s| Some((s.layout?, s.current_record()?.1.id)));
        let mut released = Vec::new();
        for (key, read) in reads {
            let known = self
                .w
                .recovery
                .as_mut()
                .and_then(|r| r.records.remove(&key));
            // A record held for its related records' changes alone has no
            // edits of its own: it is checked as a whole (#152).
            let fields = drafts.get(&key).cloned().or_else(|| {
                known
                    .as_ref()
                    .filter(|known| known.whole_record_conflict)
                    .map(|_| Vec::new())
            });
            let (Some(known), Some(fields)) = (known, fields) else {
                released.push(key);
                continue;
            };
            let drafted = Drafted {
                layout: key.0,
                record: key.1,
                fields,
                received: known.basis,
            };
            let settled = self.settle(
                &drafted,
                Some(&read),
                current == Some(key),
                known.whole_record_conflict,
            );
            let recovery = self.w.recovery.get_or_insert_with(Recovery::default);
            match settled {
                Settled::Kept(checked) => {
                    recovery.records.insert(key, checked);
                }
                Settled::Applied(note) => {
                    recovery.notes.push(note);
                    released.push(key);
                }
            }
        }
        // The related records read again: one still not read, changed or
        // gone holds its record again (#152).
        if !related.is_empty() {
            let edits: Vec<_> = self
                .w
                .document
                .all_related_edits()
                .into_iter()
                .filter(|edit| {
                    matches!(edit.field.row, crate::document::RelatedRow::Record(record)
                    if related.iter().any(|((table, id), _)| {
                        *table == edit.field.table && u64::try_from(*id).ok() == Some(record)
                    }))
                })
                .collect();
            let schema = self.w.calcs.schema().cloned();
            let recovery = self.w.recovery.get_or_insert_with(Recovery::default);
            settle_related(
                schema.as_deref(),
                &edits,
                |table, record| {
                    related
                        .iter()
                        .find(|(key, _)| *key == (table, record))
                        .map(|(_, read)| read)
                },
                recovery,
            );
            released.retain(|key| !recovery.records.contains_key(key));
        }
        self.release_records(ctx, released);
        self.forget_settled_recovery();
        let key = self.w.session.as_mut().and_then(Session::refresh);
        self.request_page(ctx, key);
    }

    /// A save of `record` was refused: `true` when the window says so in
    /// its bar rather than in an alert. Its edits stay in the window, and
    /// nothing sends them again by itself.
    ///
    /// - The host changed the record since the edits began (FileMaker's
    ///   306) or no longer has it (101): the edits are held and the record
    ///   read again, to be compared with the values the window showed when
    ///   they began (`rechecked`).
    /// - Another user is editing the record (301, or 300 for the file): the
    ///   edits wait for the user's Save Record, which tries again, still
    ///   answering to the version and values they began from; or for
    ///   Revert Record.
    ///
    /// Edits a reconnect checked always go this way. Otherwise a refusal
    /// while a script runs (its transaction's or its own writes) stays the
    /// script's, an alert as before.
    pub(super) fn recheck_refused_save(
        &mut self,
        ctx: &egui::Context,
        layout: u32,
        record: i64,
        error: &DataError,
    ) -> bool {
        let key = (layout, record);
        let checked = self
            .w
            .recovery
            .as_ref()
            .is_some_and(|r| r.records.contains_key(&key));
        let count = self.w.document.drafts().get(&key).map_or(0, Vec::len);
        if !checked && (count == 0 || self.script.running) {
            return false;
        }
        let note = match error.code {
            Some(306) => CHANGED.to_owned(),
            Some(101) => MISSING.to_owned(),
            Some(300 | 301) => {
                let (changes, _, were, them) = phrasing(count.max(1));
                let stay = if count > 1 { "stay" } else { "stays" };
                format!(
                    "{} {changes} {were} not saved and {stay} in this window: Save Record \
                     tries again; Revert Record gives {them} up.",
                    error.message.trim()
                )
            }
            _ => return false,
        };
        let basis = self.draft_basis(key);
        let whole_record_conflict = error.code == Some(306)
            && self
                .w
                .uncommitted
                .as_ref()
                .is_some_and(|(layout, record, _)| (*layout, *record) == key);
        let recovery = self.w.recovery.get_or_insert_with(Recovery::default);
        let entry = recovery.records.entry(key).or_insert_with(|| Checked {
            note: String::new(),
            basis,
            hold: None,
            whole_record_conflict: false,
        });
        entry.whole_record_conflict |= whole_record_conflict;
        entry.note = note;
        if matches!(error.code, Some(306 | 101)) {
            entry.hold = Some(Hold::Unreadable);
            // Its related records too: one changed since is read again and
            // its version taken, or every save would be refused again (#152).
            let related = related_rows(&self.w.document.all_related_edits(), &[key]);
            self.request(
                ctx,
                Request::Recheck {
                    records: vec![key],
                    related,
                },
            );
        }
        true
    }

    /// The values the window showed of record `key` when its edits began,
    /// or as it last received them.
    fn draft_basis(&self, key: (u32, i64)) -> Option<Values> {
        self.w
            .retained
            .get(&key)
            .cloned()
            .flatten()
            .or_else(|| received(self.w.session.as_ref()?, key.0, key.1))
    }

    /// A save of `record` went through: its edits are no longer checked,
    /// and edits begun after it answer to the version it wrote.
    pub(super) fn recovered_record_saved(&mut self, ctx: &egui::Context, layout: u32, record: i64) {
        if self.w.retained.contains_key(&(layout, record)) {
            self.release_records(ctx, vec![(layout, record)]);
        }
        if let Some(recovery) = &mut self.w.recovery {
            if recovery.records.remove(&(layout, record)).is_some() && recovery.records.is_empty() {
                // The reconnect's notes are done with.
                self.w.recovery = None;
            }
        }
    }

    /// The window began or gave up unsaved edits since it last looked: a
    /// record it began edits of is retained, its values as the window shows
    /// them kept with it; one whose edits went (saved, reverted, discarded,
    /// or committed by a script) is released. Only this window's versions
    /// change. Nothing is asked of a host whose connection is gone: a
    /// reconnect retains the edits it reads back (`recover`).
    pub(super) fn retain_drafts(&mut self, ctx: &egui::Context) {
        if self.w.connection_lost.is_some() {
            return;
        }
        let Some(session) = &self.w.session else {
            return;
        };
        let mut drafted = self.w.document.drafted_records();
        let candidates = self.w.document.entry_candidate_records();
        drafted.extend(&candidates);
        // Worker-only drafts need not have an accepted UI page. Genuine UI
        // input still gets a pin on such a record, and an existing UI pin
        // follows its draft into the worker until that record settles.
        let worker_owned = self
            .w
            .uncommitted
            .as_ref()
            .map(|(layout, record, _)| (*layout, *record));
        if let Some(key) = worker_owned.filter(|key| self.w.retained.contains_key(key)) {
            drafted.insert(key);
        }
        let mut ui_drafted = self.w.document.ui_drafted_records();
        ui_drafted.extend(candidates);
        let begun: Vec<(u32, i64)> = ui_drafted
            .iter()
            .filter(|key| !self.w.retained.contains_key(key))
            .copied()
            .collect();
        let ended: Vec<(u32, i64)> = self
            .w
            .retained
            .keys()
            .filter(|key| !drafted.contains(key))
            .copied()
            .collect();
        for key in &begun {
            let shown = received(session, key.0, key.1);
            self.w.retained.insert(*key, shown);
        }
        if !begun.is_empty() {
            let versions = begun
                .into_iter()
                .map(|(layout, record)| {
                    let version = session.loaded_pages().find_map(|page| {
                        (page.key.layout_id == layout)
                            .then(|| page.data.versions.get(&record).copied())
                            .flatten()
                    });
                    (layout, record, version)
                })
                .collect();
            self.request(ctx, Request::Retain(versions));
        }
        self.release_records(ctx, ended);
    }

    /// The window keeps no edits of `keys` any more: only its own versions
    /// of them go, and edits begun afterwards are retained afresh.
    fn release_records(&mut self, ctx: &egui::Context, keys: Vec<(u32, i64)>) {
        if keys.is_empty() {
            return;
        }
        for key in &keys {
            self.w.retained.remove(key);
            self.w.document.forget_edit_preparations(key.0, key.1);
        }
        self.request(ctx, Request::ReleaseRecords(keys));
    }

    /// OK: the notes are put away; the records whose edits were checked
    /// stay checked.
    pub(super) fn dismiss_recovery(&mut self) {
        if let Some(recovery) = &mut self.w.recovery {
            if recovery.holds() {
                return;
            }
            recovery.notes.clear();
            for checked in recovery.records.values_mut() {
                checked.note.clear();
            }
        }
        self.forget_settled_recovery();
    }

    /// Nothing left to say or check: the window forgets the reconnect.
    fn forget_settled_recovery(&mut self) {
        if self
            .w
            .recovery
            .as_ref()
            .is_some_and(|r| r.records.is_empty() && !r.shows())
        {
            self.w.recovery = None;
        }
    }

    /// Discard My Changes: the held edits go, and the window shows the
    /// host's records.
    pub(super) fn discard_recovered_edits(&mut self, ctx: &egui::Context) {
        let Some(recovery) = &mut self.w.recovery else {
            return;
        };
        let held: Vec<(u32, i64)> = recovery
            .records
            .iter()
            .filter(|(_, c)| c.hold.is_some())
            .map(|(key, _)| *key)
            .collect();
        let worker = self
            .w
            .uncommitted
            .as_ref()
            .map(|(layout, record, _)| (*layout, *record))
            .filter(|key| held.contains(key));
        for key in &held {
            if Some(*key) == worker {
                continue;
            }
            recovery.records.remove(key);
            self.w.document.clear_record(key.0, key.1);
        }
        self.w.document.cancel_pending_focus();
        self.release_records(
            ctx,
            held.into_iter()
                .filter(|key| Some(*key) != worker)
                .collect(),
        );
        self.forget_settled_recovery();
        if worker.is_some() {
            // Revert through the owning engine so pending repetitions,
            // related drafts, new records and container references are
            // discarded too. Keep the hold until that revert succeeds.
            self.save_on_worker(ctx, crate::scripting::Start::Revert);
            return;
        }
        let key = self.w.session.as_mut().and_then(Session::refresh);
        self.request_page(ctx, key);
    }

    /// Revert Record gives up the current record's edits, checked or held
    /// ones too, and only this window's version of the record.
    pub(super) fn release_current_record(&mut self, ctx: &egui::Context) {
        let current = self
            .w
            .session
            .as_ref()
            .and_then(|s| Some((s.layout?, s.current_record()?.1.id)));
        let Some(current) = current else {
            return;
        };
        let checked = self
            .w
            .recovery
            .as_mut()
            .is_some_and(|r| r.records.remove(&current).is_some());
        if checked || self.w.retained.contains_key(&current) {
            self.release_records(ctx, vec![current]);
        }
        self.forget_settled_recovery();
    }

    /// The file `file` closes: a reconnect of it goes with it (its
    /// password zeroized as it drops).
    pub(super) fn forget_reconnect(&mut self, file: u64) {
        if self.reconnect.as_ref().is_some_and(|d| d.file == file) {
            self.reconnect = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::{Field, FieldKind, Record};

    fn page(values: &[(&str, &str)]) -> RecordPage {
        RecordPage {
            versions: Default::default(),
            fields: values
                .iter()
                .map(|(name, _)| Field {
                    name: (*name).into(),
                    kind: FieldKind::Text,
                })
                .collect(),
            records: vec![Record {
                id: 7,
                values: values
                    .iter()
                    .map(|(_, v)| (!v.is_empty()).then(|| (*v).to_owned()))
                    .collect(),
            }],
            total: 1,
        }
    }

    fn drafted(fields: &[(&str, &str, &str)], received: &[(&str, &str)]) -> Drafted {
        Drafted {
            layout: 1,
            record: 7,
            fields: fields
                .iter()
                .map(|(name, original, value)| FieldDraft {
                    name: (*name).into(),
                    original: (*original).into(),
                    value: (*value).into(),
                })
                .collect(),
            received: Some(
                received
                    .iter()
                    .map(|(n, v)| ((*n).into(), (*v).into()))
                    .collect(),
            ),
        }
    }

    /// A reconnect changes only its own file's windows: another file's
    /// lost window keeps its banner and its edit; the reconnected one finds
    /// its record and keeps its edit, checked.
    #[test]
    fn a_reconnect_leaves_another_files_windows_as_they_are() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows[0].file_id = 9;
        app.window_files.insert(2, 9);
        for id in [1, 2] {
            app.with_window(id, |app| {
                let session = app.w.session.as_ref().unwrap();
                let draft = format!("draft {id}");
                app.w.document.set_field(session, "Name", draft).unwrap();
                app.lose_connection(&DataError::connection_lost("gone"));
            });
        }
        app.apply(&ctx, Action::Reconnect);
        app.reconnect.as_mut().unwrap().password = Zeroizing::new("typed".into());
        app.send_reconnect(&ctx);
        let dialog = app.reconnect.as_ref().unwrap();
        assert!(dialog.pending && dialog.password.is_empty());
        // Every lost window is probed: the worker reopens what it can.
        assert_eq!(dialog.probes.len(), 2);
        let mut read = page(&[("Name", "Ada")]);
        read.records[0].id = 1;
        let own = app.w.file_id;
        app.reconnected(
            &ctx,
            Ok(Reconnected {
                host_version: None,
                records: vec![((1, 1, 1), Ok(read))],
                positions: vec![(1, Ok(Some(0)))],
                recovered: vec![(own, None)],
                related: Vec::new(),
                unsent: Vec::new(),
                recreated: Vec::new(),
                carried: Vec::new(),
            }),
        );
        assert!(app.reconnect.is_none());
        assert!(app.w.connection_lost.is_none());
        assert_eq!(app.w.document.unsaved(), 1, "the edit is kept");
        let recovery = app.w.recovery.as_ref().unwrap();
        assert!(!recovery.holds(), "{recovery:?}");
        assert!(recovery.records[&(1, 1)].note.contains("has not changed"));
        let other = &app.windows[0];
        assert!(other.connection_lost.is_some());
        assert_eq!(other.document.unsaved(), 1);
        assert!(other.recovery.is_none());
    }

    /// The worker reopened another file of the lost connection with this
    /// one (#1011): its window is recovered too, its record found and its
    /// edit checked; a window of a file the worker did not reopen stays
    /// lost.
    #[test]
    fn a_reconnect_recovers_the_other_files_the_worker_reopened() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows[0].file_id = 9;
        app.window_files.insert(2, 9);
        for id in [1, 2] {
            app.with_window(id, |app| {
                let session = app.w.session.as_ref().unwrap();
                let draft = format!("draft {id}");
                app.w.document.set_field(session, "Name", draft).unwrap();
                app.lose_connection(&DataError::connection_lost("gone"));
            });
        }
        app.apply(&ctx, Action::Reconnect);
        app.reconnect.as_mut().unwrap().password = Zeroizing::new("typed".into());
        app.send_reconnect(&ctx);
        let mut read = page(&[("Name", "Ada")]);
        read.records[0].id = 1;
        let own = app.w.file_id;
        app.reconnected(
            &ctx,
            Ok(Reconnected {
                host_version: None,
                records: vec![((1, 1, 1), Ok(read.clone())), ((2, 1, 1), Ok(read))],
                positions: vec![(1, Ok(Some(0))), (2, Ok(Some(0)))],
                recovered: vec![(own, None), (9, None)],
                related: Vec::new(),
                unsent: Vec::new(),
                recreated: Vec::new(),
                carried: Vec::new(),
            }),
        );
        assert!(app.w.connection_lost.is_none());
        let other = &app.windows[0];
        assert!(
            other.connection_lost.is_none(),
            "the other file is open again"
        );
        assert_eq!(other.document.unsaved(), 1, "its edit is kept");
        assert!(other.recovery.is_some());
    }

    /// A user's unsaved edit of a related record across a reconnect
    /// (#152): read back on the new session, it stays when the host's copy
    /// is at the version the edit began at, and holds the whole record it
    /// was made through when the host changed it; it is never dropped. A
    /// save the loss left unanswered while the record had related changes
    /// refuses the reconnect: whether they reached the host cannot be told.
    #[test]
    fn a_related_edit_is_kept_or_holds_its_record_after_a_reconnect() {
        use crate::document::{RelatedField, RelatedRow};
        let field = RelatedField {
            layout: 1,
            parent: 1,
            table: 7,
            row: RelatedRow::Record(5),
            field: 3,
        };
        let read = |version| {
            Ok(Some(std::sync::Arc::new(om_data::host::RecordValues {
                values: Default::default(),
                mod_count: Some(version),
                ..Default::default()
            })))
        };
        let ctx = egui::Context::default();
        let lost = || {
            let mut app = crate::app::tests::two_record_windows();
            app.w
                .document
                .put_related_edit(field, 70, Some(4), "Before", "Mine");
            app.lose_connection(&DataError::connection_lost("gone"));
            app.apply(&ctx, Action::Reconnect);
            app.reconnect.as_mut().unwrap().password = Zeroizing::new("typed".into());
            app
        };

        let mut app = lost();
        app.w.unanswered = Some(Write::Update {
            layout: 1,
            record: 1,
        });
        app.send_reconnect(&ctx);
        let dialog = app.reconnect.as_ref().unwrap();
        assert!(!dialog.pending, "not sent");
        assert!(dialog.error.as_ref().unwrap().contains("related records"));

        for (version, held) in [(4, None), (5, Some(Hold::Changed))] {
            let mut app = lost();
            app.send_reconnect(&ctx);
            let dialog = app.reconnect.as_ref().unwrap();
            assert!(dialog.pending);
            assert_eq!(dialog.probes[0].related.len(), 1);
            let own = app.w.file_id;
            app.reconnected(
                &ctx,
                Ok(Reconnected {
                    host_version: None,
                    records: Vec::new(),
                    positions: vec![(1, Ok(Some(0)))],
                    recovered: vec![(own, None)],
                    related: vec![((1, 7, 5), read(version))],
                    unsent: Vec::new(),
                    recreated: Vec::new(),
                    carried: Vec::new(),
                }),
            );
            assert_eq!(
                app.w.document.all_related_edits().len(),
                1,
                "the edit stays (version {version})"
            );
            let hold = app
                .w
                .recovery
                .as_ref()
                .and_then(|r| r.records.get(&(1, 1)))
                .and_then(|checked| checked.hold);
            assert_eq!(hold, held, "version {version}");
        }
    }

    /// A user's related edits synchronized into the record's uncommitted
    /// changes do not refuse the reconnect (the review of #1062); a related
    /// record deleted through it does. Check Again keeps a record held for
    /// a related record that could not be read held until that one is read,
    /// and only then lets Keep My Changes save over the host's values.
    #[test]
    fn synchronized_related_edits_reconnect_and_check_again_reads_them() {
        use crate::document::{RelatedField, RelatedRow};
        use crate::scripting::window::{RelatedDraft, Uncommitted};
        let field = RelatedField {
            layout: 1,
            parent: 1,
            table: 7,
            row: RelatedRow::Record(5),
            field: 3,
        };
        let ctx = egui::Context::default();
        let draft = |deleted: bool| RelatedDraft {
            reserved: None,
            matched_cells: Default::default(),
            occurrence: 70,
            table: 7,
            record: Some(5),
            cells: if deleted {
                Default::default()
            } else {
                [(3, Some("Mine".to_owned()))].into()
            },
            typed: if deleted {
                Default::default()
            } else {
                [3].into()
            },
            version: Some(4),
            begun: None,
            matched: false,
            creation_parent: None,
            deleted,
            saved: Default::default(),
            prepared: None,
        };
        let lost = |deleted: bool| {
            let mut app = crate::app::tests::two_record_windows();
            if !deleted {
                app.w
                    .document
                    .put_related_edit(field, 70, Some(4), "Before", "Mine");
            }
            app.w.uncommitted = Some((
                1,
                1,
                Uncommitted {
                    related: vec![draft(deleted)],
                    ..Uncommitted::default()
                },
            ));
            app.lose_connection(&DataError::connection_lost("gone"));
            app.apply(&ctx, Action::Reconnect);
            app
        };
        let app = lost(true);
        assert!(app.reconnect.is_none(), "a deletion cannot be checked");
        assert_eq!(app.error.as_deref(), Some(SCRIPT_CHANGES));

        let mut app = lost(false);
        assert!(app.reconnect.is_some(), "typing is checked, not refused");
        app.reconnect.as_mut().unwrap().password = Zeroizing::new("typed".into());
        app.send_reconnect(&ctx);
        assert!(app.reconnect.as_ref().unwrap().pending);
        let own = app.w.file_id;
        app.reconnected(
            &ctx,
            Ok(Reconnected {
                host_version: None,
                records: Vec::new(),
                positions: vec![(1, Ok(Some(0)))],
                recovered: vec![(own, None)],
                related: vec![((1, 7, 5), Err(DataError::new("busy")))],
                unsent: Vec::new(),
                recreated: Vec::new(),
                carried: Vec::new(),
            }),
        );
        let hold = |app: &OpenMakerApp| {
            app.w
                .recovery
                .as_ref()
                .and_then(|r| r.records.get(&(1, 1)))
                .and_then(|checked| checked.hold)
        };
        assert_eq!(hold(&app), Some(Hold::Unreadable));
        let parent = || {
            let mut read = page(&[("Name", "Ada")]);
            read.records[0].id = 1;
            Ok(read)
        };
        // The record reads, its related record still does not.
        app.rechecked(
            &ctx,
            vec![((1, 1), parent())],
            vec![((7, 5), Err(DataError::new("busy")))],
        );
        assert_eq!(
            hold(&app),
            Some(Hold::Unreadable),
            "not Keep before it reads"
        );
        let read = Ok(Some(std::sync::Arc::new(om_data::host::RecordValues {
            values: Default::default(),
            mod_count: Some(4),
            ..Default::default()
        })));
        app.rechecked(&ctx, vec![((1, 1), parent())], vec![((7, 5), read)]);
        assert_eq!(hold(&app), Some(Hold::Changed), "read: Keep or Discard");
        assert_eq!(
            app.w.document.all_related_edits().len(),
            1,
            "the edit stays"
        );
    }

    #[test]
    fn edits_are_checked_against_the_record_read_back() {
        let before = [("Name", "Ada"), ("City", "Paris")];
        // An edit to empty text is an edit too.
        let edit = drafted(&[("Name", "Ada", "")], &before);
        let read = |values: &[(&str, &str)]| Some(Ok(page(values)));
        assert_eq!(check(&edit, read(&before).as_ref()), Check::Unchanged);
        assert_eq!(
            check(&edit, read(&[("Name", ""), ("City", "Paris")]).as_ref()),
            Check::Applied
        );
        assert_eq!(
            check(&edit, read(&[("Name", "Eve"), ("City", "Paris")]).as_ref()),
            Check::Changed
        );
        // Another field changed on the host: the record changed.
        assert_eq!(
            check(&edit, read(&[("Name", "Ada"), ("City", "Rome")]).as_ref()),
            Check::Changed
        );
        // One edit on the host and the other field as it was: neither
        // unchanged nor the edits saved, a change.
        let two = drafted(&[("Name", "A", "B"), ("City", "C", "D")], &[]);
        let host = [("Name", "B"), ("City", "C")];
        assert_eq!(check(&two, read(&host).as_ref()), Check::Changed);
        let host = [("Name", "B"), ("City", "D")];
        assert_eq!(check(&two, read(&host).as_ref()), Check::Applied);
        let host = [("Name", "A"), ("City", "C")];
        assert_eq!(check(&two, read(&host).as_ref()), Check::Unchanged);
        let mut gone = page(&before);
        gone.records.clear();
        assert_eq!(check(&edit, Some(&Ok(gone))), Check::Deleted);
        let failed = Err(DataError::new("no route"));
        assert_eq!(
            check(&edit, Some(&failed)),
            Check::Unreadable("no route".into())
        );
        assert!(matches!(check(&edit, None), Check::Unreadable(_)));
    }

    /// Window 1 of `app` lost its connection with an edit of record 1,
    /// and reconnected with the record read back as `read`.
    fn reconnected_with(
        app: &mut OpenMakerApp,
        ctx: &egui::Context,
        read: Result<RecordPage, DataError>,
    ) {
        let session = app.w.session.as_ref().unwrap();
        app.w
            .document
            .set_field(session, "Name", "draft".into())
            .unwrap();
        app.lose_connection(&DataError::connection_lost("gone"));
        app.apply(ctx, Action::Reconnect);
        app.send_reconnect(ctx);
        let own = app.w.file_id;
        app.reconnected(
            ctx,
            Ok(Reconnected {
                host_version: None,
                records: vec![((1, 1, 1), read)],
                positions: vec![(1, Ok(Some(0)))],
                recovered: vec![(own, None)],
                related: Vec::new(),
                unsent: Vec::new(),
                recreated: Vec::new(),
                carried: Vec::new(),
            }),
        );
    }

    /// A record that could not be read back holds its edit: Keep does not
    /// release it and nothing saves it, until a read succeeds.
    #[test]
    fn an_unreadable_record_stays_held_until_it_is_read() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows.clear();
        reconnected_with(&mut app, &ctx, Err(DataError::new("no route")));
        let hold = |app: &OpenMakerApp| app.w.recovery.as_ref().unwrap().records[&(1, 1)].hold;
        assert_eq!(hold(&app), Some(Hold::Unreadable));
        app.apply(&ctx, Action::KeepRecoveredEdits);
        assert_eq!(
            hold(&app),
            Some(Hold::Unreadable),
            "Keep does not release it"
        );
        app.apply(&ctx, Action::CommitRecord);
        assert_eq!(app.error.as_deref(), Some(HELD));
        assert!(
            app.w.in_flight.is_none() && !app.w.document.pending,
            "nothing sent"
        );
        app.error = None;
        let mut read = page(&[("Name", "Ada")]);
        read.records[0].id = 1;
        app.rechecked(&ctx, vec![((1, 1), Ok(read))], Vec::new());
        assert_eq!(hold(&app), None);
        assert_eq!(app.w.document.unsaved(), 1);
    }

    /// Record 1's host copy, with `name`.
    fn host_copy(name: &str) -> RecordPage {
        let mut read = page(&[("Name", name)]);
        read.records[0].id = 1;
        read
    }

    /// The host's answer to the window's save of record 1.
    fn save_refused(app: &mut OpenMakerApp, ctx: &egui::Context, window: u32, error: DataError) {
        app.with_window(window, |app| app.w.document.pending = true);
        let epoch = app.epoch;
        app.on_response(
            ctx,
            crate::worker::Response {
                epoch,
                window,
                reply: crate::worker::Reply::Updated {
                    layout_id: 1,
                    record_id: 1,
                    result: Err(error),
                },
            },
        );
    }

    fn changed_on_host() -> DataError {
        DataError::with_code(306, "Another client changed this record.")
    }

    fn checked(app: &OpenMakerApp) -> &Checked {
        &app.w.recovery.as_ref().unwrap().records[&(1, 1)]
    }

    /// A window's own edit, emptying a field, is refused because another
    /// client changed the record: it stays in the window, held, and the
    /// record is compared with what the window showed when the edit
    /// began. Nothing saves it until the user keeps it; a later change on
    /// the host holds it again; Discard gives it up.
    #[test]
    fn an_ordinary_save_over_a_changed_record_holds_the_emptied_edit() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows.clear();
        let session = app.w.session.as_ref().unwrap();
        app.w
            .document
            .set_field(session, "Name", String::new())
            .unwrap();
        app.retain_drafts(&ctx);
        let shown = app.w.retained[&(1, 1)].as_ref().unwrap();
        assert_eq!(shown["Name"], "Ada", "the values the edit began from");

        save_refused(&mut app, &ctx, 1, changed_on_host());
        assert!(app.error.is_none(), "the bar says so, not an alert");
        assert_eq!(checked(&app).hold, Some(Hold::Unreadable));
        assert_eq!(checked(&app).note, CHANGED);
        app.rechecked(&ctx, vec![((1, 1), Ok(host_copy("Eve")))], Vec::new());
        assert_eq!(checked(&app).hold, Some(Hold::Changed));
        let draft = |app: &OpenMakerApp| {
            app.w
                .document
                .value(app.w.session.as_ref().unwrap(), "Name")
        };
        assert_eq!(draft(&app).as_deref(), Some(""), "the emptied draft stays");
        app.apply(&ctx, Action::CommitRecord);
        assert_eq!(app.error.as_deref(), Some(HELD));
        assert!(!app.w.document.pending, "nothing sent");
        app.error = None;

        app.apply(&ctx, Action::KeepRecoveredEdits);
        assert_eq!(checked(&app).hold, None);
        // The host changes the record again before the kept edit is saved.
        save_refused(&mut app, &ctx, 1, changed_on_host());
        assert_eq!(checked(&app).hold, Some(Hold::Unreadable));
        app.rechecked(&ctx, vec![((1, 1), Ok(host_copy("Zed")))], Vec::new());
        assert_eq!(checked(&app).hold, Some(Hold::Changed));
        assert_eq!(draft(&app).as_deref(), Some(""));

        app.apply(&ctx, Action::DiscardRecoveredEdits);
        assert_eq!(app.w.document.unsaved(), 0);
        assert!(app.w.recovery.is_none(), "{:?}", app.w.recovery);
        assert!(app.w.retained.is_empty(), "released");
    }

    /// A record the host no longer has holds the edit with nothing to keep.
    #[test]
    fn an_edit_of_a_record_gone_from_the_host_can_only_be_discarded() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows.clear();
        let session = app.w.session.as_ref().unwrap();
        app.w
            .document
            .set_field(session, "Name", "Mine".into())
            .unwrap();
        app.retain_drafts(&ctx);
        save_refused(&mut app, &ctx, 1, DataError::with_code(101, "gone"));
        assert_eq!(checked(&app).note, MISSING);
        let mut gone = host_copy("Ada");
        gone.records.clear();
        app.rechecked(&ctx, vec![((1, 1), Ok(gone))], Vec::new());
        assert_eq!(checked(&app).hold, Some(Hold::Deleted));
        app.apply(&ctx, Action::KeepRecoveredEdits);
        assert_eq!(checked(&app).hold, Some(Hold::Deleted), "nothing to keep");
        assert_eq!(app.w.document.unsaved(), 1);
    }

    /// Another user editing the record refuses the save: the edit stays as
    /// it was, retried only by the user, and a reload of the window's
    /// records meanwhile does not change what it is compared with when the
    /// host refuses the retry because the record changed.
    #[test]
    fn a_lock_refusal_keeps_the_edit_and_its_baseline_for_an_explicit_retry() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows.clear();
        let session = app.w.session.as_ref().unwrap();
        app.w
            .document
            .set_field(session, "Name", "Mine".into())
            .unwrap();
        app.retain_drafts(&ctx);
        let busy = "This record is being modified by Eve. Try again when they are done.";
        save_refused(&mut app, &ctx, 1, DataError::with_code(301, busy));
        assert!(app.error.is_none(), "the bar says so, not an alert");
        assert!(!app.w.document.pending, "nothing is sent again by itself");
        assert_eq!(checked(&app).hold, None, "Save Record tries again");
        assert!(
            checked(&app).note.starts_with(busy),
            "{}",
            checked(&app).note
        );
        assert!(checked(&app).note.contains("Save Record tries again"));
        assert!(app.w.recovery.as_ref().unwrap().shows());

        // The window's records reload with the other user's change.
        let session = app.w.session.as_mut().unwrap();
        let key = session.refresh().unwrap();
        session.accept_page(key, Ok(host_copy("Theirs"))).unwrap();
        app.retain_drafts(&ctx);
        let draft = app
            .w
            .document
            .value(app.w.session.as_ref().unwrap(), "Name");
        assert_eq!(draft.as_deref(), Some("Mine"));
        assert_eq!(app.w.retained[&(1, 1)].as_ref().unwrap()["Name"], "Ada");
        assert_eq!(checked(&app).basis.as_ref().unwrap()["Name"], "Ada");

        save_refused(&mut app, &ctx, 1, changed_on_host());
        app.rechecked(&ctx, vec![((1, 1), Ok(host_copy("Theirs")))], Vec::new());
        assert_eq!(checked(&app).hold, Some(Hold::Changed));
    }

    /// A save refused while a script runs stays the script's: an alert, no
    /// hold.
    #[test]
    fn a_refusal_while_a_script_runs_stays_an_alert() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows.clear();
        let session = app.w.session.as_ref().unwrap();
        app.w
            .document
            .set_field(session, "Name", "Mine".into())
            .unwrap();
        app.script.running = true;
        save_refused(&mut app, &ctx, 1, changed_on_host());
        app.script.running = false;
        assert_eq!(
            app.error.as_deref(),
            Some("Another client changed this record.")
        );
        assert!(app.w.recovery.is_none());
        assert_eq!(app.w.document.unsaved(), 1, "the edit stays");
    }

    /// Two windows edit the same record (layout 1, ID 1): settling or
    /// giving up one window's edit leaves the other's edit, its baseline
    /// and its hold as they were.
    #[test]
    fn one_windows_choice_leaves_another_windows_edit_of_the_record() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        for (id, text) in [(1, "front"), (2, "")] {
            app.with_window(id, |app| {
                let session = app.w.session.as_ref().unwrap();
                app.w
                    .document
                    .set_field(session, "Name", text.into())
                    .unwrap();
                app.retain_drafts(&ctx);
            });
        }
        for id in [1, 2] {
            save_refused(&mut app, &ctx, id, changed_on_host());
            app.with_window(id, |app| {
                app.rechecked(&ctx, vec![((1, 1), Ok(host_copy("Theirs")))], Vec::new());
                assert_eq!(checked(app).hold, Some(Hold::Changed), "window {id}");
            });
        }
        app.with_window(2, |app| app.apply(&ctx, Action::DiscardRecoveredEdits));
        let behind = &app.windows[0];
        assert!(behind.recovery.is_none() && behind.retained.is_empty());
        assert_eq!(behind.document.unsaved(), 0);
        assert_eq!(checked(&app).hold, Some(Hold::Changed), "still held");
        assert!(app.w.retained.contains_key(&(1, 1)), "still retained");
        assert_eq!(app.w.document.unsaved(), 1);
        // The front window keeps and saves its own edit; the other has none.
        app.apply(&ctx, Action::KeepRecoveredEdits);
        let epoch = app.epoch;
        app.w.document.pending = true;
        app.on_response(
            &ctx,
            crate::worker::Response {
                epoch,
                window: 1,
                reply: crate::worker::Reply::Updated {
                    layout_id: 1,
                    record_id: 1,
                    result: Ok(host_copy("front").records.remove(0)),
                },
            },
        );
        assert!(app.w.recovery.is_none() && app.w.retained.is_empty());
        assert_eq!(app.w.document.unsaved(), 0);
    }

    /// The fresh layout list reaches every window of the reconnected file:
    /// one keeps its renamed layout, the other's layout is gone, so it
    /// shows the first one and holds the edit made on the one gone.
    #[test]
    fn every_window_of_the_file_takes_the_fresh_layout_list() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        let listed = |names: &[(u32, &str)]| -> Vec<LayoutInfo> {
            names
                .iter()
                .map(|(id, name)| LayoutInfo {
                    id: *id,
                    name: (*name).into(),
                })
                .collect()
        };
        let before = listed(&[(1, "Tasks"), (2, "Notes")]);
        app.w.session.as_mut().unwrap().layouts = before.clone();
        {
            let session = app.windows[0].session.as_mut().unwrap();
            session.layouts = before;
            let key = session.select_layout(2);
            let mut notes = page(&[("Name", "Bob")]);
            notes.records[0].id = 1;
            session.accept_page(key, Ok(notes)).unwrap();
        }
        app.with_window(2, |app| {
            let session = app.w.session.as_ref().unwrap();
            app.w
                .document
                .set_field(session, "Name", "draft".into())
                .unwrap();
        });
        app.lose_connection(&DataError::connection_lost("gone"));
        app.apply(&ctx, Action::Reconnect);
        app.send_reconnect(&ctx);
        let mut bob = page(&[("Name", "Bob")]);
        bob.records[0].id = 1;
        let own = app.w.file_id;
        app.reconnected(
            &ctx,
            Ok(Reconnected {
                host_version: None,
                records: vec![((2, 2, 1), Ok(bob))],
                positions: vec![(1, Ok(Some(0))), (2, Ok(Some(0)))],
                recovered: vec![(own, None)],
                related: Vec::new(),
                unsent: Vec::new(),
                recreated: Vec::new(),
                carried: Vec::new(),
            }),
        );
        assert!(
            app.w.reconnected && app.windows[0].reconnected,
            "both wait for the list"
        );
        let fresh = listed(&[(1, "Tasks renamed"), (3, "Added")]);
        let epoch = app.epoch;
        app.on_response(
            &ctx,
            crate::worker::Response {
                epoch,
                window: 1,
                reply: crate::worker::Reply::Layouts(Ok((fresh.clone(), Vec::new()))),
            },
        );
        for doc in std::iter::once(&app.w).chain(&app.windows) {
            let session = doc.session.as_ref().unwrap();
            assert_eq!(session.layouts, fresh, "window {}", doc.id);
            assert_eq!(session.layout, Some(1), "window {}", doc.id);
            assert!(session.pending.is_some(), "window {} reloads", doc.id);
            assert!(!doc.reconnected);
        }
        let behind = &app.windows[0];
        assert_eq!(behind.document.unsaved(), 1, "the edit is kept");
        let recovery = behind.recovery.as_ref().unwrap();
        assert_eq!(recovery.records[&(2, 1)].hold, Some(Hold::LayoutGone));
        assert!(
            recovery.notes.iter().any(|n| n.contains("“Notes”")),
            "{recovery:?}"
        );
        assert!(app.w.recovery.is_none(), "{:?}", app.w.recovery);
    }
}

#[cfg(test)]
mod focus_retention_tests {
    use crate::worker::{Request, Worker};
    use om_data::runtime::TriggerEvent;

    /// Script-owned record drafts keep their own engine baseline; when a
    /// UI draft moves into that engine, its existing accepted pin survives.
    #[test]
    fn worker_owned_drafts_do_not_create_or_release_ui_pins() {
        let ctx = egui::Context::default();
        let mut app = crate::app::tests::two_record_windows();
        app.windows.clear();
        let (worker, jobs) = Worker::capture_jobs();
        app.worker = worker;
        let session = app.w.session.as_mut().unwrap();
        session.page.as_mut().unwrap().data.versions.insert(1, 7);
        app.w
            .document
            .put_draft(1, 1, "Name", "Ada".into(), "Worker".into());
        let open = crate::scripting::Uncommitted {
            new: true,
            ..Default::default()
        };
        app.w.uncommitted = Some((1, 1, open.clone()));
        app.retain_drafts(&ctx);
        assert!(
            jobs.try_recv().is_err(),
            "script-owned draft needs no UI pin"
        );
        assert!(app.w.retained.is_empty());

        // A later real UI edit must acquire its own accepted-page pin,
        // even though the record still has worker-owned changes.
        app.w
            .document
            .set_field(app.w.session.as_ref().unwrap(), "Name", "Mine".into())
            .unwrap();
        app.retain_drafts(&ctx);
        assert!(matches!(
            jobs.try_recv().unwrap().request,
            Request::Retain(ref rows) if rows == &vec![(1, 1, Some(7))]
        ));
        app.w.uncommitted = Some((1, 1, open));
        app.w.document.clear_record(1, 1);
        app.retain_drafts(&ctx);
        assert!(
            jobs.try_recv().is_err(),
            "the UI pin follows its draft into the worker"
        );
        assert!(app.w.retained.contains_key(&(1, 1)));
        app.w.uncommitted = None;
        app.retain_drafts(&ctx);
        assert!(matches!(
            jobs.try_recv().unwrap().request,
            Request::ReleaseRecords(_)
        ));
        assert!(app.w.retained.is_empty());
    }

    /// A newly typed draft can leave/enter focus before the frame's later
    /// actions run. A worker can execute its first queued job immediately:
    /// no later UI request may be needed to make that trigger's save safe.
    #[test]
    fn first_edit_is_retained_before_a_focus_trigger_can_save_it() {
        for event in [
            TriggerEvent::ObjectValidate,
            TriggerEvent::ObjectSave,
            TriggerEvent::ObjectExit,
            TriggerEvent::ObjectEnter,
        ] {
            let ctx = egui::Context::default();
            let mut app = crate::app::tests::two_record_windows();
            app.windows.clear();
            let (worker, jobs) = Worker::capture_jobs();
            app.worker = worker;
            let session = app.w.session.as_mut().unwrap();
            session.page.as_mut().unwrap().data.versions.insert(1, 7);
            session.render = crate::session::RenderState::Loaded(Box::new(om_data::RenderLayout {
                layout: om_layout::Layout {
                    objects: om_layout::ObjectStore {
                        root: vec![20],
                        objects: vec![om_layout::LayoutObject {
                            id: 20,
                            field_obj: Some(om_layout::FieldObject {
                                field_spec: Some(om_layout::FieldSpec {
                                    table_key: 0,
                                    field_key: 1,
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }),
                            triggers: vec![om_layout::ScriptTrigger {
                                event: event.id(),
                                script: Some(1),
                                file_reference: None,
                                external: false,
                                modes: 1,
                                option: 0,
                                extra: Vec::new(),
                                parameter: None,
                            }],
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                field_bindings: [(20, "Name".into())].into(),
            }));
            app.w
                .document
                .set_field(session, "Name", "Mine".into())
                .unwrap();
            assert!(app.w.retained.is_empty(), "the very first edit: {event:?}");
            let entering = event == TriggerEvent::ObjectEnter;
            app.focus_changed(
                &ctx,
                (!entering).then(|| ("Name".into(), None)),
                entering.then(|| ("Name".into(), None)),
                true,
                None,
            );
            let pin = jobs.try_recv().expect("retention must be queued first");
            assert_eq!(pin.window, 1);
            assert!(
                matches!(pin.request, Request::Retain(ref rows) if rows == &vec![(1, 1, Some(7))]),
                "first request for {event:?}: {:?}",
                pin.request
            );
            let job = jobs
                .try_recv()
                .expect("focus trigger must follow retention");
            let Request::Script(job) = job.request else {
                panic!("not a trigger script: {event:?}")
            };
            assert_eq!(job.window.drafts.get("Name"), Some(&Some("Mine".into())));
            assert!(jobs.try_recv().is_err(), "no later pin needed: {event:?}");
        }
    }
}
