//! The Ask page (quick Gemini questions) and its settings tab.
use crate::app::{App, Cmd, ASK_H, ASK_PAGE};
use crate::ask::{self, Entry, History};
use crate::paint::{capsule, Card, H};
use crate::theme::{self, W};
use eframe::egui::{self, Color32, Rect};
use std::sync::mpsc::Receiver;

/// Answers on top, the text field just above the action row, like a chat app.
pub(crate) const BODY: (f32, f32, f32, f32) = (42.0, 36.0, 335.0, ASK_H - 36.0 - 78.0);
const FIELD: (f32, f32, f32, f32) = (42.0, ASK_H - 72.0, 335.0, 28.0);
const ACTIONS_Y: f32 = ASK_H - 37.0;
const PRUNE_EVERY: f64 = 600.0;

pub(crate) struct Attachment {
    pub image: ask::Image,
    pub thumb: Option<egui::ColorImage>,
    pub texture: Option<egui::TextureHandle>,
}

#[derive(Default)]
pub(crate) struct AskState {
    pub buf: String,
    pub focused: bool,
    pub history: History,
    pub thread: u64,
    pub pending: Option<(Receiver<Result<String, String>>, String)>,
    pub pending_image: bool,
    pub attachment: Option<Attachment>,
    pub show_history: bool,
    pub has_key: bool,
    pub key_buf: String,
    pub key_status: Option<(String, bool)>,
    pub pruned_at: f64,
    pub was_on_page: bool,
    /// Focus the field on the next frame (hotkey, new picture).
    pub want_focus: bool,
    /// Let go of the field on the next frame (before a screen snip).
    pub release: bool,
}

impl AskState {
    pub fn load(keep_days: u32) -> Self {
        AskState { history: History::load(keep_days), thread: new_thread(), has_key: ask::load_key().is_some(), ..Default::default() }
    }
    pub fn thread_entries(&self) -> Vec<&Entry> { self.history.thread(self.thread) }
    pub fn last_answer(&self) -> Option<String> {
        self.thread_entries().last().filter(|e| !e.error).map(|e| e.a.clone())
    }
}

fn new_thread() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub(crate) fn ago(now: u64, at: u64) -> String {
    let d = now.saturating_sub(at);
    match d {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86_399 => format!("{}h ago", d / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{}d ago", d / 86_400),
    }
}

pub(crate) fn keep_label(days: u32) -> String {
    match days { 0 => "Off".into(), 1 => "1 day".into(), d => format!("{d} days") }
}

fn thumbnail(img: &ask::Image) -> Option<egui::ColorImage> {
    let decoded = image::load_from_memory(&img.data).ok()?.thumbnail(120, 72).to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([decoded.width() as usize, decoded.height() as usize], decoded.as_raw()))
}

fn with_image_tag(q: &str, image: bool) -> String { if image { format!("[image] {q}") } else { q.to_string() } }

impl App {
    /// Ctrl+Alt+Space: opens the island on Ask with the cursor in the field; again closes it.
    pub fn toggle_ask(&mut self) -> bool {
        if self.key_open && self.open && self.page == ASK_PAGE { self.close_keyboard_open(); self.ask.release = true; self.hover_lock = true; return false; }
        self.open_ask();
        true
    }

    fn open_ask(&mut self) {
        if !self.module_on(ASK_PAGE) { self.set_page_visible(ASK_PAGE, true); }
        if self.page != ASK_PAGE { self.show_page(ASK_PAGE); }
        self.reveal_unread();
        self.forced_open_until = f64::INFINITY;
        self.key_open = true;
        self.key_open_at = self.t;
        self.ask.show_history = false;
        self.ask.want_focus = true;
    }

    /// Attaches a picture (snip, file or paste) and opens Ask ready to type.
    pub fn ask_attach(&mut self, img: &crate::clipboard_image::Rgba) -> bool {
        match ask::prepare_image(img) {
            Some(image) => {
                let thumb = thumbnail(&image);
                self.ask.attachment = Some(Attachment { image, thumb, texture: None });
                self.open_ask();
                true
            }
            None => { self.say("Couldn't read that picture"); false }
        }
    }

    pub fn ask_attach_file(&mut self, path: &std::path::Path) -> bool {
        match ask::load_image_file(path) {
            Ok(img) => self.ask_attach(&img),
            Err(e) => { self.say(e); false }
        }
    }

    /// Collapses the island so it isn't in the screen snip.
    /// A finished screen snip: attach it, and send straight away if a question was typed before snipping.
    pub fn ask_snip_arrived(&mut self, img: &crate::clipboard_image::Rgba, ctx: &egui::Context) -> bool {
        if !self.ask_attach(img) { return false; }
        let typed = self.ask.buf.trim().to_string();
        if !typed.is_empty() && self.ask.pending.is_none() {
            self.ask.buf.clear();
            if !self.ask_submit(typed.clone(), Some(ctx.clone())) { self.ask.buf = typed; }
        }
        true
    }

    pub fn ask_before_snip(&mut self) {
        self.say("Drag over the area to ask about");
        self.close_keyboard_open();
        self.ask.release = true;
        self.ask.focused = false;
        self.hover_lock = true;
    }

    /// Sends the current input (or `question`) to Gemini on a background thread.
    pub(crate) fn ask_submit(&mut self, question: String, ctx: Option<egui::Context>) -> bool {
        let mut question = question.trim().to_string();
        if self.ask.pending.is_some() { return false; }
        if question.is_empty() {
            if self.ask.attachment.is_none() { return false; }
            question = ask::IMAGE_ONLY_QUESTION.into();
        }
        if !self.ask.has_key { self.say("Add your Gemini key in Settings > Ask"); return false; }
        let context: Vec<(String, String)> = self.ask.thread_entries().iter().filter(|e| !e.error).map(|e| (e.q.clone(), e.a.clone())).collect();
        let image = self.ask.attachment.take().map(|a| a.image);
        self.ask.pending_image = image.is_some();
        let rx = ask::spawn(self.settings.ask_model.clone(), context, question.clone(), image, ctx);
        self.ask.pending = Some((rx, question));
        self.ask.show_history = false;
        true
    }

    /// Accepts a finished answer (also used by tests).
    pub(crate) fn ask_finish(&mut self, question: String, result: Result<String, String>) {
        let error = result.is_err();
        let a = result.unwrap_or_else(|e| e);
        if error && a.contains("API key") && !a.contains("region") { self.ask.has_key = ask::load_key().is_some(); }
        let image = std::mem::take(&mut self.ask.pending_image);
        self.ask.history.entries.push(Entry { q: question, a, at: ask::now_secs(), thread: self.ask.thread, error, image });
        self.ask.history.prune(self.settings.ask_keep_days, ask::now_secs());
        if !error { self.ask.history.save(self.settings.ask_keep_days); }
    }

    pub(crate) fn ask_tick(&mut self) {
        if let Some((rx, q)) = &self.ask.pending {
            match rx.try_recv() {
                Ok(result) => { let q = q.clone(); self.ask.pending = None; self.ask_finish(q, result); }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => { let q = q.clone(); self.ask.pending = None; self.ask_finish(q, Err("Gemini request stopped unexpectedly".into())); }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.t - self.ask.pruned_at > PRUNE_EVERY {
            self.ask.pruned_at = self.t;
            if self.ask.history.prune(self.settings.ask_keep_days, ask::now_secs()) { self.ask.history.save(self.settings.ask_keep_days); }
        }
        if self.page != ASK_PAGE || !self.open { self.ask.was_on_page = false; if self.ask.focused { self.ask.focused = false; } }
    }

    pub(crate) fn ask_new_chat(&mut self) {
        self.ask.thread = new_thread();
        self.ask.show_history = false;
        self.ask.buf.clear();
    }

    fn ask_input(&mut self, c: &Card, cmds: &mut Vec<Cmd>) {
        let ctx = c.ui.ctx().clone();
        let (fx, fy, fw, fh) = FIELD;
        c.fill(fx, fy, fw, fh, fh / 2.0, theme::BTN);
        if self.ask.focused { c.stroke(fx, fy, fw, fh, fh / 2.0, 1.0, theme::BLUE.gamma_multiply(0.7)); }
        // Attached picture: a small thumbnail at the left end; click it to remove.
        let mut text_x = fx + 13.0;
        let mut remove = false;
        if let Some(att) = &mut self.ask.attachment {
            if att.texture.is_none() { if let Some(thumb) = att.thumb.take() { att.texture = Some(ctx.load_texture("ask-attachment", thumb, egui::TextureOptions::LINEAR)); } }
            let (tw, th) = (34.0, 20.0);
            let r = c.hit(fx + 5.0, fy + 4.0, tw, th, "ask-thumb").on_hover_text(format!("{}×{} picture · click to remove", att.image.w, att.image.h));
            let rect = c.rect(fx + 5.0, fy + 4.0, tw, th);
            match &att.texture {
                Some(tex) => {
                    let size = tex.size_vec2();
                    let scale = (rect.width() / size.x).max(rect.height() / size.y);
                    let shown = egui::vec2(rect.width() / (size.x * scale), rect.height() / (size.y * scale));
                    let uv = Rect::from_center_size(egui::pos2(0.5, 0.5), shown);
                    c.painter.add(egui::epaint::RectShape::filled(rect, 5.0 * c.s, Color32::WHITE).with_texture(tex.id(), uv));
                }
                None => c.fill(fx + 5.0, fy + 4.0, tw, th, 5.0, theme::BTN_HOVER),
            }
            if r.hovered() {
                c.fill(fx + 5.0, fy + 4.0, tw, th, 5.0, Color32::from_black_alpha(150));
                crate::paint::close(c, fx + 5.0 + tw / 2.0, fy + 4.0 + th / 2.0, 3.5, theme::WHITE);
            }
            remove = r.clicked();
            text_x = fx + 5.0 + tw + 8.0;
        }
        if remove { self.ask.attachment = None; }
        let icons_w = 76.0;
        let rect = c.rect(text_x, fy + 6.0, fx + fw - icons_w - text_x, 17.0);
        let mut ui = egui::Ui::new(ctx.clone(), egui::Id::new(("ask-input-ui", c.live)), egui::UiBuilder::new().max_rect(rect).layer_id(c.painter.layer_id()).style(c.ui.style().clone()));
        ui.set_clip_rect(rect.intersect(c.painter.clip_rect()));
        ui.set_opacity(c.painter.opacity());
        if !c.live || !self.ask.has_key { ui.disable(); }
        let hint = if !self.ask.has_key { "Add a Gemini key to start" }
            else if self.ask.pending.is_some() { "Thinking…" }
            else if self.ask.attachment.is_some() { "Ask about this picture" }
            else { "Ask anything · Enter to send" };
        let mut buf = std::mem::take(&mut self.ask.buf);
        let resp = ui.add(egui::TextEdit::singleline(&mut buf)
            .id(egui::Id::new(("ask-input", c.live)))
            .frame(false)
            .margin(egui::Margin::ZERO)
            .text_color(theme::WHITE)
            .font(c.font(theme::size::BODY, W::Regular))
            .hint_text(egui::RichText::new(hint).color(theme::DIM).font(c.font(theme::size::BODY, W::Regular)))
            .desired_width(rect.width()));
        self.ask.buf = buf;

        // Right end: snip a screen area, pick a picture file, send.
        let cy = fy + fh / 2.0;
        let icon = |x: f32, id: &str, hint: &str| -> (egui::Response, f32) {
            let r = c.hit(x - 11.0, fy + 2.0, 22.0, fh - 4.0, id).on_hover_text(hint);
            let (h, _) = c.hover_t(&r);
            if h > 0.01 { c.circle(x, cy, 10.5, crate::paint::mix(theme::BTN, theme::BTN_HOVER, h)); }
            (r, h)
        };
        let enabled = self.ask.has_key && self.ask.pending.is_none();
        let ink = if enabled { Color32::from_gray(205) } else { Color32::from_gray(110) };
        let (snip, _) = icon(fx + fw - 64.0, "ask-snip", "Snip part of the screen and ask about it");
        {
            // Corner brackets of a selection box.
            let (x, y, s, k) = (fx + fw - 64.0, cy, 6.0, 3.0);
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let (px, py) = (x + dx * s, y + dy * s * 0.8);
                c.path(&[(px - dx * k, py), (px, py), (px, py - dy * k)], 1.4, ink);
            }
        }
        let (file, _) = icon(fx + fw - 41.0, "ask-image", "Attach a picture file (or paste one with Ctrl+V)");
        {
            let (x, y) = (fx + fw - 41.0, cy);
            c.stroke(x - 6.5, y - 5.0, 13.0, 10.0, 2.0, 1.3, ink);
            c.path(&[(x - 5.0, y + 3.5), (x - 1.5, y - 0.5), (x + 1.0, y + 2.0), (x + 2.5, y + 0.5), (x + 5.0, y + 3.5)], 1.2, ink);
            c.circle(x + 2.6, y - 2.4, 1.1, ink);
        }
        let send = c.hit(fx + fw - 28.0, fy, 28.0, fh, "ask-send").on_hover_text("Send (Enter)");
        let (sh, _) = c.hover_t(&send);
        if self.ask.pending.is_some() {
            let n = ((self.t * 3.0) as usize) % 4;
            c.text(fx + fw - 16.0, fy + 18.5, &"•".repeat(n.max(1)), 11.0, W::Bold, theme::BLUE, H::Center);
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        } else {
            let ready = !self.ask.buf.trim().is_empty() || self.ask.attachment.is_some();
            let x = fx + fw - 16.0;
            c.circle(x, cy, 10.0, if ready { theme::BLUE } else { crate::paint::mix(theme::BTN_HOVER, Color32::from_gray(80), sh) });
            c.path(&[(x - 4.0, cy - 0.5), (x, cy - 4.5), (x + 4.0, cy - 0.5)], 1.6, theme::WHITE);
            c.seg((x, cy - 4.0), (x, cy + 4.5), 1.6, theme::WHITE);
        }
        if !c.live { return; }
        if enabled && snip.clicked() { cmds.push(Cmd::AskSnip); }
        if enabled && file.clicked() { cmds.push(Cmd::AskPickImage); }
        if resp.clicked() || resp.gained_focus() { cmds.push(Cmd::Focus); }
        let arrived = self.key_open && !self.ask.was_on_page;
        if self.ask.has_key && (self.ask.want_focus || arrived) { resp.request_focus(); cmds.push(Cmd::Focus); }
        self.ask.want_focus = false;
        self.ask.was_on_page = true;
        let was_focused = self.ask.focused;
        // Enter makes a single-line field drop focus in the same frame, so use last frame's focus.
        let (enter, esc, paste_key, pasted_text) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape),
            i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } if modifiers.command)),
            i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))));
        if (was_focused || resp.has_focus()) && enter || send.clicked() {
            let q = std::mem::take(&mut self.ask.buf);
            if !self.ask_submit(q.clone(), Some(ctx.clone())) { self.ask.buf = q; }
            resp.request_focus();
        }
        // Ctrl+V with a picture (and no text) on the clipboard attaches the picture.
        if (was_focused || resp.has_focus()) && paste_key && !pasted_text { cmds.push(Cmd::AskPasteImage); }
        // Esc in the field closes an island that a hotkey opened.
        if was_focused && esc && self.key_open { self.close_keyboard_open(); self.hover_lock = true; }
        let lost_window = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false))));
        if std::mem::take(&mut self.ask.release) || lost_window { resp.surrender_focus(); self.ask.focused = false; }
        else { self.ask.focused = resp.has_focus(); }
    }

    pub(crate) fn page_ask(&mut self, c: &Card, cmds: &mut Vec<Cmd>) {
        let ctx = c.ui.ctx().clone();
        let entries_now = self.ask.thread_entries().len();
        self.ask_input(c, cmds);

        // Body.
        let (bx, by, bw, bh) = BODY;
        if !self.ask.has_key {
            c.text(bx, by + 22.0, "Ask Gemini quick questions right from the island.", 12.0, W::Regular, theme::WHITE, H::Left);
            c.text(bx, by + 40.0, "Paste a free Google AI Studio key in Settings > Ask.", 11.0, W::Regular, theme::DIM, H::Left);
            let w = c.text_w("Open settings", 11.0, W::Semibold) + 22.0;
            let (r, inner) = capsule(c, bx, by + 54.0, w, 24.0, "ask-open-settings", theme::BLUE_DEEP, theme::BLUE.gamma_multiply(0.6));
            inner.centered_text(bx + w / 2.0, by + 66.0, "Open settings", 11.0, W::Semibold, theme::WHITE, H::Center);
            if r.clicked() { self.settings_tab = 13; self.open_settings(); }
            let w2 = c.text_w("Get a free key ↗", 11.0, W::Semibold) + 22.0;
            let (r2, inner2) = capsule(c, bx + w + 8.0, by + 54.0, w2, 24.0, "ask-get-key", theme::BTN, theme::BTN_HOVER);
            inner2.centered_text(bx + w + 8.0 + w2 / 2.0, by + 66.0, "Get a free key ↗", 11.0, W::Semibold, theme::WHITE, H::Center);
            if r2.on_hover_text(ask::KEY_URL).clicked() { cmds.push(Cmd::OpenUrl(ask::KEY_URL.into())); }
            return;
        }
        // Actions sit in a bottom row on either side of the page dots.
        let pill = |x: f32, label: &str, id: &str, hint: &str| -> egui::Response {
            let w = c.text_w(label, 10.5, W::Semibold) + 18.0;
            let (r, inner) = capsule(c, x, ACTIONS_Y, w, 20.0, id, theme::BTN, theme::BTN_HOVER);
            inner.centered_text(x + w / 2.0, ACTIONS_Y + 10.0, label, 10.5, W::Semibold, theme::WHITE, H::Center);
            r.on_hover_text(hint)
        };
        let pill_w = |label: &str| c.text_w(label, 10.5, W::Semibold) + 18.0;
        let hist_label = if self.ask.show_history { "‹ Back" } else { "History" };
        if pill(42.0, hist_label, "ask-history", "Earlier questions").clicked() { self.ask.show_history = !self.ask.show_history; }
        let mut right = 377.0;
        if !self.ask.show_history {
            if let Some(answer) = self.ask.last_answer() {
                right -= pill_w("Copy");
                if pill(right, "Copy", "ask-copy", "Copy the last answer").clicked() { cmds.push(Cmd::Copy(answer)); self.say("Answer copied"); }
                right -= 6.0;
            }
        }
        if entries_now > 0 || self.ask.show_history {
            right -= pill_w("New chat");
            if pill(right, "New chat", "ask-new", "Start a new conversation").clicked() { self.ask_new_chat(); }
        }
        let body = c.rect(bx, by, bw, bh);
        let mut content = egui::Ui::new(ctx.clone(), egui::Id::new(("ask-body", c.live)), egui::UiBuilder::new().max_rect(body).layer_id(c.painter.layer_id()).style(c.ui.style().clone()));
        content.set_clip_rect(body.intersect(c.painter.clip_rect()));
        content.set_opacity(c.painter.opacity());
        if !c.live { content.disable(); }
        content.spacing_mut().item_spacing.y = 3.0 * c.s;
        content.style_mut().spacing.scroll.floating = false;
        content.spacing_mut().scroll.bar_width = 4.0 * c.s;
        let small = c.font(10.5, W::Regular);
        let q_font = c.font(11.0, W::Semibold);
        let a_font = c.font(12.0, W::Regular);
        if self.ask.show_history {
            let now = ask::now_secs();
            let mut threads: Vec<(u64, &Entry, usize, u64)> = Vec::new();
            for e in self.ask.history.entries.iter().filter(|e| !e.error) {
                if let Some(t) = threads.iter_mut().find(|t| t.0 == e.thread) { t.2 += 1; t.3 = e.at; } else { threads.push((e.thread, e, 1, e.at)); }
            }
            threads.reverse();
            let mut open = None;
            let mut clear = false;
            egui::ScrollArea::vertical().id_salt("ask-history-scroll").max_height(bh * c.s).auto_shrink([false, false]).show(&mut content, |ui| {
                if threads.is_empty() {
                    ui.label(egui::RichText::new(if self.settings.ask_keep_days == 0 { "History is off. Turn it on in Settings > Ask." } else { "No saved questions yet." }).font(small.clone()).color(theme::DIM));
                }
                for (thread, first, count, last) in &threads {
                    let more = if *count > 1 { format!("  +{}", count - 1) } else { String::new() };
                    let text = format!("{}{}", c.fit(&with_image_tag(&first.q, first.image), 11.0, W::Semibold, 250.0), more);
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::Button::new(egui::RichText::new(text).font(q_font.clone()).color(theme::WHITE)).frame(false));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { ui.label(egui::RichText::new(ago(now, *last)).font(small.clone()).color(theme::DIM)); });
                        if r.clicked() { open = Some(*thread); }
                    });
                }
                if !threads.is_empty() {
                    ui.add_space(4.0);
                    let note = format!("Kept for {} · ", keep_label(self.settings.ask_keep_days));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(note).font(small.clone()).color(theme::DIM));
                        if ui.add(egui::Button::new(egui::RichText::new("Clear history").font(small.clone()).color(theme::RED)).frame(false)).clicked() { clear = true; }
                    });
                }
            });
            if let Some(t) = open { self.ask.thread = t; self.ask.show_history = false; }
            if clear { self.ask.history.clear(); self.ask_new_chat(); self.say("History cleared"); }
            return;
        }
        let entries: Vec<Entry> = self.ask.thread_entries().into_iter().cloned().collect();
        let pending_q = self.ask.pending.as_ref().map(|(_, q)| with_image_tag(q, self.ask.pending_image));
        if entries.is_empty() && pending_q.is_none() {
            c.text(bx, by + 20.0, "Ask a quick question, or snip the screen to ask about it.", 12.0, W::Regular, theme::WHITE, H::Left);
            let keep = if self.settings.ask_keep_days == 0 { "History is off.".to_string() } else { format!("History is kept for {}, then deleted.", keep_label(self.settings.ask_keep_days)) };
            c.text(bx, by + 38.0, &format!("{} · {}", ask::model_label(&self.settings.ask_model), keep), 10.5, W::Regular, theme::DIM, H::Left);
            c.text(bx, by + 56.0, "Ctrl+Alt+Space opens Ask from anywhere.", 10.5, W::Regular, theme::DIM, H::Left);
            return;
        }
        egui::ScrollArea::vertical().id_salt(("ask-thread-scroll", self.ask.thread)).max_height(bh * c.s).auto_shrink([false, false]).stick_to_bottom(true).show(&mut content, |ui| {
            for (i, e) in entries.iter().enumerate() {
                if i > 0 { ui.add_space(5.0); }
                ui.add(egui::Label::new(egui::RichText::new(with_image_tag(&e.q, e.image)).font(q_font.clone()).color(theme::DIM)).wrap());
                ui.add(egui::Label::new(egui::RichText::new(&e.a).font(a_font.clone()).color(if e.error { Color32::from_rgb(255, 150, 120) } else { theme::WHITE })).wrap().selectable(!e.error));
            }
            if let Some(q) = &pending_q {
                if !entries.is_empty() { ui.add_space(5.0); }
                ui.add(egui::Label::new(egui::RichText::new(q).font(q_font.clone()).color(theme::DIM)).wrap());
                ui.label(egui::RichText::new("Thinking…").font(a_font.clone()).color(theme::DIM));
            }
        });
    }

    pub(crate) fn ask_body_rect() -> Rect {
        let (x, y, w, h) = BODY;
        Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }

    pub(crate) fn ask_settings(&mut self, ui: &mut egui::Ui) {
        use crate::extras::{settings_card, settings_hint, settings_label, settings_segmented, settings_control_row_right};
        settings_label(ui, "Google AI Studio key");
        settings_card(ui, |ui| {
            if self.ask.has_key {
                settings_control_row_right(ui, "Key saved in Windows Credential Manager", |ui| {
                    if ui.button("Remove key").clicked() {
                        self.ask.key_status = Some(match ask::delete_key() { Ok(()) => { self.ask.has_key = false; ("Key removed".into(), false) } Err(e) => (e, true) });
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.ask.key_buf).password(true).hint_text("Paste your API key").desired_width((ui.available_width() - 70.0).max(120.0)));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Save").clicked() || enter {
                        let key = std::mem::take(&mut self.ask.key_buf);
                        self.ask.key_status = Some(match ask::save_key(&key) { Ok(()) => { self.ask.has_key = true; ("Key saved".into(), false) } Err(e) => { self.ask.key_buf = key; (e, true) } });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("No key yet?").size(12.0));
                    ui.hyperlink_to("Get a free key at aistudio.google.com", ask::KEY_URL);
                });
            }
            if let Some((msg, err)) = &self.ask.key_status {
                ui.label(egui::RichText::new(msg).size(12.0).color(if *err { Color32::from_rgb(220, 110, 90) } else { Color32::from_rgb(70, 170, 100) }));
            }
        });
        settings_hint(ui, "The key is stored in Windows Credential Manager, never in Daozai's settings file.");

        settings_label(ui, "Model");
        settings_card(ui, |ui| {
            let mut index = ask::MODELS.iter().position(|(id, _)| *id == self.settings.ask_model).unwrap_or(0);
            let options: Vec<(usize, &str)> = ask::MODELS.iter().enumerate().map(|(i, (_, l))| (i, *l)).collect();
            settings_control_row_right(ui, "Gemini model", |ui| { if settings_segmented(ui, &mut index, &options, 82.0) { self.settings.ask_model = ask::MODELS[index].0.into(); } });
        });
        settings_hint(ui, "Flash gives better answers; Flash-Lite is faster and has a larger free allowance.");

        settings_label(ui, "History");
        settings_card(ui, |ui| {
            let mut days = self.settings.ask_keep_days;
            let labels: Vec<String> = ask::KEEP_DAYS.iter().map(|d| keep_label(*d)).collect();
            let options: Vec<(u32, &str)> = ask::KEEP_DAYS.iter().zip(labels.iter()).map(|(d, l)| (*d, l.as_str())).collect();
            settings_control_row_right(ui, "Keep questions for", |ui| {
                if settings_segmented(ui, &mut days, &options, 62.0) {
                    self.settings.ask_keep_days = days;
                    self.ask.history.prune(days, ask::now_secs());
                    self.ask.history.save(days);
                }
            });
            settings_control_row_right(ui, &format!("{} saved", self.ask.history.entries.iter().filter(|e| !e.error).count()), |ui| {
                if ui.button("Clear history").clicked() { self.ask.history.clear(); self.ask_new_chat(); }
            });
        });
        settings_hint(ui, "Older questions are deleted automatically. Off keeps nothing on disk.");
        settings_label(ui, "Shortcuts and pictures");
        settings_hint(ui, "Ctrl+Alt+Space opens Ask from any app with the cursor in the text box; Esc or the same keys close it.");
        settings_hint(ui, "Snip (the corner-brackets button) lets you select part of the screen; the picture is attached to your next question. You can also attach a picture file or paste one with Ctrl+V. Pictures are sent only with that question and never saved.");
        settings_hint(ui, "Questions are sent to Google's Gemini API. On the free tier Google may use them to improve its products, so avoid sending private data.");
    }
}
