//! Calculations in Browse mode on the sample provider's "Contact Card"
//! layout (`OPENMAKER_PROVIDER=mock`): an unstored calculation field, a
//! hide condition, conditional formatting with `Self`, a `{{symbol}}`, and
//! a calculation that follows the user's unsaved edit.

mod common;

use egui::epaint::Shape;
use egui::Color32;
use egui_kittest::kittest::Queryable;

/// The sample's conditional formatting fill (`mock_calc::CONDITION_FILL`).
const CONDITION_FILL: Color32 = Color32::from_rgb(204, 26, 26);

fn open_card() -> common::App {
    let mut app = common::mock();
    common::connect_to_mock(&mut app);
    common::select_layout(&mut app, "Contact Card");
    common::wait_for_label(&mut app, "First0 Last0");
    app
}

fn has_condition_fill(app: &common::App) -> bool {
    app.output()
        .shapes
        .iter()
        .any(|shape| matches!(&shape.shape, Shape::Rect(rect) if rect.fill == CONDITION_FILL))
}

#[test]
fn unstored_calculations_hide_conditions_and_formatting_follow_the_record() {
    let mut app = open_card();
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Record 1: First Name is First0, so "Preferred contact" shows; Last
    // Name is not Last1, so it keeps the theme's fill.
    app.get_by_label("Preferred contact");
    app.get_by_label("Card 1");
    assert!(!has_condition_fill(&app));

    common::click(&mut app, "Next");
    common::wait_for_label(&mut app, "First1 Last1");
    common::wait_until(&mut app, "the hide condition", |app| {
        app.query_by_label("Preferred contact").is_none()
    });
    app.get_by_label("Card 2");
    assert!(app.query_by_label("First0 Last0").is_none());
    assert!(
        has_condition_fill(&app),
        "Self = \"Last1\" paints the conditional fill"
    );

    common::click(&mut app, "Previous");
    common::wait_for_label(&mut app, "Preferred contact");
    common::wait_until(&mut app, "the plain fill", |app| !has_condition_fill(app));
}

#[test]
fn an_unstored_calculation_shows_the_unsaved_edit() {
    let mut app = open_card();
    common::text_field(&app, "First Name").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    common::text_field(&app, "First Name").type_text("Zed");
    app.run_steps(2);
    common::wait_for_label(&mut app, "Zed Last0");
    // The edit also decides the hide condition before it is saved.
    common::wait_until(&mut app, "the hide condition", |app| {
        app.query_by_label("Preferred contact").is_none()
    });
}
