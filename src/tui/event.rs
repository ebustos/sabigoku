//! Unified event queue (04 §1, §4): every source posts `Event` into ONE channel
//! and the UI thread is the sole consumer, so `tick(event)` sees a total order.
//! Worker-result variants arrive with their subsystems (ROD-434+).

use std::sync::mpsc;
use std::time::Duration;

use ratatui::crossterm::event::{self as ct, KeyEvent};

use super::workers::{CancelFlag, Drain};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    /// ~100ms clock (04 §8): spinner, debounces, deadlines.
    Tick,
    /// Shell demo worker (ROD-433): generation-tagged so the loop demonstrates
    /// the 04 §6 stale drop. Dies when real subsystem events land (ROD-434+).
    DemoProgress { token: u64, percent: u8 },
    DemoDone { token: u64 },
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
                Err(_) => return,
            }
            let Ok(raw) = ct::read() else { return };
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
