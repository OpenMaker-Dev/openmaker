//! A drop-down list's auto-complete rule (`choice::completion`, #870).

use om_data::choice::{completion, Choices};
use om_data::host::ValueListEntry;

fn list(values: &[&str], custom: bool) -> Choices {
    Choices {
        items: values
            .iter()
            .map(|value| ValueListEntry {
                value: (*value).to_owned(),
                label: (*value).to_owned(),
            })
            .collect(),
        custom,
    }
}

/// The first item in the list's order that begins with the typed text,
/// ignoring case, and is longer: "A" gives "Acme", as FileMaker Pro 26.0.3
/// shows it (#870's reading); dividers, an item equal to the text and an
/// empty text give none.
#[test]
fn the_first_longer_item_beginning_with_the_typed_text_completes_it() {
    let items = list(&["-", "Beta", "Acme", "Acorn", "Éclair", "Ac"], true);
    let complete = |typed: &str| completion(&items, typed).map(|item| item.value.as_str());
    assert_eq!(complete("A"), Some("Acme"));
    assert_eq!(complete("ac"), Some("Acme"));
    assert_eq!(complete("Aco"), Some("Acorn"));
    assert_eq!(complete("é"), Some("Éclair"));
    assert_eq!(complete("Acme"), None);
    assert_eq!(complete("Ax"), None);
    assert_eq!(complete(""), None);
    assert_eq!(complete("-"), None, "a divider is no item");
}
