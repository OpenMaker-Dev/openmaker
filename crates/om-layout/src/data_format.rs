//! Per-object `KFormat` (`0d`) data presentation, independent of field values.
//!
//! Classic extended attributes retain a font-height prefix followed by numeric,
//! date and time settings. Only observed fixed layouts are decoded. Unknown
//! flags and trailing bytes remain available in `raw`. Storage and supported
//! flags follow `docs/re/data-format.md` (native read/XML/formatter evidence).

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NumberFormat {
    pub flags: u16,
    pub character_style: u8,
    pub negative_style: u8,
    /// Up to six UTF-16 units, used as one prefix or suffix.
    pub symbol: String,
    pub thousands_separator: char,
    pub decimal_separator: char,
    /// Native `decimalDigits` is signed; retain negative values verbatim.
    pub decimal_places: i16,
    pub negative_color: [u8; 4],
    pub true_string: String,
    pub false_string: String,
}

impl NumberFormat {
    pub fn as_entered(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn grouping(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn show_symbol(&self) -> bool {
        self.flags & 4 != 0
    }
    pub fn fixed_decimals(&self) -> bool {
        self.flags & 8 != 0
    }
    pub fn symbol_before(&self) -> bool {
        self.flags & 0x0800 != 0
    }
    pub fn negative_before_symbol(&self) -> bool {
        self.flags & 0x0400 != 0
    }
    pub fn hide_zero(&self) -> bool {
        self.flags & 0x2000 != 0
    }
    pub fn boolean(&self) -> bool {
        self.flags & 0x20 != 0
    }
    pub fn general(&self) -> bool {
        self.flags & 0x100 != 0
    }
    pub fn percent(&self) -> bool {
        self.flags & 0x14 == 0x14
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DateFormat {
    /// Packed format/character/month/day style bytes, in their stored order.
    pub flags: u32,
    pub separator: char,
    /// `International::DateElement`, documented in file-metadata.md 2.3.
    pub elements: [u8; 4],
    pub separators: [String; 5],
}

impl DateFormat {
    /// 0 as entered, 1 custom, 2 numeric short year, 3 and 4 named month,
    /// 5 and 6 named weekday and month, 7 numeric full year, 8 Short System
    /// Date, 9 Long System Date.
    pub fn format(&self) -> u8 {
        (self.flags >> 24) as u8
    }
    pub fn character_style(&self) -> u8 {
        (self.flags >> 16) as u8
    }
    pub fn month_style(&self) -> u8 {
        (self.flags >> 8) as u8
    }
    pub fn day_style(&self) -> u8 {
        self.flags as u8
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimeFormat {
    pub flags: u16,
    pub character_style: u8,
    pub hour_style: u8,
    pub minute_second_style: u8,
    pub separator: char,
    pub am: String,
    pub pm: String,
    /// The optional marker used by the 24-hour format (up to ten units).
    pub marker_24h: String,
}

impl TimeFormat {
    pub fn as_entered(&self) -> bool {
        self.flags & 8 != 0
    }
    pub fn show_hours(&self) -> bool {
        self.flags & 1 != 0
    }
    pub fn show_minutes(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn show_seconds(&self) -> bool {
        self.flags & 4 != 0
    }
    pub fn clock_24h(&self) -> bool {
        self.flags & 0x10 != 0
    }
    pub fn marker_before(&self) -> bool {
        self.flags & (if self.clock_24h() { 0x20 } else { 0x40 }) != 0
    }
    pub fn use_separator(&self) -> bool {
        self.flags & 0x80 != 0
    }
    pub fn system_format(&self) -> bool {
        self.flags & 0x600 != 0
    }
}

/// Where a field object puts a picture in its frame (`graphicFormat`,
/// data-format.md "Graphic format"): the flags of Claris's DDR XML grammar
/// as the `u16` at offset 210 of a field object's 213-byte `KFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicFormat(pub u16);

/// One axis of [`GraphicFormat`]'s alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicAlign {
    /// Left or top.
    Start,
    Center,
    /// Right or bottom.
    End,
}

impl Default for GraphicFormat {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl GraphicFormat {
    pub const SHRINK: u16 = 0x0001;
    pub const GROW: u16 = 0x0002;
    pub const PROPORTIONS: u16 = 0x0004;
    pub const LEFT: u16 = 0x0008;
    pub const TOP: u16 = 0x0010;
    pub const RIGHT: u16 = 0x0020;
    pub const COLORIZE: u16 = 0x0040;
    pub const BOTTOM: u16 = 0x0080;
    /// "Optimize for: Interactive content" (containers.md 6.6): the field
    /// shows a PDF to scroll and page through, and plays audio and video,
    /// rather than showing a picture.
    pub const INTERACTIVE: u16 = 0x0100;
    /// "Start playback automatically" of an interactive container
    /// (containers.md 6.6).
    pub const AUTOPLAY: u16 = 0x0200;
    /// Reduce to fit, keeping proportions, centered: what every field
    /// object of the Contacts starter but its large photo stores.
    pub const DEFAULT: GraphicFormat = GraphicFormat(Self::SHRINK | Self::PROPORTIONS);

    /// The format a field object's `KFormat` value stores; `None` for
    /// values without it (the 2-byte label form and the 210-byte form).
    pub fn decode(kformat: &[u8]) -> Option<GraphicFormat> {
        (kformat.len() == 213)
            .then(|| GraphicFormat(u16::from_be_bytes([kformat[210], kformat[211]])))
    }

    pub fn shrink(self) -> bool {
        self.0 & Self::SHRINK != 0
    }

    pub fn grow(self) -> bool {
        self.0 & Self::GROW != 0
    }

    pub fn proportions(self) -> bool {
        self.0 & Self::PROPORTIONS != 0
    }

    /// "Optimize for: Interactive content" rather than images.
    pub fn interactive(self) -> bool {
        self.0 & Self::INTERACTIVE != 0
    }

    /// "Start playback automatically" (of an interactive container).
    pub fn autoplay(self) -> bool {
        self.0 & Self::AUTOPLAY != 0
    }

    /// Left, right, else centered. Both bits set reads as left (unmeasured).
    pub fn horizontal(self) -> GraphicAlign {
        Self::align(self.0, Self::LEFT, Self::RIGHT)
    }

    /// Top, bottom, else centered. Both bits set reads as top (unmeasured).
    pub fn vertical(self) -> GraphicAlign {
        Self::align(self.0, Self::TOP, Self::BOTTOM)
    }

    fn align(bits: u16, start: u16, end: u16) -> GraphicAlign {
        if bits & start != 0 {
            GraphicAlign::Start
        } else if bits & end != 0 {
            GraphicAlign::End
        } else {
            GraphicAlign::Center
        }
    }

    /// Where a picture of `picture` (width, height) goes in a frame of
    /// `frame`: its left and top relative to the frame's, its width and
    /// height, all in the same unit. Neither shrink nor grow keeps the
    /// picture's size and crops it to the frame; shrink reduces a picture
    /// that does not fit, grow enlarges one that is smaller, both make it
    /// fit. With proportions the limiting side fits and the other follows;
    /// without, each side fits on its own. The result can extend past the
    /// frame, which clips it. Empty or non-finite sizes place nothing.
    pub fn place(self, picture: (f64, f64), frame: (f64, f64)) -> [f64; 4] {
        let (w, h) = picture;
        let (fw, fh) = (frame.0.max(0.0), frame.1.max(0.0));
        if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite())
            || !(fw.is_finite() && fh.is_finite())
        {
            return [0.0; 4];
        }
        let side = |size: f64, fit: f64| match (self.shrink(), self.grow()) {
            (false, false) => size,
            (true, false) => size.min(fit),
            (false, true) => size.max(fit),
            (true, true) => fit,
        };
        let (pw, ph) = if self.proportions() {
            let fit = (fw / w).min(fh / h);
            let scale = side(1.0, fit);
            (w * scale, h * scale)
        } else {
            (side(w, fw), side(h, fh))
        };
        let offset = |align: GraphicAlign, size: f64, frame: f64| match align {
            GraphicAlign::Start => 0.0,
            GraphicAlign::Center => (frame - size) / 2.0,
            GraphicAlign::End => frame - size,
        };
        [
            offset(self.horizontal(), pw, fw),
            offset(self.vertical(), ph, fh),
            pw,
            ph,
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataFormat {
    pub font_height: u16,
    pub number: NumberFormat,
    pub date: DateFormat,
    pub time: TimeFormat,
    pub raw: Vec<u8>,
}

impl DataFormat {
    /// The text metric preceding optional number/date/time settings. Native
    /// XML calls it `fontHeight`; negative signed sentinels are not heights.
    pub fn font_height(bytes: &[u8]) -> Option<u16> {
        let height = i16::from_be_bytes(bytes.get(..2)?.try_into().ok()?);
        (height > 0).then_some(height as u16)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if !matches!(bytes.len(), 210 | 213) {
            return None;
        }
        let word = |at| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
        let long = |at| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        let character = |at| char::from_u32(u32::from(word(at)?));
        let string = |at, len| {
            let units: Vec<_> = bytes
                .get(at..at + len)?
                .chunks_exact(2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
                .take_while(|unit| *unit != 0)
                .collect();
            String::from_utf16(&units).ok()
        };
        Some(Self {
            font_height: word(0)?,
            number: NumberFormat {
                flags: word(2)?,
                character_style: bytes[4],
                negative_style: bytes[5],
                symbol: string(6, 12)?,
                thousands_separator: character(18)?,
                decimal_separator: character(20)?,
                decimal_places: i16::from_be_bytes([bytes[26], bytes[27]]),
                negative_color: bytes[22..26].try_into().ok()?,
                true_string: string(28, 16)?,
                false_string: string(44, 16)?,
            },
            date: DateFormat {
                flags: long(60)?,
                separator: character(64)?,
                elements: bytes[66..70].try_into().ok()?,
                separators: [
                    string(70, 16)?,
                    string(86, 16)?,
                    string(102, 16)?,
                    string(118, 16)?,
                    string(134, 16)?,
                ],
            },
            time: TimeFormat {
                flags: word(150)?,
                character_style: bytes[152],
                hour_style: bytes[153],
                minute_second_style: bytes[154],
                separator: character(156)?,
                am: string(158, 16)?,
                pm: string(174, 16)?,
                marker_24h: string(190, 20)?,
            },
            raw: bytes.to_vec(),
        })
    }

    /// The settings FileMaker Pro 26.0.3 stores for a field object or table
    /// view column it adds (layout-model.md 10.14, data-format.md "A new
    /// field's record"): General numbers, as-entered dates and times, the
    /// symbol, separators and strings Pro keeps for the other formats, and
    /// graphic format 5 (shrink, keep proportions). Its text height is 0,
    /// the "no height" every field record of the starter solutions carries;
    /// Pro writes its theme's height there, which OpenMaker does not derive.
    /// The bytes no field explains (the time style's reserved byte, the last
    /// byte) are 0, as in every record seen.
    pub fn new_field() -> Self {
        let mut raw = vec![0; 213];
        raw[210..212].copy_from_slice(&NEW_FIELD_GRAPHIC.to_be_bytes());
        Self {
            font_height: 0,
            number: NumberFormat {
                flags: 0x0900,
                character_style: 0,
                negative_style: 0,
                symbol: "$".into(),
                thousands_separator: ',',
                decimal_separator: '.',
                decimal_places: 2,
                negative_color: [0xdd, 0, 0, 0],
                true_string: "Yes".into(),
                false_string: "No".into(),
            },
            date: DateFormat {
                flags: 0,
                separator: '/',
                elements: [3, 6, 1, 8],
                separators: [
                    String::new(),
                    ", ".into(),
                    " ".into(),
                    ", ".into(),
                    String::new(),
                ],
            },
            time: TimeFormat {
                flags: 0x008f,
                character_style: 0,
                hour_style: 0,
                minute_second_style: 1,
                separator: ':',
                am: " AM".into(),
                pm: " PM".into(),
                marker_24h: String::new(),
            },
            raw,
        }
    }

    /// The record, written field by field at the offsets data-format.md
    /// gives: the text height, the numeric, date and time streams, each
    /// string zero-padded in its fixed array. The bytes no field explains
    /// (the time style's reserved byte at 155, and from 210 on the graphic
    /// format and the last byte) are kept from [`DataFormat::raw`] when it
    /// is a whole record, else 0. `None` when a string or a character does
    /// not fit its array: nothing is cut short.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let mut out = match self.raw.len() {
            210 | 213 => self.raw.clone(),
            _ => vec![0; 213],
        };
        let word = |out: &mut Vec<u8>, at: usize, v: u16| {
            out[at..at + 2].copy_from_slice(&v.to_be_bytes());
        };
        let character = |c: char| u16::try_from(u32::from(c)).ok();
        let string = |out: &mut Vec<u8>, at: usize, len: usize, text: &str| {
            let units: Vec<u16> = text.encode_utf16().collect();
            // Each array keeps a zero unit after its string
            // (data-format.md): a string that fills it is refused too.
            if (units.len() + 1) * 2 > len || units.contains(&0) {
                return None;
            }
            out[at..at + len].fill(0);
            for (i, unit) in units.iter().enumerate() {
                out[at + 2 * i..at + 2 * i + 2].copy_from_slice(&unit.to_be_bytes());
            }
            Some(())
        };
        word(&mut out, 0, self.font_height);
        let n = &self.number;
        word(&mut out, 2, n.flags);
        out[4] = n.character_style;
        out[5] = n.negative_style;
        string(&mut out, 6, 12, &n.symbol)?;
        word(&mut out, 18, character(n.thousands_separator)?);
        word(&mut out, 20, character(n.decimal_separator)?);
        out[22..26].copy_from_slice(&n.negative_color);
        out[26..28].copy_from_slice(&n.decimal_places.to_be_bytes());
        string(&mut out, 28, 16, &n.true_string)?;
        string(&mut out, 44, 16, &n.false_string)?;
        let d = &self.date;
        out[60..64].copy_from_slice(&d.flags.to_be_bytes());
        word(&mut out, 64, character(d.separator)?);
        out[66..70].copy_from_slice(&d.elements);
        for (i, separator) in d.separators.iter().enumerate() {
            string(&mut out, 70 + 16 * i, 16, separator)?;
        }
        let t = &self.time;
        word(&mut out, 150, t.flags);
        out[152] = t.character_style;
        out[153] = t.hour_style;
        out[154] = t.minute_second_style;
        word(&mut out, 156, character(t.separator)?);
        string(&mut out, 158, 16, &t.am)?;
        string(&mut out, 174, 16, &t.pm)?;
        string(&mut out, 190, 20, &t.marker_24h)?;
        Some(out)
    }
}

/// The graphic format of a new field's record: shrink and keep proportions
/// (`0x0005`), as on every field object but the large containers of the
/// starter solutions and on Pro 26's new records (data-format.md).
pub const NEW_FIELD_GRAPHIC: u16 = 0x0005;

#[cfg(test)]
mod tests {
    use super::*;

    /// A field record FileMaker Pro 26.0.3 wrote for a column it added
    /// (the twins of #886; the text height of the theme, 14, first), and
    /// the most common field record of the English starter solutions.
    fn pro_new_column() -> Vec<u8> {
        let mut bytes = vec![0; 213];
        let put = |bytes: &mut Vec<u8>, at: usize, data: &[u8]| {
            bytes[at..at + data.len()].copy_from_slice(data)
        };
        let units =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_be_bytes).collect() };
        put(&mut bytes, 0, &[0x00, 0x0e, 0x09, 0x00]);
        put(&mut bytes, 6, &units("$"));
        put(&mut bytes, 18, &units(",."));
        put(&mut bytes, 22, &[0xdd, 0, 0, 0, 0, 2]);
        put(&mut bytes, 28, &units("Yes"));
        put(&mut bytes, 44, &units("No"));
        put(&mut bytes, 64, &units("/"));
        put(&mut bytes, 66, &[3, 6, 1, 8]);
        put(&mut bytes, 86, &units(", "));
        put(&mut bytes, 102, &units(" "));
        put(&mut bytes, 118, &units(", "));
        put(&mut bytes, 150, &[0x00, 0x8f, 0, 0, 1]);
        put(&mut bytes, 156, &units(":"));
        put(&mut bytes, 158, &units(" AM"));
        put(&mut bytes, 174, &units(" PM"));
        put(&mut bytes, 210, &[0, 5, 0]);
        bytes
    }

    /// Records as FileMaker wrote them read and write back unchanged, field
    /// by field; a new field's record is Pro's but for the text height; a
    /// string too long for its array is not written.
    #[test]
    fn records_write_back_field_by_field() {
        let pro = pro_new_column();
        let decoded = DataFormat::decode(&pro).unwrap();
        assert_eq!(decoded.font_height, 14);
        assert_eq!(decoded.encode().as_deref(), Some(&pro[..]));
        let mut starter = pro.clone();
        starter[0..2].fill(0);
        starter[6..8].copy_from_slice(&[0, b'#']);
        starter[27] = 0;
        starter[60..150].fill(0);
        starter[154] = 0;
        let decoded = DataFormat::decode(&starter).unwrap();
        assert_eq!(decoded.encode().as_deref(), Some(&starter[..]));
        let mut fresh = pro.clone();
        fresh[0..2].fill(0);
        assert_eq!(
            DataFormat::new_field().encode().as_deref(),
            Some(&fresh[..])
        );
        let mut long = DataFormat::new_field();
        long.number.symbol = "seven!!".into();
        assert_eq!(long.encode(), None);
        // Six units fill the symbol's array and leave no terminator.
        long.number.symbol = "ABCDEF".into();
        assert_eq!(long.encode(), None);
        long.number.symbol = "ABCDE".into();
        assert!(long.encode().is_some());
    }

    #[test]
    fn classic_data_format_keeps_numeric_settings_and_uninterpreted_metadata() {
        let mut bytes = vec![0; 213];
        bytes[0..2].copy_from_slice(&11u16.to_be_bytes());
        bytes[2..4].copy_from_slice(&0x200cu16.to_be_bytes());
        bytes[6..8].copy_from_slice(&('"' as u16).to_be_bytes());
        bytes[8..12].copy_from_slice(&[0, 32, 0, 88]);
        bytes[18..22].copy_from_slice(&[0, 44, 0, 46]);
        bytes[26..28].copy_from_slice(&3u16.to_be_bytes());
        bytes[64..70].copy_from_slice(&[0, 47, 4, 1, 7, 0]);
        bytes[156..158].copy_from_slice(&[0, 58]);
        bytes[150..152].copy_from_slice(&0x0083u16.to_be_bytes());
        bytes[212] = 0xa5;
        let format = DataFormat::decode(&bytes).unwrap();
        assert_eq!(format.font_height, 11);
        assert!(format.number.show_symbol());
        assert!(format.number.fixed_decimals());
        assert!(!format.number.symbol_before());
        assert!(!format.number.as_entered());
        assert_eq!(format.number.symbol, "\" X");
        assert_eq!(format.number.decimal_places, 3);
        assert_eq!(format.date.elements, [4, 1, 7, 0]);
        assert_eq!(format.time.separator, ':');
        assert!(format.time.show_hours());
        assert!(format.time.show_minutes());
        assert!(!format.time.show_seconds());
        assert_eq!(format.raw, bytes);
        assert!(DataFormat::decode(&bytes[..209]).is_none());
        assert!(DataFormat::decode(&[0, 11]).is_none());
        assert_eq!(DataFormat::font_height(&[0, 11]), Some(11));
        assert_eq!(GraphicFormat::decode(&bytes), Some(GraphicFormat(0)));
        bytes[210..213].copy_from_slice(&[0, 7, 0]);
        assert_eq!(GraphicFormat::decode(&bytes), Some(GraphicFormat(7)));
        assert_eq!(GraphicFormat::decode(&bytes[..210]), None);
        assert_eq!(DataFormat::font_height(&[0xff, 0xff]), None);
        assert_eq!(DataFormat::font_height(&[0]), None);
    }

    #[test]
    fn numeric_decimal_places_keep_the_signed_native_value() {
        let mut bytes = vec![0; 210];
        bytes[26..28].copy_from_slice(&(-1i16).to_be_bytes());
        let format = DataFormat::decode(&bytes).unwrap();
        assert_eq!(i32::from(format.number.decimal_places), -1);
        assert_eq!(format.raw, bytes);
    }

    #[test]
    fn as_entered_and_grouping_are_independent_of_symbol_position() {
        let mut number = NumberFormat {
            flags: 0x0801,
            symbol: "$".into(),
            thousands_separator: ',',
            decimal_separator: '.',
            decimal_places: 2,
            ..Default::default()
        };
        assert!(number.as_entered());
        assert!(!number.grouping());
        number.flags = 0x2802;
        assert!(number.grouping());
        assert!(!number.as_entered());
        assert!(!number.show_symbol());
        assert!(number.symbol_before());
    }

    /// A 40 x 20 picture in a 20 x 20 frame and a 10 x 5 picture in a
    /// 20 x 20 frame, under each scaling and some alignments: [left, top,
    /// width, height] relative to the frame.
    /// The format FileMaker Pro 26.0.3 stored for one container field
    /// object saved three ways (containers.md 6.6): its 213-byte record's
    /// bytes 210-211 `00 07` for images, `01 07` for interactive content,
    /// `03 07` with playback starting by itself.
    #[test]
    fn a_container_is_optimized_for_images_or_interactive_content() {
        let record = |high: u8| {
            let mut bytes = [0u8; 213];
            bytes[210] = high;
            bytes[211] = 0x07;
            bytes
        };
        for (high, interactive, autoplay) in [(0, false, false), (1, true, false), (3, true, true)]
        {
            let format = GraphicFormat::decode(&record(high)).unwrap();
            assert_eq!(
                (format.interactive(), format.autoplay()),
                (interactive, autoplay),
                "{format:?}"
            );
            // The scaling stays: reduce or enlarge, keeping proportions.
            assert!(format.shrink() && format.grow() && format.proportions());
        }
    }

    #[test]
    fn graphic_formats_place_pictures() {
        const P: u16 = GraphicFormat::PROPORTIONS;
        let large = (40.0, 20.0);
        let small = (10.0, 5.0);
        let frame = (20.0, 20.0);
        for (bits, picture, expected) in [
            // Crop: the picture's own size, centered.
            (0, large, [-10.0, 0.0, 40.0, 20.0]),
            (P, small, [5.0, 7.5, 10.0, 5.0]),
            // Crop, aligned left and bottom.
            (
                GraphicFormat::LEFT | GraphicFormat::BOTTOM,
                large,
                [0.0, 0.0, 40.0, 20.0],
            ),
            (
                GraphicFormat::RIGHT | GraphicFormat::TOP,
                small,
                [10.0, 0.0, 10.0, 5.0],
            ),
            // Reduce: a large picture fits, a small one keeps its size.
            (1 | P, large, [0.0, 5.0, 20.0, 10.0]),
            (1 | P, small, [5.0, 7.5, 10.0, 5.0]),
            (1, large, [0.0, 0.0, 20.0, 20.0]),
            (1, small, [5.0, 7.5, 10.0, 5.0]),
            // Enlarge: a small picture fits, a large one keeps its size.
            (2 | P, small, [0.0, 5.0, 20.0, 10.0]),
            (2 | P, large, [-10.0, 0.0, 40.0, 20.0]),
            (2, large, [-10.0, 0.0, 40.0, 20.0]),
            (2, small, [0.0, 0.0, 20.0, 20.0]),
            // Reduce or enlarge: always fits.
            (3 | P, large, [0.0, 5.0, 20.0, 10.0]),
            (3 | P | GraphicFormat::TOP, small, [0.0, 0.0, 20.0, 10.0]),
            (3, large, [0.0, 0.0, 20.0, 20.0]),
        ] {
            assert_eq!(
                GraphicFormat(bits).place(picture, frame),
                expected,
                "{bits:#x} {picture:?}"
            );
        }
        assert_eq!(GraphicFormat(7).place((0.0, 5.0), frame), [0.0; 4]);
        assert_eq!(GraphicFormat(7).place((f64::NAN, 5.0), frame), [0.0; 4]);
        assert_eq!(
            GraphicFormat(7).place((4.0, 2.0), (0.0, 0.0)),
            [0.0, 0.0, 0.0, 0.0]
        );
    }
}
