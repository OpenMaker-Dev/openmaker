//! The dialogs of Export Records, Save Records as Excel and Import Records
//! shown "With dialog" (`crate::scripting::host` asks for them): the output
//! file with its type, character set and export order; the source's field
//! mapping with the import action and options; the Import Summary.

use egui::{RichText, ScrollArea};
use om_transfer::{ExportCharset, FileType, ImportAction, ImportCharset};

use crate::scripting::{
    Answer, ContainerFileDialog, ExportDialog, ImportDialog, ImportSourceDialog,
};
use crate::{theme, widgets};

/// A file type as FileMaker Pro's Type menu names it.
pub(super) fn type_name(t: FileType) -> &'static str {
    t.name()
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(egui::FontId::new(theme::BODY, theme::semibold()))
            .color(theme::TEXT),
    );
}

/// The export dialog; the answer once the user chose.
pub(super) fn export(
    ui: &mut egui::Ui,
    d: &mut ExportDialog,
    pick: &mut usize,
    order_pick: &mut usize,
) -> Option<Answer> {
    let excel = d.types == [FileType::ExcelExport];
    heading(
        ui,
        if excel {
            "Save Records as Excel"
        } else {
            "Export Records"
        },
    );
    ui.add_space(6.0);
    egui::Grid::new("export_file")
        .num_columns(2)
        .show(ui, |ui| {
            let path_label = ui.label("Save as:");
            let width = ui.available_width().max(200.0);
            ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(width))
                .labelled_by(path_label.id);
            ui.end_row();
            let type_label = ui.label("Type:");
            egui::ComboBox::from_id_salt("export_type")
                .selected_text(type_name(d.file_type))
                .show_ui(ui, |ui| {
                    for t in d.types.clone() {
                        if ui
                            .selectable_value(&mut d.file_type, t, type_name(t))
                            .changed()
                        {
                            // Keep the name, change the extension.
                            if let Some((stem, _)) = d.path.rsplit_once('.') {
                                d.path = format!("{stem}.{}", t.extension());
                            }
                        }
                    }
                })
                .response
                .labelled_by(type_label.id);
            ui.end_row();
            let charset_label = ui.label("Output file character set:");
            egui::ComboBox::from_id_salt("export_charset")
                .selected_text(d.charset.name())
                .show_ui(ui, |ui| {
                    for c in ExportCharset::ALL {
                        ui.selectable_value(&mut d.charset, c, c.name());
                    }
                })
                .response
                .labelled_by(charset_label.id);
            ui.end_row();
        });
    ui.add_enabled(
        false,
        egui::Checkbox::new(
            &mut d.formatting,
            "Apply current layout's data formatting to exported data",
        ),
    )
    .on_disabled_hover_text("Layout data formatting is not supported yet.");
    if d.file_type == FileType::XmlExport {
        ui.weak("FMPXMLRESULT XML; XSLT is not supported.");
    }
    if let Some(current) = &mut d.current_record {
        ui.radio_value(current, false, "Records being browsed");
        ui.radio_value(current, true, "Current record");
    }
    ui.add_space(6.0);
    ui.columns(2, |cols| {
        cols[0].label(RichText::new("Fields").font(theme::small()));
        ScrollArea::vertical()
            .id_salt("export_available")
            .max_height(180.0)
            .show(&mut cols[0], |ui| {
                for (i, name) in d.available.iter().enumerate() {
                    let r = ui.selectable_label(*pick == i, name);
                    if r.clicked() {
                        *pick = i;
                    }
                    if r.double_clicked() && !d.chosen.contains(&i) {
                        d.chosen.push(i);
                    }
                }
            });
        cols[1].label(RichText::new("Fields to export").font(theme::small()));
        ScrollArea::vertical()
            .id_salt("export_order")
            .max_height(180.0)
            .show(&mut cols[1], |ui| {
                for (n, i) in d.chosen.iter().enumerate() {
                    let name = d.available.get(*i).map_or("", String::as_str);
                    if ui.selectable_label(*order_pick == n, name).clicked() {
                        *order_pick = n;
                    }
                }
            });
    });
    ui.horizontal(|ui| {
        if ui.button("Move").clicked() && *pick < d.available.len() && !d.chosen.contains(pick) {
            d.chosen.push(*pick);
        }
        if ui.button("Move All").clicked() {
            d.chosen = (0..d.available.len()).collect();
        }
        if ui.button("Clear").clicked() && *order_pick < d.chosen.len() {
            d.chosen.remove(*order_pick);
        }
        if ui.button("Up").clicked() && *order_pick > 0 && *order_pick < d.chosen.len() {
            d.chosen.swap(*order_pick, *order_pick - 1);
            *order_pick -= 1;
        }
        if ui.button("Down").clicked() && *order_pick + 1 < d.chosen.len() {
            d.chosen.swap(*order_pick, *order_pick + 1);
            *order_pick += 1;
        }
    });
    ui.add_space(12.0);
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "Cancel", false, true).clicked() || escape {
            answer = Some(Answer::Export(None));
        }
        let ok = !d.chosen.is_empty() && !d.path.trim().is_empty();
        let label = if excel { "Save" } else { "Export" };
        if widgets::dialog_button(ui, label, true, ok).clicked() {
            answer = Some(Answer::Export(Some(d.clone())));
        }
    });
    answer
}

/// Source options precede parsing, so the explicit format and encoding
/// reach the same reader used by stored script profiles.
pub(super) fn import_source(ui: &mut egui::Ui, d: &mut ImportSourceDialog) -> Option<Answer> {
    heading(ui, "Import Records from File");
    ui.label(format!("Target: {}", d.target));
    ui.add_space(6.0);
    egui::Grid::new("import_source")
        .num_columns(2)
        .show(ui, |ui| {
            let label = ui.label("Source file:");
            ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(330.0))
                .labelled_by(label.id);
            ui.end_row();
            let label = ui.label("File type:");
            egui::ComboBox::from_id_salt("import_type")
                .selected_text(type_name(d.file_type))
                .show_ui(ui, |ui| {
                    for t in ImportSourceDialog::TYPES {
                        ui.selectable_value(&mut d.file_type, t, type_name(t));
                    }
                })
                .response
                .labelled_by(label.id);
            ui.end_row();
            if d.file_type == FileType::Excel2007 {
                let label = ui.label("Worksheet number:");
                ui.add(egui::DragValue::new(&mut d.sheet).range(1..=u32::MAX))
                    .labelled_by(label.id);
                ui.end_row();
            } else {
                let label = ui.label("Source character set:");
                egui::ComboBox::from_id_salt("import_charset")
                    .selected_text(d.charset.name())
                    .show_ui(ui, |ui| {
                        for c in ImportCharset::ALL {
                            ui.selectable_value(&mut d.charset, c, c.name());
                        }
                    })
                    .response
                    .labelled_by(label.id);
                ui.end_row();
            }
        });
    ui.weak("Enter an absolute path, or a path relative to your Documents folder.");
    if d.file_type == FileType::Excel2007 {
        ui.weak("Worksheets are numbered from 1. Named ranges are not supported.");
    }
    ui.add_space(12.0);
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "Cancel", false, true).clicked() || escape {
            answer = Some(Answer::ImportSource(None));
        }
        if widgets::dialog_button(ui, "Continue", true, !d.path.trim().is_empty()).clicked() {
            answer = Some(Answer::ImportSource(Some(d.clone())));
        }
    });
    answer
}

/// The import mapping dialog.
pub(super) fn import(ui: &mut egui::Ui, d: &mut ImportDialog) -> Option<Answer> {
    heading(ui, "Specify Import Order");
    ui.label(format!("Target: {}", d.target));
    ui.label(RichText::new(&d.path).font(theme::small()));
    ui.add_space(6.0);
    ScrollArea::vertical()
        .id_salt("import_map")
        .max_height(220.0)
        .show(ui, |ui| {
            egui::Grid::new("import_map_grid")
                .num_columns(3)
                .striped(true)
                .show(ui, |ui| {
                    ui.label(RichText::new("Source fields").font(theme::small()));
                    ui.label(RichText::new("Target fields").font(theme::small()));
                    ui.label(RichText::new("Mapping").font(theme::small()));
                    ui.end_row();
                    for (i, column) in d.columns.iter().enumerate() {
                        ui.label(if column.is_empty() {
                            format!("({})", i + 1)
                        } else {
                            column.replace('\r', " ")
                        });
                        let target = d.map.get(i).copied().flatten();
                        let text = target
                            .and_then(|t| d.targets.get(t))
                            .map_or("Don't import", String::as_str);
                        egui::ComboBox::from_id_salt(("import_target", i))
                            .selected_text(text)
                            .show_ui(ui, |ui| {
                                let mut choice = target;
                                ui.selectable_value(&mut choice, None, "Don't import");
                                for (t, name) in d.targets.iter().enumerate() {
                                    ui.selectable_value(&mut choice, Some(t), name);
                                }
                                if choice != target {
                                    if d.map.len() <= i {
                                        d.map.resize(i + 1, None);
                                    }
                                    d.map[i] = choice;
                                }
                            })
                            .response
                            .widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::Role::ComboBox,
                                    true,
                                    format!("Target field for source column {}", i + 1),
                                )
                            });
                        d.match_fields.resize(d.columns.len(), false);
                        ui.add_enabled(
                            d.action == ImportAction::UpdateMatching && d.map[i].is_some(),
                            egui::Checkbox::new(&mut d.match_fields[i], "Match key"),
                        );
                        ui.end_row();
                    }
                });
        });
    ui.add_space(6.0);
    ui.label("Import action");
    for (action, text) in [
        (ImportAction::Add, "Add new records"),
        (
            ImportAction::UpdateFoundSet,
            "Update existing records in found set",
        ),
        (
            ImportAction::UpdateMatching,
            "Update matching records in found set",
        ),
    ] {
        ui.radio_value(&mut d.action, action, text);
    }
    if d.action != ImportAction::Add {
        ui.checkbox(&mut d.add_remaining, "Add remaining data as new records");
    }
    if d.action == ImportAction::UpdateMatching {
        ui.weak("Choose at least one match key. Other mapped columns are imported. Only found records are updated.");
    }
    ui.checkbox(
        &mut d.names_row,
        "Don't import first record (contains field names)",
    );
    ui.checkbox(
        &mut d.auto_enter,
        "Perform auto-enter options while importing",
    );
    ui.add_space(12.0);
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "Cancel", false, true).clicked() || escape {
            answer = Some(Answer::Import(None));
        }
        let matching = d.action == ImportAction::UpdateMatching;
        let key = |i: usize| d.match_fields.get(i) == Some(&true);
        let ok = d.map.iter().any(Option::is_some)
            && (!matching || d.map.iter().enumerate().any(|(i, t)| t.is_some() && key(i)));
        if widgets::dialog_button(ui, "Import", true, ok).clicked() {
            answer = Some(Answer::Import(Some(d.clone())));
        }
    });
    answer
}

/// The Import Summary.
pub(super) fn summary(
    ui: &mut egui::Ui,
    total: usize,
    added: usize,
    updated: usize,
    skipped: usize,
) -> Option<Answer> {
    heading(ui, "Import Summary");
    ui.add_space(6.0);
    egui::Grid::new("import_summary")
        .num_columns(2)
        .show(ui, |ui| {
            for (label, n) in [
                ("Total records added / updated", added + updated),
                ("Records skipped due to errors", skipped),
                ("Total records in source", total),
            ] {
                ui.label(label);
                ui.label(n.to_string());
                ui.end_row();
            }
        });
    ui.add_space(12.0);
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "OK", true, true).clicked() || enter {
            answer = Some(Answer::Confirm(true));
        }
    });
    answer
}

/// The file an Insert step reads or Export Field Contents writes; nothing
/// is read or written here. The worker checks the field again.
pub(super) fn container_file(ui: &mut egui::Ui, d: &mut ContainerFileDialog) -> Option<Answer> {
    heading(ui, &d.title);
    ui.add_space(6.0);
    if !d.field.is_empty() {
        ui.label(format!(
            "{} {}",
            if d.export { "From field" } else { "Into field" },
            d.field
        ));
        ui.add_space(4.0);
    }
    egui::Grid::new("container_file")
        .num_columns(2)
        .show(ui, |ui| {
            let label = ui.label(if d.export { "Save as:" } else { "File:" });
            let width = ui.available_width().max(200.0);
            ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(width))
                .labelled_by(label.id);
            ui.end_row();
        });
    ui.weak("Enter an absolute path, or a path relative to your Documents folder.");
    ui.add_space(12.0);
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let ok = !d.path.trim().is_empty();
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "Cancel", false, true).clicked() || escape {
            answer = Some(Answer::ContainerFile(None));
        }
        let label = if d.export { "Save" } else { "Insert" };
        if widgets::dialog_button(ui, label, true, ok).clicked() && answer.is_none() {
            answer = Some(Answer::ContainerFile(Some(d.clone())));
        }
    });
    answer
}

/// The PDF's file, chosen as in a save panel, and its records; no database
/// edits are made here.
pub(super) fn pdf(ui: &mut egui::Ui, d: &mut crate::scripting::PdfDialog) -> Option<Answer> {
    heading(ui, "Save Records as PDF");
    ui.add_space(8.0);
    let enter = d.chooser.show(ui);
    ui.add_space(6.0);
    ui.radio_value(&mut d.current_record, false, "Records being browsed");
    ui.radio_value(&mut d.current_record, true, "Current record");
    ui.add_space(12.0);
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let chosen = d.chooser.chosen();
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "Cancel", false, true).clicked() || escape {
            answer = Some(Answer::Pdf(None));
        }
        let save = widgets::dialog_button(ui, "Save", true, chosen.is_some()).clicked();
        if let (true, Some(path)) = (save || enter, &chosen) {
            d.path = path.display().to_string();
            answer = Some(Answer::Pdf(Some(d.clone())));
        }
    });
    answer
}
