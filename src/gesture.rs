//! Maps raw hotkey press/release events to start/stop recording: record while held.

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
}

#[derive(Default)]
pub struct Gesture {
    recording: bool,
}

impl Gesture {
    pub fn press(&mut self) -> Option<Action> {
        // Already recording means this is key auto-repeat.
        (!std::mem::replace(&mut self.recording, true)).then_some(Action::Start)
    }

    pub fn release(&mut self) -> Option<Action> {
        std::mem::take(&mut self.recording).then_some(Action::Stop)
    }

    /// Recording was stopped by something other than the hotkey.
    pub fn reset(&mut self) {
        self.recording = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_while_held() {
        let mut g = Gesture::default();
        assert_eq!(g.press(), Some(Action::Start));
        assert_eq!(g.press(), None, "auto-repeat");
        assert_eq!(g.release(), Some(Action::Stop));
        assert_eq!(g.release(), None);
        assert_eq!(g.press(), Some(Action::Start));
    }

    #[test]
    fn release_after_auto_stop_does_nothing() {
        let mut g = Gesture::default();
        g.press();
        g.reset();
        assert_eq!(g.release(), None);
        assert_eq!(g.press(), Some(Action::Start));
    }
}
