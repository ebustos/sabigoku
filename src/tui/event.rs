//! Unified event queue (04 §1, §4): every source posts `Event` into ONE channel
//! and the UI thread is the sole consumer, so `tick(event)` sees a total order.
//! Worker-result variants arrive with their subsystems (ROD-434+).

use std::sync::mpsc;

use ratatui::crossterm::event::KeyEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    /// ~100ms clock (04 §8): spinner, debounces, deadlines.
    Tick,
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
