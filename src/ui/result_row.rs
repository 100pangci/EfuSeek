use efuseek::core::entry::Entry;
use gtk::{glib, prelude::*};
use std::rc::Rc;

pub type ContextMenu = Rc<dyn Fn(&gtk::ListItem, &gtk::Label, f64, f64)>;

pub fn column(
    title: &str,
    expand: bool,
    field: impl Fn(&Entry) -> String + 'static,
    context_menu: ContextMenu,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        item.set_child(Some(&label));
        let gesture = gtk::GestureClick::new();
        gesture.set_button(3);
        let (item, weak_label, context_menu) =
            (item.downgrade(), label.downgrade(), context_menu.clone());
        gesture.connect_pressed(move |gesture, _, x, y| {
            if let (Some(item), Some(label)) = (item.upgrade(), weak_label.upgrade()) {
                context_menu(&item, &label, x, y);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        label.add_controller(gesture);
    });
    factory.connect_bind(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let Some(label) = item.child().and_downcast::<gtk::Label>() else {
            return;
        };
        let entry = row.borrow::<Entry>();
        let text = field(&entry);
        label.set_text(&text);
        label.set_tooltip_text(Some(&text));
    });
    factory.connect_unbind(|_, object| {
        if let Some(item) = object.downcast_ref::<gtk::ListItem>()
            && let Some(label) = item.child()
        {
            while let Some(child) = label.first_child() {
                if let Ok(popover) = child.downcast::<gtk::Popover>() {
                    popover.popdown();
                    if popover.parent().is_some() {
                        popover.unparent();
                    }
                } else {
                    break;
                }
            }
        }
    });
    gtk::ColumnViewColumn::builder()
        .title(title)
        .factory(&factory)
        .expand(expand)
        .resizable(true)
        .build()
}

pub fn show_context_menu(
    item: &gtk::ListItem,
    label: &gtk::Label,
    selection: &gtk::SingleSelection,
    config: &efuseek::config::Config,
    overlay: &adw::ToastOverlay,
    point: (f64, f64),
) {
    let Some(row) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
        return;
    };
    let entry = row.borrow::<Entry>().clone();
    if entry.path.is_empty() {
        return;
    }
    selection.set_selected(item.position());
    let menu = gtk::gio::Menu::new();
    let actions = gtk::gio::SimpleActionGroup::new();
    let local = efuseek::core::path_map::map_path(&entry.path, &config.path_map).ok();
    for (id, title, parent) in [("open", "打开", false), ("parent", "打开所在目录", true)] {
        let action = gtk::gio::SimpleAction::new(id, None);
        let (entry, maps, overlay) = (entry.clone(), config.path_map.clone(), overlay.downgrade());
        action.connect_activate(move |_, _| {
            let overlay = overlay.clone();
            efuseek::core::opener::open(&entry, parent, &maps, move |result| {
                if let (Err(message), Some(overlay)) = (result, overlay.upgrade()) {
                    overlay.add_toast(adw::Toast::new(&message));
                }
            });
        });
        actions.add_action(&action);
        menu.append(Some(title), Some(&format!("row.{id}")));
    }
    for (id, title, text) in [
        ("name", "复制文件名", Some(entry.name)),
        (
            "local",
            if local.is_some() {
                "复制本机完整路径"
            } else {
                "复制本机完整路径（未配置映射）"
            },
            local.map(|p| p.to_string_lossy().into_owned()),
        ),
        ("original", "复制 EFU 原始路径", Some(entry.path)),
    ] {
        let action = gtk::gio::SimpleAction::new(id, None);
        action.set_enabled(text.is_some());
        let clipboard = label.clipboard();
        action.connect_activate(move |_, _| {
            if let Some(text) = &text {
                clipboard.set_text(text);
            }
        });
        actions.add_action(&action);
        menu.append(Some(title), Some(&format!("row.{id}")));
    }
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.insert_action_group("row", Some(&actions));
    popover.set_parent(label);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        point.0 as i32,
        point.1 as i32,
        1,
        1,
    )));
    popover.connect_closed(|popover| {
        // GTK closes a menu before dispatching the clicked item's action. Keep
        // its action group alive until that dispatch completes.
        let popover = popover.clone();
        glib::idle_add_local_once(move || {
            if popover.parent().is_some() {
                popover.unparent();
            }
        });
    });
    popover.popup();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_label(widget: &gtk::Widget, text: &str) -> Option<gtk::Label> {
        if let Some(label) = widget.downcast_ref::<gtk::Label>()
            && label.text() == text
        {
            return Some(label.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(label) = find_label(&widget, text) {
                return Some(label);
            }
            child = widget.next_sibling();
        }
        None
    }

    fn click_menu_item(popover: &gtk::PopoverMenu, text: &str) {
        let label = find_label(popover.upcast_ref(), text).expect("menu item label");
        let mut widget = label.upcast::<gtk::Widget>();
        loop {
            if glib::subclass::SignalId::lookup("clicked", widget.type_()).is_some() {
                // Exercise native close-before-action dispatch, not activate_action
                // directly: premature unparenting silently loses the clicked action.
                widget.emit_by_name::<()>("clicked", &[]);
                return;
            }
            widget = widget.parent().expect("native clickable menu item");
        }
    }

    // Explicit opt-in: normal Core tests never require a display or clipboard.
    #[test]
    fn native_headers_context_selection_and_clipboard() {
        if std::env::var_os("EFUSEEK_GUI_TEST").is_none() {
            return;
        }
        gtk::init().expect("GTK display");
        let store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
        store.append(&glib::BoxedAnyObject::new(Entry::new(
            "/tmp/opencode/local.txt".into(),
            None,
            None,
            None,
        )));
        store.append(&glib::BoxedAnyObject::new(Entry::new(
            "Z:\\日本\\unmapped.mkv".into(),
            None,
            None,
            None,
        )));
        let selection = gtk::SingleSelection::new(Some(store));
        let view = gtk::ColumnView::new(Some(selection.clone()));
        let menu: ContextMenu = {
            let selection = selection.clone();
            Rc::new(move |item, label, x, y| {
                show_context_menu(
                    item,
                    label,
                    &selection,
                    &Default::default(),
                    &adw::ToastOverlay::new(),
                    (x, y),
                )
            })
        };
        let column = column("名称", false, |entry| entry.name.clone(), menu);
        column.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
        view.append_column(&column);
        let sorter = view
            .sorter()
            .and_downcast::<gtk::ColumnViewSorter>()
            .unwrap();
        for direction in [gtk::SortType::Ascending, gtk::SortType::Descending] {
            view.sort_by_column(Some(&column), direction);
            assert_eq!(sorter.primary_sort_column(), Some(column.clone()));
            assert_eq!(sorter.primary_sort_order(), direction);
        }
        let window = gtk::Window::builder()
            .child(&view)
            .default_width(500)
            .default_height(200)
            .build();
        window.present();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let label = loop {
            context.iteration(false);
            if let Some(label) = find_label(window.upcast_ref(), "unmapped.mkv") {
                break label;
            }
            assert!(std::time::Instant::now() < deadline, "rows not realized");
        };
        let controllers = label.observe_controllers();
        let gesture = (0..controllers.n_items())
            .find_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
            .unwrap();
        gesture.emit_by_name::<()>("pressed", &[&1i32, &5f64, &5f64]);
        assert_eq!(selection.selected(), 1);
        let popover = label
            .first_child()
            .and_downcast::<gtk::PopoverMenu>()
            .expect("native menu");
        // Disabled local-path action must not replace the existing clipboard contents.
        label.clipboard().set_text("unchanged");
        assert!(popover.activate_action("row.local", None).is_ok());
        let clipboard = label.clipboard();
        assert_eq!(
            context
                .block_on(clipboard.read_text_future())
                .unwrap()
                .as_deref(),
            Some("unchanged")
        );
        popover.activate_action("row.original", None).unwrap();
        assert_eq!(
            context
                .block_on(clipboard.read_text_future())
                .unwrap()
                .as_deref(),
            Some("Z:\\日本\\unmapped.mkv")
        );
        click_menu_item(&popover, "复制文件名");
        assert!(
            popover.parent().is_some(),
            "menu actions must survive native close dispatch"
        );
        assert_eq!(
            context
                .block_on(clipboard.read_text_future())
                .unwrap()
                .as_deref(),
            Some("unmapped.mkv")
        );
        popover.popdown();
        // A native absolute path can be copied without any mapping.
        let label = find_label(window.upcast_ref(), "local.txt").unwrap();
        let controllers = label.observe_controllers();
        let gesture = (0..controllers.n_items())
            .find_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
            .unwrap();
        gesture.emit_by_name::<()>("pressed", &[&1i32, &5f64, &5f64]);
        let popover = label
            .first_child()
            .and_downcast::<gtk::PopoverMenu>()
            .unwrap();
        popover.activate_action("row.local", None).unwrap();
        assert_eq!(
            context
                .block_on(label.clipboard().read_text_future())
                .unwrap()
                .as_deref(),
            Some("/tmp/opencode/local.txt")
        );
        popover.popdown();
        window.close();
        while context.pending() {
            context.iteration(false);
        }
    }
}
