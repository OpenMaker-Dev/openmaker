//! Synthetic style-resolution microbenchmark, independent of external files.
//! Run `cargo run --release -p om-layout --example style_resolution_bench`.
//! `-- --dump` emits every fixture output for an exact before/after comparison.
//! CSV columns: workload, sample (0 is warmup), nanoseconds for 51,200 styles.
//! Compare matching release builds on the same CPU; this is not a GUI FPS test.

use om_layout::style::*;
use std::{hint::black_box, time::Instant};

fn property(id: i32, seed: i32) -> Property {
    Property {
        id,
        int: Some(seed),
        string: (id == 64).then(|| "Arial".to_string()),
        color: Some(Color {
            red: seed as f32 / 100.,
            green: 0.25,
            blue: 0.5,
            alpha: 1.,
        }),
        length: Some(Length {
            value: seed as f64,
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn fixture(mode: &str) -> (ThemeStyles, ThemeStyles, Vec<ObjectStyle>) {
    let ids = [
        7, 70, 514, 64, 69, 65, 67, 74, 75, 611, 101, 102, 103, 104, 28, 29, 30, 31, 20, 21, 22,
        23, 24, 25, 26, 27, 35,
    ];
    let theme = if mode == "empty" {
        ThemeStyles::default()
    } else {
        ThemeStyles {
            type_styles: [1, 61, 72, 80, 51]
                .into_iter()
                .map(|kind| TypeStyle {
                    object_types: vec![kind],
                    styles: Some(StyleSet {
                        components: [1, 3]
                            .into_iter()
                            .map(|component| ComponentStyle {
                                component,
                                state: 1,
                                properties: ids
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| mode == "full" || i % 4 == 0)
                                    .map(|(_, &id)| property(id, kind as i32))
                                    .collect(),
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    };
    let local = ThemeStyles::default();
    let objects = (0..256)
        .map(|i| ObjectStyle {
            object_type_id: [61, 72, 80, 51][i % 4],
            local_styles: (mode != "empty" && i % 8 == 0).then(|| StyleSet {
                components: vec![ComponentStyle {
                    component: 1,
                    state: 1,
                    properties: vec![property(65, 700)],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect();
    (theme, local, objects)
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    for mode in ["empty", "sparse", "full"] {
        let (theme, local, objects) = fixture(mode);
        if args.iter().any(|a| a == "--dump") {
            for object in &objects {
                for state in [1, 2, 9, 10, 11, 12, 13, 16, 17, 19, 6] {
                    for component in [1, 3] {
                        println!(
                            "{mode} {state} {component} {:?}",
                            resolve_style(Some(object), 61, Some(&local), &theme, component, state)
                        );
                    }
                }
            }
            continue;
        }
        for sample in 0..11 {
            let start = Instant::now();
            for _ in 0..100 {
                for (i, object) in objects.iter().enumerate() {
                    // Normal object and text-component rendering; occasional hover/focus.
                    let state = [1, 1, 1, 1, 2, 1, 9, 1][i % 8];
                    for component in [1, 3] {
                        black_box(resolve_style(
                            Some(black_box(object)),
                            61,
                            Some(black_box(&local)),
                            black_box(&theme),
                            component,
                            black_box(state),
                        ));
                    }
                }
            }
            println!("{mode},{sample},{}", start.elapsed().as_nanos());
        }
    }
}
