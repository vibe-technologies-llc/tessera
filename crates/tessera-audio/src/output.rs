use std::{
    collections::VecDeque,
    io::Cursor,
    iter,
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use pipewire::{
    self as pw,
    main_loop::MainLoopRc,
    properties::properties,
    spa::{
        param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS},
        pod::{Object, Pod, Value, serialize::PodSerializer},
        sys::{
            SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR, SPA_PARAM_EnumFormat,
            SPA_TYPE_OBJECT_Format,
        },
        utils::Direction,
    },
    stream::{Stream, StreamBox, StreamFlags},
};
use tessera_timeline::Time;

use crate::{CHANNELS, Error};

const SAMPLE_BYTES: usize = size_of::<f32>();
const FRAME_BYTES: usize = CHANNELS * SAMPLE_BYTES;
const BLOCK_FRAMES: usize = 1024;
const QUEUED_AHEAD: Duration = Duration::from_millis(200);
const STREAM_NAME: &str = "Tessera";
const THREAD_NAME: &str = "tessera-audio-output";
const FEEDER_THREAD_NAME: &str = "tessera-audio-feeder";

pub struct Output {
    shared: Arc<Shared>,
    sample_rate: u32,
    quit: pw::channel::Sender<()>,
    stream_thread: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    queue: VecDeque<f32>,
    clock: Clock,
    stopping: bool,
}

#[derive(Debug, Default)]
struct Clock {
    consumed: i64,
    anchor: Option<Anchor>,
    reported: i64,
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    heard: i64,
    at: Instant,
}

type Ready = mpsc::Sender<Result<(), Error>>;

impl Output {
    pub fn start(
        sample_rate: u32,
        mut source: impl FnMut(&mut [f32]) + Send + 'static,
    ) -> Result<Self, Error> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let (quit, quit_receiver) = pw::channel::channel();
        let (ready, readiness) = mpsc::channel();
        let stream_thread = thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn({
                let shared = shared.clone();
                move || {
                    if let Err(error) = run_stream(shared, sample_rate, quit_receiver, &ready) {
                        ready.send(Err(error)).ok();
                    }
                }
            })
            .map_err(|_| Error::OutputThread)?;
        let started = readiness.recv().unwrap_or(Err(Error::OutputThread));
        if let Err(error) = started {
            stream_thread.join().ok();
            return Err(error);
        }
        let output = Self {
            shared: shared.clone(),
            sample_rate,
            quit,
            stream_thread: Some(stream_thread),
        };
        thread::Builder::new()
            .name(FEEDER_THREAD_NAME.into())
            .spawn(move || feed(&shared, sample_rate, &mut source))
            .map_err(|_| Error::OutputThread)?;
        Ok(output)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn position(&self) -> i64 {
        self.shared
            .lock()
            .clock
            .position(Instant::now(), self.sample_rate)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.shared.lock().stopping = true;
        self.shared.wake.notify_all();
        self.quit.send(()).ok();
        if let Some(stream_thread) = self.stream_thread.take() {
            stream_thread.join().ok();
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock {
    fn advance(&mut self, taken: usize, device_delay: i64, now: Instant) {
        self.anchor = Some(Anchor {
            heard: self.consumed - device_delay,
            at: now,
        });
        self.consumed += taken as i64;
    }

    fn position(&mut self, now: Instant, sample_rate: u32) -> i64 {
        let Some(anchor) = self.anchor else {
            return self.reported;
        };
        let since = Time::from_duration(now.saturating_duration_since(anchor.at));
        let estimate = (anchor.heard + since.to_samples(sample_rate)).clamp(0, self.consumed);
        self.reported = self.reported.max(estimate);
        self.reported
    }
}

fn feed(shared: &Shared, sample_rate: u32, source: &mut impl FnMut(&mut [f32])) {
    let ahead = Time::from_duration(QUEUED_AHEAD).to_samples(sample_rate) as usize * CHANNELS;
    let mut block = vec![0.0; BLOCK_FRAMES * CHANNELS];
    loop {
        {
            let mut state = shared.lock();
            while !state.stopping && state.queue.len() >= ahead {
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.stopping {
                return;
            }
        }
        source(&mut block);
        shared.lock().queue.extend(&block);
    }
}

fn run_stream(
    shared: Arc<Shared>,
    sample_rate: u32,
    quit: pw::channel::Receiver<()>,
    ready: &Ready,
) -> Result<(), Error> {
    pw::init();
    let main_loop = MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let stream = StreamBox::new(
        &core,
        STREAM_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::AUDIO_CHANNELS => CHANNELS.to_string(),
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| process(stream, &shared, sample_rate))
        .register()?;
    let format = format_pod(sample_rate)?;
    let format = Pod::from_bytes(&format)
        .ok_or_else(|| Error::Format("the serialized format is not a pod".into()))?;
    stream.connect(
        Direction::Output,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut [format],
    )?;
    let _quit = quit.attach(main_loop.loop_(), {
        let main_loop = main_loop.clone();
        move |()| main_loop.quit()
    });
    ready.send(Ok(())).ok();
    main_loop.run();
    Ok(())
}

fn format_pod(sample_rate: u32) -> Result<Vec<u8>, Error> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(sample_rate);
    info.set_channels(CHANNELS as u32);
    let mut position = [0; MAX_CHANNELS];
    position[..CHANNELS].copy_from_slice(&[SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR]);
    info.set_position(position);
    let object = Value::Object(Object {
        type_: SPA_TYPE_OBJECT_Format,
        id: SPA_PARAM_EnumFormat,
        properties: info.into(),
    });
    PodSerializer::serialize(Cursor::new(Vec::new()), &object)
        .map(|(serialized, _)| serialized.into_inner())
        .map_err(|error| Error::Format(format!("{error:?}")))
}

fn process(stream: &Stream, shared: &Shared, sample_rate: u32) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let requested = usize::try_from(buffer.requested()).unwrap_or(usize::MAX);
    let device_delay = stream
        .time()
        .map_or(0, |time| device_delay_frames(&time, sample_rate));
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let Some(bytes) = data.data() else {
        return;
    };
    let capacity = bytes.len() / FRAME_BYTES;
    let frames = match requested {
        0 => capacity,
        requested => requested.min(capacity),
    };
    {
        let mut state = shared.lock();
        let taken = fill(&mut state.queue, &mut bytes[..frames * FRAME_BYTES]);
        state.clock.advance(taken, device_delay, Instant::now());
    }
    shared.wake.notify_all();
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = FRAME_BYTES as i32;
    *chunk.size_mut() = (frames * FRAME_BYTES) as u32;
}

fn device_delay_frames(time: &pw::stream::Time, sample_rate: u32) -> i64 {
    let rate = time.rate();
    let graph_delay = match rate.denom {
        0 => 0,
        denom => {
            i128::from(time.delay()) * i128::from(rate.num) * i128::from(sample_rate)
                / i128::from(denom)
        }
    };
    graph_delay as i64 + time.buffered() as i64
}

fn fill(queue: &mut VecDeque<f32>, bytes: &mut [u8]) -> usize {
    let frames = bytes.len() / FRAME_BYTES;
    let taken = (queue.len() / CHANNELS).min(frames);
    let samples = queue.drain(..taken * CHANNELS).chain(iter::repeat(0.0));
    let (slots, _) = bytes.as_chunks_mut::<SAMPLE_BYTES>();
    for (slot, sample) in slots.iter_mut().zip(samples) {
        *slot = sample.to_le_bytes();
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn decoded(bytes: &[u8]) -> Vec<f32> {
        let (samples, _) = bytes.as_chunks::<SAMPLE_BYTES>();
        samples.iter().copied().map(f32::from_le_bytes).collect()
    }

    #[test]
    fn filling_takes_whole_frames_and_pads_with_silence() {
        let mut queue = VecDeque::from([0.1, 0.2, 0.3, 0.4, 0.5]);
        let mut bytes = [0xff; 3 * FRAME_BYTES];
        assert_eq!(fill(&mut queue, &mut bytes), 2);
        assert_eq!(decoded(&bytes), [0.1, 0.2, 0.3, 0.4, 0.0, 0.0]);
        assert_eq!(queue, [0.5]);
    }

    #[test]
    fn the_clock_waits_for_the_device_to_play_its_first_samples() {
        let start = Instant::now();
        let mut clock = Clock::default();
        assert_eq!(clock.position(start, RATE), 0);
        clock.advance(1024, 2048, start);
        assert_eq!(clock.position(start, RATE), 0);
        assert_eq!(clock.position(start + Duration::from_millis(40), RATE), 0);
        assert_eq!(clock.position(start + Duration::from_millis(50), RATE), 352);
    }

    #[test]
    fn the_clock_interpolates_between_callbacks_up_to_what_was_consumed() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.advance(1024, 0, start);
        clock.advance(1024, 512, start + Duration::from_millis(20));
        assert_eq!(clock.position(start + Duration::from_millis(20), RATE), 512);
        assert_eq!(
            clock.position(start + Duration::from_millis(30), RATE),
            512 + 480
        );
        assert_eq!(clock.position(start + Duration::from_secs(5), RATE), 2048);
    }

    #[test]
    fn the_clock_never_runs_backwards() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.advance(1024, 0, start);
        clock.advance(1024, 0, start + Duration::from_millis(10));
        assert_eq!(
            clock.position(start + Duration::from_millis(30), RATE),
            1984
        );
        clock.advance(1024, 800, start + Duration::from_millis(30));
        assert_eq!(
            clock.position(start + Duration::from_millis(30), RATE),
            1984
        );
    }

    #[test]
    fn a_starved_clock_stops_where_the_samples_ran_out() {
        let start = Instant::now();
        let mut clock = Clock::default();
        clock.advance(100, 0, start);
        clock.advance(0, 0, start + Duration::from_millis(20));
        assert_eq!(clock.position(start + Duration::from_secs(1), RATE), 100);
    }
}
