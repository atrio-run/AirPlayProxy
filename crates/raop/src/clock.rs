//! The sender's clock, which the receiver follows, and the audio timeline counted on it.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::SAMPLE_RATE;

/// Seconds between the NTP epoch, 1900, and the Unix one.
const UNIX_EPOCH_IN_NTP: u64 = 0x83AA_7E80;

/// A time in NTP format: seconds in the high 32 bits, their fraction in the low ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ntp(pub u64);

impl Ntp {
    fn from_duration(duration: Duration) -> Self {
        let fraction = ((duration.subsec_nanos() as u64) << 32) / 1_000_000_000;
        Self((duration.as_secs() << 32) | fraction)
    }

    fn to_duration(self) -> Duration {
        let nanos = ((self.0 & 0xFFFF_FFFF) * 1_000_000_000) >> 32;
        Duration::new(self.0 >> 32, nanos as u32)
    }

    /// The frame of the timeline that is current at this time.
    pub fn frame(self) -> u64 {
        ((self.0 as u128 * SAMPLE_RATE as u128) >> 32) as u64
    }

    /// The time at which `frame` becomes current.
    pub fn of_frame(frame: u64) -> Self {
        Self((((frame as u128) << 32).div_ceil(SAMPLE_RATE as u128)) as u64)
    }

    pub fn to_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }
}

/// Reads the wall time once and advances steadily from there, whatever happens to the wall time.
#[derive(Clone, Copy)]
pub struct Clock {
    started: Instant,
    start: Duration,
}

impl Clock {
    pub fn new() -> Self {
        let since_unix_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            started: Instant::now(),
            start: since_unix_epoch + Duration::from_secs(UNIX_EPOCH_IN_NTP),
        }
    }

    pub fn now(&self) -> Ntp {
        Ntp::from_duration(self.start + self.started.elapsed())
    }

    /// How long until the clock reads `time`, or `None` once it has.
    pub fn until(&self, time: Ntp) -> Option<Duration> {
        time.to_duration()
            .checked_sub(self.start + self.started.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holds_one_sample_rate_of_frames() {
        let second = Ntp(1 << 32);
        assert_eq!(second.frame(), SAMPLE_RATE as u64);
        assert_eq!(Ntp::of_frame(SAMPLE_RATE as u64), second);
    }

    #[test]
    fn frames_survive_the_round_trip_through_time() {
        for frame in [0, 1, 351, 352, 44_099, 170_000_000_000_000] {
            assert_eq!(Ntp::of_frame(frame).frame(), frame);
        }
    }

    #[test]
    fn converts_fractions_of_a_second() {
        let time = Ntp::from_duration(Duration::from_millis(1500));
        assert_eq!(time.0 >> 32, 1);
        assert_eq!(time.0 & 0xFFFF_FFFF, 1 << 31);
        assert_eq!(time.to_duration(), Duration::from_millis(1500));
    }

    #[test]
    fn the_clock_advances() {
        let clock = Clock::new();
        let earlier = clock.now();
        assert!(clock.until(Ntp(earlier.0 + (1 << 32))).is_some());
        assert!(clock.until(Ntp(earlier.0 - (1 << 32))).is_none());
    }
}
