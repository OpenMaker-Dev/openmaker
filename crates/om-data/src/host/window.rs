//! The desktop window `Get` describes (calc-functions.md 6.3 O1, O5): the
//! screen, the usable desktop area, the window's frame and content area,
//! its zoom and bars, in points of the main screen.
//!
//! FileMaker Pro measures a window's position from the top left of the
//! desktop area (the screen less the menu bar and the Dock), not from the
//! screen's, and gives layout objects' bounds from the same origin. The
//! content area is what shows the layout: the window less its title bar,
//! status toolbar and layout bar (105 points of chrome on macOS with the
//! toolbar shown, 28 without).

use om_calc::{Number, Value};

use super::session::selector::*;

/// A rectangle in points: its top left corner and size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Frame {
    pub fn new(left: f64, top: f64, width: f64, height: f64) -> Frame {
        Frame {
            left,
            top,
            width,
            height,
        }
    }
}

/// What a desktop host knows about its window and screen.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowInfo {
    /// The main screen.
    pub screen: Frame,
    /// Bits per pixel (`Get ( ScreenDepth )`).
    pub depth: u32,
    /// Device pixels per point (`Get ( ScreenScaleFactor )`).
    pub scale: f64,
    /// The usable desktop area, in screen coordinates.
    pub desktop: Frame,
    /// The window's outer frame, in screen coordinates.
    pub frame: Frame,
    /// The area showing the layout, in screen coordinates.
    pub content: Frame,
    /// Percent (`Get ( WindowZoomLevel )`).
    pub zoom: u16,
    /// The status toolbar (`Get ( StatusAreaState )`), the menu bar and
    /// the text ruler.
    pub toolbar: bool,
    pub menubar: bool,
    pub text_ruler: bool,
    pub visible: bool,
    /// The window is the active one of the active application (a layout
    /// object's `isActive`).
    pub active: bool,
    /// `Get ( WindowStyle )`: 0 document, 1 floating document, 2 dialog,
    /// 3 card.
    pub style: i64,
    /// `Get ( WindowUUID )`: 32 upper-case hex digits, new for each window.
    pub uuid: String,
}

impl Default for WindowInfo {
    fn default() -> WindowInfo {
        WindowInfo {
            screen: Frame::default(),
            depth: 32,
            scale: 1.0,
            desktop: Frame::default(),
            frame: Frame::default(),
            content: Frame::default(),
            zoom: 100,
            toolbar: true,
            menubar: true,
            text_ruler: false,
            visible: true,
            active: false,
            style: 0,
            uuid: String::new(),
        }
    }
}

/// A coordinate as FileMaker Pro gives it: whole points, halves away from
/// zero (O5 at 150 %: 792.5 is `793`).
pub fn points(n: f64) -> i64 {
    n.round() as i64
}

impl WindowInfo {
    /// The content area's top left, from the desktop area's.
    pub fn content_origin(&self) -> (f64, f64) {
        (
            self.content.left - self.desktop.left,
            self.content.top - self.desktop.top,
        )
    }

    /// The zoom as a factor.
    pub fn scale_factor(&self) -> f64 {
        f64::from(self.zoom.max(1)) / 100.0
    }

    /// The window and screen selectors; `None` for the rest.
    pub fn get(&self, selector: u8) -> Option<Value> {
        let number = |n: i64| Some(Value::number(n));
        let flag = |b: bool| number(i64::from(b));
        match selector {
            SCREEN_DEPTH => number(i64::from(self.depth)),
            SCREEN_HEIGHT => number(points(self.screen.height)),
            SCREEN_WIDTH => number(points(self.screen.width)),
            SCREEN_SCALE_FACTOR => Some(Value::Number(Number::parse_text(
                &format!("{}", (self.scale * 1e6).round() / 1e6),
                '.',
                ',',
            ))),
            WINDOW_LEFT => number(points(self.frame.left - self.desktop.left)),
            WINDOW_TOP => number(points(self.frame.top - self.desktop.top)),
            WINDOW_WIDTH => number(points(self.frame.width)),
            WINDOW_HEIGHT => number(points(self.frame.height)),
            WINDOW_CONTENT_WIDTH => number(points(self.content.width)),
            WINDOW_CONTENT_HEIGHT => number(points(self.content.height)),
            WINDOW_DESKTOP_WIDTH => number(points(self.desktop.width)),
            WINDOW_DESKTOP_HEIGHT => number(points(self.desktop.height)),
            WINDOW_ZOOM_LEVEL => number(i64::from(self.zoom)),
            WINDOW_VISIBLE => flag(self.visible),
            WINDOW_STYLE => number(self.style),
            // Only FileMaker Go turns a window.
            WINDOW_ORIENTATION => number(-1),
            STATUS_AREA_STATE => flag(self.toolbar),
            MENUBAR_STATE => flag(self.menubar),
            TEXT_RULER_VISIBLE => flag(self.text_ruler),
            WINDOW_UUID => Some(Value::text(&self.uuid)),
            _ => None,
        }
    }
}

/// A new window's UUID: 32 upper-case hex digits.
pub fn new_window_uuid() -> String {
    super::session::uuid().replace('-', "")
}

#[cfg(test)]
mod tests {
    use om_calc::{Ctx, Date, Locale};

    use super::*;

    /// FileMaker Pro's window of O1: moved to 100, 120 on a 1920 x 1080
    /// screen whose desktop area starts 66 points right and 30 down.
    pub(crate) fn probe_window(toolbar: bool) -> WindowInfo {
        let chrome = if toolbar { 105.0 } else { 28.0 };
        let height = if toolbar { 900.0 } else { 848.0 };
        WindowInfo {
            screen: Frame::new(0.0, 0.0, 1920.0, 1080.0),
            desktop: Frame::new(66.0, 30.0, 1854.0, 1050.0),
            frame: Frame::new(166.0, 150.0, 1100.0, height),
            content: Frame::new(166.0, 150.0 + chrome, 1100.0, height - chrome),
            toolbar,
            uuid: "CB1FCC2C5F55438CA3EC703A52FA87BA".into(),
            ..WindowInfo::default()
        }
    }

    fn text(window: &WindowInfo, selector: u8) -> String {
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
        window
            .get(selector)
            .map(|v| v.to_text(&cx).into_string())
            .unwrap_or_default()
    }

    #[test]
    fn the_window_answers_from_the_desktop_area() {
        let w = probe_window(true);
        for (selector, want) in [
            (SCREEN_DEPTH, "32"),
            (SCREEN_HEIGHT, "1080"),
            (SCREEN_WIDTH, "1920"),
            (SCREEN_SCALE_FACTOR, "1"),
            (STATUS_AREA_STATE, "1"),
            (WINDOW_HEIGHT, "900"),
            (WINDOW_LEFT, "100"),
            (WINDOW_TOP, "120"),
            (WINDOW_VISIBLE, "1"),
            (WINDOW_WIDTH, "1100"),
            (WINDOW_DESKTOP_HEIGHT, "1050"),
            (WINDOW_DESKTOP_WIDTH, "1854"),
            (WINDOW_CONTENT_HEIGHT, "795"),
            (WINDOW_CONTENT_WIDTH, "1100"),
            (WINDOW_ZOOM_LEVEL, "100"),
            (WINDOW_STYLE, "0"),
            (WINDOW_ORIENTATION, "-1"),
            (MENUBAR_STATE, "1"),
            (TEXT_RULER_VISIBLE, "0"),
            (WINDOW_UUID, "CB1FCC2C5F55438CA3EC703A52FA87BA"),
        ] {
            assert_eq!(text(&w, selector), want, "selector {selector}");
        }
        // The toolbar hidden: the window loses the toolbar's 52 points and
        // the content gains the layout bar's 25.
        let w = probe_window(false);
        for (selector, want) in [
            (STATUS_AREA_STATE, "0"),
            (WINDOW_TOP, "120"),
            (WINDOW_LEFT, "100"),
            (WINDOW_HEIGHT, "848"),
            (WINDOW_WIDTH, "1100"),
            (WINDOW_CONTENT_HEIGHT, "820"),
            (WINDOW_CONTENT_WIDTH, "1100"),
            (WINDOW_DESKTOP_HEIGHT, "1050"),
            (WINDOW_DESKTOP_WIDTH, "1854"),
        ] {
            assert_eq!(text(&w, selector), want, "selector {selector}");
        }
        assert_eq!(probe_window(true).content_origin(), (100.0, 225.0));
        assert_eq!(new_window_uuid().len(), 32);
    }
}
