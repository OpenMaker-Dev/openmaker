//! The functions pane of Specify Calculation (42): every function of the
//! catalog with its signature, the Get functions, the file's custom
//! functions, a search field, the view menu (all functions by name, by
//! type, or one category) and the Description panel.
//!
//! The list is `om_data::formula_editor::functions`, which name
//! completion offers too: names, signatures, categories and descriptions
//! from the catalogs `calc-functions.csv` (DBEngine string resources
//! 130-156), `get-selectors.csv` (resource 131) and the category names of
//! resource 135 in `enumerations.csv`.

use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Role, Sense, Stroke, Ui, WidgetInfo};
use om_data::formula_editor::functions::CUSTOM;
#[cfg(test)]
pub(super) use om_data::formula_editor::functions::{catalog, custom_functions};
pub(super) use om_data::formula_editor::functions::{categories, listed, Entry};

use super::syntax::Insertion;
use super::{bar_chart, help_glyph, BODY_TEXT, LIST_FONT};
use crate::{theme, widgets};

/// The function's page in the Claris help (the `doc_url` of
/// calc-function-spec.json: the lowercased name, `get-<selector>` for
/// Get functions, a few pages named otherwise, and none for the
/// undocumented functions).
pub(super) fn help_url(entry: &Entry) -> Option<String> {
    const NAMED: [(&str, &str); 5] = [
        ("If", "if-function"),
        ("SetRecursion", "set-recursion"),
        ("ComputeModel", "compute-model"),
        ("ConvertFromFileMakerPath", "convert-from-filemaker-path"),
        ("ConvertToFileMakerPath", "convert-to-filemaker-path"),
    ];
    const UNDOCUMENTED: [&str; 6] = [
        "JSONUniqueValues",
        "JSONRemoveValues",
        "JSONValueExists",
        "JSONValueCount",
        "JSONAnyValuesExist",
        "IndexItems",
    ];
    let page = if entry.category == CUSTOM {
        return None;
    } else if entry.is_get() {
        let selector = entry.selector().unwrap_or_default();
        format!("get-{}", selector.to_lowercase())
    } else if UNDOCUMENTED.contains(&entry.name.as_str()) {
        return None;
    } else {
        NAMED
            .iter()
            .find(|(name, _)| *name == entry.name)
            .map_or_else(|| entry.name.to_lowercase(), |(_, page)| (*page).to_owned())
    };
    Some(format!(
        "https://help.claris.com/en/pro-help/content/{page}.html"
    ))
}

/// What the list shows (the view menu).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum View {
    ByName,
    /// Grouped under the category names.
    ByType,
    /// One category of resource 135.
    Category(usize),
}

/// A row of the list: a category heading (by type) or a function.
#[derive(Clone, Copy)]
enum Row<'a> {
    Heading(&'a str),
    Function(&'a Entry),
}

/// Row height of the list (42: 20 pt apart).
const ROW: f32 = 20.0;
/// The Description panel's heading and body (42).
const HEADING: f32 = 18.0;
const DESCRIPTION: f32 = 90.0;

pub(super) struct FunctionsPane {
    pub search: String,
    pub view: View,
    /// The signature of the selected function.
    pub selected: Option<String>,
}

impl Default for FunctionsPane {
    fn default() -> Self {
        Self {
            search: String::new(),
            view: View::ByName,
            selected: None,
        }
    }
}

impl FunctionsPane {
    /// Draw the pane in `rect`; a double-clicked function to insert.
    pub fn ui(&mut self, ui: &mut Ui, rect: Rect, all: &[Entry]) -> Option<Insertion> {
        let mut insert = None;
        // Search and the view menu (42: the field 4 pt in, the menu's
        // bars 13 pt from the edge).
        let search = Rect::from_min_max(
            pos2(rect.left() + 4.0, rect.top() + 8.0),
            pos2(rect.right() - 24.0, rect.top() + 26.0),
        );
        super::search_box(ui, search, &mut self.search, "Search Functions");
        let view_button = Rect::from_center_size(
            pos2(rect.right() - 12.5, search.center().y - 0.5),
            vec2(18.0, 18.0),
        );
        self.view_menu(ui, view_button, all);
        let query = self.search.trim().to_lowercase();
        let matching = |e: &&Entry| query.is_empty() || e.key.contains(&query);
        let mut rows: Vec<Row<'_>> = Vec::new();
        match self.view {
            View::ByName => rows.extend(all.iter().filter(matching).map(Row::Function)),
            View::ByType => {
                for (category, name) in categories().iter().enumerate().skip(2) {
                    let listed: Vec<&Entry> = all
                        .iter()
                        .filter(|e| e.category == category)
                        .filter(matching)
                        .collect();
                    if !listed.is_empty() {
                        rows.push(Row::Heading(name));
                        rows.extend(listed.into_iter().map(Row::Function));
                    }
                }
            }
            View::Category(category) => rows.extend(
                all.iter()
                    .filter(|e| e.category == category)
                    .filter(matching)
                    .map(Row::Function),
            ),
        }
        let list = Rect::from_min_max(
            pos2(rect.left(), search.bottom() + 16.0),
            pos2(rect.right(), rect.bottom() - DESCRIPTION),
        );
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list));
        child.set_clip_rect(list.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .id_salt("calculation_functions")
            .auto_shrink([false, false])
            .show_rows(&mut child, ROW, rows.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for row in &rows[range] {
                    match row {
                        Row::Heading(name) => heading_row(ui, name),
                        Row::Function(entry) => {
                            if let Some(i) = self.function_row(ui, entry) {
                                insert = Some(i);
                            }
                        }
                    }
                }
            });
        let selected = self
            .selected
            .as_ref()
            .and_then(|s| all.iter().find(|e| &e.signature == s));
        description(
            ui,
            Rect::from_min_max(pos2(rect.left(), rect.bottom() - DESCRIPTION), rect.max),
            selected,
        );
        insert
    }

    fn function_row(&mut self, ui: &mut Ui, entry: &Entry) -> Option<Insertion> {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
        let selected = self.selected.as_deref() == Some(entry.signature.as_str());
        if selected {
            ui.painter().rect_filled(
                Rect::from_min_max(
                    pos2(rect.left() + 8.0, rect.top()),
                    pos2(rect.right() - 4.0, rect.bottom()),
                ),
                4.0,
                theme::SELECTED,
            );
        }
        ui.painter().text(
            pos2(rect.left() + 16.0, rect.center().y),
            Align2::LEFT_CENTER,
            &entry.signature,
            FontId::proportional(LIST_FONT),
            if selected {
                Color32::WHITE
            } else {
                theme::TEXT
            },
        );
        response
            .widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, &entry.signature));
        if response.clicked() || response.double_clicked() {
            self.selected = Some(entry.signature.clone());
        }
        response.double_clicked().then(|| entry.insertion())
    }

    /// The bars button: All Functions by Name or by Type, or a category.
    fn view_menu(&mut self, ui: &mut Ui, rect: Rect, all: &[Entry]) {
        let response = ui.interact(rect, ui.scope_id().with("function_view"), Sense::click());
        bar_chart(ui.painter(), rect, super::BARS);
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Function View"));
        egui::Popup::menu(&response)
            .style(theme::menu_popup_style)
            .show(|ui| {
                let mut choice = |ui: &mut Ui, label: &str, view: View| {
                    if widgets::menu_item(ui, label, None, self.view == view, true).clicked() {
                        self.view = view;
                    }
                };
                choice(ui, "All Functions by Name", View::ByName);
                choice(ui, "All Functions by Type", View::ByType);
                widgets::menu_separator(ui);
                for (category, name) in categories().iter().enumerate().skip(2) {
                    if all.iter().any(|e| e.category == category) {
                        choice(ui, name, View::Category(category));
                    }
                }
            });
    }
}

/// A category name over its functions, when listed by type.
fn heading_row(ui: &mut Ui, name: &str) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::new(LIST_FONT, theme::semibold()),
        theme::CAPTION,
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
}

/// The Description panel (42): the selected function's description, with
/// its help page behind the "?".
fn description(ui: &mut Ui, rect: Rect, entry: Option<&Entry>) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme::CONTENT_BG);
    let rule = Stroke::new(1.0, super::RULE);
    painter.hline(rect.x_range(), rect.top() + 0.5, rule);
    let heading = Rect::from_min_size(rect.min + vec2(0.0, 1.0), vec2(rect.width(), HEADING));
    painter.hline(rect.x_range(), heading.bottom() + 0.5, rule);
    widgets::put_label(
        ui,
        Rect::from_min_max(
            pos2(heading.left() + 6.0, heading.top()),
            pos2(heading.right() - 24.0, heading.bottom()),
        ),
        "Description",
        FontId::new(theme::SMALL, theme::semibold()),
        theme::TEXT,
        egui::Align::Min,
    );
    let url = entry.and_then(help_url);
    let help = Rect::from_center_size(
        pos2(heading.right() - 16.0, heading.center().y),
        vec2(14.0, 14.0),
    );
    let response = ui.interact(
        help,
        ui.scope_id().with("function_help"),
        if url.is_some() {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    help_glyph(
        ui.painter(),
        help,
        if url.is_some() {
            theme::ICON
        } else {
            theme::DISABLED
        },
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, url.is_some(), "Function Help"));
    if let (true, Some(url)) = (response.clicked(), url) {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
    let body = Rect::from_min_max(
        pos2(rect.left() + 8.0, heading.bottom() + 6.0),
        rect.max - vec2(8.0, 4.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body));
    match entry
        .map(|e| e.description.as_str())
        .filter(|d| !d.is_empty())
    {
        Some(text) => {
            child.add(egui::Label::new(
                egui::RichText::new(text).size(BODY_TEXT).color(theme::TEXT),
            ));
        }
        None => {
            child.vertical_centered(|ui| {
                ui.add_space(18.0);
                ui.label(
                    egui::RichText::new("No description")
                        .font(FontId::new(theme::SMALL, theme::semibold()))
                        .color(theme::DISABLED),
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::host::CalcSchema;

    #[test]
    fn the_catalog_lists_functions_alphabetically_as_filemaker_does() {
        // 42, from the top.
        let first: Vec<&str> = catalog()
            .iter()
            .take(12)
            .map(|e| e.signature.as_str())
            .collect();
        assert_eq!(
            first,
            [
                "Abs ( number )",
                "Acos ( number )",
                "AddEmbeddings ( v1 ; v2 )",
                "Asin ( number )",
                "Atan ( number )",
                "Average ( field {; field...} )",
                "Base64Decode ( text {; fileNameWithExtension } )",
                "Base64Encode ( data )",
                "Base64EncodeRFC ( RFCNumber ; data )",
                "BaseTableComment ( fileName ; baseTableName )",
                "BaseTableIDs ( fileName )",
                "BaseTableNames ( fileName )",
            ]
        );
        let get = catalog()
            .iter()
            .find(|e| e.signature == "Get ( ScriptParameter )")
            .unwrap();
        assert_eq!(categories()[get.category], "Get");
        assert!(!get.description.is_empty());
        assert!(!catalog().iter().any(|e| e.signature == "Get ( flag )"));
        let abs = &catalog()[0];
        assert_eq!(categories()[abs.category], "Number");
        assert_eq!(abs.description, "Returns the absolute value of a number.");
    }

    #[test]
    fn help_pages_are_the_function_specs_doc_urls() {
        let spec = include_str!("../../../../../docs/re/catalogs/calc-function-spec.json");
        // The generated JSON has one `"name"` and one `"doc_url"` per entry
        // at this indent.
        let mut urls = std::collections::HashMap::new();
        let (mut name, mut constant) = (None, false);
        for line in spec.lines() {
            if let Some(n) = line.strip_prefix("   \"name\": \"") {
                name = Some(n.trim_end_matches("\",").to_owned());
            } else if let Some(kind) = line.strip_prefix("   \"kind\": ") {
                constant = kind.starts_with("\"constant\"");
            } else if let Some(url) = line.strip_prefix("   \"doc_url\": ") {
                let url = url.trim_end_matches(',');
                let url = (url != "null").then(|| url.trim_matches('"').to_owned());
                let name = name.take().unwrap();
                if !constant {
                    urls.insert(name, url);
                }
            }
        }
        let mut checked = 0;
        for entry in catalog() {
            let key = if entry.is_get() {
                entry.name.replace(' ', "")
            } else {
                entry.name.clone()
            };
            if let Some(url) = urls.get(&key) {
                assert_eq!(&help_url(entry), url, "{}", entry.name);
                checked += 1;
            }
        }
        assert!(checked > 350, "{checked}");
    }

    #[test]
    fn custom_functions_list_their_parameters() {
        let mut schema = CalcSchema::new("f", om_data::calc::Locale::en_us());
        let body = om_data::formula::Expr::Empty;
        schema.add_custom_function(
            1,
            om_data::calc::eval::CustomFunction {
                name: "Initials".into(),
                parameters: vec!["first".into(), "last".into()],
                body: body.clone(),
            },
        );
        schema.add_custom_function(
            2,
            om_data::calc::eval::CustomFunction {
                name: "Today2".into(),
                parameters: vec![],
                body,
            },
        );
        let entries = custom_functions(Some(&schema));
        let signatures: Vec<&str> = entries.iter().map(|e| e.signature.as_str()).collect();
        assert_eq!(signatures, ["Initials ( first ; last )", "Today2"]);
        assert_eq!(categories()[entries[0].category], "Custom");
        let insertion = entries[0].insertion();
        assert_eq!(insertion.select, 11..16);
    }
}
