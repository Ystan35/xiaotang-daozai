//! 剪贴板页里图片条目的缩略图、再次复制与文字识别。
use crate::{app::App, clipboard_image::ClipImage};
use eframe::egui::{self, Color32, Pos2, Rect};

impl App {
    /// 缩略图按文件名缓存；只保留当前记录里还在的。
    pub(crate) fn clip_thumb(&mut self, ctx: &egui::Context, image: &ClipImage) -> Option<egui::TextureHandle> {
        if let Some(tex) = self.clip_thumbs.get(&image.file) { return tex.clone(); }
        let dir = self.clipboard.lock().images_dir();
        let tex = dir.and_then(|dir| std::fs::read(image.thumb_path(&dir)).ok())
            .and_then(|bytes| crate::clipboard_image::decode_png(&bytes))
            .map(|img| ctx.load_texture(format!("clip-{}", image.file), egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.px), egui::TextureOptions::LINEAR));
        self.clip_thumbs.insert(image.file.clone(), tex.clone());
        tex
    }

    /// 在卡片坐标 (x, cy) 处画缩略图（左对齐、垂直居中），返回占用宽度。
    pub(crate) fn paint_clip_thumb(&mut self, c: &crate::paint::Card, image: &ClipImage, x: f32, cy: f32, h: f32, max_w: f32) -> f32 {
        let ratio = if image.h == 0 { 1.0 } else { image.w as f32 / image.h as f32 };
        let w = (h * ratio).clamp(h * 0.6, max_w);
        let Some(tex) = self.clip_thumb(c.ui.ctx(), image) else {
            c.fill(x, cy - h / 2.0, w, h, 3.0, Color32::from_gray(60));
            return w;
        };
        let rect = Rect::from_min_max(c.pos(x, cy - h / 2.0), c.pos(x + w, cy + h / 2.0));
        // 宽高比和格子不一致时裁掉多出的部分，不拉伸。
        let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);
        let (cell, img) = (w / h, tw / th);
        let uv = if img > cell { let k = cell / img; Rect::from_min_max(Pos2::new((1.0 - k) / 2.0, 0.0), Pos2::new((1.0 + k) / 2.0, 1.0)) }
            else { let k = img / cell; Rect::from_min_max(Pos2::new(0.0, (1.0 - k) / 2.0), Pos2::new(1.0, (1.0 + k) / 2.0)) };
        c.painter.image(tex.id(), rect, uv, Color32::WHITE);
        w
    }

    pub(crate) fn clip_image_tooltip(&mut self, ui: &mut egui::Ui, image: &ClipImage, text: &str) {
        if let Some(tex) = self.clip_thumb(ui.ctx(), image) {
            let size = tex.size_vec2();
            let k = (240.0 / size.x.max(size.y)).min(1.0);
            ui.image((tex.id(), size * k));
        }
        ui.label(format!("{}×{}", image.w, image.h));
        if !text.is_empty() {
            let preview: String = text.chars().take(400).collect();
            ui.label(format!("{preview}{}", if text.chars().count() > 400 { "…" } else { "" }));
        }
        ui.label("点击复制图片");
    }

    pub(crate) fn copy_clip_image(&mut self, image: &ClipImage) {
        let Some(dir) = self.clipboard.lock().images_dir() else { return };
        #[cfg(windows)]
        self.say(if crate::clipboard_image::copy(&image.path(&dir), crate::clipboard::HISTORY_COPY_FORMAT) { "已复制图片" } else { "图片已被删除或复制失败" });
        #[cfg(not(windows))]
        { let _ = (dir, image); self.say("复制图片仅支持 Windows"); }
    }

    pub(crate) fn clip_ocr_pending(&self, image: &ClipImage) -> bool {
        self.clip_ocr.as_ref().is_some_and(|(file, _)| *file == image.file)
    }

    /// 已识别过的直接复制文字；否则在后台识别，完成后复制并记到这条记录上。
    pub(crate) fn recognize_clip_image(&mut self, ctx: &egui::Context, image: &ClipImage, text: &str) {
        if !text.is_empty() { self.copy_ocr_text(text); return; }
        if self.clip_ocr.is_some() { self.say("正在识别上一张图片"); return; }
        let Some(dir) = self.clipboard.lock().images_dir() else { return };
        let path = image.path(&dir);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let started = std::thread::Builder::new().name("daozai-ocr".into()).spawn(move || {
            let _ = tx.send(crate::clipboard_image::recognize(&path));
            ctx.request_repaint();
        });
        if started.is_ok() { self.clip_ocr = Some((image.file.clone(), rx)); }
    }

    fn copy_ocr_text(&mut self, text: &str) {
        #[cfg(windows)]
        let copied = crate::clipboard::copy_history(text);
        #[cfg(not(windows))]
        let copied = false;
        let count = text.chars().filter(|c| !c.is_whitespace()).count();
        self.say(if copied { format!("已复制识别出的 {count} 个字") } else { "复制失败，请重试".to_string() });
    }

    pub(crate) fn clipboard_ocr_tick(&mut self) {
        let Some((file, rx)) = &self.clip_ocr else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("文字识别失败".into()),
        };
        let file = file.clone();
        self.clip_ocr = None;
        match result {
            Ok(text) if text.trim().is_empty() => self.say("图片里没有识别到文字"),
            Ok(text) => { self.clipboard.lock().set_image_text(&file, &text); self.copy_ocr_text(&text); }
            Err(e) => self.say(e),
        }
    }
}
