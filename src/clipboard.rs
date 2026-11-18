//! 剪贴板记录：最近复制的文字，在置物架页的「剪贴板」里查看、再次复制、固定。
//!
//! - 记文字；打开「记录图片」后也记截图和复制的图片（见 `clipboard_image`）。文件不记录。
//! - 最近记录与固定条目都保存在本机 `clipboard.json`。
//! - 密码管理器等标记为「不进剪贴板历史」的内容一律跳过
//!   （`ExcludeClipboardContentFromMonitorProcessing` / `CanIncludeInClipboardHistory = 0`）。
//! - Windows 上用 `AddClipboardFormatListener` 等系统通知，没有轮询；
//!   监听线程只在剪贴板真正变化时醒来。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 未固定条目的上限；固定的不占这个名额。
pub const MAX_RECENT: usize = 30;
/// 单条最长字符数，超过的不记录（大段日志、整份文件内容）。
pub const MAX_CHARS: usize = 20_000;
pub const MAX_PINNED: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    /// 文字条目的内容；图片条目里是识别出的文字（未识别时为空）。
    pub text: String,
    pub image: Option<crate::clipboard_image::ClipImage>,
    /// 本地时间 `HH:MM`。
    pub at: String,
    pub pinned: bool,
}

#[derive(Debug, Default)]
pub struct History {
    pub items: Vec<Clip>,
    /// 每次变化 +1，界面据此判断是否需要重绘。
    pub version: u64,
    path: Option<std::path::PathBuf>,
}

impl History {
    /// 新内容放最前；同样的文字只留一条（移到最前、保留固定状态）。
    pub fn push(&mut self, text: &str, at: &str) -> bool {
        if text.trim().is_empty() || text.chars().count() > MAX_CHARS { return false; }
        let pinned = match self.items.iter().position(|c| c.image.is_none() && c.text == text) {
            Some(0) => return false,
            Some(i) => self.items.remove(i).pinned,
            None => false,
        };
        self.insert(Clip { text: text.to_string(), image: None, at: at.to_string(), pinned });
        true
    }

    /// 同一张图只留一条（移到最前，保留固定状态和已识别的文字）。
    pub fn push_image(&mut self, image: crate::clipboard_image::ClipImage, at: &str) -> bool {
        let (pinned, text) = match self.items.iter().position(|c| c.image.as_ref().is_some_and(|i| i.file == image.file)) {
            Some(0) => return false,
            Some(i) => { let old = self.items.remove(i); (old.pinned, old.text) }
            None => (false, String::new()),
        };
        self.insert(Clip { text, image: Some(image), at: at.to_string(), pinned });
        true
    }

    /// 记下图片识别出的文字。
    pub fn set_image_text(&mut self, file: &str, text: &str) {
        if let Some(clip) = self.items.iter_mut().find(|c| c.image.as_ref().is_some_and(|i| i.file == file)) {
            clip.text = text.chars().take(MAX_CHARS).collect();
            self.changed();
        }
    }

    /// 图片所在目录；内存里的临时记录（测试）没有。
    pub fn images_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.path.as_ref()?.parent()?.join(crate::clipboard_image::DIR))
    }

    fn insert(&mut self, clip: Clip) {
        self.items.insert(0, clip);
        let mut recent = 0;
        self.items.retain(|c| c.pinned || { recent += 1; recent <= MAX_RECENT });
        self.changed();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
            self.changed();
        }
    }

    /// 清空未固定的条目，固定的保留。
    pub fn clear_recent(&mut self) {
        self.items.retain(|c| c.pinned);
        self.changed();
    }

    pub fn toggle_pin(&mut self, index: usize) -> bool {
        let pinned_count = self.items.iter().filter(|c| c.pinned).count();
        let Some(item) = self.items.get_mut(index) else { return false; };
        if !item.pinned && pinned_count >= MAX_PINNED { return false; }
        item.pinned = !item.pinned;
        self.changed();
        true
    }

    fn to_json(&self) -> Vec<u8> {
        let rows: Vec<_> = self.items.iter().map(|c| {
            let mut row = serde_json::json!({"pinned": c.pinned,"text": c.text, "at": c.at});
            if let Some(i) = &c.image { row["image"] = serde_json::json!({"file": i.file, "w": i.w, "h": i.h}); }
            row
        }).collect();
        serde_json::to_vec(&rows).unwrap_or_default()
    }

    pub fn from_json(raw: &[u8]) -> Self {
        let rows: Vec<serde_json::Value> = serde_json::from_slice(raw).unwrap_or_default();
        let items = rows.iter().filter_map(|row| Some(Clip {
            text: row.get("text")?.as_str()?.to_string(),
            image: row.get("image").and_then(|i| {
                let file = i.get("file")?.as_str()?;
                crate::clipboard_image::ClipImage::valid_name(file).then(|| crate::clipboard_image::ClipImage {
                    file: file.to_string(),
                    w: i.get("w").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    h: i.get("h").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                })
            }),
            at: row.get("at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            pinned: row.get("pinned").and_then(|v| v.as_bool()).unwrap_or(true),
        })).filter(|c| !c.text.is_empty() || c.image.is_some()).take(MAX_PINNED + MAX_RECENT).collect();
        Self { items, ..Self::default() }
    }

    fn path() -> std::path::PathBuf { crate::model::config_dir().join("clipboard.json") }

    pub fn load() -> Self {
        Self::load_from(Self::path())
    }

    fn load_from(path: std::path::PathBuf) -> Self {
        let mut history = std::fs::read(&path).map(|raw| Self::from_json(&raw)).unwrap_or_default();
        history.path = Some(path);
        history
    }

    fn changed(&mut self) {
        self.version += 1;
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
            let _ = crate::model::atomic_write(path, &self.to_json());
        }
        self.prune_images();
    }

    /// 删掉不再被任何记录引用的图片和缩略图。
    fn prune_images(&self) {
        let Some(dir) = self.images_dir() else { return };
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let keep: std::collections::HashSet<String> = self.items.iter().filter_map(|c| c.image.as_ref())
            .flat_map(|i| [i.file.clone(), i.file.replace(".png", ".thumb.png")]).collect();
        // 刚写入、还没加进记录的图片（监听线程正在保存）先不删。
        let fresh = |entry: &std::fs::DirEntry| entry.metadata().and_then(|m| m.modified()).ok()
            .and_then(|t| t.elapsed().ok()).is_none_or(|age| age < std::time::Duration::from_secs(60));
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".png") && !keep.contains(&name) && !fresh(&entry) { let _ = std::fs::remove_file(entry.path()); }
        }
    }
}

/// 界面与监听线程共用。
#[derive(Clone)]
pub struct Shared {
    pub history: Arc<Mutex<History>>,
    pub enabled: Arc<AtomicBool>,
    /// 是否也记录图片；默认关闭。
    pub images: Arc<AtomicBool>,
}

impl Shared {
    pub fn new(enabled: bool) -> Self {
        Self { history: Arc::new(Mutex::new(History::load())), enabled: Arc::new(AtomicBool::new(enabled)), images: Arc::new(AtomicBool::new(false)) }
    }
    pub fn set_images(&self, on: bool) { self.images.store(on, Ordering::Relaxed); }
    pub fn images_enabled(&self) -> bool { self.is_enabled() && self.images.load(Ordering::Relaxed) }
    pub fn set_enabled(&self, on: bool) { self.enabled.store(on, Ordering::Relaxed); }
    pub fn is_enabled(&self) -> bool { self.enabled.load(Ordering::Relaxed) }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, History> {
        self.history.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn record_update(&self, text: &str, from_history: bool) -> bool {
        !from_history && self.record(text)
    }
    pub fn record(&self, text: &str) -> bool {
        if !self.is_enabled() { return false; }
        let at = chrono::Local::now().format("%H:%M").to_string();
        self.lock().push(text, &at)
    }
    /// 存图片文件时不持有锁，免得卡住界面。
    pub fn record_image(&self, image: &crate::clipboard_image::Rgba) -> bool {
        if !self.images_enabled() { return false; }
        let Some(dir) = self.lock().images_dir() else { return false };
        let Some(stored) = crate::clipboard_image::store(image, &dir) else { return false };
        let at = chrono::Local::now().format("%H:%M").to_string();
        self.lock().push_image(stored, &at)
    }
}

impl Default for Shared {
    fn default() -> Self { Self::new(true) }
}

/// 列表里的一行预览：换行与连续空白压成一个空格。
pub fn preview(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- Windows 监听

/// 剪贴板监听的持有者；销毁时让监听线程退出并注销系统监听，不等待线程结束。
pub struct Watcher {
    #[cfg(windows)]
    stop: Arc<win::Stop>,
}

#[cfg(windows)]
impl Drop for Watcher {
    fn drop(&mut self) { self.stop.request(); }
}

#[cfg(windows)]
pub fn watch(shared: Shared, repaint: impl Fn() + Send + 'static) -> Option<Watcher> {
    let stop = Arc::new(win::Stop::default());
    let thread_stop = stop.clone();
    std::thread::Builder::new().name("daozai-clipboard".into()).spawn(move || {
        // 启动失败（极少见）时剪贴板页只是没有新记录，不影响其他功能。
        let _ = win::run(shared, Box::new(repaint), &thread_stop);
    }).ok()?;
    Some(Watcher { stop })
}

#[cfg(not(windows))]
pub fn watch(_shared: Shared, _repaint: impl Fn() + Send + 'static) -> Option<Watcher> { Some(Watcher {}) }

#[cfg(windows)]
pub(crate) const HISTORY_COPY_FORMAT: &str = "Daozai.ClipboardHistoryCopy";

#[cfg(windows)]
pub fn copy_history(text: &str) -> bool {
    use clipboard_win::{Clipboard, raw, register_format};
    let Some(format) = register_format(HISTORY_COPY_FORMAT) else { return false };
    let Ok(_clipboard) = Clipboard::new_attempts(10) else { return false };
    raw::set_string(text).is_ok() && raw::set_without_clear(format.get(), &[1]).is_ok()
}

#[cfg(windows)]
mod win {
    use super::Shared;
    use std::cell::RefCell;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    use windows::Win32::UI::WindowsAndMessaging::*;

    const WM_CLIPBOARDUPDATE: u32 = 0x031D;

    struct State { shared: Shared, repaint: Box<dyn Fn() + Send> }

    /// 监听线程建好消息窗口后登记线程号；持有者销毁时先置位再读线程号，两边总有一方看到对方。
    #[derive(Default)]
    pub struct Stop { requested: std::sync::atomic::AtomicBool, thread: std::sync::atomic::AtomicU32 }
    impl Stop {
        pub fn request(&self) {
            use std::sync::atomic::Ordering::SeqCst;
            self.requested.store(true, SeqCst);
            let thread = self.thread.load(SeqCst);
            if thread != 0 { unsafe { let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)); } }
        }
        fn register(&self) -> bool {
            use std::sync::atomic::Ordering::SeqCst;
            self.thread.store(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }, SeqCst);
            !self.requested.load(SeqCst)
        }
    }
    thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_CLIPBOARDUPDATE {
            STATE.with(|state| {
                if let Some(state) = state.borrow().as_ref() {
                    if state.shared.is_enabled() {
                        let changed = match read(hwnd, state.shared.images_enabled()) {
                            Some((Content::Text(text), from_history)) => state.shared.record_update(&text, from_history),
                            Some((Content::Image(raw), false)) => decode(raw).is_some_and(|image| state.shared.record_image(&image)),
                            _ => false,
                        };
                        if changed { (state.repaint)(); }
                    }
                }
            });
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    /// 密码管理器、远程桌面等会给敏感内容加上这些格式，表示不要进入剪贴板历史。
    unsafe fn excluded() -> bool {
        let exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        let ignore = RegisterClipboardFormatW(w!("Clipboard Viewer Ignore"));
        if (exclude != 0 && IsClipboardFormatAvailable(exclude).is_ok()) || (ignore != 0 && IsClipboardFormatAvailable(ignore).is_ok()) {
            return true;
        }
        // 值为 DWORD 0 表示不允许进入历史。
        let history = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
        if history == 0 || IsClipboardFormatAvailable(history).is_err() { return false; }
        let Ok(handle) = GetClipboardData(history) else { return true; };
        let memory = HGLOBAL(handle.0);
        let ptr = GlobalLock(memory) as *const u32;
        if ptr.is_null() { return true; }
        let allowed = GlobalSize(memory) >= 4 && *ptr != 0;
        let _ = GlobalUnlock(memory);
        !allowed
    }

    enum Raw { Png(Vec<u8>), Dib(Vec<u8>) }
    enum Content { Text(String), Image(Raw) }

    /// 剪贴板关着以后再解码，不占着剪贴板。
    fn decode(raw: Raw) -> Option<crate::clipboard_image::Rgba> {
        match raw {
            Raw::Png(bytes) => crate::clipboard_image::decode_png(&bytes),
            Raw::Dib(bytes) => crate::clipboard_image::decode_dib(&bytes),
        }
    }

    unsafe fn bytes(format: u32) -> Option<Vec<u8>> {
        const MAX: usize = 256 << 20;
        let handle = GetClipboardData(format).ok()?;
        let memory = HGLOBAL(handle.0);
        let ptr = GlobalLock(memory) as *const u8;
        if ptr.is_null() { return None; }
        let size = GlobalSize(memory);
        let out = (size <= MAX).then(|| std::slice::from_raw_parts(ptr, size).to_vec());
        let _ = GlobalUnlock(memory);
        out
    }

    /// 有文字时只记文字；没有文字且开了「记录图片」时读图片，优先无损的 `PNG` 格式。
    unsafe fn read(hwnd: HWND, images: bool) -> Option<(Content, bool)> {
        const CF_DIB: u32 = 8;
        let text = IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok();
        let png = RegisterClipboardFormatW(w!("PNG"));
        let has_png = png != 0 && IsClipboardFormatAvailable(png).is_ok();
        if !text && !(images && (has_png || IsClipboardFormatAvailable(CF_DIB).is_ok())) { return None; }
        // 其他程序可能正占用剪贴板，稍等重试几次。
        let mut opened = false;
        for _ in 0..5 {
            if OpenClipboard(hwnd).is_ok() { opened = true; break; }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened { return None; }
        let content = (|| {
            if excluded() { return None; }
            let from_history = clipboard_win::register_format(super::HISTORY_COPY_FORMAT)
                .is_some_and(|format| IsClipboardFormatAvailable(format.get()).is_ok());
            if !text {
                let raw = if has_png { bytes(png).map(Raw::Png) } else { None };
                return raw.or_else(|| bytes(CF_DIB).map(Raw::Dib)).map(|raw| (Content::Image(raw), from_history));
            }
            let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let memory = HGLOBAL(handle.0);
            let ptr = GlobalLock(memory) as *const u16;
            if ptr.is_null() { return None; }
            let max = (GlobalSize(memory) / 2).min(super::MAX_CHARS * 2 + 1);
            let mut len = 0;
            while len < max && *ptr.add(len) != 0 { len += 1; }
            let text = (len <= super::MAX_CHARS * 2).then(|| String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
            let _ = GlobalUnlock(memory);
            text.map(|text| (Content::Text(text), from_history))
        })();
        let _ = CloseClipboard();
        content
    }

    pub fn run(shared: Shared, repaint: Box<dyn Fn() + Send>, stop: &Stop) -> windows::core::Result<()> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = w!("DaozaiClipboardWatcher");
            let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, HWND_MESSAGE, None, instance, None)?;
            // 消息窗口建好后线程已有消息队列，此后投递的 WM_QUIT 不会丢。
            if !stop.register() { let _ = DestroyWindow(hwnd); return Ok(()); }
            STATE.with(|state| *state.borrow_mut() = Some(State { shared, repaint }));
            AddClipboardFormatListener(hwnd)?;
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = RemoveClipboardFormatListener(hwnd);
            let _ = DestroyWindow(hwnd);
            STATE.with(|state| *state.borrow_mut() = None);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn dropping_the_watcher_ends_the_listener_thread(){
        // 先停后启和启动后再停两种顺序都必须让线程退出，不能留下常驻的监听窗口。
        for stop_first in [true,false] {
            let stop=Arc::new(win::Stop::default());let thread_stop=stop.clone();
            let shared=Shared{history:Default::default(),enabled:Arc::new(AtomicBool::new(false)),images:Default::default()};
            if stop_first {stop.request();}
            let (done,finished)=std::sync::mpsc::channel();
            std::thread::spawn(move||{let _=win::run(shared,Box::new(||{}),&thread_stop);let _=done.send(());});
            if !stop_first {std::thread::sleep(std::time::Duration::from_millis(200));stop.request();}
            finished.recv_timeout(std::time::Duration::from_secs(3)).expect("监听线程应在停止后退出");
        }
    }

    #[test]
    fn history_copy_preserves_order_but_external_copy_promotes() {
        let shared = Shared { history: Default::default(), enabled: Arc::new(AtomicBool::new(true)), images: Default::default() };
        shared.lock().items.clear();
        shared.record("较早的一条");
        shared.record("最近的一条");
        let before = shared.lock().items.clone();
        assert!(!shared.record_update("较早的一条", true));
        assert_eq!(shared.lock().items, before);
        assert!(shared.record_update("较早的一条", false));
        assert_eq!(shared.lock().items[0].text, "较早的一条");
    }
    #[test]
    fn history_dedupes_caps_and_keeps_pins() {
        let mut h = History::default();
        assert!(!h.push("   \n", "09:00"), "空白不记录");
        assert!(h.push("a", "09:00") && h.push("b", "09:01"));
        assert!(!h.push("b", "09:02"), "与最新一条相同不重复记录");
        assert!(h.push("a", "09:03"));
        assert_eq!(h.items.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(h.items[0].at, "09:03");
        h.items[1].pinned = true; // b
        for i in 0..(MAX_RECENT + 5) { h.push(&format!("n{i}"), "10:00"); }
        assert_eq!(h.items.iter().filter(|c| !c.pinned).count(), MAX_RECENT);
        assert!(h.items.iter().any(|c| c.text == "b" && c.pinned), "固定的不会被挤掉");
        h.clear_recent();
        assert_eq!(h.items.len(), 1);
        assert!(!h.push(&"x".repeat(MAX_CHARS + 1), "10:00"), "超长内容不记录");
    }

    #[test]
    fn reopening_restores_history_and_respects_manual_cleanup() {
        let path = std::env::temp_dir().join(format!("daozai-clip-{}.json", std::process::id()));
        std::fs::write(&path, br#"[{"text":"old","at":"08:00"}]"#).unwrap();
        let mut history = History::load_from(path.clone());
        assert!(history.items[0].pinned);
        history.push("保留", "09:00");
        history.push("删除", "09:01");
        history.remove(0);
        let mut reopened = History::load_from(path.clone());
        assert_eq!(reopened.items, history.items);
        reopened.clear_recent();
        let final_history = History::load_from(path.clone());
        assert_eq!(final_history.items.len(), 1);
        assert_eq!(final_history.items[0].text, "old");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn images_dedupe_keep_text_and_round_trip() {
        use crate::clipboard_image::ClipImage;
        let img = |n: u8| ClipImage { file: format!("{:016x}.png", n), w: 10, h: 5 };
        let mut h = History::default();
        assert!(h.push_image(img(1), "09:00"));
        assert!(!h.push_image(img(1), "09:01"), "与最新一张相同不重复记录");
        assert!(h.push("文字", "09:02"));
        h.set_image_text(&img(1).file, "识别结果");
        assert!(h.push("识别结果", "09:03"), "同样的文字与图片条目互不合并");
        assert!(h.push_image(img(1), "09:04"));
        assert_eq!(h.items.len(), 3);
        assert_eq!((h.items[0].image.as_ref(), h.items[0].text.as_str()), (Some(&img(1)), "识别结果"));
        let back = History::from_json(&h.to_json());
        assert_eq!(back.items, h.items);
        let tampered = br#"[{"text":"","image":{"file":"../secret.png","w":1,"h":1}},{"text":"","image":{"file":"00000000000000aa.png","w":2,"h":3}}]"#;
        let back = History::from_json(tampered);
        assert_eq!(back.items.len(), 1, "不认识的文件名丢弃");
        assert_eq!(back.items[0].image.as_ref().map(|i| (i.w, i.h)), Some((2, 3)));
    }

    #[test]
    fn removed_images_are_deleted_from_disk() {
        use crate::clipboard_image::{store, Rgba};
        let root = std::env::temp_dir().join(format!("daozai-clipdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut h = History::load_from(root.join("clipboard.json"));
        let dir = h.images_dir().unwrap();
        let a = store(&Rgba { w: 1, h: 1, px: vec![1, 2, 3, 255] }, &dir).unwrap();
        let b = store(&Rgba { w: 1, h: 1, px: vec![9, 9, 9, 255] }, &dir).unwrap();
        h.push_image(a.clone(), "09:00");
        h.push_image(b.clone(), "09:01");
        // 模拟一分钟前写入，避开「刚保存」的保护。
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        for p in [a.path(&dir), a.thumb_path(&dir), b.path(&dir), b.thumb_path(&dir)] {
            std::fs::File::options().write(true).open(p).unwrap().set_modified(old).unwrap();
        }
        h.remove(1); // a
        assert!(!a.path(&dir).exists() && !a.thumb_path(&dir).exists());
        assert!(b.path(&dir).exists() && b.thumb_path(&dir).exists());
        assert_eq!(History::load_from(root.join("clipboard.json")).items, h.items);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pinned_round_trip_and_preview() {
        let mut h = History::default();
        h.push("第一行\n第二行  末尾", "08:00");
        h.push("临时", "08:01");
        h.items[1].pinned = true;
        let back = History::from_json(&h.to_json());
        assert_eq!(back.items, h.items);
        assert_eq!(preview(&back.items[1].text), "第一行 第二行 末尾");
        assert!(History::from_json(b"not json").items.is_empty());
    }
}
