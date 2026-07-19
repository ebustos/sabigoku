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
    /// image). For keys that change images, `set`.
    pub fn ensure(&mut self, key: &str, img: &DynamicImage) {
        if self.slots.contains_key(key) {
            return;
        }
        self.set(key, img);
    }

    /// Replace unconditionally (the detail slot re-keys per selection).
    pub fn set(&mut self, key: &str, img: &DynamicImage) {
        let (req_tx, req_rx) = mpsc::channel();
        let proto = ThreadProtocol::new(req_tx, Some(self.picker.new_resize_protocol(img.clone())));
        self.slots.insert(key.to_string(), Slot { proto, req_rx });
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
        pool.ensure("u1", &img());
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
        pool.ensure("u1", &img());
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
        pool.ensure("u1", &img());
        pool.ensure("u1", &img());
        assert_eq!(pool.slots.len(), 1);
        pool.set("detail", &img());
        pool.set("detail", &img());
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
}
