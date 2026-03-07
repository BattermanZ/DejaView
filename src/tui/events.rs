use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyEvent};

pub enum Event {
    Tick,
    Key(KeyEvent),
}

pub fn next_event(tick_rate: Duration) -> Result<Event> {
    if event::poll(tick_rate)?
        && let CEvent::Key(key) = event::read()?
    {
        return Ok(Event::Key(key));
    }
    Ok(Event::Tick)
}
