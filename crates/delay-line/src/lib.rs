//! A delay line for interleaved audio frames: every frame leaves a fixed time after it entered.
//!
//! The writer and the reader run on different clocks, so the amount of audio held drifts. The
//! reader steers it back to the delay by dropping or repeating single frames: seldom while the
//! held amount is near the delay, more often the farther it is. Only a burst larger than the spare
//! capacity discards a stretch of audio, and only a writer that stops makes the line fill up again
//! from silence.

/// The held amount is averaged over `1 << SMOOTHING` pulls before the reader steers it, so that the
/// bursts in which audio arrives do not count as drift.
const SMOOTHING: u32 = 8;
/// Pulls between two frames dropped or repeated while the average is one tolerance away from the
/// delay. The pace grows with the square of that distance, up to a frame per pull.
const SPARSE: i64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulled {
    /// `out` holds the frames that were due.
    Audio,
    /// `out` holds silence: the line has not yet collected the delay.
    Filling,
    /// The writer stopped: `out` holds the last frames followed by silence.
    Stalled,
}

pub struct DelayLine {
    ring: Box<[u8]>,
    frame_bytes: usize,
    start: usize,
    held: usize,
    delay: usize,
    smoothed: usize,
    /// What the average has been away from the delay, squared and summed over the pulls since the
    /// last frame dropped or repeated. A frame is due when it reaches `due`.
    owed: i64,
    due: i64,
    filling: bool,
    dropped: u32,
    repeated: u32,
}

impl DelayLine {
    /// A line for `capacity` frames of `frame_bytes` bytes each, with no delay set.
    pub fn new(frame_bytes: usize, capacity: usize) -> Self {
        Self {
            ring: vec![0; capacity * frame_bytes].into_boxed_slice(),
            frame_bytes,
            start: 0,
            held: 0,
            delay: 0,
            smoothed: 0,
            owed: 0,
            due: 0,
            filling: true,
            dropped: 0,
            repeated: 0,
        }
    }

    /// The frames dropped so far to steer the held amount back to the delay: the writer's clock
    /// runs faster than the reader's.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    /// The frames repeated so far to steer the held amount back to the delay: the writer's clock
    /// runs slower than the reader's.
    pub fn repeated(&self) -> u32 {
        self.repeated
    }

    /// Empties the line and sets the frames it holds back. While the average of the held amount
    /// is `tolerance` frames away from `delay`, the reader drops or repeats one frame every
    /// `SPARSE` pulls; at twice that distance, four times as often. The tolerance must be at
    /// least `1 << SMOOTHING` frames, or the average trails the steering and overshoots.
    pub fn set_delay(&mut self, delay: usize, tolerance: usize) {
        assert!(delay * self.frame_bytes <= self.ring.len());
        self.delay = delay * self.frame_bytes;
        self.due = (tolerance * tolerance) as i64 * SPARSE;
        self.clear();
    }

    pub fn clear(&mut self) {
        self.start = 0;
        self.held = 0;
        self.owed = 0;
        self.filling = true;
    }

    /// Appends whole frames and returns how many frames were discarded to make room. A burst that
    /// does not fit discards the oldest audio, down to the delay.
    pub fn push(&mut self, mut frames: &[u8]) -> usize {
        let capacity = self.ring.len();
        let mut discarded = 0;

        if self.held + frames.len() > capacity {
            let excess = self.held + frames.len() - self.delay;
            let from_line = excess.min(self.held);
            self.start = (self.start + from_line) % capacity;
            self.held -= from_line;
            frames = &frames[excess - from_line..];
            self.smoothed = self.delay << SMOOTHING;
            self.owed = 0;
            discarded = excess / self.frame_bytes;
        }

        let end = (self.start + self.held) % capacity;
        let first = frames.len().min(capacity - end);
        self.ring[end..end + first].copy_from_slice(&frames[..first]);
        self.ring[..frames.len() - first].copy_from_slice(&frames[first..]);
        self.held += frames.len();

        discarded
    }

    /// Fills `out`, which holds at least two frames, with the frames due now.
    pub fn pull(&mut self, out: &mut [u8], silence: u8) -> Pulled {
        if self.filling {
            if self.held < self.delay.max(out.len()) {
                out.fill(silence);
                return Pulled::Filling;
            }
            self.filling = false;
            self.smoothed = self.held << SMOOTHING;
            self.owed = 0;
        }

        self.smoothed = self.smoothed + self.held - (self.smoothed >> SMOOTHING);
        let level = self.smoothed >> SMOOTHING;
        let frame = self.frame_bytes;
        let off = (level as i64 - self.delay as i64) / frame as i64;
        // What exceeds one frame carries over, so that any pace up to a frame per pull is kept.
        self.owed = (self.owed + off * off.abs()).clamp(-2 * self.due, 2 * self.due);
        let mut wanted = out.len();

        if off > 0 && self.owed >= self.due && self.held >= wanted + frame {
            self.start = (self.start + frame) % self.ring.len();
            self.held -= frame;
            self.owed -= self.due;
            self.dropped = self.dropped.wrapping_add(1);
        } else if off < 0 && self.owed <= -self.due {
            wanted -= frame;
            self.owed += self.due;
        }

        if self.held < wanted {
            let left = self.held;
            self.take(&mut out[..left]);
            out[left..].fill(silence);
            self.filling = true;
            return Pulled::Stalled;
        }

        self.take(&mut out[..wanted]);
        if wanted < out.len() {
            out.copy_within(wanted - frame..wanted, wanted);
            self.repeated = self.repeated.wrapping_add(1);
        }

        Pulled::Audio
    }

    fn take(&mut self, out: &mut [u8]) {
        let capacity = self.ring.len();
        let first = out.len().min(capacity - self.start);
        let rest = out.len() - first;
        out[..first].copy_from_slice(&self.ring[self.start..self.start + first]);
        out[first..].copy_from_slice(&self.ring[..rest]);
        self.start = (self.start + out.len()) % capacity;
        self.held -= out.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame carries its sequence number and the reader time, in frames, at which it entered.
    const FRAME: usize = 8;
    const SILENCE: u8 = 0xAA;
    const DELAY: usize = 24_000;
    const TOLERANCE: usize = 441;
    const SLACK: usize = 8_820;
    const PULL: usize = 256;
    const BURST: usize = 882;

    struct Played {
        sequence: u32,
        entered: u32,
    }

    struct Rig {
        line: DelayLine,
        next: u32,
        now: u32,
        discarded: usize,
    }

    impl Rig {
        fn new() -> Self {
            let mut line = DelayLine::new(FRAME, DELAY + SLACK);
            line.set_delay(DELAY, TOLERANCE);
            Self {
                line,
                next: 0,
                now: 0,
                discarded: 0,
            }
        }

        fn push(&mut self, frames: usize) {
            let mut bytes = Vec::with_capacity(frames * FRAME);
            for _ in 0..frames {
                bytes.extend_from_slice(&self.next.to_le_bytes());
                bytes.extend_from_slice(&self.now.to_le_bytes());
                self.next += 1;
            }
            self.discarded += self.line.push(&bytes);
        }

        fn pull(&mut self) -> (Pulled, Vec<Played>) {
            let mut out = [0; PULL * FRAME];
            let pulled = self.line.pull(&mut out, SILENCE);
            self.now += PULL as u32;
            let played = out
                .as_chunks::<FRAME>()
                .0
                .iter()
                .filter(|frame| frame.iter().any(|byte| *byte != SILENCE))
                .map(|frame| Played {
                    sequence: u32::from_le_bytes(frame[..4].try_into().unwrap()),
                    entered: u32::from_le_bytes(frame[4..].try_into().unwrap()),
                })
                .collect();
            (pulled, played)
        }
    }

    /// What a run measured: the smallest and largest delay, in frames, of any frame played, and
    /// the frames dropped and repeated.
    struct Run {
        shortest: u32,
        longest: u32,
        dropped: u32,
        repeated: u32,
    }

    /// Runs a writer whose clock differs from the reader's by `drift` parts per million.
    fn run(drift: i64, pulls: usize) -> Run {
        let mut rig = Rig::new();
        let mut owed = 0i64;
        let mut last = None;
        let (mut shortest, mut longest) = (u32::MAX, 0);

        for _ in 0..pulls {
            owed += PULL as i64 * (1_000_000 + drift);
            while owed >= BURST as i64 * 1_000_000 {
                rig.push(BURST);
                owed -= BURST as i64 * 1_000_000;
            }

            let started = rig.now;
            let (pulled, played) = rig.pull();
            assert_ne!(pulled, Pulled::Stalled);
            for frame in played {
                if let Some(last) = last {
                    let step = frame.sequence - last;
                    assert!(step <= 2, "frames {last} and {} are not adjacent", frame.sequence);
                }
                last = Some(frame.sequence);
                let delay = started - frame.entered;
                shortest = shortest.min(delay);
                longest = longest.max(delay);
            }
        }

        assert_eq!(rig.discarded, 0);
        Run {
            shortest,
            longest,
            dropped: rig.line.dropped(),
            repeated: rig.line.repeated(),
        }
    }

    /// How far from the delay, in frames, the held amount may get with clocks that differ by
    /// `drift` parts per million: the distance at which the steering keeps pace with the drift,
    /// and the bursts around it.
    fn margin(drift: i64) -> u32 {
        let drifted_per_pull = PULL as f64 * drift.abs() as f64 / 1e6;
        let keeps_pace = TOLERANCE as f64 * (drifted_per_pull * SPARSE as f64).sqrt();
        keeps_pace as u32 + (TOLERANCE + BURST + PULL) as u32
    }

    fn assert_holds_the_delay(drift: i64) {
        let half_an_hour = 30 * 60 * 44_100 / PULL;
        let Run {
            shortest, longest, ..
        } = run(drift, half_an_hour);
        let margin = margin(drift);
        assert!(shortest + margin >= DELAY as u32, "shortest delay {shortest}");
        assert!(longest <= DELAY as u32 + margin, "longest delay {longest}");
    }

    #[test]
    fn stays_silent_until_the_delay_is_collected() {
        let mut rig = Rig::new();
        let mut first = None;

        while first.is_none() {
            rig.push(PULL);
            let started = rig.now;
            let (_, played) = rig.pull();
            first = played.first().map(|frame| (frame.sequence, started));
        }

        let (sequence, started) = first.unwrap();
        assert_eq!(sequence, 0);
        assert!((DELAY - PULL..=DELAY).contains(&(started as usize)));
    }

    #[test]
    fn holds_the_delay_with_matched_clocks() {
        assert_holds_the_delay(0);
    }

    #[test]
    fn holds_the_delay_with_a_faster_writer() {
        assert_holds_the_delay(20);
        assert_holds_the_delay(200);
        assert_holds_the_delay(2_000);
    }

    #[test]
    fn holds_the_delay_with_a_slower_writer() {
        assert_holds_the_delay(-20);
        assert_holds_the_delay(-200);
        assert_holds_the_delay(-2_000);
    }

    #[test]
    fn counts_the_frames_it_drops_and_repeats() {
        let ten_minutes = 10 * 60 * 44_100 / PULL;
        let drifted = (ten_minutes * PULL * 2_000 / 1_000_000) as u32;
        let margin = margin(2_000);

        let faster = run(2_000, ten_minutes);
        assert!(faster.dropped.abs_diff(drifted) <= margin, "dropped {}", faster.dropped);
        assert!(faster.repeated <= BURST as u32, "repeated {}", faster.repeated);

        let slower = run(-2_000, ten_minutes);
        assert!(slower.repeated.abs_diff(drifted) <= margin, "repeated {}", slower.repeated);
        assert!(slower.dropped <= BURST as u32, "dropped {}", slower.dropped);

        let matched = run(0, ten_minutes);
        assert!(matched.dropped + matched.repeated <= BURST as u32);
    }

    /// Runs matched clocks with no bursts, adds `extra` frames at once, and returns the pulls
    /// between one dropped frame and the next over the half minute that follows.
    fn pulls_between_drops(extra: usize) -> Vec<usize> {
        let mut rig = Rig::new();
        rig.push(DELAY);
        rig.pull();
        rig.push(extra);

        let mut between = Vec::new();
        let mut since: Option<usize> = None;
        for _ in 0..30 * 44_100 / PULL {
            rig.push(PULL);
            let dropped = rig.line.dropped();
            assert_eq!(rig.pull().0, Pulled::Audio);
            since = match since {
                Some(pulls) if rig.line.dropped() != dropped => {
                    between.push(pulls + 1);
                    Some(0)
                }
                Some(pulls) => Some(pulls + 1),
                None if rig.line.dropped() != dropped => Some(0),
                None => None,
            };
        }
        assert_eq!(rig.line.repeated(), 0);
        assert_eq!(rig.discarded, 0);
        between
    }

    #[test]
    fn steers_seldom_near_the_delay() {
        let between = pulls_between_drops(TOLERANCE);

        assert!(between.len() >= 10, "{} frames dropped", between.len());
        let closest = *between.iter().min().unwrap();
        assert!(closest >= SPARSE as usize - 1, "frames dropped {closest} pulls apart");
    }

    #[test]
    fn steers_more_often_farther_from_the_delay() {
        let near = pulls_between_drops(TOLERANCE);
        let far = pulls_between_drops(4 * TOLERANCE);

        assert!(far.len() > 4 * near.len(), "{} and {} frames dropped", far.len(), near.len());
        let closest = *far.iter().min().unwrap();
        assert!(closest >= SPARSE as usize / 16 - 1, "frames dropped {closest} pulls apart");
    }

    /// The frames dropped or repeated in ten seconds of matched clocks, after ten that are not
    /// counted. With `late`, the writer first delivers nothing for 180 ms, then all it kept back.
    fn steered_in_ten_seconds(late: bool) -> u32 {
        let mut rig = Rig::new();
        let ten_seconds = 10 * 44_100 / PULL;
        let mut owed = 0;
        let mut steady = |rig: &mut Rig| {
            for _ in 0..ten_seconds {
                owed += PULL;
                if owed >= BURST {
                    rig.push(BURST);
                    owed -= BURST;
                }
                assert_ne!(rig.pull().0, Pulled::Stalled);
            }
        };
        steady(&mut rig);
        let before = rig.line.dropped() + rig.line.repeated();

        if late {
            let kept_back = 180 * 44_100 / 1000 / PULL;
            for _ in 0..kept_back {
                assert_eq!(rig.pull().0, Pulled::Audio);
            }
            rig.push(kept_back * PULL);
        }
        steady(&mut rig);

        assert_eq!(rig.discarded, 0);
        rig.line.dropped() + rig.line.repeated() - before
    }

    #[test]
    fn audio_that_arrives_late_is_not_taken_for_drift() {
        let usual = steered_in_ten_seconds(false);
        let late = steered_in_ten_seconds(true);
        assert!(late <= usual + 10, "{late} frames dropped or repeated, {usual} without the wait");
    }

    #[test]
    fn discards_the_oldest_audio_when_a_burst_does_not_fit() {
        let mut rig = Rig::new();
        rig.push(DELAY + SLACK);
        rig.push(BURST);

        assert_eq!(rig.discarded, SLACK + BURST);
        let (pulled, played) = rig.pull();
        assert_eq!(pulled, Pulled::Audio);
        assert_eq!(played[0].sequence as usize, SLACK + BURST);
    }

    #[test]
    fn plays_what_is_left_and_fills_up_again_when_the_writer_stops() {
        let mut rig = Rig::new();
        rig.push(DELAY + PULL / 2);

        let mut sequences = Vec::new();
        loop {
            let (pulled, played) = rig.pull();
            sequences.extend(played.iter().map(|frame| frame.sequence));
            if pulled == Pulled::Stalled {
                break;
            }
        }
        // The reader repeats a frame now and then while the held amount falls.
        sequences.dedup();
        assert!(sequences.iter().copied().eq(0..(DELAY + PULL / 2) as u32));

        rig.push(DELAY - 1);
        assert_eq!(rig.pull().0, Pulled::Filling);
        rig.push(1);
        let (pulled, played) = rig.pull();
        assert_eq!(pulled, Pulled::Audio);
        assert_eq!(played[0].sequence as usize, DELAY + PULL / 2);
    }

    #[test]
    fn clearing_drops_the_held_audio() {
        let mut rig = Rig::new();
        rig.push(DELAY);
        rig.line.clear();

        assert_eq!(rig.pull().0, Pulled::Filling);
        rig.push(DELAY);
        assert_eq!(rig.pull().1[0].sequence as usize, DELAY);
    }
}
