//! Where the window sits on the desktop, for the window selectors and
//! layout objects' screen bounds (calc-functions.md 6.3 O1, O5): the main
//! screen, the desktop area it leaves for windows (the visible display
//! area on Mac and the X11 work area on Linux), the window's frame, its client
//! area and the content area that shows the layout, in points.

use std::sync::OnceLock;

use om_data::host::{Frame, WindowInfo};

/// The window as the UI last drew it, in screen points.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Placement {
    pub screen: Frame,
    pub desktop: Frame,
    /// The current display's physical work area when its origin differs
    /// from the desktop origin FileMaker exposes to position selectors.
    pub work_area: Option<Frame>,
    /// The outer frame, the client area inside the system's title bar and
    /// borders, and the content area showing the layout.
    pub frame: Frame,
    pub inner: Frame,
    pub content: Frame,
    /// Device pixels per point.
    pub scale: f64,
    /// The window has the keyboard focus.
    pub active: bool,
    /// The status toolbar and layout bar were shown, and their height,
    /// which the content area gains when they are hidden.
    pub toolbar: bool,
    pub bars: f64,
    /// The menu bar was shown, and its height as last drawn, which the
    /// content area gains when it is hidden (and gives up when shown).
    pub menubar: bool,
    pub menu: f64,
    /// The content area was measured as drawn, rather than taken to be the
    /// whole client area before the layout was first drawn.
    pub measured: bool,
}

fn frame(rect: egui::Rect) -> Frame {
    Frame::new(
        f64::from(rect.left()),
        f64::from(rect.top()),
        f64::from(rect.width()),
        f64::from(rect.height()),
    )
}

/// The window's placement from the viewport the system reports, with the
/// content area `content` (window coordinates) and the bars above it: the
/// toolbars (`toolbar`, `bars` high) and the menu bar (`menubar`, `menu`
/// high).
pub(crate) fn placement(
    ctx: &egui::Context,
    content: Option<egui::Rect>,
    toolbar: bool,
    bars: f32,
    menubar: bool,
    menu: f32,
) -> Placement {
    let measured = content.is_some();
    let (viewport, screen_rect, points) = ctx.input(|i| {
        (
            i.viewport().clone(),
            i.viewport_rect(),
            i.pixels_per_point(),
        )
    });
    let inner = viewport.inner_rect.unwrap_or(screen_rect);
    let outer = viewport.outer_rect.unwrap_or(inner);
    let screen = viewport.monitor_size.map_or(frame(outer), |size| {
        Frame::new(0.0, 0.0, f64::from(size.x), f64::from(size.y))
    });
    let desktop = work_area()
        .map(|[left, top, width, height]| {
            let p = f64::from(points);
            Frame::new(left / p, top / p, width / p, height / p)
        })
        .unwrap_or(screen);
    let scale = viewport.native_pixels_per_point.unwrap_or(points);
    #[cfg(target_os = "macos")]
    let (desktop, work_area) = viewport
        .outer_rect
        .and_then(|outer| mac_work_areas(frame(outer), screen, f64::from(scale / points)))
        .map_or((desktop, None), |(desktop, area)| (desktop, Some(area)));
    #[cfg(not(target_os = "macos"))]
    let work_area = None;
    let content = content.unwrap_or(egui::Rect::from_min_size(egui::Pos2::ZERO, inner.size()));
    Placement {
        screen,
        desktop,
        work_area,
        frame: frame(outer),
        inner: frame(inner),
        content: frame(content.translate(inner.min.to_vec2())),
        scale: f64::from(scale),
        active: viewport.focused.unwrap_or(false),
        toolbar,
        bars: f64::from(bars),
        menubar,
        menu: f64::from(menu),
        measured,
    }
}

impl Placement {
    /// What `Get` reads, with the window's bars and zoom as a script left
    /// them: hiding the toolbar gives its height to the content area.
    pub(crate) fn window_info(
        &self,
        toolbar: bool,
        menubar: bool,
        zoom: u16,
        uuid: &str,
    ) -> WindowInfo {
        let mut content = self.content;
        let change = match (self.toolbar, toolbar) {
            (true, false) => self.bars,
            (false, true) => -self.bars,
            _ => 0.0,
        } + match (self.menubar, menubar) {
            (true, false) => self.menu,
            (false, true) => -self.menu,
            _ => 0.0,
        };
        content.top -= change;
        content.height += change;
        WindowInfo {
            screen: self.screen,
            scale: self.scale,
            desktop: self.desktop,
            frame: self.frame,
            content,
            zoom,
            toolbar,
            menubar,
            active: self.active,
            uuid: uuid.to_owned(),
            ..WindowInfo::default()
        }
    }

    /// Move/Resize Window: `left` and `top` from the desktop area's top
    /// left, `width` and `height` the outer frame's, as FileMaker Pro
    /// measures them (O1); the client and content areas keep their place
    /// in the frame.
    pub(crate) fn move_resize(
        &mut self,
        left: Option<f64>,
        top: Option<f64>,
        width: Option<f64>,
        height: Option<f64>,
    ) {
        let (dx, dy) = (
            left.map_or(0.0, |l| self.desktop.left + l - self.frame.left),
            top.map_or(0.0, |t| self.desktop.top + t - self.frame.top),
        );
        let (dw, dh) = (
            width.map_or(0.0, |w| w - self.frame.width),
            height.map_or(0.0, |h| h - self.frame.height),
        );
        for f in [&mut self.frame, &mut self.inner, &mut self.content] {
            f.left += dx;
            f.top += dy;
            f.width = (f.width + dw).max(0.0);
            f.height = (f.height + dh).max(0.0);
        }
    }

    /// Adjust Window [Resize to Fit] (script-steps.md, Adjust Window): the
    /// outer frame's width and height that give the content area `layout`
    /// (its width and height in points) at `zoom`, with the bars as a
    /// script left them, no further than the desktop area reaches from the
    /// frame's top left. Where the frame does not start inside the desktop
    /// area (another monitor's, which the work area may not describe), the
    /// desktop area's size alone limits it. `None` before the content area
    /// was measured, or while a bar the script shows was never drawn: the
    /// bars' place in the frame is not known yet.
    pub(crate) fn fit(
        &self,
        toolbar: bool,
        menubar: bool,
        zoom: u16,
        layout: (f64, f64),
    ) -> Option<(f64, f64)> {
        let unknown = |shown: bool, drawn: bool, height: f64| shown && !drawn && height <= 0.0;
        if !self.measured
            || unknown(toolbar, self.toolbar, self.bars)
            || unknown(menubar, self.menubar, self.menu)
        {
            return None;
        }
        let info = self.window_info(toolbar, menubar, zoom, "");
        let scale = f64::from(zoom) / 100.0;
        let area = self.work_area.unwrap_or(info.desktop);
        let room = |start: f64, origin: f64, extent: f64| {
            let left = origin + extent - start;
            if start >= origin && left > 0.0 {
                left
            } else {
                extent
            }
        };
        let limit = |need: f64, room: f64| if room > 0.0 { need.min(room) } else { need };
        let width = limit(
            info.frame.width - info.content.width + layout.0 * scale,
            room(info.frame.left, area.left, area.width),
        );
        let height = limit(
            info.frame.height - info.content.height + layout.1 * scale,
            room(info.frame.top, area.top, area.height),
        );
        Some((width.max(1.0), height.max(1.0)))
    }

    /// The system's title bar and borders: the outer frame less the client
    /// area, across and down.
    pub(crate) fn chrome(&self) -> (f64, f64) {
        (
            (self.frame.width - self.inner.width).max(0.0),
            (self.frame.height - self.inner.height).max(0.0),
        )
    }

    /// Card `card` on this window, its parent: a card has no frame, title
    /// bar or toolbar, so its frame is its content area.
    pub(crate) fn card(&self, card: &crate::scripting::Card) -> Placement {
        let area = card_frame(self.content, card);
        Placement {
            frame: area,
            inner: area,
            content: area,
            toolbar: false,
            bars: 0.0,
            menubar: false,
            menu: 0.0,
            ..self.clone()
        }
    }
}

/// Where a card shows on its parent's layout area `area` (Claris, New
/// Window: From Top and From Left are measured from the parent's layout
/// area). Without them it is centred on the area, and without a size it
/// takes the area's (OpenMaker's choices).
pub(crate) fn card_frame(area: Frame, card: &crate::scripting::Card) -> Frame {
    let width = card.width.unwrap_or(area.width).max(1.0);
    let height = card.height.unwrap_or(area.height).max(1.0);
    Frame::new(
        area.left + card.left.unwrap_or((area.width - width) / 2.0),
        area.top + card.top.unwrap_or((area.height - height) / 2.0),
        width,
        height,
    )
}

/// AppKit reports screen rectangles in global Cocoa points with y going
/// up. Winit anchors positions at the primary screen's top-left.
#[cfg(target_os = "macos")]
fn mac_work_areas(window: Frame, monitor: Frame, points: f64) -> Option<(Frame, Frame)> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;
    use objc2_core_foundation::CGRect;

    let screens = NSScreen::screens(MainThreadMarker::new()?);
    let primary = screens.firstObject()?;
    let height = primary.frame().size.height;
    let top_left = |rect: CGRect| {
        Frame::new(
            rect.origin.x * points,
            (height - rect.origin.y - rect.size.height) * points,
            rect.size.width * points,
            rect.size.height * points,
        )
    };
    let origin = top_left(primary.visibleFrame());
    let displays = screens
        .iter()
        .map(|screen| (top_left(screen.frame()), top_left(screen.visibleFrame())));
    window_work_areas(window, monitor, origin, displays)
}

/// The current display supplies the work area's size. FileMaker measures
/// positions from the primary work area's origin on both displays (§6.3).
#[cfg(any(target_os = "macos", test))]
fn window_work_areas(
    window: Frame,
    monitor: Frame,
    primary: Frame,
    displays: impl Iterator<Item = (Frame, Frame)>,
) -> Option<(Frame, Frame)> {
    let matches_monitor = |screen: Frame| {
        (screen.width - monitor.width).abs() < 1.0 && (screen.height - monitor.height).abs() < 1.0
    };
    let overlap = |screen: Frame| {
        let right = (screen.left + screen.width).min(window.left + window.width);
        let bottom = (screen.top + screen.height).min(window.top + window.height);
        let width = (right - screen.left.max(window.left)).max(0.0);
        let height = (bottom - screen.top.max(window.top)).max(0.0);
        width * height
    };
    let (_, visible) = displays.max_by(|(a, _), (b, _)| {
        matches_monitor(*a)
            .cmp(&matches_monitor(*b))
            .then_with(|| overlap(*a).total_cmp(&overlap(*b)))
    })?;
    let desktop = Frame::new(primary.left, primary.top, visible.width, visible.height);
    Some((desktop, visible))
}

/// The desktop's work area in pixels (`_NET_WORKAREA` of the current
/// desktop), read once; `None` without an X11 display that sets it.
fn work_area() -> Option<[f64; 4]> {
    static AREA: OnceLock<Option<[f64; 4]>> = OnceLock::new();
    *AREA.get_or_init(read_work_area)
}

#[cfg(target_os = "linux")]
fn read_work_area() -> Option<[f64; 4]> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
    std::env::var_os("DISPLAY")?;
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let atom = |name: &[u8]| {
        conn.intern_atom(true, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
            .filter(|a| *a != 0)
    };
    let cardinals = |property: u32, count: u32| -> Option<Vec<u32>> {
        let reply = conn
            .get_property(false, root, property, AtomEnum::CARDINAL, 0, count)
            .ok()?
            .reply()
            .ok()?;
        let values: Vec<u32> = reply.value32()?.collect();
        Some(values)
    };
    let desktop = atom(b"_NET_CURRENT_DESKTOP")
        .and_then(|a| cardinals(a, 1))
        .and_then(|v| v.first().copied())
        .unwrap_or(0) as usize;
    let areas = cardinals(atom(b"_NET_WORKAREA")?, 1024)?;
    let a = areas
        .get(desktop * 4..desktop * 4 + 4)
        .or_else(|| areas.get(0..4))?;
    let area = [a[0], a[1], a[2], a[3]].map(f64::from);
    (area[2] > 0.0 && area[3] > 0.0).then_some(area)
}

#[cfg(not(target_os = "linux"))]
fn read_work_area() -> Option<[f64; 4]> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FileMaker Pro's probe window: 1100 x 900 at screen 166, 150 on a
    /// desktop area from 66, 30, with 105 points above the content.
    fn probe() -> Placement {
        Placement {
            screen: Frame::new(0.0, 0.0, 1920.0, 1080.0),
            desktop: Frame::new(66.0, 30.0, 1854.0, 1050.0),
            work_area: None,
            frame: Frame::new(166.0, 150.0, 1100.0, 900.0),
            inner: Frame::new(166.0, 178.0, 1100.0, 872.0),
            content: Frame::new(166.0, 255.0, 1100.0, 795.0),
            scale: 1.0,
            active: false,
            toolbar: true,
            bars: 77.0,
            menubar: false,
            menu: 0.0,
            measured: true,
        }
    }

    #[test]
    fn mac_display_size_and_position_origin_can_come_from_different_screens() {
        let primary = Frame::new(48.0, 25.0, 1552.0, 875.0);
        let upper = Frame::new(-400.0, -1175.0, 2000.0, 1175.0);
        let displays = [
            (Frame::new(0.0, 0.0, 1600.0, 900.0), primary),
            (Frame::new(-400.0, -1200.0, 2000.0, 1200.0), upper),
        ];
        let window = Frame::new(300.0, -1000.0, 800.0, 600.0);
        let (desktop, visible) =
            window_work_areas(window, displays[1].0, primary, displays.into_iter()).unwrap();
        assert_eq!(desktop, Frame::new(48.0, 25.0, 2000.0, 1175.0));
        assert_eq!(visible, upper);
    }

    #[test]
    fn move_resize_measures_from_the_desktop_area() {
        let mut p = probe();
        p.move_resize(Some(10.0), Some(40.0), Some(1000.0), Some(800.0));
        assert_eq!(p.frame, Frame::new(76.0, 70.0, 1000.0, 800.0));
        assert_eq!(p.content, Frame::new(76.0, 175.0, 1000.0, 695.0));
        assert_eq!(p.chrome(), (0.0, 28.0));
        let info = p.window_info(true, true, 100, "");
        assert_eq!(info.content_origin(), (10.0, 145.0));
        // Hidden bars give the content their height.
        let hidden = probe().window_info(false, true, 100, "");
        assert_eq!(hidden.content, Frame::new(166.0, 178.0, 1100.0, 872.0));
    }

    /// Resize to Fit (#1115): the frame that gives the content area the
    /// layout at the zoom, within the desktop area from the frame's top
    /// left, or within its size where the frame starts outside it (another
    /// monitor); a menu bar a script shows takes its height too; nothing
    /// before the content area was measured or while that bar was never
    /// drawn.
    #[test]
    fn fit_gives_the_content_area_the_layout() {
        let p = probe();
        // 105 points of the frame are not content: title bar and bars.
        assert_eq!(
            p.fit(true, false, 100, (800.0, 300.0)),
            Some((800.0, 405.0))
        );
        assert_eq!(
            p.fit(true, false, 150, (800.0, 300.0)),
            Some((1200.0, 555.0))
        );
        // Larger than what the desktop area leaves right of and below it.
        assert_eq!(
            p.fit(true, false, 100, (3000.0, 2000.0)),
            Some((1754.0, 930.0))
        );
        // On a monitor the desktop area does not describe.
        let mut other = probe();
        other.frame.left = 2000.0;
        assert_eq!(
            other.fit(true, false, 100, (3000.0, 300.0)),
            Some((1854.0, 405.0))
        );
        // A menu bar hidden when drawn, which the script shows.
        let mut menu = probe();
        menu.menu = 24.0;
        assert_eq!(
            menu.fit(true, true, 100, (800.0, 300.0)),
            Some((800.0, 429.0))
        );
        // One never drawn: its height is not known yet.
        assert_eq!(probe().fit(true, true, 100, (800.0, 300.0)), None);
        let mut unmeasured = probe();
        unmeasured.measured = false;
        assert_eq!(unmeasured.fit(true, false, 100, (800.0, 300.0)), None);
    }
}
