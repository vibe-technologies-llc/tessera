use gpui::Context;
use tessera_timeline::Time;

#[derive(Default)]
pub struct Playhead {
    time: Time,
}

impl Playhead {
    pub fn time(&self) -> Time {
        self.time
    }

    pub fn seek(&mut self, time: Time, cx: &mut Context<Self>) {
        let time = time.max(Time::ZERO);
        if time != self.time {
            self.time = time;
            cx.notify();
        }
    }
}
