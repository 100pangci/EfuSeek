use adw::prelude::*;
use efuseek::{
    config::Config,
    core::{
        entry::Entry,
        opener,
        path_map::map_path,
        sort::{Sort, SortDirection, SortField},
        worker::{Event, SearchRequest, Workers},
    },
};
use gtk::{gdk, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

fn selected(selection: &gtk::SingleSelection) -> Option<Entry> {
    selection
        .selected_item()
        .and_downcast::<glib::BoxedAnyObject>()
        .map(|o| o.borrow::<Entry>().clone())
        .filter(|entry| !entry.path.is_empty())
}

fn searching_feedback(search: &gtk::SearchEntry, label: &gtk::Label) {
    if search.text().trim().is_empty() {
        label.set_text("全部索引（按需加载）");
    } else {
        label.set_text(&format!(
            "正在搜索“{}”…（列表仍为上一次结果）",
            search.text()
        ));
    }
}

fn refresh(
    search: &gtk::SearchEntry,
    workers: &Workers,
    generation: &Cell<u64>,
    model: &super::virtual_list::VirtualList,
    selection: &gtk::SingleSelection,
    total: u64,
    sort: Sort,
) {
    generation.set(generation.get().wrapping_add(1));
    workers
        .latest
        .store(generation.get(), std::sync::atomic::Ordering::Relaxed);
    if search.text().trim().is_empty() {
        // Detach before a whole-model reset: GtkSingleSelection otherwise tries
        // to locate the old selected object's identity across the entire index.
        selection.set_model(None::<&gtk::gio::ListModel>);
        model.browse(total, generation.get(), workers.pages.clone(), sort);
        selection.set_model(Some(model));
    } else {
        let _ = workers.search.send(SearchRequest {
            generation: generation.get(),
            text: search.text().to_string(),
            sort,
        });
    }
}
fn open_selection(
    selection: &gtk::SingleSelection,
    config: &Config,
    overlay: &adw::ToastOverlay,
    parent: bool,
) {
    if let Some(entry) = selected(selection) {
        let overlay = overlay.downgrade();
        opener::open(&entry, parent, &config.path_map, move |result| {
            if let (Err(message), Some(overlay)) = (result, overlay.upgrade()) {
                overlay.add_toast(adw::Toast::new(&message));
            }
        });
    }
}

pub fn build(app: &adw::Application) {
    let defaults = Config::default();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(efuseek::WINDOW_TITLE)
        .default_width(defaults.window_width)
        .default_height(defaults.window_height)
        .build();
    let overlay = adw::ToastOverlay::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let settings = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("设置")
        .sensitive(false)
        .build();
    header.pack_end(&settings);
    content.append(&header);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("搜索文件和文件夹…")
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(12)
        .build();
    // GTK's built-in delay is disabled: debounce is controlled by config.toml.
    search.set_search_delay(0);
    content.append(&search);
    let model = super::virtual_list::VirtualList::new();
    // Shared by row factories; populated after loading config, before rows are bound.
    let menu_config = Rc::new(RefCell::new(Config::default()));
    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let view = gtk::ColumnView::new(Some(selection.clone()));
    view.set_single_click_activate(false);
    view.set_show_column_separators(true);
    let context_menu: super::result_row::ContextMenu = {
        let (selection, config, overlay) = (
            selection.downgrade(),
            menu_config.clone(),
            overlay.downgrade(),
        );
        Rc::new(move |item, label, x, y| {
            if let (Some(selection), Some(overlay)) = (selection.upgrade(), overlay.upgrade()) {
                super::result_row::show_context_menu(
                    item,
                    label,
                    &selection,
                    &config.borrow(),
                    &overlay,
                    (x, y),
                );
            }
        })
    };
    let name = super::result_row::column(
        "名称",
        false,
        |e| format!("{} {}", if e.is_dir { "📁" } else { "📄" }, e.name),
        context_menu.clone(),
    );
    name.set_fixed_width(300);
    view.append_column(&name);
    let size = super::result_row::column("大小", false, Entry::display_size, context_menu.clone());
    size.set_fixed_width(110);
    view.append_column(&size);
    let modified = super::result_row::column(
        "修改时间",
        false,
        Entry::display_modified,
        context_menu.clone(),
    );
    modified.set_fixed_width(180);
    view.append_column(&modified);
    // These sorters only enable native header interactions. No SortListModel is installed.
    for column in [&name, &size, &modified] {
        column.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
    }
    // The path column is installed below after loading the current mapping configuration.
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&view)
        .build();
    content.append(&scroll);
    let info_label = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .selectable(true)
        .wrap(true)
        .build();
    let status = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_end(12)
        .margin_top(4)
        .margin_bottom(10)
        .wrap(true)
        .build();
    content.append(&info_label);
    // Search feedback is independent of periodic index status and page loads:
    // neither may relabel old rows as matches for newly typed text.
    let search_status = gtk::Label::builder()
        .xalign(0.0)
        .margin_start(12)
        .margin_end(12)
        .wrap(true)
        .build();
    content.append(&search_status);
    content.append(&status);
    overlay.set_child(Some(&content));
    window.set_content(Some(&overlay));
    window.present();
    search.grab_focus();

    let config = match Config::load() {
        Ok(config) => {
            *menu_config.borrow_mut() = config;
            menu_config
        }
        Err(error) => {
            status.set_text(&format!("配置加载失败：{error:#}"));
            return;
        }
    };
    let database = match config.borrow().database_path() {
        Ok(database) => database,
        Err(error) => {
            status.set_text(&format!("缓存路径错误：{error:#}"));
            return;
        }
    };
    window.set_default_size(config.borrow().window_width, config.borrow().window_height);
    model.configure(
        config.borrow().browse_page_size,
        config.borrow().browse_cache_pages,
    );
    {
        let config = config.clone();
        view.append_column(&super::result_row::column(
            "本机路径",
            true,
            move |e| {
                if e.path.is_empty() {
                    return String::new();
                }
                map_path(&e.path, &config.borrow().path_map)
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| format!("{}（未映射）", e.path))
            },
            context_menu,
        ));
    }
    info_label.set_text(&if config.borrow().efu_path.as_os_str().is_empty() {
        "索引：未选择".into()
    } else {
        format!("索引：{}", config.borrow().efu_path.display())
    });
    status.set_text("正在加载本地缓存…");
    let workers = Rc::new(RefCell::new(Workers::start(
        config.borrow().clone(),
        database,
    )));
    let generation = Rc::new(Cell::new(0_u64));
    let total = Rc::new(Cell::new(0_u64));
    let sort = Rc::new(Cell::new(Sort::default()));
    {
        let (workers, generation, model, selection, total, search, sort, search_status) = (
            workers.clone(),
            generation.clone(),
            model.clone(),
            selection.clone(),
            total.clone(),
            search.clone(),
            sort.clone(),
            search_status.clone(),
        );
        if let Some(sorter) = view.sorter().and_downcast::<gtk::ColumnViewSorter>() {
            sorter.connect_changed(move |sorter, _| {
                let field = match sorter.primary_sort_column() {
                    Some(column) if column == name => SortField::Name,
                    Some(column) if column == size => SortField::Size,
                    Some(column) if column == modified => SortField::Modified,
                    _ => SortField::Default,
                };
                let next = Sort {
                    field,
                    direction: if sorter.primary_sort_order() == gtk::SortType::Descending {
                        SortDirection::Descending
                    } else {
                        SortDirection::Ascending
                    },
                };
                if sort.replace(next) != next {
                    searching_feedback(&search, &search_status);
                    refresh(
                        &search,
                        &workers.borrow(),
                        &generation,
                        &model,
                        &selection,
                        total.get(),
                        next,
                    );
                }
            });
        }
    }
    let debounce = Rc::new(RefCell::new(None::<glib::SourceId>));
    {
        let (workers, generation, debounce, total, model, selection, config, sort, search_status) = (
            workers.clone(),
            generation.clone(),
            debounce.clone(),
            total.clone(),
            model.clone(),
            selection.clone(),
            config.clone(),
            sort.clone(),
            search_status.clone(),
        );
        search.connect_search_changed(move |search| {
            searching_feedback(search, &search_status);
            generation.set(generation.get().wrapping_add(1));
            workers
                .borrow()
                .latest
                .store(generation.get(), std::sync::atomic::Ordering::Relaxed);
            if let Some(source) = debounce.borrow_mut().take() {
                source.remove();
            }
            let (workers, search, generation, pending, model, total, selection, sort) = (
                workers.clone(),
                search.clone(),
                generation.clone(),
                debounce.clone(),
                model.clone(),
                total.clone(),
                selection.clone(),
                sort.clone(),
            );
            *debounce.borrow_mut() = Some(glib::timeout_add_local_once(
                Duration::from_millis(config.borrow().debounce_ms),
                move || {
                    pending.borrow_mut().take();
                    refresh(
                        &search,
                        &workers.borrow(),
                        &generation,
                        &model,
                        &selection,
                        total.get(),
                        sort.get(),
                    );
                },
            ));
        });
    }
    {
        let (selection, config, overlay) = (selection.clone(), config.clone(), overlay.clone());
        view.connect_activate(move |_, position| {
            selection.set_selected(position);
            open_selection(&selection, &config.borrow(), &overlay, false);
        });
    }
    let parent_on_activate = Rc::new(Cell::new(false));
    let shortcuts = gtk::EventControllerKey::new();
    shortcuts.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (search, selection, config, overlay, parent_on_activate) = (
            search.clone(),
            selection.clone(),
            config.clone(),
            overlay.clone(),
            parent_on_activate.clone(),
        );
        shortcuts.connect_key_pressed(move |_, key, _, modifiers| {
            let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            parent_on_activate.set(control);
            let search_focused = search.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN);
            match super::keybindings::capture_action(key, control, search_focused) {
                Some(super::keybindings::CaptureAction::FocusSearch) => {
                    search.grab_focus();
                }
                Some(super::keybindings::CaptureAction::OpenParent) => {
                    open_selection(&selection, &config.borrow(), &overlay, true)
                }
                None => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }
    window.add_controller(shortcuts);
    {
        let (selection, config, overlay) = (selection.clone(), config.clone(), overlay.clone());
        // GtkText emits activate only after the input method has declined the key.
        search.connect_activate(move |_| {
            open_selection(
                &selection,
                &config.borrow(),
                &overlay,
                parent_on_activate.get(),
            )
        });
        search.connect_stop_search(|search| {
            search.set_text("");
            search.grab_focus();
        });
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
    {
        let (search, view, selection, config, overlay, model) = (
            search.clone(),
            view.clone(),
            selection.clone(),
            config.clone(),
            overlay.clone(),
            model.clone(),
        );
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            if control && matches!(key, gdk::Key::l | gdk::Key::L | gdk::Key::f | gdk::Key::F) {
                search.grab_focus();
            } else if key == gdk::Key::Escape {
                search.set_text("");
                search.grab_focus();
            } else if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) {
                open_selection(&selection, &config.borrow(), &overlay, control);
            } else if (matches!(key, gdk::Key::Up | gdk::Key::Down)
                || (control && matches!(key, gdk::Key::Home | gdk::Key::End)))
                && model.n_items() > 0
            {
                let current = selection.selected();
                let next = if key == gdk::Key::End {
                    model.n_items() - 1
                } else if key == gdk::Key::Home || current == gtk::INVALID_LIST_POSITION {
                    0
                } else if key == gdk::Key::Down {
                    current.saturating_add(1).min(model.n_items() - 1)
                } else {
                    current.saturating_sub(1)
                };
                selection.set_selected(next);
                view.grab_focus();
                view.scroll_to(next, None, gtk::ListScrollFlags::FOCUS, None);
            } else {
                return glib::Propagation::Proceed;
            }
            glib::Propagation::Stop
        });
    }
    window.add_controller(keys);
    settings.set_sensitive(true);
    {
        let (
            window,
            config,
            workers,
            model,
            generation,
            total,
            status,
            info_label,
            debounce,
            search_status,
        ) = (
            window.downgrade(),
            config.clone(),
            workers.clone(),
            model.clone(),
            generation.clone(),
            total.clone(),
            status.clone(),
            info_label.clone(),
            debounce.clone(),
            search_status.clone(),
        );
        settings.connect_clicked(move |_| {
            let Some(window) = window.upgrade() else {
                return;
            };
            let (
                config,
                workers,
                model,
                generation,
                total,
                status,
                info_label,
                debounce,
                overlay,
                search_status,
            ) = (
                config.clone(),
                workers.clone(),
                model.clone(),
                generation.clone(),
                total.clone(),
                status.clone(),
                info_label.clone(),
                debounce.clone(),
                overlay.clone(),
                search_status.clone(),
            );
            let current = config.borrow().clone();
            super::settings::show(&window, &current, move |updated| {
                if updated.efu_path == config.borrow().efu_path {
                    *config.borrow_mut() = updated;
                    model.rebind();
                    return;
                }
                let database = match updated.database_path() {
                    Ok(database) => database,
                    Err(error) => {
                        overlay.add_toast(adw::Toast::new(&format!("缓存路径错误：{error:#}")));
                        return;
                    }
                };
                if let Some(source) = debounce.borrow_mut().take() {
                    source.remove();
                }
                generation.set(generation.get().wrapping_add(1));
                let replacement = Workers::start(updated.clone(), database);
                replacement
                    .latest
                    .store(generation.get(), std::sync::atomic::Ordering::Relaxed);
                *workers.borrow_mut() = replacement;
                *config.borrow_mut() = updated;
                total.set(0);
                model.results(vec![]);
                search_status.set_text("");
                info_label.set_text(&format!("索引：{}", config.borrow().efu_path.display()));
                status.set_text("正在切换索引…");
            });
        });
    }
    let weak_window = window.downgrade();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        if weak_window.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = workers.borrow().events.try_recv() {
            match event {
                Event::Status {
                    message,
                    info,
                    changed,
                } => {
                    status.set_text(&message);
                    if let Some(info) = info {
                        total.set(info.count);
                        let updated = chrono::DateTime::from_timestamp(info.built_at, 0)
                            .map(|date| {
                                date.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_else(|| "未知".into());
                        info_label.set_text(&format!(
                            "索引：{}    条目数：{}    最后更新：{}",
                            info.source
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                            info.count,
                            updated
                        ));
                    }
                    if changed {
                        searching_feedback(&search, &search_status);
                        refresh(
                            &search,
                            &workers.borrow(),
                            &generation,
                            &model,
                            &selection,
                            total.get(),
                            sort.get(),
                        );
                    }
                }
                Event::Results {
                    generation: received,
                    count,
                    entries,
                } if received == generation.get() => {
                    selection.set_model(None::<&gtk::gio::ListModel>);
                    model.search(count, received, workers.borrow().pages.clone(), sort.get());
                    model.page(received, 0, entries);
                    selection.set_model(Some(&model));
                    if model.n_items() > 0 {
                        selection.set_selected(0);
                    }
                    search_status
                        .set_text(&format!("“{}”：找到 {count} 条（按需加载）", search.text()));
                }
                Event::SearchError {
                    generation: received,
                    message,
                } if received == generation.get() => {
                    search_status.set_text(&message);
                }
                Event::Page {
                    generation: received,
                    offset,
                    entries,
                } if received == generation.get() => {
                    let selected = selection.selected();
                    let replacing = selected >= offset
                        && selected < offset.saturating_add(entries.len() as u32);
                    if replacing {
                        selection.set_selected(gtk::INVALID_LIST_POSITION);
                    }
                    model.page(received, offset, entries);
                    if replacing {
                        selection.set_selected(selected);
                        view.scroll_to(selected, None, gtk::ListScrollFlags::FOCUS, None);
                    } else if selected == gtk::INVALID_LIST_POSITION
                        && offset == 0
                        && model.n_items() > 0
                    {
                        selection.set_selected(0);
                    }
                }
                _ => (),
            }
        }
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
pub(super) fn test_pending_search_feedback() {
    // Called by the existing opt-in GUI test, on the same GTK thread.
    let search = gtk::SearchEntry::new();
    let feedback = gtk::Label::new(None);
    let index_status = gtk::Label::new(None);
    let model = super::virtual_list::VirtualList::new();
    model.results(vec![Entry::new("/old.txt".into(), None, None, None)]);
    for text in ["a", "ab", "abc", "ab", "a", "ext:mkv", "path:Anime"] {
        search.set_text(text);
        searching_feedback(&search, &feedback);
        assert!(feedback.text().contains("正在搜索"));
        assert!(feedback.text().contains("上一次结果"));
        index_status.set_text("索引最新");
        assert!(feedback.text().contains("上一次结果"));
        let row = model
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .expect("old row");
        assert_eq!(row.borrow::<Entry>().path, "/old.txt");
    }
    search.set_text("");
    searching_feedback(&search, &feedback);
    assert_eq!(feedback.text(), "全部索引（按需加载）");
}
