//! Focused synthetic acceptance, using the existing PDF ordinary report fixture.
use super::*;
use egui_kittest::{kittest::Queryable, Harness};
use std::time::{Duration, Instant};

struct Store;
impl crate::licensing::LicenseStore for Store {
    fn load(&mut self) -> Result<Option<crate::licensing::LicenseKey>, DataError> {
        Ok(None)
    }
    fn save(&mut self, _: &crate::licensing::LicenseKey) -> Result<(), DataError> {
        Ok(())
    }
    fn remove(&mut self) -> Result<(), DataError> {
        Ok(())
    }
}

fn wait(app: &mut Harness<'_, OpenMakerApp>, mut ready: impl FnMut(&OpenMakerApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        app.step();
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        if ready(app.state()) {
            app.run_steps(3);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Preview did not become ready: mode={:?}, running={}, prompt={}",
            app.state().w.mode,
            app.state().script_running(),
            app.state().script.prompt.is_some()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn click(app: &mut Harness<'_, OpenMakerApp>, label: &str) {
    app.get_by_label(label).click();
    app.run_steps(2);
}
fn shot(app: &mut Harness<'_, OpenMakerApp>, name: &str) {
    if let Some(dir) = std::env::var_os("OM_PREVIEW_ARTIFACT_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        app.set_render_cursor(false);
        app.render()
            .unwrap()
            .save(std::path::Path::new(&dir).join(format!("{name}.png")))
            .unwrap();
    }
}

#[test]
fn title_grand_preview_renders_title_and_grand_only_pages_with_async_anchors() {
    let (provider, events) = crate::scripting::session::title_grand_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    app.state_mut().apply(&ctx, Action::GoToRecord(2));
    app.state_mut().w.session.as_mut().unwrap().page_size = 2;
    app.state_mut().apply(&ctx, Action::Refresh);
    wait(&mut app, OpenMakerApp::is_settled);
    let before = events().len();
    click(&mut app, "Preview");
    let ready = |app: &mut Harness<'_, OpenMakerApp>, text: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        while app.query_by_label(text).is_none() {
            app.step();
            std::thread::sleep(Duration::from_millis(5));
            assert!(Instant::now() < deadline, "Preview never displayed {text}");
        }
        app.run_steps(3);
    };
    ready(&mut app, "Leading total 5625");
    app.get_by_label("Title header / Row 07 - café / record 1");
    app.get_by_label("Title footer / Row 05 - café / record 9");
    app.get_by_label("Leading page 1");
    assert!(app
        .query_by_label("Header / Row 07 - café / record 1")
        .is_none());
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        Some((0, 4))
    );
    shot(&mut app, "title-grand-first");
    assert!(
        events()[before..]
            .iter()
            .any(|e| e.ends_with("records:8:2")),
        "the first-page title footer must load the last found record"
    );

    // Refresh while on the grand-only page: no stale anchor may be borrowed.
    let before_last = events().len();
    app.state_mut().apply(&ctx, Action::GoToPage(3));
    app.state_mut().apply(&ctx, Action::Refresh);
    let session = app.state().w.session.as_ref().unwrap();
    assert!(session.report_plan().unwrap().is_none());
    assert!(session.record_at(8).is_none());
    ready(&mut app, "SQL page total 36");
    app.get_by_label("Header / Row 05 - café / record 9");
    app.get_by_label("Trailing total 5625");
    app.get_by_label("Grand record / Row 05 - café / record 9");
    app.get_by_label("Grand page 4");
    app.get_by_label("Page total 36");
    app.get_by_label("Footer / Row 05 - café / record 9");
    assert!(
        app.query_by_label("Row 05 - café / record 9").is_none(),
        "no body on the grand-only page"
    );
    assert!(app.query_by_label("Leading total 5625").is_none());
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.preview_position(), Some((3, 4)));
    assert_eq!(session.current, 2);
    assert_eq!(session.current_record().unwrap().1.id, 9);
    shot(&mut app, "title-grand-last");
    assert!(events()[before_last..]
        .iter()
        .any(|e| e.ends_with("records:8:2")));
    assert!(!events()[before_last..]
        .iter()
        .any(|e| e.ends_with("records:4:2") || e.ends_with("records:6:2")));
    click(&mut app, "Exit Preview");
    assert_eq!(app.state().window_state().record, Some(9));
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

/// Sample a rendered physical page at the PDF test's picture points. The
/// page is found by the header's green frame (220 x 64 points, at 300, 8).
fn assert_preview_pictures(app: &mut Harness<'_, OpenMakerApp>, page: usize) -> usize {
    use om_layout::PartKind;
    app.set_render_cursor(false);
    let rendered = app.render().unwrap();
    let scale = f64::from(app.ctx.pixels_per_point());
    let session = app.state().w.session.as_ref().unwrap();
    let plan = session.report_plan().unwrap().unwrap();
    let crate::session::RenderState::Loaded(document) = &session.render else {
        panic!("no layout");
    };
    let layout = &document.layout;
    let (mut min, mut max) = ([u32::MAX; 2], [0; 2]);
    for (x, y, pixel) in rendered.enumerate_pixels() {
        if [25, 153, 51]
            .iter()
            .zip(pixel.0)
            .all(|(e, a)| a.abs_diff(*e) <= 8)
        {
            min = [min[0].min(x), min[1].min(y)];
            max = [max[0].max(x), max[1].max(y)];
        }
    }
    for (extent, points) in [(max[0] + 1 - min[0], 220.0), (max[1] + 1 - min[1], 64.0)] {
        assert!(
            (f64::from(extent) - points * scale).abs() <= 2.0,
            "{min:?} {max:?}"
        );
    }
    let header = plan.pages[page]
        .iter()
        .find(|p| layout.parts[p.part].kind() == PartKind::Header)
        .unwrap();
    let origin = [
        f64::from(min[0]) - (plan.paper.margins.left + 300.0) * scale,
        f64::from(min[1]) - (header.top + 8.0) * scale,
    ];
    crate::report::tests::assert_pictures(layout, &plan, page, "Preview", |x, y| {
        let pixel = rendered.get_pixel(
            (origin[0] + x * scale) as u32,
            (origin[1] + y * scale) as u32,
        );
        [pixel[0], pixel[1], pixel[2]]
    })
}

#[test]
fn preview_paints_static_pictures_like_the_pdf_on_repeated_pages() {
    let (provider, events) = crate::scripting::session::picture_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    click(&mut app, "Preview");
    let ready = |app: &mut Harness<'_, OpenMakerApp>, text: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        while app.query_by_label(text).is_none() {
            app.step();
            std::thread::sleep(Duration::from_millis(5));
            assert!(Instant::now() < deadline, "Preview never displayed {text}");
        }
        app.run_steps(3);
    };
    ready(&mut app, "Row 04 - café / record 4");
    app.get_by_label("Logo");
    shot(&mut app, "pictures-first");
    assert_eq!(assert_preview_pictures(&mut app, 0), 6);
    // The final page repeats the header and footer pictures around one body.
    app.state_mut().apply(&ctx, Action::GoToPage(2));
    ready(&mut app, "Row 05 - café / record 9");
    shot(&mut app, "pictures-last");
    assert_eq!(assert_preview_pictures(&mut app, 2), 3);
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

/// Preview shows an ordinary button and a group from the same scene as
/// the PDF (#263). The page is saved (`OM_PREVIEW_ARTIFACT_DIR`) before it
/// is checked, so a refusal is kept too.
#[test]
fn preview_shows_buttons_and_groups_like_the_pdf() {
    let (provider, events) = crate::scripting::session::button_group_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    click(&mut app, "Preview");
    let deadline = Instant::now() + Duration::from_secs(30);
    // Every record's body has the group and the button.
    while app.query_all_by_label("Grouped caption").next().is_none()
        && app.query_by_label_contains("Preview unavailable").is_none()
    {
        app.step();
        std::thread::sleep(Duration::from_millis(5));
        assert!(Instant::now() < deadline, "Preview never displayed a page");
    }
    app.run_steps(3);
    shot(&mut app, "buttons-groups");
    assert!(
        app.query_by_label_contains("Preview unavailable").is_none(),
        "{:?}",
        app.query_by_label_contains("Preview unavailable")
            .map(|n| format!("{n:?}"))
    );
    let bodies = app.query_all_by_label("Grouped caption").count();
    assert!(bodies > 0);
    assert_eq!(app.query_all_by_label("Print button").count(), bodies);
    // Each body keeps its record's Name (the fixture's field 1, "Row NN -
    // café"): the first page's records in found order.
    let names = [
        "Row 07 - café",
        "Row 02 - café",
        "Row 09 - café",
        "Row 04 - café",
    ];
    assert_eq!(bodies, names.len());
    for name in names {
        app.get_by_label(name);
    }
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

/// Preview formats each record as the PDF does (#263): on the first page
/// (records 7, 2, 9 and 4) only record 9's field takes its condition's fill,
/// and a format reading its object's `LayoutObjectUUID` holds.
#[test]
fn preview_applies_conditional_formatting_per_record_like_the_pdf() {
    let (provider, _) = crate::scripting::session::conditional_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    click(&mut app, "Preview");
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.query_by_label("Row 04 - café").is_none()
        && app.query_by_label_contains("Preview unavailable").is_none()
    {
        app.step();
        std::thread::sleep(Duration::from_millis(5));
        assert!(Instant::now() < deadline, "Preview never displayed a page");
    }
    app.run_steps(3);
    shot(&mut app, "conditional");
    assert!(
        app.query_by_label_contains("Preview unavailable").is_none(),
        "{:?}",
        app.query_by_label_contains("Preview unavailable")
            .map(|n| format!("{n:?}"))
    );
    app.set_render_cursor(false);
    let rendered = app.render().unwrap();
    let scale = f64::from(app.ctx.pixels_per_point());
    // The condition's fill (1.0, 0.9, 0.2).
    let fill = [255u8, 230, 51];
    let (mut min, mut max) = ([u32::MAX; 2], [0; 2]);
    for (x, y, pixel) in rendered.enumerate_pixels() {
        if fill.iter().zip(pixel.0).all(|(e, a)| a.abs_diff(*e) <= 3) {
            min = [min[0].min(x), min[1].min(y)];
            max = [max[0].max(x), max[1].max(y)];
        }
    }
    // One field's frame, 330 x 26 points: not one per record.
    for (extent, points) in [(max[0] + 1 - min[0], 330.0), (max[1] + 1 - min[1], 26.0)] {
        assert!(
            (f64::from(extent) - points * scale).abs() <= 2.0,
            "{min:?} {max:?}"
        );
    }
    // The Amount field's format reads its own LayoutObjectUUID: it fills
    // green (0.6, 0.9, 0.6) on every record.
    let green = [153u8, 230, 153];
    assert!(rendered
        .pixels()
        .any(|pixel| green.iter().zip(pixel.0).all(|(e, a)| a.abs_diff(*e) <= 3)));
}

fn shows(app: &mut Harness<'_, OpenMakerApp>, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.query_by_label(text).is_none() {
        app.step();
        std::thread::sleep(Duration::from_millis(5));
        assert!(Instant::now() < deadline, "Preview never displayed {text}");
    }
    app.run_steps(3);
}

#[test]
fn grouped_preview_pages_match_pdf_sub_summaries_and_keep_browse_state() {
    let (provider, events) = crate::scripting::session::grouped_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    app.state_mut().apply(&ctx, Action::GoToRecord(2));
    wait(&mut app, OpenMakerApp::is_settled);
    click(&mut app, "Preview");
    // The same sorted groups, anchors, totals and physical pages as the
    // saved PDF (`grouped_pdf_print_and_preview_pages_share_sorted_sub_summaries`).
    shows(&mut app, "Leading East 2250 from Row 07 - café / record 1");
    for label in [
        "Trailing East 2250 to Row 09 - café / record 3",
        "Trailing page 1 group 3 sql 9",
        "Footer page 1",
    ] {
        app.get_by_label(label);
    }
    // North's heading moved to page 2 with its first record (#263).
    assert!(app
        .query_by_label("Leading North 2375 from Row 04 - café / record 4")
        .is_none());
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.preview_position(), Some((0, 3)));
    shot(&mut app, "grouped-first");
    // North opens page 2 and continues across the next page boundary.
    app.state_mut().apply(&ctx, Action::GoToPage(1));
    shows(&mut app, "Leading North 2375 from Row 04 - café / record 4");
    app.get_by_label("Leading page 2");
    assert!(app
        .query_by_label("Trailing North 2375 to Row 06 - café / record 7")
        .is_none());
    shot(&mut app, "grouped-crossing");
    app.state_mut().apply(&ctx, Action::GoToPage(2));
    shows(&mut app, "Grand total 5625 page 3");
    for label in [
        "Trailing North 2375 to Row 06 - café / record 7",
        "Trailing page 3 group 12 sql 27",
        "Leading West 1000 from Row 03 - café / record 8",
        "Trailing West 1000 to Row 05 - café / record 9",
        "Trailing page 3 group 6 sql 27",
    ] {
        app.get_by_label(label);
    }
    shot(&mut app, "grouped-last");
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.preview_position(), Some((2, 3)));
    assert_eq!(session.current, 2);
    assert_eq!(session.current_record().unwrap().1.id, 9);
    click(&mut app, "Exit Preview");
    assert_eq!(app.state().window_state().record, Some(9));
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

#[test]
fn list_view_shows_sub_summary_parts_once_per_sorted_group() {
    let (provider, events) = crate::scripting::session::list_groups_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    app.state_mut()
        .apply(&ctx, Action::SetView(crate::app::ViewMode::List));
    wait(&mut app, OpenMakerApp::is_settled);
    shows(&mut app, "Row 04 - café / record 3");
    app.run_steps(10);
    shot(&mut app, "list-groups");
    // East East | North, sorted by Region: a group's leading part before
    // its first record and its trailing part after its last, each with
    // the group's total (875 + 250, 500), never the found set's (1625).
    shows(&mut app, "Trailing North 500 to Row 04 - café / record 3");
    let order = [
        "Leading East 1125 from Row 07 - café / record 1",
        "Record 1",
        "Record 2",
        "Trailing East 1125 to Row 02 - café / record 2",
        "Leading North 500 from Row 04 - café / record 3",
        "Record 3",
        "Trailing North 500 to Row 04 - café / record 3",
    ];
    let tops: Vec<f32> = order
        .iter()
        .map(|label| app.get_by_label(label).rect().top())
        .collect();
    assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "{tops:?}");
    // Each band also has its "page" line; each record one list item.
    assert_eq!(app.query_all_by_label_contains("Leading page").count(), 2);
    assert_eq!(app.query_all_by_label_contains("Trailing page").count(), 2);
    assert_eq!(
        app.query_all_by_role(egui::accesskit::Role::ListItem)
            .count(),
        3
    );
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.current, 0);
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

#[test]
fn sliding_preview_measures_shrunk_parts_like_the_pdf_and_keeps_browse_state() {
    let (provider, events) = crate::scripting::session::sliding_fixture();
    let (_, line, lift) = crate::scripting::session::sliding_metrics();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    app.state_mut().apply(&ctx, Action::GoToRecord(2));
    // Record ranges smaller than a page: measuring reads the found set's
    // ids and values, not Browse's record pages.
    app.state_mut().w.session.as_mut().unwrap().page_size = 2;
    app.state_mut().apply(&ctx, Action::Refresh);
    wait(&mut app, OpenMakerApp::is_settled);
    click(&mut app, "Preview");
    shows(&mut app, "Sliding / page 1");
    shows(&mut app, "Row 01 - café");
    // The plan the PDF test saves: two pages (fixed bodies need three),
    // record 6 opening page 2, each body as high as its rows need.
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.preview_position(), Some((0, 2)));
    let plan = session.report_plan().unwrap().unwrap();
    let bodies: Vec<Vec<(usize, f64)>> = plan
        .pages
        .iter()
        .map(|page| {
            page.iter()
                .filter(|p| p.part == 2)
                .map(|p| (p.record, p.height))
                .collect()
        })
        .collect();
    let order = [7, 2, 9, 4, 8, 1, 6, 3, 5];
    assert_eq!(
        bodies.iter().map(Vec::len).collect::<Vec<_>>(),
        [6, 3],
        "{bodies:?}"
    );
    for (index, height) in bodies.iter().flatten() {
        assert!((height - (140.0 - lift(order[*index]))).abs() < 0.001);
    }
    // Painted geometry: zoom 100%, so object rects differ by layout points.
    let top = |app: &Harness<'_, OpenMakerApp>, label: &str| app.get_by_label(label).rect().min.y;
    let near = |a: f32, b: f64| (f64::from(a) - b).abs() < 0.05;
    let name7 = top(&app, "Row 07 - café");
    let name2 = top(&app, "Row 02 - café");
    assert!(near(name2 - name7, 140.0 - lift(7)), "{}", name2 - name7);
    assert!(near(top(&app, "2 Synthetic Way") - name2, line + 2.0));
    assert!(app.query_by_label("Suite 2").is_none());
    assert!(near(top(&app, "Note 1 of 2") - name2, 2.0 * line + 6.0));
    assert!(near(top(&app, "Suite 7") - name7, 2.0 * line + 4.0));
    // The fixed label beside each shrinking Name keeps its place.
    let mut fixed: Vec<f32> = app
        .query_all_by_label("Fixed")
        .map(|node| node.rect().min.y)
        .collect();
    fixed.sort_by(f32::total_cmp);
    let names: Vec<f32> = order[..6]
        .iter()
        .map(|id| top(&app, &format!("Row {id:02} - café")))
        .collect();
    assert_eq!(fixed.len(), 6);
    assert!(fixed.iter().zip(&names).all(|(f, n)| near(f - n, 0.0)));
    assert!(app.query_by_label("Row 06 - café").is_none());
    assert!(app.query_by_label("Region West").is_none());
    shot(&mut app, "sliding-first");
    app.state_mut().apply(&ctx, Action::GoToPage(1));
    shows(&mut app, "Sliding / page 2");
    shows(&mut app, "Row 06 - café");
    app.get_by_label("Region West");
    assert!(app.query_by_label("Region North").is_none());
    let name6 = top(&app, "Row 06 - café");
    let name3 = top(&app, "Row 03 - café");
    assert!(near(name3 - name6, 140.0 - lift(6) + 30.0));
    shot(&mut app, "sliding-last");
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.preview_position(), Some((1, 2)));
    assert_eq!(session.current_record().unwrap().1.id, 9);
    click(&mut app, "Exit Preview");
    assert_eq!(app.state().window_state().record, Some(9));
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

#[test]
fn preview_found_set_pages_render_and_controls_preserve_browse_context() {
    let (provider, events) = crate::scripting::session::preview_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1000.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    app.state_mut().apply(&ctx, Action::GoToRecord(2));
    // Force a fetch window smaller than a report page: page 1 crosses two
    // ranges, and the last physical page is outside all initially loaded rows.
    app.state_mut().w.session.as_mut().unwrap().page_size = 2;
    app.state_mut().apply(&ctx, Action::Refresh);
    wait(&mut app, OpenMakerApp::is_settled);
    let before_preview = events().len();
    click(&mut app, "Preview");
    // Wait for the accessible scene rather than a toolbar caption (which is
    // available while records and calculations are still loading).
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.query_by_label("Row 07 - café / record 1").is_none() {
        app.step();
        std::thread::sleep(Duration::from_millis(5));
        assert!(Instant::now() < deadline);
    }
    app.run_steps(3);
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        Some((0, 3))
    );
    assert_eq!(
        app.get_by_label("Page Number").value().as_deref(),
        Some("1")
    );
    app.get_by_label("Row 04 - café / record 4");
    app.get_by_label("Repeated footer");
    app.get_by_label("Page 1");
    shot(&mut app, "preview-first");
    app.set_size(vec2(1060.0, 640.0));
    app.run_steps(3);
    let mut end = om_data::ScriptStep::new(81);
    end.value = 2;
    app.state_mut().start_script(
        &ctx,
        Start::Button {
            step: Box::new(end),
            formula: None,
            literal: String::new(),
            portal: None,
        },
    );
    wait(&mut app, |a| {
        !a.script_running()
            && a.w.document.scroll.is_measured()
            && a.w.document.scroll.measured_motion(&ctx).0 > 100.0
    });
    let end_offset = app.state().w.document.scroll.measured_motion(&ctx).0;
    app.event(egui::Event::PointerMoved(egui::pos2(300.0, 300.0)));
    app.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 100.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: egui::Modifiers::NONE,
    });
    app.run_steps(8);
    assert!(app.state().w.document.scroll.measured_motion(&ctx).0 < end_offset - 10.0);
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        Some((0, 3)),
        "vertical motion stays on the physical page"
    );
    app.state_mut().apply(&ctx, Action::ZoomIn);
    app.run_steps(3);
    assert_eq!(app.state().w.document.zoom.percent(), 150);
    app.state_mut().apply(&ctx, Action::ActualSize);
    app.run_steps(3);
    // Records menu Last and the toolbar Previous/Next operate on pages.
    click(&mut app, "Records");
    click(&mut app, "Go to Record");
    click(&mut app, "Last");
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.query_by_label("Row 05 - café / record 9").is_none() {
        app.step();
        std::thread::sleep(Duration::from_millis(5));
        assert!(Instant::now() < deadline);
    }
    app.run_steps(3);
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        Some((2, 3))
    );
    assert_eq!(
        app.get_by_label("Page Number").value().as_deref(),
        Some("3")
    );
    assert!(app.query_by_label("Row 07 - café / record 1").is_none());
    assert!(
        app.state().w.document.scroll.measured_motion(&ctx).0 < 1.0,
        "a new physical page starts at its top"
    );
    app.set_size(vec2(1060.0, 1000.0));
    app.run_steps(3);
    app.get_by_label("Page 3");
    assert!(
        !events()[before_preview..]
            .iter()
            .any(|e| e.ends_with("records:4:2") || e.ends_with("records:6:2")),
        "unvisited middle page bodies need not be downloaded"
    );
    shot(&mut app, "preview-last");
    click(&mut app, "Previous");
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        Some((1, 3))
    );
    click(&mut app, "Next");
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        Some((2, 3))
    );
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.current, 2);
    assert_eq!(session.current_record().unwrap().1.id, 9);
    click(&mut app, "Exit Preview");
    assert_eq!(app.state().w.session.as_ref().unwrap().current, 2);
    assert_eq!(app.state().window_state().record, Some(9));
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}

#[test]
fn preview_page_identity_is_per_window_and_reload_failure_cannot_show_old_page() {
    let (mut provider, _) = crate::scripting::session::preview_fixture();
    let render = provider.render_layout(1).unwrap().unwrap();
    let mut session = |file: &str| {
        let mut session = Session::new(
            Target {
                host: "synthetic.invalid".into(),
                file: file.into(),
                local: None,
            },
            2,
        );
        let key = session.select_layout(1);
        session
            .accept_render_layout(1, Ok(Some(render.clone())))
            .unwrap();
        session
            .accept_page(key, Ok(provider.records(1, 0, 2).unwrap()))
            .unwrap();
        session
    };
    let mut app = OpenMakerApp::with_license_store(
        Box::new(om_data::mock::MockProvider::new()),
        Box::new(Store),
    );
    app.w.session = Some(session("Root.fmp12"));
    app.w.mode = Mode::Preview;
    let mut other = windows::Doc::new(2);
    other.file_id = 42; // Reused layout/record ids in another owning file.
    other.session = Some(session("Peer.fmp12"));
    other.mode = Mode::Preview;
    app.windows.push(other);
    let ctx = egui::Context::default();
    app.apply(&ctx, Action::GoToPage(2));
    app.with_window(2, |app| app.apply(&ctx, Action::GoToPage(1)));
    assert_eq!(
        app.w.session.as_ref().unwrap().preview_position(),
        Some((2, 3))
    );
    assert_eq!(
        app.with_window(2, |app| app.w.session.as_ref().unwrap().preview_position()),
        Some(Some((1, 3)))
    );
    let original = app.w.session.as_ref().unwrap().page.as_ref().unwrap().key;
    let old_plan = app
        .w
        .session
        .as_ref()
        .unwrap()
        .report_plan()
        .unwrap()
        .unwrap();
    app.add_window(3);
    app.on_script_event(
        &ctx,
        crate::scripting::ScriptEvent::PaperSettings {
            file: 0,
            paper: om_layout::paper::PaperSettings {
                width: 612.0,
                height: 1008.0,
                ..Default::default()
            },
        },
    );
    let session = app.w.session.as_ref().unwrap();
    assert_eq!(session.preview_position(), Some((0, 2)));
    assert!(!std::sync::Arc::ptr_eq(
        &old_plan,
        &session.report_plan().unwrap().unwrap()
    ));
    assert_eq!(session.page.as_ref().unwrap().key, original);
    assert_eq!(
        app.windows
            .iter()
            .find(|d| d.id == 3)
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .paper,
        session.paper
    );
    assert_eq!(
        app.windows
            .iter()
            .find(|d| d.id == 2)
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .preview_position(),
        Some((1, 3))
    );
    app.add_window(4);
    assert_eq!(
        app.windows
            .iter()
            .find(|d| d.id == 4)
            .unwrap()
            .session
            .as_ref()
            .unwrap()
            .paper,
        app.w.session.as_ref().unwrap().paper
    );
    let session = app.w.session.as_mut().unwrap();
    let old = session.request_range(8).unwrap();
    let new = session.refresh().unwrap();
    assert!(session.report_plan().unwrap().is_none());
    session
        .accept_page(old, Ok(provider.records(1, 8, 2).unwrap()))
        .unwrap();
    assert!(
        session.record_at(8).is_none(),
        "old range reply is canceled by refresh"
    );
    session
        .accept_page(new, Err(DataError::new("synthetic reload failure")))
        .unwrap_err();
    assert!(session
        .report_plan()
        .unwrap_err()
        .contains("synthetic reload failure"));
    assert_eq!(session.current, 0);
}

#[test]
fn page_setup_menu_toolbar_round_trip_changes_preview_without_record_writes() {
    let (provider, events) = crate::scripting::session::preview_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1000.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    let key = app
        .state()
        .w
        .session
        .as_ref()
        .unwrap()
        .page
        .as_ref()
        .unwrap()
        .key;
    // Page preferences must not invoke navigation's commit boundary.
    app.state_mut()
        .w
        .document
        .put_draft(1, 7, "Text", "stored".into(), "unsaved".into());
    click(&mut app, "File");
    click(&mut app, "Page Setup...");
    wait(&mut app, |a| a.script.prompt.is_some());
    shot(&mut app, "page-setup-dialog");
    click(&mut app, "Cancel");
    wait(&mut app, |a| !a.script_running());
    assert_eq!(
        app.state()
            .w
            .session
            .as_ref()
            .unwrap()
            .page
            .as_ref()
            .unwrap()
            .key,
        key
    );
    assert!(app.state().record_modified());
    assert!(!events().iter().any(|e| e.ends_with(":write")));
    // Discard only this synthetic draft explicitly before Preview's own commit.
    app.state_mut().apply(&ctx, Action::RevertRecord);
    wait(&mut app, OpenMakerApp::is_settled);
    let key = app
        .state()
        .w
        .session
        .as_ref()
        .unwrap()
        .page
        .as_ref()
        .unwrap()
        .key;
    click(&mut app, "Preview");
    wait(&mut app, |a| {
        a.w.mode == Mode::Preview && !a.script_running()
    });
    app.state_mut().apply(&ctx, Action::GoToPage(2));
    click(&mut app, "Page Setup");
    wait(&mut app, |a| a.script.prompt.is_some());
    click(&mut app, "Paper size");
    click(&mut app, "A4");
    click(&mut app, "Landscape");
    click(&mut app, "OK");
    wait(&mut app, |a| {
        !a.script_running() && a.w.session.as_ref().unwrap().paper.is_some()
    });
    let session = app.state().w.session.as_ref().unwrap();
    assert_eq!(session.preview_position(), Some((0, 5)));
    let plan = session.report_plan().unwrap().unwrap();
    assert!((plan.paper.width - 841.8898).abs() < 0.01);
    assert!((plan.paper.height - 595.2756).abs() < 0.01);
    assert_eq!(session.page.as_ref().unwrap().key, key);
    shot(&mut app, "page-setup-a4-landscape");
    click(&mut app, "Page Setup");
    wait(&mut app, |a| a.script.prompt.is_some());
    // Accept the reopened dialog without edits: its current settings must round trip.
    click(&mut app, "OK");
    wait(&mut app, |a| !a.script_running());
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        Some((0, 5))
    );
    assert!(!events().iter().any(|e| e.ends_with(":write")));

    assert_eq!(
        app.state().w.session.as_ref().unwrap().paper,
        Some(session_paper())
    );

    fn session_paper() -> om_layout::paper::PaperSettings {
        om_layout::paper::PaperSettings {
            width: 595.2756,
            height: 841.8898,
            orientation: om_layout::paper::Orientation::Landscape,
        }
    }
}

#[test]
fn print_menu_shortcut_and_preview_toolbar_use_owned_fake_dispatch() {
    use crate::printing::PageRange;
    let (provider, events) = crate::scripting::session::title_grand_fixture();
    let fake = std::sync::Arc::new(crate::printing::tests::Fake::default());
    let dispatch = fake.clone();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1000.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_print_dispatcher(provider, Box::new(Store), dispatch);
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    click(&mut app, "File");
    click(&mut app, "Print...");
    wait(&mut app, |a| a.script.prompt.is_some());
    click(&mut app, "Printer");
    click(&mut app, "Synthetic;$(literal)");
    // DragValue's keyboard entry path supplies the actual dispatched copies.
    app.get_by_label("Copies").click();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.event(egui::Event::Text("3".into()));
    app.run_steps(3);
    let enter = |app: &mut Harness<'_, OpenMakerApp>, label: &str, value: &str| {
        app.get_by_label(label).click();
        app.run_steps(2);
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        app.event(egui::Event::Text(value.into()));
        app.run_steps(3);
    };
    click(&mut app, "Page range");
    enter(&mut app, "From", "2");
    enter(&mut app, "To", "4");
    shot(&mut app, "print-page-range");
    app.get_by_role_and_label(egui::Role::Button, "Print")
        .click();
    app.run_steps(2);
    wait(&mut app, |a| !a.script_running());
    {
        let jobs = fake.jobs.lock().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].options.printer.as_deref(),
            Some("Synthetic;$(literal)")
        );
        assert_eq!(jobs[0].options.copies, 3);
        assert_eq!(jobs[0].options.pages, PageRange::Range { from: 2, to: 4 });
        // Selection reaches submission alongside the complete report, keeping
        // even excluded-page context and full-found-set grand summaries.
        let text = String::from_utf8_lossy(&jobs[0].bytes);
        assert!(text.contains("/Count 4"));
        let font = ttf_parser::Face::parse(include_bytes!("../assets/fonts/Inter-Regular.ttf"), 0)
            .unwrap();
        for expected in [
            "Title header / Row 07 - café / record 1",
            "Leading total 5625",
            "Trailing total 5625",
            "Grand page 4",
            "Page total 36",
            "SQL page total 36",
        ] {
            let glyphs: String = expected
                .chars()
                .map(|ch| format!("{:04X}", font.glyph_index(ch).unwrap().0))
                .collect();
            assert_eq!(
                text.matches(&format!("<{glyphs}> Tj")).count(),
                1,
                "{expected}"
            );
        }
        assert!(!jobs[0].path.exists());
        if let Some(dir) = std::env::var_os("OM_PRINT_ARTIFACT_DIR") {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                std::path::Path::new(&dir).join("range-complete-report.pdf"),
                &jobs[0].bytes,
            )
            .unwrap();
        }
    }
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::P);
    wait(&mut app, |a| a.script.prompt.is_some());
    click(&mut app, "Cancel");
    wait(&mut app, |a| !a.script_running());
    assert_eq!(fake.jobs.lock().unwrap().len(), 1);
    // Inline invalid bounds cannot submit via Enter; Escape still cancels.
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::P);
    wait(&mut app, |a| a.script.prompt.is_some());
    click(&mut app, "Page range");
    enter(&mut app, "From", "0");
    app.get_by_label("Enter positive whole page numbers with From <= To (maximum 2147483647)");
    app.key_press(egui::Key::Enter);
    app.run_steps(3);
    assert!(app.state().script.prompt.is_some());
    assert_eq!(fake.jobs.lock().unwrap().len(), 1);
    app.key_press(egui::Key::Escape);
    wait(&mut app, |a| !a.script_running());
    assert_eq!(fake.jobs.lock().unwrap().len(), 1);
    click(&mut app, "Preview");
    wait(&mut app, |a| {
        a.w.mode == Mode::Preview && !a.script_running()
    });
    let position = app.state().w.session.as_ref().unwrap().preview_position();
    click(&mut app, "Print");
    wait(&mut app, |a| a.script.prompt.is_some());
    click(&mut app, "Current record");
    click(&mut app, "Page range");
    enter(&mut app, "From", "1");
    enter(&mut app, "To", "1");
    // The toolbar and modal have the same label; choose the modal's control.
    app.get_all_by_role_and_label(egui::Role::Button, "Print")
        .last()
        .unwrap()
        .click();
    app.run_steps(2);
    wait(&mut app, |a| !a.script_running());
    let jobs = fake.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(
        jobs[1].options.printer, None,
        "job choices do not silently persist"
    );
    assert_eq!(jobs[1].options.copies, 1);
    assert!(jobs[1].options.current_record);
    assert_eq!(jobs[1].options.pages, PageRange::Range { from: 1, to: 1 });
    // A single record still has two physical pages with these grand parts.
    assert!(String::from_utf8_lossy(&jobs[1].bytes).contains("/Count 2"));
    assert!(!jobs[1].path.exists());
    assert_eq!(
        app.state().w.session.as_ref().unwrap().preview_position(),
        position
    );
    drop(jobs);
    // All ignores inactive malformed inputs, and the next job starts at All.
    click(&mut app, "Print");
    wait(&mut app, |a| a.script.prompt.is_some());
    click(&mut app, "Page range");
    enter(&mut app, "From", "bad");
    enter(&mut app, "To", "0");
    click(&mut app, "All pages");
    app.get_all_by_role_and_label(egui::Role::Button, "Print")
        .last()
        .unwrap()
        .click();
    app.run_steps(2);
    wait(&mut app, |a| !a.script_running());
    let jobs = fake.jobs.lock().unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[2].options.pages, PageRange::All);
    assert!(!jobs[2].options.current_record);
    assert!(String::from_utf8_lossy(&jobs[2].bytes).contains("/Count 4"));
    assert!(!events().iter().any(|e| e.ends_with(":write")));
}

#[test]
fn variable_changes_remeasure_preview_without_a_provider_generation() {
    use crate::scripting::{ScriptEvent, WindowUpdate};
    use om_data::calc::{Value, ValueType};
    use om_data::host::FieldDef;
    use std::sync::Arc;
    let (provider, events) = crate::scripting::session::sliding_fixture();
    let mut app = Harness::builder()
        .renderer(crate::test_renderer::renderer())
        .with_size(vec2(1060.0, 1100.0))
        .build_eframe(move |cc| {
            let mut app = OpenMakerApp::with_license_store(provider, Box::new(Store));
            app.form.host = "synthetic.invalid".into();
            app.form.file = "Root.fmp12".into();
            app.form.account = "synthetic".into();
            app.apply(&cc.egui_ctx, Action::Connect);
            app
        });
    wait(&mut app, |a| a.is_settled() && a.w.calcs.schema().is_some());
    let ctx = app.ctx.clone();
    {
        let a = app.state_mut();
        let mut schema = a.w.calcs.schema().unwrap().as_ref().clone();
        let compiled = om_data::formula::compile(
            "$$ReviewNotes",
            &om_data::formula::Context::new(schema.names()),
        )
        .unwrap();
        schema.add_field(
            5,
            FieldDef::calculation(
                1,
                "Text",
                ValueType::Text,
                om_data::formula::Formula {
                    context: None,
                    formatted: Some(compiled.tokens),
                    polish: compiled.expr,
                },
                false,
            ),
        );
        a.w.calcs.set_schema(Some(Arc::new(schema)));
        a.w.globals
            .set("$$ReviewNotes", 1, Value::text("Alpha\rBeta\rGamma"));
        let session = a.w.session.as_mut().unwrap();
        let mut document = match &session.render {
            crate::session::RenderState::Loaded(d) => (**d).clone(),
            _ => panic!("layout not loaded"),
        };
        document.layout.parts = vec![om_layout::Part {
            part_type: 4,
            height: 200.0,
            ..Default::default()
        }];
        document.layout.objects.objects.retain(|o| o.id == 52);
        document.layout.objects.root = vec![52];
        let object = &mut document.layout.objects.objects[0];
        object.bounds = Some(om_layout::Rect {
            left: 12.0,
            right: 312.0,
            top: 0.0,
            bottom: 150.0,
            ..Default::default()
        });
        session.accept_render_layout(1, Ok(Some(document))).unwrap();
    }
    click(&mut app, "Preview");
    let deadline = Instant::now() + Duration::from_secs(30);
    while app.query_all_by_label("Alpha").count() == 0 {
        app.step();
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            Instant::now() < deadline,
            "initial variable field did not render"
        );
    }
    app.run_steps(3);
    let before = app
        .state()
        .w
        .session
        .as_ref()
        .unwrap()
        .report_plan()
        .unwrap()
        .unwrap();
    let height_before = before.pages[0][0].height;
    assert_eq!(before.pages.len(), 2);
    // Deliver the same Globals then Finished sequence the script worker emits.
    let mut changed = app.state().w.globals.clone();
    changed.set("$$ReviewNotes", 1, Value::text(""));
    {
        let a = app.state_mut();
        a.on_response(
            &ctx,
            crate::worker::Response {
                epoch: a.epoch,
                window: a.w.id,
                reply: crate::worker::Reply::Script(ScriptEvent::Globals {
                    file: a.w.file_id,
                    globals: Box::new(changed.clone()),
                }),
            },
        );
        a.on_script_event(
            &ctx,
            ScriptEvent::Finished {
                window: Box::new(WindowUpdate {
                    file: a.w.file_id,
                    state: a.window_state(),
                    data_changed: false,
                    refreshed: false,
                    objects: vec![],
                    scroll: vec![],
                    fronted: Vec::new(),
                    originals: Default::default(),
                    found_set: None,
                    background: false,
                }),
                globals: Box::new(changed),
                message: None,
            },
        );
    }
    assert!(app
        .state()
        .w
        .session
        .as_ref()
        .unwrap()
        .report_groups_wanted()
        .is_some());
    let message = "Preview unavailable: The report content changed after its pages were planned";
    wait(&mut app, |a| {
        a.w.session.as_ref().unwrap().preview_position() == Some((0, 1))
    });
    let after = app
        .state()
        .w
        .session
        .as_ref()
        .unwrap()
        .report_plan()
        .unwrap()
        .unwrap();
    assert_eq!(after.pages[0][0].height, 50.0);
    assert!(height_before > after.pages[0][0].height);
    assert!(app.query_by_label(message).is_none());
    assert!(!Arc::ptr_eq(&before, &after));
    // A final worker update must also invalidate when no separate Globals
    // broadcast preceded it.
    let mut changed = app.state().w.globals.clone();
    changed.set("$$ReviewNotes", 1, Value::text("Alpha\rBeta\rGamma"));
    {
        let a = app.state_mut();
        a.on_script_event(
            &ctx,
            ScriptEvent::Finished {
                window: Box::new(WindowUpdate {
                    file: a.w.file_id,
                    state: a.window_state(),
                    data_changed: false,
                    refreshed: false,
                    objects: vec![],
                    scroll: vec![],
                    fronted: Vec::new(),
                    originals: Default::default(),
                    found_set: None,
                    background: false,
                }),
                globals: Box::new(changed),
                message: None,
            },
        );
    }
    wait(&mut app, |a| {
        a.w.session.as_ref().unwrap().preview_position() == Some((0, 2))
    });
    let restored = app
        .state()
        .w
        .session
        .as_ref()
        .unwrap()
        .report_plan()
        .unwrap()
        .unwrap();
    assert_eq!(restored.pages[0][0].height, height_before);
    assert!(!Arc::ptr_eq(&after, &restored));
    assert!(app.query_by_label(message).is_none());
    assert!(!events()
        .iter()
        .any(|e| e.ends_with(":write") || e.contains(":found:")));
}
