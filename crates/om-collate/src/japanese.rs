//! Japanese (index language 31) text keys (query-program.md 7.2,
//! "Japanese"). The Japanese tailoring gives kana, their sound marks and the
//! kanji of JIS X 0208 level 1 weights of its own; every other character
//! keys from the table, as in English ([`crate::text_key`]). FileMaker keys
//! one code point at a time, so a text keys as its characters' weights in
//! order.
//!
//! A kanji outside level 1 (level 2, JIS X 0212, the extensions and the
//! compatibility ideographs) has a table weight whose order is not derived;
//! the combining semi-voiced mark U+309A and the halfwidth sound marks are
//! not established. A text holding one has no key ([`japanese_key`]), so
//! whoever needs its key refuses that text.

/// Each kana's weights in a Japanese key: hiragana `40xx`, katakana `44xx`,
/// halfwidth katakana the fullwidth ones', a voiced kana its plain kana then
/// `01f4` and a semi-voiced one then `01f8`, and the kana iteration marks.
/// From the value-index keys FileMaker Server's engine wrote for the probe's
/// kana strings (catalogs/index-key-vectors.tsv, `kana` rows), in code point
/// order.
const KANA: &[(char, &[u16])] = &[
    ('\u{3041}', &[0x4000]),         // ぁ
    ('\u{3042}', &[0x4001]),         // あ
    ('\u{3043}', &[0x4002]),         // ぃ
    ('\u{3044}', &[0x4003]),         // い
    ('\u{3045}', &[0x4004]),         // ぅ
    ('\u{3046}', &[0x4005]),         // う
    ('\u{3047}', &[0x4007]),         // ぇ
    ('\u{3048}', &[0x4008]),         // え
    ('\u{3049}', &[0x4009]),         // ぉ
    ('\u{304a}', &[0x400a]),         // お
    ('\u{304b}', &[0x400c]),         // か
    ('\u{304c}', &[0x400c, 0x01f4]), // が
    ('\u{304d}', &[0x400e]),         // き
    ('\u{304e}', &[0x400e, 0x01f4]), // ぎ
    ('\u{304f}', &[0x4010]),         // く
    ('\u{3050}', &[0x4010, 0x01f4]), // ぐ
    ('\u{3051}', &[0x4013]),         // け
    ('\u{3052}', &[0x4013, 0x01f4]), // げ
    ('\u{3053}', &[0x4015]),         // こ
    ('\u{3054}', &[0x4015, 0x01f4]), // ご
    ('\u{3055}', &[0x4017]),         // さ
    ('\u{3056}', &[0x4017, 0x01f4]), // ざ
    ('\u{3057}', &[0x4019]),         // し
    ('\u{3058}', &[0x4019, 0x01f4]), // じ
    ('\u{3059}', &[0x401b]),         // す
    ('\u{305a}', &[0x401b, 0x01f4]), // ず
    ('\u{305b}', &[0x401d]),         // せ
    ('\u{305c}', &[0x401d, 0x01f4]), // ぜ
    ('\u{305d}', &[0x401f]),         // そ
    ('\u{305e}', &[0x401f, 0x01f4]), // ぞ
    ('\u{305f}', &[0x4021]),         // た
    ('\u{3060}', &[0x4021, 0x01f4]), // だ
    ('\u{3061}', &[0x4023]),         // ち
    ('\u{3062}', &[0x4023, 0x01f4]), // ぢ
    ('\u{3063}', &[0x4025]),         // っ
    ('\u{3064}', &[0x4026]),         // つ
    ('\u{3065}', &[0x4026, 0x01f4]), // づ
    ('\u{3066}', &[0x4028]),         // て
    ('\u{3067}', &[0x4028, 0x01f4]), // で
    ('\u{3068}', &[0x402a]),         // と
    ('\u{3069}', &[0x402a, 0x01f4]), // ど
    ('\u{306a}', &[0x402c]),         // な
    ('\u{306b}', &[0x402d]),         // に
    ('\u{306c}', &[0x402e]),         // ぬ
    ('\u{306d}', &[0x402f]),         // ね
    ('\u{306e}', &[0x4030]),         // の
    ('\u{306f}', &[0x4031]),         // は
    ('\u{3070}', &[0x4031, 0x01f4]), // ば
    ('\u{3071}', &[0x4031, 0x01f8]), // ぱ
    ('\u{3072}', &[0x4034]),         // ひ
    ('\u{3073}', &[0x4034, 0x01f4]), // び
    ('\u{3074}', &[0x4034, 0x01f8]), // ぴ
    ('\u{3075}', &[0x4037]),         // ふ
    ('\u{3076}', &[0x4037, 0x01f4]), // ぶ
    ('\u{3077}', &[0x4037, 0x01f8]), // ぷ
    ('\u{3078}', &[0x403a]),         // へ
    ('\u{3079}', &[0x403a, 0x01f4]), // べ
    ('\u{307a}', &[0x403a, 0x01f8]), // ぺ
    ('\u{307b}', &[0x403d]),         // ほ
    ('\u{307c}', &[0x403d, 0x01f4]), // ぼ
    ('\u{307d}', &[0x403d, 0x01f8]), // ぽ
    ('\u{307e}', &[0x4040]),         // ま
    ('\u{307f}', &[0x4041]),         // み
    ('\u{3080}', &[0x4042]),         // む
    ('\u{3081}', &[0x4043]),         // め
    ('\u{3082}', &[0x4044]),         // も
    ('\u{3083}', &[0x4045]),         // ゃ
    ('\u{3084}', &[0x4046]),         // や
    ('\u{3085}', &[0x4047]),         // ゅ
    ('\u{3086}', &[0x4048]),         // ゆ
    ('\u{3087}', &[0x4049]),         // ょ
    ('\u{3088}', &[0x404a]),         // よ
    ('\u{3089}', &[0x404c]),         // ら
    ('\u{308a}', &[0x404d]),         // り
    ('\u{308b}', &[0x404e]),         // る
    ('\u{308c}', &[0x404f]),         // れ
    ('\u{308d}', &[0x4050]),         // ろ
    ('\u{308e}', &[0x4051]),         // ゎ
    ('\u{308f}', &[0x4052]),         // わ
    ('\u{3090}', &[0x4053]),         // ゐ
    ('\u{3091}', &[0x4054]),         // ゑ
    ('\u{3092}', &[0x4055]),         // を
    ('\u{3093}', &[0x4056]),         // ん
    ('\u{3094}', &[0x4005, 0x01f4]), // ゔ
    ('\u{3095}', &[0x400b]),         // ゕ
    ('\u{3096}', &[0x4012]),         // ゖ
    ('\u{309d}', &[0x11dc]),         // ゝ
    ('\u{309e}', &[0x11dc, 0x01f4]), // ゞ
    ('\u{30a1}', &[0x4400]),         // ァ
    ('\u{30a2}', &[0x4401]),         // ア
    ('\u{30a3}', &[0x4402]),         // ィ
    ('\u{30a4}', &[0x4403]),         // イ
    ('\u{30a5}', &[0x4404]),         // ゥ
    ('\u{30a6}', &[0x4405]),         // ウ
    ('\u{30a7}', &[0x4407]),         // ェ
    ('\u{30a8}', &[0x4408]),         // エ
    ('\u{30a9}', &[0x4409]),         // ォ
    ('\u{30aa}', &[0x440a]),         // オ
    ('\u{30ab}', &[0x440c]),         // カ
    ('\u{30ac}', &[0x440c, 0x01f4]), // ガ
    ('\u{30ad}', &[0x440e]),         // キ
    ('\u{30ae}', &[0x440e, 0x01f4]), // ギ
    ('\u{30af}', &[0x4411]),         // ク
    ('\u{30b0}', &[0x4411, 0x01f4]), // グ
    ('\u{30b1}', &[0x4414]),         // ケ
    ('\u{30b2}', &[0x4414, 0x01f4]), // ゲ
    ('\u{30b3}', &[0x4416]),         // コ
    ('\u{30b4}', &[0x4416, 0x01f4]), // ゴ
    ('\u{30b5}', &[0x4419]),         // サ
    ('\u{30b6}', &[0x4419, 0x01f4]), // ザ
    ('\u{30b7}', &[0x441c]),         // シ
    ('\u{30b8}', &[0x441c, 0x01f4]), // ジ
    ('\u{30b9}', &[0x441f]),         // ス
    ('\u{30ba}', &[0x441f, 0x01f4]), // ズ
    ('\u{30bb}', &[0x4421]),         // セ
    ('\u{30bc}', &[0x4421, 0x01f4]), // ゼ
    ('\u{30bd}', &[0x4423]),         // ソ
    ('\u{30be}', &[0x4423, 0x01f4]), // ゾ
    ('\u{30bf}', &[0x4425]),         // タ
    ('\u{30c0}', &[0x4425, 0x01f4]), // ダ
    ('\u{30c1}', &[0x4427]),         // チ
    ('\u{30c2}', &[0x4427, 0x01f4]), // ヂ
    ('\u{30c3}', &[0x4429]),         // ッ
    ('\u{30c4}', &[0x442a]),         // ツ
    ('\u{30c5}', &[0x442a, 0x01f4]), // ヅ
    ('\u{30c6}', &[0x442c]),         // テ
    ('\u{30c7}', &[0x442c, 0x01f4]), // デ
    ('\u{30c8}', &[0x442f]),         // ト
    ('\u{30c9}', &[0x442f, 0x01f4]), // ド
    ('\u{30ca}', &[0x4431]),         // ナ
    ('\u{30cb}', &[0x4432]),         // ニ
    ('\u{30cc}', &[0x4434]),         // ヌ
    ('\u{30cd}', &[0x4435]),         // ネ
    ('\u{30ce}', &[0x4436]),         // ノ
    ('\u{30cf}', &[0x4438]),         // ハ
    ('\u{30d0}', &[0x4438, 0x01f4]), // バ
    ('\u{30d1}', &[0x4438, 0x01f8]), // パ
    ('\u{30d2}', &[0x443c]),         // ヒ
    ('\u{30d3}', &[0x443c, 0x01f4]), // ビ
    ('\u{30d4}', &[0x443c, 0x01f8]), // ピ
    ('\u{30d5}', &[0x4440]),         // フ
    ('\u{30d6}', &[0x4440, 0x01f4]), // ブ
    ('\u{30d7}', &[0x4440, 0x01f8]), // プ
    ('\u{30d8}', &[0x4444]),         // ヘ
    ('\u{30d9}', &[0x4444, 0x01f4]), // ベ
    ('\u{30da}', &[0x4444, 0x01f8]), // ペ
    ('\u{30db}', &[0x4448]),         // ホ
    ('\u{30dc}', &[0x4448, 0x01f4]), // ボ
    ('\u{30dd}', &[0x4448, 0x01f8]), // ポ
    ('\u{30de}', &[0x444b]),         // マ
    ('\u{30df}', &[0x444c]),         // ミ
    ('\u{30e0}', &[0x444e]),         // ム
    ('\u{30e1}', &[0x444f]),         // メ
    ('\u{30e2}', &[0x4450]),         // モ
    ('\u{30e3}', &[0x4451]),         // ャ
    ('\u{30e4}', &[0x4452]),         // ヤ
    ('\u{30e5}', &[0x4453]),         // ュ
    ('\u{30e6}', &[0x4454]),         // ユ
    ('\u{30e7}', &[0x4455]),         // ョ
    ('\u{30e8}', &[0x4456]),         // ヨ
    ('\u{30e9}', &[0x4458]),         // ラ
    ('\u{30ea}', &[0x445a]),         // リ
    ('\u{30eb}', &[0x445c]),         // ル
    ('\u{30ec}', &[0x445e]),         // レ
    ('\u{30ed}', &[0x4460]),         // ロ
    ('\u{30ee}', &[0x4461]),         // ヮ
    ('\u{30ef}', &[0x4462]),         // ワ
    ('\u{30f0}', &[0x4464]),         // ヰ
    ('\u{30f1}', &[0x4466]),         // ヱ
    ('\u{30f2}', &[0x4468]),         // ヲ
    ('\u{30f3}', &[0x446a]),         // ン
    ('\u{30f4}', &[0x4405, 0x01f4]), // ヴ
    ('\u{30f5}', &[0x440b]),         // ヵ
    ('\u{30f6}', &[0x4413]),         // ヶ
    ('\u{30f7}', &[0x4462, 0x01f4]), // ヷ
    ('\u{30f8}', &[0x4464, 0x01f4]), // ヸ
    ('\u{30f9}', &[0x4466, 0x01f4]), // ヹ
    ('\u{30fa}', &[0x4468, 0x01f4]), // ヺ
    ('\u{30fd}', &[0x11de]),         // ヽ
    ('\u{30fe}', &[0x11de, 0x01f4]), // ヾ
    ('\u{ff66}', &[0x4468]),         // ｦ
    ('\u{ff67}', &[0x4400]),         // ｧ
    ('\u{ff68}', &[0x4402]),         // ｨ
    ('\u{ff69}', &[0x4404]),         // ｩ
    ('\u{ff6a}', &[0x4407]),         // ｪ
    ('\u{ff6b}', &[0x4409]),         // ｫ
    ('\u{ff6c}', &[0x4451]),         // ｬ
    ('\u{ff6d}', &[0x4453]),         // ｭ
    ('\u{ff6e}', &[0x4455]),         // ｮ
    ('\u{ff6f}', &[0x4429]),         // ｯ
    ('\u{ff70}', &[0x11dd]),         // ｰ
    ('\u{ff71}', &[0x4401]),         // ｱ
    ('\u{ff72}', &[0x4403]),         // ｲ
    ('\u{ff73}', &[0x4405]),         // ｳ
    ('\u{ff74}', &[0x4408]),         // ｴ
    ('\u{ff75}', &[0x440a]),         // ｵ
    ('\u{ff76}', &[0x440c]),         // ｶ
    ('\u{ff77}', &[0x440e]),         // ｷ
    ('\u{ff78}', &[0x4411]),         // ｸ
    ('\u{ff79}', &[0x4414]),         // ｹ
    ('\u{ff7a}', &[0x4416]),         // ｺ
    ('\u{ff7b}', &[0x4419]),         // ｻ
    ('\u{ff7c}', &[0x441c]),         // ｼ
    ('\u{ff7d}', &[0x441f]),         // ｽ
    ('\u{ff7e}', &[0x4421]),         // ｾ
    ('\u{ff7f}', &[0x4423]),         // ｿ
    ('\u{ff80}', &[0x4425]),         // ﾀ
    ('\u{ff81}', &[0x4427]),         // ﾁ
    ('\u{ff82}', &[0x442a]),         // ﾂ
    ('\u{ff83}', &[0x442c]),         // ﾃ
    ('\u{ff84}', &[0x442f]),         // ﾄ
    ('\u{ff85}', &[0x4431]),         // ﾅ
    ('\u{ff86}', &[0x4432]),         // ﾆ
    ('\u{ff87}', &[0x4434]),         // ﾇ
    ('\u{ff88}', &[0x4435]),         // ﾈ
    ('\u{ff89}', &[0x4436]),         // ﾉ
    ('\u{ff8a}', &[0x4438]),         // ﾊ
    ('\u{ff8b}', &[0x443c]),         // ﾋ
    ('\u{ff8c}', &[0x4440]),         // ﾌ
    ('\u{ff8d}', &[0x4444]),         // ﾍ
    ('\u{ff8e}', &[0x4448]),         // ﾎ
    ('\u{ff8f}', &[0x444b]),         // ﾏ
    ('\u{ff90}', &[0x444c]),         // ﾐ
    ('\u{ff91}', &[0x444e]),         // ﾑ
    ('\u{ff92}', &[0x444f]),         // ﾒ
    ('\u{ff93}', &[0x4450]),         // ﾓ
    ('\u{ff94}', &[0x4452]),         // ﾔ
    ('\u{ff95}', &[0x4454]),         // ﾕ
    ('\u{ff96}', &[0x4456]),         // ﾖ
    ('\u{ff97}', &[0x4458]),         // ﾗ
    ('\u{ff98}', &[0x445a]),         // ﾘ
    ('\u{ff99}', &[0x445c]),         // ﾙ
    ('\u{ff9a}', &[0x445e]),         // ﾚ
    ('\u{ff9b}', &[0x4460]),         // ﾛ
    ('\u{ff9c}', &[0x4462]),         // ﾜ
    ('\u{ff9d}', &[0x446a]),         // ﾝ
];

/// A combining or spacing voiced sound mark (U+3099, U+309B).
const VOICED: u16 = 0x01f4;
/// The spacing semi-voiced sound mark U+309C.
const SEMI_VOICED: u16 = 0x01f8;
/// `〆` (U+3006), which keys after `ン` (`446a`) where English gives it
/// two table weights: the probe's `゛゜゙゚ー々〆〇〻` word `々〆` is
/// `01 11d7 446b`.
const SHIME: u16 = 0x446b;

/// A JIS X 0208 level-1 kanji at row r, cell n weighs
/// `0x967E + (r − 1) × 94 + (n − 1)` (`一`, row 16, cell 76, is `9c4b`).
const LEVEL_1_BASE: u16 = 0x967e;

/// How a character keys in the Japanese form.
enum Weight {
    /// The tailoring's weights.
    Japanese(&'static [u16]),
    Kanji(u16),
    /// The table's weights, as in English.
    Table,
    /// No key: a kanji outside level 1, or a halfwidth sound mark.
    Unkeyed,
}

fn weight(c: char) -> Weight {
    if let Ok(i) = KANA.binary_search_by_key(&c, |(k, _)| *k) {
        return Weight::Japanese(KANA[i].1);
    }
    match c {
        '\u{3099}' | '\u{309b}' => return Weight::Japanese(&[VOICED]),
        '\u{309c}' => return Weight::Japanese(&[SEMI_VOICED]),
        '\u{3006}' => return Weight::Japanese(&[SHIME]),
        // U+309A after U+3099 added nothing in the probe, though 7.2 reads
        // it as 01f8: how it keys is open, as are the halfwidth marks.
        '\u{309a}' | '\u{ff9e}' | '\u{ff9f}' => return Weight::Unkeyed,
        _ => {}
    }
    if !ideograph(c) {
        return Weight::Table;
    }
    match jis_level_1(c) {
        Some(index) => Weight::Kanji(LEVEL_1_BASE + index),
        None => Weight::Unkeyed,
    }
}

/// A CJK ideograph: the unified blocks, their extensions and the
/// compatibility ideographs.
fn ideograph(c: char) -> bool {
    matches!(
        u32::from(c),
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x2_0000..=0x3_ffff
    )
}

/// `(row − 1) × 94 + (cell − 1)` of a JIS X 0208 level-1 kanji (rows 16 to
/// 47); `None` for any other character.
fn jis_level_1(c: char) -> Option<u16> {
    let mut buffer = [0; 4];
    let (bytes, _, unmappable) = encoding_rs::EUC_JP.encode(c.encode_utf8(&mut buffer));
    // EUC-JP writes row r, cell n as 0xA0 + r, 0xA0 + n.
    match *bytes {
        [lead @ 0xb0..=0xcf, trail @ 0xa1..=0xfe] if !unmappable => {
            Some(u16::from(lead - 0xa1) * 94 + u16::from(trail - 0xa1))
        }
        _ => None,
    }
}

/// The Japanese value-index key of `text`, before an index cuts it: `01`,
/// then each character's weights in order. `None` when `text` holds a
/// character without a key ([`Weight::Unkeyed`]).
pub fn japanese_key(text: &str) -> Option<Vec<u8>> {
    let mut key = vec![0x01];
    // A run of characters keyed from the table, as English keys them.
    let mut table = String::new();
    let flush = |table: &mut String, key: &mut Vec<u8>| {
        if !table.is_empty() {
            key.extend_from_slice(&crate::text_key(table)[1..]);
            table.clear();
        }
    };
    for c in text.chars() {
        match weight(c) {
            Weight::Table => table.push(c),
            Weight::Japanese(weights) => {
                flush(&mut table, &mut key);
                key.extend(weights.iter().flat_map(|w| w.to_be_bytes()));
            }
            Weight::Kanji(weight) => {
                flush(&mut table, &mut key);
                key.extend(weight.to_be_bytes());
            }
            Weight::Unkeyed => return None,
        }
    }
    flush(&mut table, &mut key);
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// query-program.md 7.2's examples: kana, a voiced and a semi-voiced
    /// kana, halfwidth katakana, level-1 kanji and Latin text; a level-2
    /// kanji has no key.
    #[test]
    fn japanese_keys_are_the_specs() {
        let key = |text| {
            japanese_key(text).map(|key| key.iter().map(|b| format!("{b:02x}")).collect::<String>())
        };
        assert_eq!(key("ば").as_deref(), Some("01403101f4"));
        assert_eq!(key("パ").as_deref(), Some("01443801f8"));
        assert_eq!(key("ﾊﾝｶｸ").as_deref(), Some("014438446a440c4411"));
        assert_eq!(key("一").as_deref(), Some("019c4b"));
        assert_eq!(key("中").as_deref(), Some("01a33f"));
        // A kana and a combining voiced mark key as the voiced kana; other
        // characters as in English.
        assert_eq!(key("は\u{3099}"), key("ば"));
        assert_eq!(japanese_key("abc"), Some(crate::text_key("abc")));
        assert_eq!(key("々〆").as_deref(), Some("0111d7446b"));
        assert_eq!(key("豈"), None);
        assert_eq!(key("は\u{309a}"), None);
        assert_eq!(key("ﾊﾞ"), None);
        assert!(KANA.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
