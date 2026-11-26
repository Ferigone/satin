//! Logical client-area geometry for the Quiet Cinder reference.
//! The renderer and pointer targets share these dimensions; only painting is scaled.

pub(super) const TAB_ROW: f32 = 44.;
pub(super) const TOOLBAR_ROW: f32 = 47.;
pub(super) const FAVORITES_ROW: f32 = 30.;
pub(super) const TOOLBAR_END: f32 = TAB_ROW + TOOLBAR_ROW;
pub(super) const CONTENT_TOP: f32 = TOOLBAR_END + FAVORITES_ROW;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Box2 {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Box2 {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub(super) fn right(self) -> f32 {
        self.x + self.w
    }
}

pub(super) fn caption(width: f32, position: usize) -> Box2 {
    let button = if width <= 660. { 35. } else { 42. };
    Box2::new(width - (3 - position) as f32 * button, 0., button, 31.)
}

pub(super) struct Toolbar {
    pub back: Box2,
    pub forward: Box2,
    pub reload: Box2,
    pub home: Option<Box2>,
    pub update: Option<Box2>,
    pub downloads: Box2,
    pub menu: Box2,
    pub address: Box2,
    pub bookmark: Box2,
    pub location: Box2,
    pub edit: Box2,
}

impl Toolbar {
    pub(super) fn new(width: f32, show_update: bool) -> Self {
        let (padding, gap, button, inline) = if width <= 410. {
            (3., 1., 25., 22.)
        } else if width <= 880. {
            (7., 3., 31., 27.)
        } else {
            (11., 8., 31., 27.)
        };
        // The reference's 47px grid row includes a one-pixel bottom border.
        let center = TAB_ROW + (TOOLBAR_ROW - 1.) / 2.;
        let control = |x| Box2::new(x, center - 15.5, button, 31.);
        let nav = |i: u8| control(padding + f32::from(i) * (button + gap));
        let home = (width > 660.).then(|| nav(3));
        let menu = control(width - padding - button);
        let downloads = control(menu.x - button - gap);
        let update = show_update.then(|| control(downloads.x - button - gap));
        let left = home.unwrap_or_else(|| nav(2)).right() + gap;
        let right = update.unwrap_or(downloads).x - gap;
        let address = Box2::new(left, center - 17., (right - left).max(0.), 34.);
        let location = Box2::new(left + 4., center - 13.5, inline, 27.);
        let bookmark = Box2::new(address.right() - inline - 4., center - 13.5, inline, 27.);
        let edit = Box2::new(
            location.right() + 5.,
            address.y + 1.,
            (bookmark.x - 5. - (location.right() + 5.)).max(0.),
            address.h - 2.,
        );
        Self {
            back: nav(0),
            forward: nav(1),
            reload: nav(2),
            home,
            update,
            downloads,
            menu,
            address,
            bookmark,
            location,
            edit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_grid_and_caption_use_the_final_cascade() {
        assert_eq!(CONTENT_TOP, 121.);
        assert_eq!(caption(1280., 0), Box2::new(1154., 0., 42., 31.));
        assert_eq!(caption(390., 2), Box2::new(355., 0., 35., 31.));
    }

    #[test]
    fn desktop_places_supported_navigation_controls_on_the_reference_grid() {
        let bar = Toolbar::new(1280., false);
        assert_eq!(bar.back, Box2::new(11., 51.5, 31., 31.));
        assert_eq!(bar.forward.x, 50.);
        assert_eq!(bar.reload.x, 89.);
        assert_eq!(bar.home.unwrap().x, 128.);
        assert_eq!(bar.address.x, 167.);
        assert_eq!((bar.address.y, bar.address.h), (50., 34.));
        assert_eq!(bar.menu.right(), 1269.);
    }

    #[test]
    fn responsive_controls_stay_in_window_and_do_not_overlap() {
        for width in [320., 390., 410., 411., 660., 661., 880., 881., 1280.] {
            let normal = Toolbar::new(width, false);
            let offered = Toolbar::new(width, true);
            let update = offered.update.unwrap();
            assert_eq!(normal.downloads, offered.downloads);
            assert_eq!(normal.menu, offered.menu);
            assert_eq!(normal.location, offered.location);
            assert_eq!(normal.address.x, offered.address.x);
            assert!(offered.address.right() < update.x);
            assert!(update.right() < offered.downloads.x);
            for bar in [normal, offered] {
                assert_eq!(bar.home.is_some(), width > 660.);
                assert_eq!(bar.back.w, if width <= 410. { 25. } else { 31. });
                assert!(bar.menu.right() <= width);
                assert!(bar.downloads.right() < bar.menu.x);
                assert!(bar.address.right() < bar.downloads.x);
                assert!(bar.location.right() < bar.edit.x);
                assert!(bar.edit.right() < bar.bookmark.x);
                assert!(bar.bookmark.right() < bar.address.right());
                assert!(bar.edit.w > 0.);
            }
        }
    }
}
