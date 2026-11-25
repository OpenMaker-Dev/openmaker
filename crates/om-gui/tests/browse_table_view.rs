//! Table view of a table whose columns are wider than the window (#1161):
//! they scroll sideways, by the wheel and to the cell the keyboard enters.
//! A synthetic table only.
mod common;

use egui::{vec2, Pos2, Role};
use egui_kittest::kittest::{By, Queryable};
use om_data::{
    Connection, DataError, DataProvider, Field, FieldKind, HostedFile, LayoutInfo, Record,
    RecordPage,
};

const COLUMNS: usize = 10;
const ROWS: i64 = 30;
/// `common::harness`'s window.
const WIDTH: f32 = 1060.0;
/// Over the table's rows.
const OVER_ROWS: Pos2 = Pos2::new(500.0, 400.0);

fn cell(row: i64, column: usize) -> String {
    format!("R{row} C{column}")
}

struct Provider;
impl DataProvider for Provider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![HostedFile {
            name: "Wide.fmp12".into(),
        }])
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn close(&mut self) {}
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![LayoutInfo {
            id: 1,
            name: "Wide".into(),
        }])
    }
    fn can_write_records(&self) -> bool {
        true
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        Ok(RecordPage {
            versions: Default::default(),
            fields: (1..=COLUMNS)
                .map(|column| Field {
                    name: format!("Column {column}"),
                    kind: FieldKind::Text,
                })
                .collect(),
            records: (1..=ROWS)
                .skip(offset as usize)
                .take(limit as usize)
                .map(|id| Record {
                    id,
                    values: (1..=COLUMNS).map(|column| Some(cell(id, column))).collect(),
                })
                .collect(),
            total: ROWS as u64,
        })
    }
}

fn wheel(app: &mut common::App, delta: egui::Vec2, modifiers: egui::Modifiers) {
    app.hover_at(OVER_ROWS);
    app.run_steps(2);
    app.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers,
    });
    app.run_steps(12);
}

/// The cell editor showing `value` (every row's cells are editors).
fn editor<'a>(app: &'a common::App, value: &'a str) -> egui_kittest::Node<'a> {
    app.get(By::new().role(Role::TextInput).value(value))
}

/// The column header `name` (the current record's cell has its name too).
fn header(app: &common::App, name: &str) -> egui::Rect {
    app.get_by_role_and_label(Role::Label, name).rect()
}

fn shown(rect: egui::Rect) -> bool {
    rect.left() >= 0.0 && rect.right() <= WIDTH
}

#[test]
fn a_table_wider_than_the_window_scrolls_sideways_to_its_last_column() {
    let mut app = common::harness(Box::new(Provider));
    common::fill(&mut app, "Path", "fmnet:/wide.example.invalid/Wide.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_value(&mut app, &cell(1, 1));
    common::click(&mut app, "Table View");
    common::wait_for_value(&mut app, &cell(2, 1));
    let last = format!("Column {COLUMNS}");
    assert!(
        header(&app, &last).left() > WIDTH,
        "the last column starts past the window's right edge"
    );

    // A sideways wheel brings the last column in, header and rows together.
    wheel(&mut app, vec2(-4000.0, 0.0), egui::Modifiers::NONE);
    assert!(shown(header(&app, &last)), "the last column's header shows");
    assert!(
        shown(editor(&app, &cell(2, COLUMNS)).rect()),
        "and its cells"
    );

    // Shift with the wheel goes back to the first column.
    wheel(&mut app, vec2(0.0, 4000.0), egui::Modifiers::SHIFT);
    assert!(shown(header(&app, "Column 1")));
    assert!(header(&app, &last).left() > WIDTH);

    // Tab from the first cell to the last brings that cell in.
    editor(&app, &cell(1, 1)).click();
    app.run_steps(2);
    for _ in 1..COLUMNS {
        app.key_press(egui::Key::Tab);
        app.run_steps(2);
    }
    app.run_steps(12);
    assert!(
        shown(editor(&app, &cell(1, COLUMNS)).rect()),
        "the cell Tab entered is in the window"
    );
}
