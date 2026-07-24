//! Encode routing (ROD-417 carry-forward finding): `ThreadProtocol` counts
//! request ids per instance, so responses off one shared channel cannot be
//! trial-routed across images; colliding ids would install the wrong poster.
//! Each image keeps a private request channel, drained after every draw into
//! one key-tagged worker queue. Responses come back on the pool's own channel
//! (their types are not comparable, so they stay out of `Event`) with a
//! `CoverEncodeReady` wake; tick applies them on the UI thread.

use std::collections::HashMap;
use std::sync::mpsc;

use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui_image::errors::Errors;
use ratatui_image::picker::{Capability, Picker, ProtocolType};
use ratatui_image::thread::{ResizeRequest, ResizeResponse, ThreadProtocol};
use ratatui_image::{FontSize, Resize, StatefulImage};

use crate::tui::event::{Event, EventTx};
use crate::tui::workers::Drain;

struct Slot {
    proto: ThreadProtocol,
    req_rx: mpsc::Receiver<ResizeRequest>,
    /// Un-cropped source, held only by the cover path (`set_cover`) so a
    /// reshape can re-crop from full resolution. `None` for grid slots, which
    /// never reshape and must not pay a second buffer (04 §7.3 RAM rail).
    src: Option<DynamicImage>,
    /// Block cells the protocol image was last cover-cropped for. Reshape only
    /// when this changes, so a static view rebuilds the protocol exactly once.
    shaped_for: Option<(u16, u16)>,
}

pub struct ProtocolPool {
    picker: Picker,
    /// None when the terminal reported no cell pixel geometry (tmux, SSH);
    /// sizing then falls back to the DESIGN fixed floors and caps.
    cell: Option<FontSize>,
    slots: HashMap<String, Slot>,
    worker_tx: mpsc::Sender<(String, ResizeRequest)>,
    resp_rx: mpsc::Receiver<(String, Result<ResizeResponse, Errors>)>,
    encode_errors: u64,
}

impl ProtocolPool {
    /// The encode worker lives on `drain` and exits when the pool drops
    /// (its request queue disconnects); posts fail silent after shutdown.
    pub fn new(picker: Picker, tx: EventTx, drain: &Drain) -> ProtocolPool {
        let cell = picker
            .capabilities()
            .iter()
            .any(|c| matches!(c, Capability::CellSize(Some(_))))
            .then(|| picker.font_size());
        let (worker_tx, worker_rx) = mpsc::channel::<(String, ResizeRequest)>();
        let (resp_tx, resp_rx) = mpsc::channel();
        // A refused spawn leaves requests queued unserved: covers stay
        // unencoded but the pool stays safe to use.
        let _ = drain.spawn("cover-encode", move || {
            while let Ok((key, req)) = worker_rx.recv() {
                let res = req.resize_encode();
                if resp_tx.send((key, res)).is_err() {
                    return;
                }
                tx.post(Event::CoverEncodeReady);
            }
        });
        ProtocolPool {
            picker,
            cell,
            slots: HashMap::new(),
            worker_tx,
            resp_rx,
            encode_errors: 0,
        }
    }

    pub fn cell(&self) -> Option<FontSize> {
        self.cell
    }

    pub fn protocol_type(&self) -> ProtocolType {
        self.picker.protocol_type()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.slots.contains_key(key)
    }

    pub fn encode_errors(&self) -> u64 {
        self.encode_errors
    }

    /// Keep an existing protocol: keys are content-stable (a url names one
    /// image), so a duplicate arrival just drops. For keys that change
    /// images, `set`.
    pub fn ensure(&mut self, key: &str, img: DynamicImage) {
        if self.slots.contains_key(key) {
            return;
        }
        self.set(key, img);
    }

    /// Replace unconditionally (the detail slot re-keys per selection).
    /// Takes the buffer by value: the pool is the render store, and this is
    /// the image's final home (no clone on the ingest path).
    pub fn set(&mut self, key: &str, img: DynamicImage) {
        let (req_tx, req_rx) = mpsc::channel();
        let proto = ThreadProtocol::new(req_tx, Some(self.picker.new_resize_protocol(img)));
        self.slots.insert(
            key.to_string(),
            Slot {
                proto,
                req_rx,
                src: None,
                shaped_for: None,
            },
        );
    }

    /// Cover ingest (ROD-461): retains the un-cropped source so `render_cover`
    /// can re-center-crop to the live block aspect on resize. One extra buffer
    /// per slot; the single detail cover uses this, never the grid.
    pub fn set_cover(&mut self, key: &str, img: DynamicImage) {
        let (req_tx, req_rx) = mpsc::channel();
        let proto = ThreadProtocol::new(req_tx, Some(self.picker.new_resize_protocol(img.clone())));
        self.slots.insert(
            key.to_string(),
            Slot {
                proto,
                req_rx,
                src: Some(img),
                shaped_for: None,
            },
        );
    }

    pub fn remove(&mut self, key: &str) {
        self.slots.remove(key);
    }

    /// Drop every slot `keep` rejects (grid eviction sync).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.slots.retain(|k, _| keep(k));
    }

    /// Crop-to-fill into the block (DESIGN 3.3). False = no image for `key`
    /// (caller draws its placeholder).
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect, key: &str) -> bool {
        let Some(slot) = self.slots.get_mut(key) else {
            return false;
        };
        frame.render_stateful_widget(
            StatefulImage::default().resize(Resize::Crop(None)),
            area,
            &mut slot.proto,
        );
        true
    }

    /// Center cover-crop into the block (ROD-461, DESIGN 3.3): scale the source
    /// to cover the block and center the overflow crop, versus `render`'s
    /// top-left raw-pixel window. Needs cell geometry; without it (tmux, SSH,
    /// halfblocks) it falls back to `render`. False = no slot for `key`.
    pub fn render_cover(&mut self, frame: &mut Frame<'_>, area: Rect, key: &str) -> bool {
        let Some(cell) = self.cell else {
            return self.render(frame, area, key);
        };
        let Some(slot) = self.slots.get_mut(key) else {
            return false;
        };
        if slot.shaped_for != Some((area.width, area.height)) {
            let cropped = slot.src.as_ref().and_then(|src| {
                cover_crop(
                    src,
                    area.width as u32 * cell.width as u32,
                    area.height as u32 * cell.height as u32,
                )
            });
            // A rebuilt protocol resets its request-id counter; the reshape is
            // gated on cell dims so the steady state rebuilds once. The only
            // stale-response window is an active resize (ROD-475's concern).
            if let Some(cropped) = cropped {
                let (req_tx, req_rx) = mpsc::channel();
                slot.proto =
                    ThreadProtocol::new(req_tx, Some(self.picker.new_resize_protocol(cropped)));
                slot.req_rx = req_rx;
                slot.shaped_for = Some((area.width, area.height));
            }
        }
        frame.render_stateful_widget(
            StatefulImage::default().resize(Resize::Scale(None)),
            area,
            &mut slot.proto,
        );
        true
    }

    /// After every draw (the carry-forward law): forward each slot's fresh
    /// resize requests into the worker queue, tagged with the slot key.
    pub fn drain_requests(&mut self) {
        for (key, slot) in &self.slots {
            while let Ok(req) = slot.req_rx.try_recv() {
                let _ = self.worker_tx.send((key.clone(), req));
            }
        }
    }

    /// On `CoverEncodeReady` (and teardown-free ticks): route responses by
    /// key. A response for an evicted key drops; a rejected update means the
    /// slot was rebuilt mid-encode and the next draw re-requests.
    pub fn apply_responses(&mut self) -> bool {
        let mut applied = false;
        while let Ok((key, res)) = self.resp_rx.try_recv() {
            match res {
                Ok(done) => {
                    if let Some(slot) = self.slots.get_mut(&key) {
                        let _ = slot.proto.update_resized_protocol(done);
                        applied = true;
                    }
                }
                Err(_) => self.encode_errors += 1,
            }
        }
        applied
    }
}

/// Largest centered sub-rect of `src` whose aspect matches the block `tw:th`,
/// at full source resolution. A cheap view copy; the filtered scale-to-fill
/// runs later on the encode worker. `None` when either target dim is zero.
fn cover_crop(src: &DynamicImage, tw: u32, th: u32) -> Option<DynamicImage> {
    if tw == 0 || th == 0 {
        return None;
    }
    let (sw, sh) = (src.width(), src.height());
    let (sw64, sh64, tw64, th64) = (sw as u64, sh as u64, tw as u64, th as u64);
    let (cw, ch) = if sw64 * th64 > tw64 * sh64 {
        // Source wider than the block: keep full height, crop the width.
        (((sh64 * tw64) / th64).max(1) as u32, sh)
    } else {
        // Source taller (or equal): keep full width, crop the height.
        (sw, ((sw64 * th64) / tw64).max(1) as u32)
    };
    Some(src.crop_imm((sw - cw) / 2, (sh - ch) / 2, cw, ch))
}

impl std::fmt::Debug for ProtocolPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtocolPool")
            .field("protocol", &self.picker.protocol_type())
            .field("cell", &self.cell)
            .field("slots", &self.slots.len())
            .field("encode_errors", &self.encode_errors)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::event;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    fn img() -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            12,
            image::Rgba([200, 40, 120, 255]),
        ))
    }

    fn pool(drain: &Drain) -> (ProtocolPool, event::EventRx) {
        let (tx, rx) = event::channel();
        (ProtocolPool::new(Picker::halfblocks(), tx, drain), rx)
    }

    fn draw(pool: &mut ProtocolPool, key: &str) {
        let mut term = Terminal::new(TestBackend::new(30, 20)).unwrap();
        term.draw(|f| {
            pool.render(f, Rect::new(0, 0, 10, 6), key);
        })
        .unwrap();
    }

    #[test]
    fn encode_roundtrip_applies_off_thread_result() {
        let drain = Drain::default();
        let (mut pool, rx) = pool(&drain);
        pool.ensure("u1", img());
        // First draw emits the resize request; drain forwards it tagged.
        draw(&mut pool, "u1");
        pool.drain_requests();
        // The worker encodes and wakes the loop.
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::CoverEncodeReady
        );
        assert!(pool.apply_responses());
        assert_eq!(pool.encode_errors(), 0);
        draw(&mut pool, "u1");
        drop(pool);
        assert!(drain.drain(Duration::from_secs(5)), "worker exits on drop");
    }

    #[test]
    fn response_for_an_evicted_key_is_dropped() {
        let drain = Drain::default();
        let (mut pool, rx) = pool(&drain);
        pool.ensure("u1", img());
        draw(&mut pool, "u1");
        pool.drain_requests();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::CoverEncodeReady
        );
        pool.remove("u1");
        assert!(!pool.apply_responses(), "late result must not apply");
        assert_eq!(pool.encode_errors(), 0);
    }

    #[test]
    fn ensure_keeps_but_set_replaces() {
        let drain = Drain::default();
        let (mut pool, _rx) = pool(&drain);
        pool.ensure("u1", img());
        pool.ensure("u1", img());
        assert_eq!(pool.slots.len(), 1);
        pool.set("detail", img());
        pool.set("detail", img());
        assert_eq!(pool.slots.len(), 2);
        pool.retain(|k| k == "detail");
        assert!(!pool.contains("u1"));
        assert!(pool.contains("detail"));
    }

    #[test]
    fn render_without_slot_reports_placeholder_needed() {
        let drain = Drain::default();
        let (mut pool, _rx) = pool(&drain);
        let mut term = Terminal::new(TestBackend::new(30, 20)).unwrap();
        term.draw(|f| {
            assert!(!pool.render(f, Rect::new(0, 0, 10, 6), "missing"));
        })
        .unwrap();
    }

    #[test]
    fn halfblocks_picker_reports_no_cell_geometry() {
        let drain = Drain::default();
        let (pool, _rx) = pool(&drain);
        assert!(pool.cell().is_none());
        assert_eq!(pool.protocol_type(), ProtocolType::Halfblocks);
    }

    #[test]
    fn cover_crop_centers_the_window() {
        use image::GenericImageView;
        // Left 20 px red, rest green. A top-left crop would open on red; the
        // centered crop opens at src x=25, so column 0 must be green.
        let mut src = image::RgbaImage::from_pixel(100, 50, image::Rgba([0, 200, 0, 255]));
        for y in 0..50 {
            for x in 0..20 {
                src.put_pixel(x, y, image::Rgba([200, 0, 0, 255]));
            }
        }
        let src = DynamicImage::ImageRgba8(src);
        let out = cover_crop(&src, 10, 10).unwrap();
        assert_eq!(
            (out.width(), out.height()),
            (50, 50),
            "square block, wide src"
        );
        assert_eq!(out.get_pixel(0, 0), image::Rgba([0, 200, 0, 255]));
    }

    #[test]
    fn cover_crop_matches_block_aspect() {
        // Tall source into a square block: keep the width, crop the height.
        let src = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            50,
            100,
            image::Rgba([10, 10, 10, 255]),
        ));
        assert_eq!(dims(cover_crop(&src, 10, 10)), (50, 50));
        // Block wider than the source: crop the height to the wide ratio.
        assert_eq!(dims(cover_crop(&src, 20, 10)), (50, 25));
        // Matching aspect returns the full frame.
        assert_eq!(dims(cover_crop(&src, 10, 20)), (50, 100));
    }

    #[test]
    fn cover_crop_rejects_zero_dims() {
        let src = img();
        assert!(cover_crop(&src, 0, 10).is_none());
        assert!(cover_crop(&src, 10, 0).is_none());
    }

    #[test]
    fn render_cover_without_cell_falls_back_and_draws() {
        // Halfblocks reports no cell geometry, so render_cover routes through
        // the top-left crop fallback rather than reshaping.
        let drain = Drain::default();
        let (mut pool, _rx) = pool(&drain);
        pool.set_cover("detail", img());
        let mut term = Terminal::new(TestBackend::new(30, 20)).unwrap();
        term.draw(|f| {
            assert!(pool.render_cover(f, Rect::new(0, 0, 10, 6), "detail"));
            assert!(!pool.render_cover(f, Rect::new(0, 0, 10, 6), "missing"));
        })
        .unwrap();
    }

    fn dims(img: Option<DynamicImage>) -> (u32, u32) {
        let img = img.unwrap();
        (img.width(), img.height())
    }
}
