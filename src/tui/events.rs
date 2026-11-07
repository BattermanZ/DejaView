use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyEvent, MouseEvent, MouseEventKind};

pub enum Event {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
}

pub fn next_event(tick_rate: Duration) -> Result<Event> {
    if event::poll(tick_rate)? {
        match event::read()? {
            CEvent::Key(key) => return Ok(Event::Key(key)),
            // Moving the pointer reports a stream of events: let them count
            // as ticks, so a run's progress is still read while it moves.
            CEvent::Mouse(mouse) if !matches!(mouse.kind, MouseEventKind::Moved) => {
                return Ok(Event::Mouse(mouse));
            }
            _ => {}
        }
    }
    Ok(Event::Tick)
}
