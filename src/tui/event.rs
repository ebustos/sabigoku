//! Unified event queue (04 §1, §4): every source posts `Event` into ONE channel
//! and the UI thread is the sole consumer, so `tick(event)` sees a total order.
//! Worker-result variants arrive with their subsystems (ROD-434+).

use std::sync::mpsc;
use std::time::Duration;

use image::DynamicImage;
use ratatui::crossterm::event::{self as ct, KeyEvent};

use super::workers::{CancelFlag, Drain};

/// One demo grid card (ROD-438 shell); the real DiscoverState rows replace
/// this in ROD-439.
#[derive(Debug, Clone, PartialEq)]
pub struct DemoCard {
    pub anilist_id: i64,
    pub title: String,
    pub cover_url: Option<String>,
}

/// No `Eq`: cover events carry pixel payloads (`DynamicImage` is `PartialEq`
/// only).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Key(KeyEvent),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    /// ~100ms clock (04 §8): spinner, debounces, deadlines.
    Tick,
    /// The input thread died on a terminal error. Keys can never arrive again,
    /// so the app must not idle on as an unquittable zombie.
    InputClosed,
    /// Detail cover result (04 §4.4); keep-check by `for_id` in tick.
    CoverDone {
        for_id: i64,
        img: DynamicImage,
    },
    CoverError {
        for_id: i64,
    },
    /// Url-keyed, no window stale-drop (04 §4.4): the slot adopts by url.
    DiscoverCoverDone {
        url: String,
        img: DynamicImage,
    },
    DiscoverCoverError {
        url: String,
    },
    /// Wake: the encode worker finished a resize. The response itself rides
    /// the pool's own channel (ratatui-image types are not comparable, so
    /// they stay out of this enum); tick applies it on the UI thread.
    CoverEncodeReady,
    /// Demo feed for the ROD-438 shell grid; DiscoverFeed replaces it (439).
    DemoFeedLoaded {
        cards: Vec<DemoCard>,
    },
    DemoFeedFailed {
        cause: String,
    },
}

pub type EventRx = mpsc::Receiver<Event>;

/// Post side of the queue. `post` never blocks and never fails: after the UI
/// receiver drops (shutdown), posts vanish instead of wedging a worker (04 §11).
#[derive(Debug, Clone)]
pub struct EventTx(mpsc::Sender<Event>);

impl EventTx {
    pub fn post(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

pub fn channel() -> (EventTx, EventRx) {
    let (tx, rx) = mpsc::channel();
    (EventTx(tx), rx)
}

/// Forward terminal input into the queue until `shutdown` flips. Polling keeps
/// the thread joinable at teardown; a blocking `read` could only be freed by a
/// keypress.
pub fn spawn_input_thread(drain: &Drain, tx: EventTx, shutdown: CancelFlag) -> bool {
    drain.spawn("input", move || {
        while !shutdown.is_cancelled() {
            match ct::poll(Duration::from_millis(50)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => {
                    tx.post(Event::InputClosed);
                    return;
                }
            }
            let Ok(raw) = ct::read() else {
                tx.post(Event::InputClosed);
                return;
            };
            match raw {
                // Repeats and releases stay out of the queue: binds fire on press.
                ct::Event::Key(k) if k.kind == ct::KeyEventKind::Press => {
                    tx.post(Event::Key(k));
                }
                ct::Event::Resize(w, h) => tx.post(Event::Resize(w, h)),
                ct::Event::FocusGained => tx.post(Event::FocusGained),
                ct::Event::FocusLost => tx.post(Event::FocusLost),
                _ => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_after_shutdown_is_silent() {
        let (tx, rx) = channel();
        drop(rx);
        tx.post(Event::Tick);
    }

    #[test]
    fn posts_arrive_in_order() {
        let (tx, rx) = channel();
        tx.post(Event::Tick);
        tx.post(Event::Resize(80, 24));
        assert_eq!(rx.recv().unwrap(), Event::Tick);
        assert_eq!(rx.recv().unwrap(), Event::Resize(80, 24));
    }
}
