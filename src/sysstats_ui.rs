//! The CPU / RAM chip on the collapsed island.
use crate::app::{App, Cmd};
use crate::bar_layout::{Layout, SYSTEM};
use crate::paint::{Card, H};
use crate::sysstats::{self, Memory};
use crate::theme::{self, W};
use eframe::egui::Color32;

/// White normally, amber when busy, red when nearly full.
pub(crate) fn load_color(percent: f32) -> Color32 {
    if percent >= 90.0 { Color32::from_rgb(255, 92, 92) } else if percent >= 70.0 { theme::YELLOW } else { theme::WHITE }
}

pub(crate) fn tooltip(cpu: Option<f32>, memory: Option<Memory>) -> String {
    let cpu = cpu.map(|c| format!("CPU {:.0}%", c)).unwrap_or_else(|| "CPU …".into());
    let ram = memory.map(|m| format!("Memory {} of {} GB ({:.0}%)", sysstats::gb(m.used), sysstats::gb(m.total), m.percent)).unwrap_or_default();
    format!("{cpu}\n{ram}\nClick to open Task Manager")
}

impl App {
    pub(crate) fn system_stats_pill(&self) -> bool {
        self.settings.show_system_stats && !self.effective_minimal() && self.sysstats.ready()
    }

    pub(crate) fn sysstats_tick(&mut self) {
        // The island repaints at least once a second, which is enough to keep the numbers current.
        if self.settings.show_system_stats && !self.effective_minimal() { self.sysstats.tick(self.t); }
    }

    pub(crate) fn draw_pill_system(&mut self, c: &Card, x: f32, layout: &Layout) {
        self.paint_pill_system(c, x, layout);
        let (width, left) = (layout.widths[SYSTEM], x - layout.widths[SYSTEM] / 2.0);
        let tip = tooltip(self.sysstats.cpu, self.sysstats.memory);
        if crate::quota_ui::bar_tooltip(c.hit(left - 2.0, 1.0, width + 4.0, 28.0, "system-pill"), tip).clicked() {
            self.pending.push(Cmd::Open("taskmgr.exe".into()));
        }
    }

    /// Paint only (no hit area); the settings preview reuses it.
    pub(crate) fn paint_pill_system(&self, c: &Card, x: f32, layout: &Layout) {
        let alpha = layout.pet_clearance(SYSTEM);
        let faded = c.faded(alpha);
        let c = &if alpha < 0.8 { faded.ghost() } else { faded };
        let width = layout.widths[SYSTEM];
        let left = x - width / 2.0;
        let rows = [("CPU", self.sysstats.cpu), ("RAM", self.sysstats.memory.map(|m| m.percent))];
        for (i, (label, value)) in rows.iter().enumerate() {
            let base = 13.5 + i as f32 * 10.5;
            c.text(left, base, label, 8.0, W::Semibold, theme::DIM, H::Left);
            let text = value.map(|v| format!("{:.0}%", v.clamp(0.0, 100.0))).unwrap_or_else(|| "–".into());
            c.text(left + width, base, &text, 9.0, W::Bold, value.map(load_color).unwrap_or(theme::DIM), H::Right);
        }
    }
}
