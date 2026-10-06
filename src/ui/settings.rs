use adw::prelude::*;
use efuseek::config::Config;
use efuseek::core::path_map::PathMap;
use gtk::{gio, glib};
use std::{cell::RefCell, rc::Rc};
use std::{path::PathBuf, time::Duration};

type MappingRows = Rc<RefCell<Vec<(gtk::Entry, gtk::Entry, gtk::Box)>>>;
fn add_mapping(group: &adw::PreferencesGroup, rows: &MappingRows, from: &str, to: &str) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(8);
    let from = gtk::Entry::builder()
        .text(from)
        .placeholder_text("例如 X:\\ 或 \\\\NAS\\共享\\")
        .hexpand(true)
        .tooltip_text("索引中的路径前缀")
        .build();
    let to = gtk::Entry::builder()
        .text(to)
        .placeholder_text("例如 /mnt/nas/share/")
        .hexpand(true)
        .tooltip_text("本机 Linux 绝对路径")
        .build();
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("删除此映射")
        .build();
    row.append(&from);
    row.append(&gtk::Label::new(Some("→")));
    row.append(&to);
    row.append(&remove);
    group.add(&row);
    rows.borrow_mut().push((from, to, row.clone()));
    let (group, rows, row) = (group.downgrade(), Rc::downgrade(rows), row.downgrade());
    remove.connect_clicked(move |_| {
        if let (Some(group), Some(rows), Some(row)) =
            (group.upgrade(), rows.upgrade(), row.upgrade())
        {
            group.remove(&row);
            rows.borrow_mut().retain(|(_, _, item)| item != &row);
        }
    });
}

pub fn show(parent: &adw::ApplicationWindow, config: &Config, saved: impl Fn(Config) + 'static) {
    let dialog = adw::PreferencesWindow::builder()
        .transient_for(parent)
        .modal(true)
        .title("设置")
        .default_width(840)
        .default_height(600)
        .build();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .title("EFU 索引文件")
        .description("只读取此文件，不扫描磁盘或 NAS。保存后立即在后台切换索引。旧缓存不会删除。")
        .build();
    let path = adw::EntryRow::builder()
        .title(".efu 文件的本机绝对路径")
        .text(config.efu_path.to_string_lossy())
        .build();
    let choose = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .tooltip_text("选择 .efu 文件")
        .valign(gtk::Align::Center)
        .build();
    path.add_suffix(&choose);
    group.add(&path);
    let mapping_group = adw::PreferencesGroup::builder().title("路径映射").description("左侧为 EFU 中的路径前缀，右侧为本机 Linux 绝对路径。最长前缀优先；直接输入反斜线，无需转义。映射只影响显示和打开，不重建索引。").build();
    let mappings: MappingRows = Rc::new(RefCell::new(vec![]));
    for mapping in &config.path_map {
        add_mapping(&mapping_group, &mappings, &mapping.from, &mapping.to);
    }
    let add = gtk::Button::builder().label("添加映射").build();
    mapping_group.set_header_suffix(Some(&add));
    {
        let (mapping_group, mappings) = (mapping_group.downgrade(), mappings.clone());
        add.connect_clicked(move |_| {
            if let Some(group) = mapping_group.upgrade() {
                add_mapping(&group, &mappings, "", "");
            }
        });
    }
    let save = gtk::Button::builder()
        .label("保存并切换索引")
        .margin_top(12)
        .build();
    save.add_css_class("suggested-action");
    let message = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .margin_top(8)
        .build();
    page.add(&group);
    page.add(&mapping_group);
    let actions = adw::PreferencesGroup::new();
    actions.add(&save);
    actions.add(&message);
    page.add(&actions);
    dialog.add(&page);
    {
        let (dialog, path) = (dialog.downgrade(), path.clone());
        choose.connect_clicked(move |_| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let picker = gtk::FileDialog::builder()
                .title("选择 Everything EFU 文件列表")
                .modal(true)
                .build();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Everything 文件列表 (*.efu)"));
            filter.add_pattern("*.efu");
            filter.add_pattern("*.EFU");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            picker.set_filters(Some(&filters));
            let path = path.clone();
            picker.open(Some(&dialog), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result
                    && let Some(file) = file.path()
                {
                    path.set_text(&file.to_string_lossy());
                }
            });
        });
    }
    {
        let (dialog, choose, path, message) = (
            dialog.downgrade(),
            choose.clone(),
            path.clone(),
            message.clone(),
        );
        let saved = std::rc::Rc::new(saved);
        save.connect_clicked(move |save| {
            let target = PathBuf::from(path.text().as_str());
            if !target.is_absolute() {
                message.set_text("请输入本机绝对路径。");
                return;
            }
            let maps: Vec<_> = mappings
                .borrow()
                .iter()
                .map(|(from, to, _)| PathMap {
                    from: from.text().to_string(),
                    to: to.text().to_string(),
                })
                .collect();
            save.set_sensitive(false);
            mapping_group.set_sensitive(false);
            choose.set_sensitive(false);
            path.set_sensitive(false);
            message.set_text("正在保存配置…");
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = send.send(Config::save_settings(target, maps));
            });
            let (save, choose, path, message, dialog, saved) = (
                save.clone(),
                choose.clone(),
                path.clone(),
                message.clone(),
                dialog.clone(),
                saved.clone(),
            );
            let mapping_group = mapping_group.clone();
            glib::timeout_add_local(Duration::from_millis(16), move || {
                match receive.try_recv() {
                    Ok(Ok(config)) => {
                        saved(config);
                        if let Some(dialog) = dialog.upgrade() {
                            dialog.close();
                        }
                    }
                    Ok(Err(error)) => {
                        message.set_text(&format!("保存失败：{error:#}"));
                        save.set_sensitive(true);
                        choose.set_sensitive(true);
                        path.set_sensitive(true);
                        mapping_group.set_sensitive(true);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        return glib::ControlFlow::Continue;
                    }
                    Err(_) => message.set_text("保存线程异常退出。"),
                }
                glib::ControlFlow::Break
            });
        });
    }
    dialog.present();
}
