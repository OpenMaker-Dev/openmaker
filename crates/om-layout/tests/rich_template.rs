//! Opt-in integration against a local, licensed Contacts template or an
//! untouched file made from it. No proprietary fixture is checked in.
use om_hbam::HbamFile;
use om_layout::{Layout, TextRun};

#[test]
#[ignore = "requires OPENMAKER_CONTACTS_TEMPLATE pointing at a local Contacts .fmp12"]
fn contacts_icons_are_original_file_library_svg_streams() {
    let path =
        std::env::var_os("OPENMAKER_CONTACTS_TEMPLATE").expect("set OPENMAKER_CONTACTS_TEMPLATE");
    let file = HbamFile::open(path).unwrap();
    let tree = file.key_tree().unwrap();
    let list = Layout::decode_from_tree(&tree, 2).unwrap().unwrap();
    let details = Layout::decode_from_tree(&tree, 3).unwrap().unwrap();
    assert_eq!(
        list.library_images.keys().copied().collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        details.library_images.keys().copied().collect::<Vec<_>>(),
        [2, 3, 4, 5, 6, 7, 8]
    );
    for (key, image) in list.library_images.iter().chain(&details.library_images) {
        assert_eq!(image.format, om_layout::LibraryImageFormat::Svg);
        let original = tree
            .at(&[31, 5, *key, u64::from(u32::from_be_bytes(*b"SVG "))])
            .unwrap()
            .assembled()
            .unwrap();
        assert_eq!(image.data, original.as_ref());
    }
    // The globe original spans multiple HBAM chunks; no preview replaces it.
    assert_eq!(details.library_images[&4].data.len(), 1175);
    let navigation = list.objects.get(7).unwrap().button.as_ref().unwrap();
    assert_eq!(
        (
            navigation.library_key,
            navigation.icon_size,
            navigation.display_type
        ),
        (1, 16, 1)
    );
}

#[test]
#[ignore = "requires OPENMAKER_CONTACTS_TEMPLATE pointing at a local Contacts .fmp12"]
fn contacts_real_text_theme_and_container_coordinates() {
    let path =
        std::env::var_os("OPENMAKER_CONTACTS_TEMPLATE").expect("set OPENMAKER_CONTACTS_TEMPLATE");
    let file = HbamFile::open(path).unwrap();
    let tree = file.key_tree().unwrap();
    let layout = Layout::decode_from_tree(&tree, 3).unwrap().unwrap();
    assert_eq!(layout.name, "Contact Details");
    assert_eq!((layout.width(), layout.height()), (1024.0, 572.0));
    assert_eq!(layout.theme_key, Some(1));
    assert!(!layout.theme_styles.type_styles.is_empty());
    let first_name = layout.objects.get(39).unwrap();
    let field_style = layout.object_style(first_name, 1, 1);
    assert_eq!(field_style.font_family.as_deref(), Some("arial,sans-serif"));
    assert_eq!(
        field_style.border_style,
        [Some(1), Some(1), Some(5), Some(1)]
    );
    assert_eq!(
        layout
            .object_style(layout.objects.get(2).unwrap(), 1, 1)
            .text_transform,
        Some(3)
    );
    assert_eq!(
        layout
            .object_style(layout.objects.get(11).unwrap(), 1, 1)
            .text_transform,
        Some(3)
    );
    assert_eq!(
        layout
            .object_style(layout.objects.get(49).unwrap(), 1, 1)
            .text_transform,
        None
    );
    assert_eq!(
        layout
            .object_style(layout.objects.get(3).unwrap(), 1, 1)
            .vertical_align,
        Some(1)
    );
    let panel = layout.objects.get(5).unwrap();
    assert_eq!(
        layout.object_style(panel, 1, 7).background.unwrap().red,
        1.0
    );
    assert_eq!(layout.objects.get(8).unwrap().placeholder, None);
    assert_eq!(layout.objects.get(10).unwrap().placeholder, None);
    assert_eq!(
        layout.objects.get(8).unwrap().tooltip.as_deref(),
        Some("Website")
    );
    assert_eq!(
        layout.objects.get(10).unwrap().tooltip.as_deref(),
        Some("Company")
    );
    assert_eq!(
        layout.objects.get(37).unwrap().placeholder.as_deref(),
        Some("Job Title")
    );
    assert_eq!(
        layout.objects.get(41).unwrap().placeholder.as_deref(),
        Some("Photo")
    );
    let band = layout.objects.get(43).unwrap();
    let color = layout.object_style(band, 1, 1).background.unwrap();
    assert!((color.red - 104.0 / 255.0).abs() < 0.00001);
    assert_eq!(color.alpha, 1.0);
    assert_eq!(
        layout.objects.get(49).unwrap().text.as_deref(),
        Some("First Name")
    );
    assert_eq!(
        layout.objects.get(48).unwrap().text_runs,
        [TextRun::MergeField("First Name".into())]
    );
    assert_eq!(
        layout.objects.get(46).unwrap().text_runs,
        [TextRun::MergeField("Last Name".into())]
    );
    assert_eq!(
        layout.objects.get(44).unwrap().text_runs,
        [TextRun::MergeField("Company".into())]
    );
    let company = layout.objects.get(5).unwrap();
    assert_eq!(company.text.as_deref(), Some("Company"));
    assert_eq!(company.tab_panel.as_ref().unwrap().tab_width, 104);
    let field = layout.absolute_bounds(10).unwrap();
    assert_eq!((field.top, field.left), (206.0, 366.0));
    assert_eq!(
        layout.objects.get(40).unwrap().placeholder.as_deref(),
        Some("Title")
    );
}

#[test]
#[ignore = "requires OPENMAKER_CONTACTS_TEMPLATE pointing at a local Contacts .fmp12"]
fn contacts_member_flags_select_list_and_form_and_native_buttons_have_targets() {
    let path =
        std::env::var_os("OPENMAKER_CONTACTS_TEMPLATE").expect("set OPENMAKER_CONTACTS_TEMPLATE");
    let file = HbamFile::open(path).unwrap();
    let tree = file.key_tree().unwrap();
    let list = Layout::decode_from_tree(&tree, 2).unwrap().unwrap();
    let details = Layout::decode_from_tree(&tree, 3).unwrap().unwrap();
    assert_eq!(
        list.def.as_ref().unwrap().flags,
        tree.at(&[4, 5, 2, 0xf1]).unwrap().uint_value().unwrap()
    );
    assert_ne!(list.def.as_ref().unwrap().flags & 0x200, 0);
    assert_eq!(details.def.as_ref().unwrap().flags & 0x200, 0);
    assert_eq!(
        list.objects
            .get(7)
            .unwrap()
            .script_step()
            .unwrap()
            .local_reference(),
        Some(3)
    );
    assert_eq!(
        details
            .objects
            .get(3)
            .unwrap()
            .script_step()
            .unwrap()
            .local_reference(),
        Some(2)
    );
    for (id, name) in [(48, "First Name"), (46, "Last Name"), (44, "Company")] {
        let button = details.objects.get(id).unwrap();
        assert_eq!(button.script_step().unwrap().local_reference(), Some(2));
        assert_eq!(button.script_parameter.as_deref(), Some(name));
        assert!(!button.script_parameter_unsupported);
    }
}
