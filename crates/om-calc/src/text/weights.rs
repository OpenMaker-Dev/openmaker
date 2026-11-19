//! Collation elements as text comparison and sorting use them: DUCET
//! 5.2.0's weights ([`om_collate::collation_elements`]) with room below each
//! primary weight for what a language places right after it.

use super::cjk;

/// One collation element. `primary` is the DUCET primary weight shifted
/// left by [`SHIFT`]; the low bits order letters and table entries placed
/// right after that weight (a language's letters, the CJK tables of
/// [`cjk`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Weights {
    pub primary: u32,
    pub secondary: u32,
    pub tertiary: u16,
}

/// How far a DUCET primary weight is shifted in [`Weights::primary`].
pub(crate) const SHIFT: u32 = 16;

impl Weights {
    /// An element with only an accent (secondary) weight.
    pub fn accent(secondary: u32) -> Weights {
        Weights {
            primary: 0,
            secondary,
            tertiary: 0x02,
        }
    }

    /// An element with a primary weight and the base secondary and
    /// tertiary weights of a lower-case letter or an ideograph.
    pub fn base(primary: u32) -> Weights {
        Weights {
            primary,
            secondary: 0x20,
            tertiary: 0x02,
        }
    }
}

impl From<om_collate::Element> for Weights {
    fn from(e: om_collate::Element) -> Weights {
        Weights {
            primary: u32::from(e.primary) << SHIFT,
            secondary: u32::from(e.secondary),
            tertiary: e.tertiary,
        }
    }
}

/// The DUCET elements of `text`. The implicit weight pairs DUCET lists for
/// compatibility ideographs of CJK Extensions A to C (`FAD0` sorts as
/// U+22844) take the weights of the ideograph they stand for
/// ([`cjk::implicit`]).
pub(crate) fn ducet(text: &str) -> Vec<Weights> {
    let elements = om_collate::collation_elements(text);
    let mut out = Vec::with_capacity(elements.len());
    let mut i = 0;
    while i < elements.len() {
        let e = elements[i];
        if let (0xFB80..=0xFBBF, Some(next)) = (e.primary, elements.get(i + 1)) {
            let code = u32::from(e.primary - 0xFB80) << 15 | u32::from(next.primary & 0x7FFF);
            if let Some(weights) = char::from_u32(code).and_then(cjk::implicit) {
                out.extend(weights);
                i += 2;
                continue;
            }
        }
        out.push(e.into());
        i += 1;
    }
    out
}

/// The DUCET primary weight of `c` (its first element's), shifted.
pub(crate) fn primary_of(c: char) -> u32 {
    om_collate::collation_elements(c.encode_utf8(&mut [0; 4]))
        .first()
        .map_or(0, |e| u32::from(e.primary) << SHIFT)
}
