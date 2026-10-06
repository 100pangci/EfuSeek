use efuseek::core::{entry::Entry, sort::Sort, worker::PageRequest};
use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    sync::mpsc::Sender,
};

mod imp {
    use super::*;
    #[derive(Default)]
    pub struct VirtualList {
        pub count: Cell<u32>,
        pub generation: Cell<u64>,
        pub sort: Cell<Sort>,
        pub page_size: Cell<u32>,
        pub max_pages: Cell<usize>,
        pub sender: RefCell<Option<Sender<PageRequest>>>,
        pub pages: RefCell<HashMap<u32, Vec<glib::BoxedAnyObject>>>,
        pub pending: RefCell<HashSet<u32>>,
        pub order: RefCell<VecDeque<u32>>,
        pub results: RefCell<Option<Vec<glib::BoxedAnyObject>>>,
        pub placeholders: RefCell<HashMap<u32, glib::WeakRef<glib::BoxedAnyObject>>>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for VirtualList {
        const NAME: &'static str = "EfuSeekVirtualList";
        type Type = super::VirtualList;
        type Interfaces = (gio::ListModel,);
    }
    impl ObjectImpl for VirtualList {}
    impl ListModelImpl for VirtualList {
        fn item_type(&self) -> glib::Type {
            glib::BoxedAnyObject::static_type()
        }
        fn n_items(&self) -> u32 {
            self.count.get()
        }
        fn item(&self, position: u32) -> Option<glib::Object> {
            if position >= self.count.get() {
                return None;
            }
            if let Some(results) = self.results.borrow().as_ref() {
                return results
                    .get(position as usize)
                    .map(|row| row.clone().upcast());
            }
            let page_size = self.page_size.get();
            let offset = position / page_size * page_size;
            if let Some(page) = self.pages.borrow().get(&offset) {
                return page
                    .get((position - offset) as usize)
                    .map(|row| row.clone().upcast());
            }
            if self.pending.borrow_mut().insert(offset)
                && let Some(sender) = self.sender.borrow().as_ref()
            {
                let _ = sender.send(PageRequest {
                    generation: self.generation.get(),
                    offset,
                    limit: page_size,
                    sort: self.sort.get(),
                });
            }
            let mut placeholders = self.placeholders.borrow_mut();
            if let Some(row) = placeholders.get(&position).and_then(glib::WeakRef::upgrade) {
                return Some(row.upcast());
            }
            if placeholders.len() > 4096 {
                placeholders.retain(|_, row| row.upgrade().is_some());
            }
            let mut entry = Entry::new(String::new(), None, None, None);
            entry.name = "正在加载…".into();
            let row = glib::BoxedAnyObject::new(entry);
            placeholders.insert(position, row.downgrade());
            Some(row.upcast())
        }
    }
}
glib::wrapper! { pub struct VirtualList(ObjectSubclass<imp::VirtualList>) @implements gio::ListModel; }

impl VirtualList {
    pub fn rebind(&self) {
        if self.imp().results.borrow().is_some() {
            let count = self.n_items();
            self.items_changed(0, count, count);
        } else {
            let pages: Vec<_> = self
                .imp()
                .pages
                .borrow()
                .iter()
                .map(|(offset, rows)| (*offset, rows.len() as u32))
                .collect();
            for (offset, count) in pages {
                self.items_changed(offset, count, count);
            }
        }
    }
    pub fn new() -> Self {
        let model: Self = glib::Object::new();
        let config = efuseek::config::Config::default();
        model.configure(config.browse_page_size, config.browse_cache_pages);
        model
    }
    pub fn configure(&self, page_size: u32, max_pages: usize) {
        self.imp().page_size.set(page_size.clamp(1, 1024));
        self.imp().max_pages.set(max_pages.clamp(1, 64));
    }
    pub fn browse(&self, total: u64, generation: u64, sender: Sender<PageRequest>, sort: Sort) {
        let imp = self.imp();
        let old = imp.count.get();
        let count = total.min(u64::from(u32::MAX)) as u32;
        imp.count.set(count);
        imp.generation.set(generation);
        imp.sort.set(sort);
        *imp.sender.borrow_mut() = Some(sender);
        imp.results.borrow_mut().take();
        imp.pages.borrow_mut().clear();
        imp.pending.borrow_mut().clear();
        imp.placeholders.borrow_mut().clear();
        imp.order.borrow_mut().clear();
        self.items_changed(0, old, count);
    }
    pub fn results(&self, entries: Vec<Entry>) {
        let imp = self.imp();
        let old = imp.count.get();
        let rows: Vec<_> = entries.into_iter().map(glib::BoxedAnyObject::new).collect();
        imp.count.set(rows.len() as u32);
        *imp.results.borrow_mut() = Some(rows);
        imp.pages.borrow_mut().clear();
        imp.pending.borrow_mut().clear();
        imp.placeholders.borrow_mut().clear();
        self.items_changed(0, old, imp.count.get());
    }
    pub fn page(&self, generation: u64, offset: u32, entries: Vec<Entry>) {
        let imp = self.imp();
        if generation != imp.generation.get() || imp.results.borrow().is_some() {
            return;
        }
        imp.pending.borrow_mut().remove(&offset);
        let remaining = imp.count.get().saturating_sub(offset);
        let rows: Vec<_> = entries
            .into_iter()
            .take(remaining as usize)
            .map(glib::BoxedAnyObject::new)
            .collect();
        let count = rows.len() as u32;
        imp.pages.borrow_mut().insert(offset, rows);
        {
            let mut order = imp.order.borrow_mut();
            order.push_back(offset);
            while order.len() > imp.max_pages.get() {
                if let Some(oldest) = order.pop_front() {
                    imp.pages.borrow_mut().remove(&oldest);
                }
            }
        }
        if count > 0 {
            self.items_changed(offset, count, count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_count_lazy_pages_and_stale_results() {
        let model = VirtualList::new();
        let page_size = model.imp().page_size.get();
        let (send, receive) = std::sync::mpsc::channel();
        model.browse(828_810, 3, send, Sort::default());
        assert_eq!(model.n_items(), 828_810);
        assert!(model.item(828_809).is_some());
        let request = receive.try_recv().expect("page request");
        assert_eq!(request.offset, 828_809 / page_size * page_size);
        assert!(model.item(828_809).is_some());
        assert!(receive.try_recv().is_err());
        model.page(2, 0, vec![Entry::new("/stale".into(), None, None, None)]);
        assert!(model.imp().pages.borrow().is_empty());
        model.page(3, 0, vec![Entry::new("/fresh".into(), None, None, None)]);
        let item = model
            .item(0)
            .and_downcast::<glib::BoxedAnyObject>()
            .expect("entry");
        assert_eq!(item.borrow::<Entry>().path, "/fresh");
        let (send, receive) = std::sync::mpsc::channel();
        let sort = Sort {
            field: efuseek::core::sort::SortField::Modified,
            direction: efuseek::core::sort::SortDirection::Descending,
        };
        model.browse(828_810, 4, send, sort);
        assert!(model.item(0).is_some());
        assert_eq!(receive.try_recv().unwrap().sort, sort);
        model.page(3, 0, vec![Entry::new("/old-sort".into(), None, None, None)]);
        assert!(model.imp().pages.borrow().is_empty());
        model.results(vec![Entry::new("/result".into(), None, None, None)]);
        assert_eq!(model.n_items(), 1);
    }
}
