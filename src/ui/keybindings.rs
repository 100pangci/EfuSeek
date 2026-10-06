use gtk::gdk;

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureAction {
    FocusSearch,
    OpenParent,
}

// Never capture text-editing keys: GtkText's IM context must see them first.
pub fn capture_action(key: gdk::Key, control: bool, search_focused: bool) -> Option<CaptureAction> {
    if control && matches!(key, gdk::Key::l | gdk::Key::L | gdk::Key::f | gdk::Key::F) {
        Some(CaptureAction::FocusSearch)
    } else if control && !search_focused && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) {
        Some(CaptureAction::OpenParent)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composition_keys_are_not_captured() {
        for key in [
            gdk::Key::Return,
            gdk::Key::KP_Enter,
            gdk::Key::Up,
            gdk::Key::Down,
            gdk::Key::Escape,
            gdk::Key::space,
        ] {
            assert_eq!(capture_action(key, false, true), None);
            assert_eq!(capture_action(key, true, true), None);
        }
        assert_eq!(
            capture_action(gdk::Key::Return, true, false),
            Some(CaptureAction::OpenParent)
        );
        assert_eq!(
            capture_action(gdk::Key::f, true, true),
            Some(CaptureAction::FocusSearch)
        );
    }
}
