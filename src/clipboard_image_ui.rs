//! 剪贴板页里图片条目的缩略图、再次复制与文字识别。
use crate::{app::{App, Cmd}, clipboard::Clip, clipboard_image::ClipImage, pages::IMG_TILES, paint::{self, capsule, Card, H}, theme::{self, size, W}};
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

    /// 在卡片坐标 (x, y, w, h) 里铺满画缩略图：比例不同时裁掉多出的部分，不拉伸。
    pub(crate) fn paint_clip_cover(&mut self, c: &Card, image: &ClipImage, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        let Some(tex) = self.clip_thumb(c.ui.ctx(), image) else {
            c.fill(x, y, w, h, radius, Color32::from_gray(60));
            return;
        };
        let rect = Rect::from_min_max(c.pos(x, y), c.pos(x + w, y + h));
        let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);
        let (cell, img) = (w / h, tw / th);
        let uv = if img > cell { let k = cell / img; Rect::from_min_max(Pos2::new((1.0 - k) / 2.0, 0.0), Pos2::new((1.0 + k) / 2.0, 1.0)) }
            else { let k = img / cell; Rect::from_min_max(Pos2::new(0.0, (1.0 - k) / 2.0), Pos2::new(1.0, (1.0 + k) / 2.0)) };
        c.painter.add(egui::epaint::RectShape::filled(rect, radius * c.s, Color32::WHITE).with_texture(tex.id(), uv));
    }

    /// 有图片记录或开着「记录图片」时，置物架多出「图片」分栏。
    pub(crate) fn clipboard_images_tab(&self) -> bool {
        self.settings.clipboard_history && (self.settings.clipboard_images || self.clipboard.lock().items.iter().any(|c| c.image.is_some()))
    }

    fn clip_image_path(&self, image: &ClipImage) -> Option<std::path::PathBuf> {
        Some(image.path(&self.clipboard.lock().images_dir()?))
    }

    /// 置物架「图片」分栏：一屏四张大缩略图，滚轮查看更多。
    pub(crate) fn page_clip_images(&mut self, c: &Card, cmds: &mut Vec<Cmd>) {
        let all = self.clipboard.lock().items.clone();
        self.clip_thumbs.retain(|file, _| all.iter().any(|clip| clip.image.as_ref().is_some_and(|i| &i.file == file)));
        let items: Vec<(usize, Clip, ClipImage)> = all.into_iter().enumerate().filter_map(|(i, clip)| { let image = clip.image.clone()?; Some((i, clip, image)) }).collect();
        let n = items.len();
        self.clip_img_first = self.clip_img_first.min(n.saturating_sub(IMG_TILES));
        self.clip_clear_button(c, items.iter().filter(|(_, clip, _)| !clip.pinned).count(), true);
        if n == 0 {
            c.dashed(42.0, 32.0, 335.0, 91.0, 12.0, 1.2, Color32::from_gray(70), 0.0);
            c.text(209.5, 72.0, "截图和复制的图片会出现在这里", 12.0, W::Semibold, theme::WHITE, H::Center);
            let hint = if self.settings.clipboard_images { "Win+Shift+S 截图后自动记录 · 点「识字」取出文字" } else { "在设置 → 剪贴板里打开「记录图片」" };
            c.text(209.5, 90.0, hint, size::TINY, W::Regular, theme::GRAY, H::Center);
            return;
        }
        let (x0, y0, tw, th, gap) = (44.5, 33.0, 78.0, 62.0, 6.0);
        let mut action: Option<(usize, u8)> = None; // 0 复制 1 固定 2 删除 3 识字 4 大图 5 文件夹
        for (slot, (index, clip, image)) in items.iter().skip(self.clip_img_first).take(IMG_TILES).enumerate() {
            let index = *index;
            let x = x0 + slot as f32 * (tw + gap);
            let r = c.hit_drag(x, y0, tw, th, ("clip-img", &image.file));
            // 拖出时带上图片文件，聊天窗口、文件夹和文档都能直接放下。
            if r.drag_started() {
                match self.clip_image_path(image) {
                    Some(path) if path.exists() => cmds.push(Cmd::DragFile(path)),
                    _ => self.say("图片文件已被删除"),
                }
            }
            let (h, pr) = c.hover_t(&r);
            let cc = c.scaled(x + tw / 2.0, y0 + th / 2.0, 1.0 - 0.03 * pr);
            self.paint_clip_cover(&cc, image, x, y0, tw, th, 8.0);
            if h > 0.01 { cc.fill(x, y0, tw, th, 8.0, Color32::from_white_alpha((14.0 * h) as u8)); }
            // 左上固定、右上删除：悬停或已固定时显示。
            if clip.pinned || h > 0.01 {
                let a = if clip.pinned { 1.0 } else { h };
                let ic = c.faded(a);
                ic.circle(x + 11.0, y0 + 11.0, 8.0, Color32::from_black_alpha(150));
                paint::pin(&ic, x + 11.0, y0 + 11.0, 8.0, if clip.pinned { theme::BLUE } else { theme::WHITE });
            }
            let pin = c.hit(x + 2.0, y0 + 2.0, 18.0, 18.0, ("clip-img-pin", &image.file));
            if h > 0.01 {
                let ic = c.faded(h);
                ic.circle(x + tw - 11.0, y0 + 11.0, 8.0, Color32::from_black_alpha(150));
                paint::close(&ic, x + tw - 11.0, y0 + 11.0, 2.4, theme::WHITE);
            }
            let delete = c.hit(x + tw - 20.0, y0 + 2.0, 18.0, 18.0, ("clip-img-delete", &image.file));
            // 下方：时间与「识字」。
            c.text(x + 2.0, y0 + th + 15.0, &clip.at, size::TINY, W::Regular, theme::DIM, H::Left);
            let pending = self.clip_ocr_pending(image);
            let (ocr, bc) = capsule(c, x + tw - 34.0, y0 + th + 4.0, 34.0, 16.0, &format!("clip-ocr-{}", image.file), theme::BTN, theme::BTN_HOVER);
            bc.text(x + tw - 17.0, y0 + th + 15.5, if pending { "识别中" } else if clip.text.is_empty() { "识字" } else { "文字" }, size::TINY, W::Semibold, theme::WHITE, H::Center);
            let ocr = ocr.on_hover_text(if clip.text.is_empty() { "识别图片里的文字并复制（在本机完成）" } else { "复制识别出的文字" });
            let menu_open = r.context_menu_opened();
            let r = if menu_open { r } else {
                let text = clip.text.clone();
                r.on_hover_ui(|ui| self.clip_image_tooltip(ui, image, &text))
            };
            r.context_menu(|ui| {
                if ui.button("复制图片").clicked() { action = Some((index, 0)); ui.close_menu(); }
                if ui.button(if clip.text.is_empty() { "识字并复制" } else { "复制识别出的文字" }).clicked() { action = Some((index, 3)); ui.close_menu(); }
                if ui.button("查看大图").clicked() { action = Some((index, 4)); ui.close_menu(); }
                if ui.button("在文件夹中显示").clicked() { action = Some((index, 5)); ui.close_menu(); }
                if ui.button(if clip.pinned { "取消固定" } else { "固定（清空时保留）" }).clicked() { action = Some((index, 1)); ui.close_menu(); }
                if ui.button("删除").clicked() { action = Some((index, 2)); ui.close_menu(); }
            });
            if action.is_some() {}
            else if delete.clicked() { action = Some((index, 2)); }
            else if pin.clicked() { action = Some((index, 1)); }
            else if ocr.clicked() { if !pending { action = Some((index, 3)); } }
            else if r.double_clicked() { action = Some((index, 4)); }
            else if r.clicked() { action = Some((index, 0)); }
        }
        if n > IMG_TILES {
            c.text(209.5, 122.0, &format!("{}–{} / {n} · 滚轮查看", self.clip_img_first + 1, (self.clip_img_first + IMG_TILES).min(n)), 10.0, W::Regular, theme::DIM, H::Center);
        }
        let Some((index, kind)) = action else { return };
        let Some((_, clip, image)) = items.iter().find(|(i, _, _)| *i == index) else { return };
        match kind {
            0 => self.copy_clip_image(image),
            1 => { if !self.clipboard.lock().toggle_pin(index) { self.say(format!("最多固定 {} 条", crate::clipboard::MAX_PINNED)); } }
            2 => self.clipboard.lock().remove(index),
            3 => self.recognize_clip_image(c.ui.ctx(), image, &clip.text),
            4 | 5 => match self.clip_image_path(image) {
                Some(path) if path.exists() => cmds.push(if kind == 4 { Cmd::Open(path) } else { Cmd::Reveal(path) }),
                _ => self.say("图片文件已被删除"),
            },
            _ => {}
        }
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
        ui.label("点击复制 · 拖到其他程序直接放下 · 双击查看大图 · 右键更多");
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
