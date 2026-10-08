pub(crate) mod palette {
    use eframe::egui::Color32;

    pub(crate) const CANVAS: Color32 = Color32::from_rgb(12, 14, 20);
    pub(crate) const SIDEBAR: Color32 = Color32::from_rgb(16, 18, 26);
    pub(crate) const SURFACE: Color32 = Color32::from_rgb(22, 25, 34);
    pub(crate) const SURFACE_HOVER: Color32 = Color32::from_rgb(31, 35, 47);
    pub(crate) const EDGE: Color32 = Color32::from_rgb(44, 49, 64);
    pub(crate) const TEXT: Color32 = Color32::from_rgb(236, 239, 246);
    pub(crate) const MUTED: Color32 = Color32::from_rgb(147, 155, 174);
    pub(crate) const ACCENT: Color32 = Color32::from_rgb(151, 164, 255);
    pub(crate) const ACCENT_DARK: Color32 = Color32::from_rgb(59, 69, 121);
    pub(crate) const ERROR: Color32 = Color32::from_rgb(245, 136, 139);
    pub(crate) const SUCCESS: Color32 = Color32::from_rgb(126, 210, 174);
}

pub(crate) mod spacing {
    pub(crate) const GAP_4: f32 = 4.0;
    pub(crate) const GAP_5: f32 = 5.0;
    pub(crate) const GAP_6: f32 = 6.0;
    pub(crate) const GAP_7: f32 = 7.0;
    pub(crate) const GAP_8: f32 = 8.0;
    pub(crate) const GAP_10: f32 = 10.0;
    pub(crate) const GAP_12: f32 = 12.0;
    pub(crate) const GAP_14: f32 = 14.0;
    pub(crate) const GAP_16: f32 = 16.0;
    pub(crate) const GAP_18: f32 = 18.0;
    pub(crate) const GAP_22: f32 = 22.0;
    pub(crate) const GAP_25: f32 = 25.0;
    pub(crate) const GAP_26: f32 = 26.0;
    pub(crate) const GAP_28: f32 = 28.0;
    pub(crate) const GAP_32: f32 = 32.0;
    pub(crate) const GAP_34: f32 = 34.0;
    pub(crate) const GAP_42: f32 = 42.0;
    pub(crate) const GAP_58: f32 = 58.0;

    pub(crate) const ITEM_HORIZONTAL: f32 = 12.0;
    pub(crate) const ITEM_VERTICAL: f32 = 10.0;
}

pub(crate) mod typography {
    pub(crate) const MICRO: f32 = 9.0;
    pub(crate) const OVERLINE: f32 = 10.0;
    pub(crate) const CAPTION: f32 = 11.0;
    pub(crate) const BODY_SMALL: f32 = 12.0;
    pub(crate) const BODY: f32 = 13.0;
    pub(crate) const BODY_LARGE: f32 = 14.0;
    pub(crate) const CONTROL_ICON_SMALL: f32 = 15.0;
    pub(crate) const ARTIST_TITLE: f32 = 17.0;
    pub(crate) const TITLE: f32 = 18.0;
    pub(crate) const CONTROL_ICON: f32 = 20.0;
    pub(crate) const NAVIGATION_ICON: f32 = 26.0;
    pub(crate) const TRACK_TITLE: f32 = 28.0;
    pub(crate) const BRAND_MARK: f32 = 30.0;
    pub(crate) const PAGE_TITLE: f32 = 34.0;
}

pub(crate) mod layout {
    pub(crate) const NAVIGATION_PANEL_WIDTH: f32 = 224.0;
    pub(crate) const TRANSPORT_PANEL_HEIGHT: f32 = 92.0;
    pub(crate) const NAVIGATION_BUTTON_HEIGHT: f32 = 42.0;
    pub(crate) const PRIMARY_BUTTON_SIZE: f32 = 42.0;
    pub(crate) const TRANSPORT_ARTWORK_SIZE: f32 = 54.0;
    pub(crate) const NOW_PLAYING_ARTWORK_SIZE: f32 = 320.0;
    pub(crate) const PROGRESS_WIDTH: f32 = 180.0;
    pub(crate) const LIBRARY_ACTIONS_WIDTH: f32 = 252.0;
    pub(crate) const TRACK_TITLE_WIDTH: f32 = 270.0;
    pub(crate) const TRACK_ROW_BUTTON_HEIGHT: f32 = 38.0;
    pub(crate) const EMPTY_STATE_MIN_HEIGHT: f32 = 170.0;
    pub(crate) const WIDE_LIBRARY_BREAKPOINT: f32 = 900.0;
    pub(crate) const COLLECTION_ROW_HEIGHT: f32 = 54.0;
    pub(crate) const STATISTIC_CARD_MIN_WIDTH: f32 = 112.0;
    pub(crate) const BORDER_WIDTH: f32 = 1.0;

    pub(crate) const NAVIGATION_INSET: i8 = 22;
    pub(crate) const CARD_INSET: i8 = 16;
    pub(crate) const TRANSPORT_INSET_X: i8 = 24;
    pub(crate) const TRANSPORT_INSET_Y: i8 = 14;
    pub(crate) const CONTENT_INSET_X: i8 = 32;
    pub(crate) const CONTENT_INSET_Y: i8 = 0;
    pub(crate) const TRACK_ROW_INSET_X: i8 = 12;
    pub(crate) const TRACK_ROW_INSET_Y: i8 = 8;
    pub(crate) const EMPTY_STATE_INSET: i8 = 28;
}

pub(crate) mod radius {
    use eframe::egui::CornerRadius;

    pub(crate) const WINDOW: CornerRadius = CornerRadius::same(6);
    pub(crate) const MENU: CornerRadius = CornerRadius::same(6);
    pub(crate) const WIDGET: CornerRadius = CornerRadius::same(2);
    pub(crate) const WIDGET_HOVERED: CornerRadius = CornerRadius::same(3);
    pub(crate) const ARTWORK_FALLBACK: f32 = 8.0;
}

pub(crate) mod motion {
    pub(crate) const ANIMATION_SECONDS: f32 = 0.2;
    pub(crate) const FRAME_REFRESH_MS: u64 = 40;
}

pub(crate) mod window {
    pub(crate) const INITIAL_SIZE: [f32; 2] = [1320.0, 840.0];
    pub(crate) const MINIMUM_SIZE: [f32; 2] = [900.0, 620.0];
}

#[cfg(test)]
mod tests {
    use super::{layout, motion, palette, radius, typography, window};

    #[test]
    fn initial_desktop_tokens_match_the_existing_visual_system() {
        assert_eq!(palette::CANVAS, eframe::egui::Color32::from_rgb(12, 14, 20));
        assert_eq!(layout::NAVIGATION_PANEL_WIDTH, 224.0);
        assert_eq!(typography::PAGE_TITLE, 34.0);
        assert_eq!(radius::WINDOW, eframe::egui::CornerRadius::same(6));
        assert_eq!(motion::ANIMATION_SECONDS, 0.2);
        assert_eq!(window::INITIAL_SIZE, [1320.0, 840.0]);
    }
}
