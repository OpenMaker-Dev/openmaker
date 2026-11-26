//! Web Viewer Setup for a web viewer drawn with the Web Viewer tool (#896):
//! a custom web address, typed as a calculation, and the dialog's six
//! options, in the order and with the titles FileMaker Pro 26.1.1 gave them. OK compiles the
//! address against the file's names (`layout_placement::compile_address`)
//! and keeps the viewer's options when FileMaker Pro was seen to store
//! that combination (`WebViewerSetup::flags`). Save Layout then places the
//! viewer with the operation `om-author`'s `place_web_viewer` uses.
//!
//! FileMaker Pro's dialog also lists website templates and has a Specify
//! button for Specify Calculation; neither is offered here, and the
//! dialog's look is OpenMaker's.

use egui::{vec2, Rect, UiBuilder};
use om_data::layout_placement::WebViewerSetup;

use crate::widgets;

/// What Web Viewer Setup set for a new web viewer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WebViewerChoice {
    /// The address as typed.
    pub address: String,
    /// Its formatted and Polish streams.
    pub formatted: Vec<u8>,
    pub polish: Vec<u8>,
    pub setup: WebViewerSetup,
    /// `externalObj.flags` for `setup`.
    pub flags: u32,
}

/// What the user did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum WebViewerSetupAnswer {
    Open,
    Cancel,
    Done(WebViewerChoice),
}

/// The dialog's state.
pub(crate) struct WebViewerSetupDialog {
    address: String,
    setup: WebViewerSetup,
    error: Option<String>,
    schema: Option<std::sync::Arc<om_data::host::CalcSchema>>,
    occurrence: Option<u64>,
}

/// The options, in the order and with the titles FileMaker Pro 26.1.1's
/// dialog gave them (#896's run).
const OPTIONS: [&str; 6] = [
    "Allow interaction with web viewer content",
    "Display content in Find mode",
    "Display progress bar",
    "Display status messages",
    "Automatically encode URL",
    "Allow JavaScript to perform FileMaker scripts",
];

impl WebViewerSetupDialog {
    /// Over a new viewer, with what was set before (`choice`) or FileMaker
    /// Pro's defaults; `schema` and `occurrence` compile the address.
    pub(crate) fn new(
        choice: Option<&WebViewerChoice>,
        schema: Option<std::sync::Arc<om_data::host::CalcSchema>>,
        occurrence: Option<u64>,
    ) -> Self {
        Self {
            address: choice.map(|c| c.address.clone()).unwrap_or_default(),
            setup: choice.map_or(WebViewerSetup::DEFAULT, |c| c.setup),
            error: None,
            schema,
            occurrence,
        }
    }

    fn option(&mut self, index: usize) -> &mut bool {
        let setup = &mut self.setup;
        match index {
            0 => &mut setup.interaction,
            1 => &mut setup.find_mode,
            2 => &mut setup.progress_bar,
            3 => &mut setup.status_messages,
            4 => &mut setup.encode_url,
            _ => &mut setup.javascript,
        }
    }

    /// The choice OK keeps, or why it cannot.
    fn choose(&self) -> Result<WebViewerChoice, String> {
        let flags = self.setup.flags().ok_or_else(|| UNMEASURED.to_owned())?;
        if self.address.trim().is_empty() {
            return Err("Type the web address.".into());
        }
        let schema = self
            .schema
            .as_deref()
            .ok_or("The file's names have not loaded yet; try again when they have.")?;
        let (formatted, polish) =
            om_data::layout_placement::compile_address(&self.address, schema, self.occurrence)
                .map_err(|refusal| refusal.to_string())?;
        Ok(WebViewerChoice {
            address: self.address.clone(),
            formatted,
            polish,
            setup: self.setup,
            flags,
        })
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> WebViewerSetupAnswer {
        let mut answer = WebViewerSetupAnswer::Open;
        let measured = self.setup.flags().is_some();
        let escaped = crate::part_setup::dialog(
            ctx,
            "web_viewer_setup",
            "Web Viewer Setup",
            vec2(560.0, 330.0),
            |ui, at| {
                let place = |ui: &mut egui::Ui, rect: Rect, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.scope_builder(UiBuilder::new().max_rect(rect), add);
                };
                place(ui, at(20.0, 8.0, 180.0, 18.0), &mut |ui| {
                    ui.label("Choose a Website");
                });
                place(ui, at(20.0, 30.0, 180.0, 22.0), &mut |ui| {
                    let _ = ui.selectable_label(true, "Custom Web Address");
                });
                place(ui, at(216.0, 8.0, 324.0, 18.0), &mut |ui| {
                    ui.label("Web Address");
                });
                place(ui, at(216.0, 30.0, 324.0, 110.0), &mut |ui| {
                    // A long address scrolls inside its box rather than
                    // growing over the options.
                    egui::ScrollArea::vertical()
                        .id_salt("web_viewer_address")
                        .max_height(110.0)
                        .show(ui, |ui| {
                            let edit = ui.add_sized(
                                [324.0, 110.0],
                                egui::TextEdit::multiline(&mut self.address)
                                    .hint_text("\"https://\" & Website"),
                            );
                            widgets::accessible_label(ui, edit.id, "Web Address");
                            if edit.changed() {
                                self.error = None;
                            }
                        });
                });
                for (index, label) in OPTIONS.iter().enumerate() {
                    place(
                        ui,
                        at(20.0, 160.0 + 22.0 * index as f32, 420.0, 18.0),
                        &mut |ui| {
                            if ui.checkbox(self.option(index), *label).changed() {
                                self.error = None;
                            }
                        },
                    );
                }
                let note = self.error.as_deref().or((!measured).then_some(UNMEASURED));
                if let Some(note) = note {
                    place(ui, at(20.0, 296.0, 340.0, 30.0), &mut |ui| {
                        ui.label(
                            egui::RichText::new(note)
                                .font(crate::theme::small())
                                .color(crate::theme::WEAK_TEXT),
                        );
                    });
                }
                let right = egui::Layout::right_to_left(egui::Align::Center);
                place(ui, at(462.0, 300.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "OK", true, measured).clicked() {
                            match self.choose() {
                                Ok(choice) => answer = WebViewerSetupAnswer::Done(choice),
                                Err(error) => self.error = Some(error),
                            }
                        }
                    });
                });
                place(ui, at(380.0, 300.0, 79.0, 22.0), &mut |ui| {
                    ui.with_layout(right, |ui| {
                        if widgets::dialog_button(ui, "Cancel", false, true).clicked() {
                            answer = WebViewerSetupAnswer::Cancel;
                        }
                    });
                });
            },
        );
        if escaped && answer == WebViewerSetupAnswer::Open {
            answer = WebViewerSetupAnswer::Cancel;
        }
        answer
    }
}

/// Why OK waits for other options.
const UNMEASURED: &str = "OpenMaker does not save this combination of options yet: what \
                          FileMaker stores for it is not measured.";
