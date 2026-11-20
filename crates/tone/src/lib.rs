//! An equalizer and a loudness correction for 16-bit stereo audio.
//!
//! The equalizer has ten bands, an octave apart, and only takes away: a band at the top passes
//! as it came in, and lower down it is lowered by up to [`DEEPEST_CUT`] decibel. A sound that
//! fills the 16 bits has no room above it, and an equalizer that raised a band would have to
//! lower everything else to make that room. With every band at the top the audio passes
//! untouched, as it does with the equalizer off.
//!
//! The loudness correction is the contour of the loudness button of a hi-fi amplifier of the
//! 1980s: the bass raised around 100 Hz and the treble around 10 kHz, by about as much as those
//! amplifiers did, over most of the volume, and less and less towards the top of it, as with
//! their tapped volume knobs. It first lowers the whole by what it raises the bass, and tells by
//! how much, so that who sets the volume of the speaker can raise it by as much: the correction
//! fades towards full volume fast enough for the speaker always to have that room.
//!
//! A filter also shifts frequencies against each other in time, which can lift a peak of the
//! wave a little above where it was though no frequency is louder: such a peak is clipped.

use std::f32::consts::TAU;

/// The middle of each band of the equalizer, in hertz.
pub const BANDS: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16_000.0,
];
/// The bands, then the bass and the treble of the loudness correction.
const FILTERS: usize = BANDS.len() + 2;
/// The most a band is lowered, in decibel.
pub const DEEPEST_CUT: i8 = -24;
/// How wide a band is around its middle: the bands meet where each has half its gain.
const BAND_SHARPNESS: f32 = 1.4;
/// How steep the loudness correction rises: gently, over more than two octaves.
const SHELF_SHARPNESS: f32 = 0.5;
/// Where the loudness correction is halfway up, in hertz: it is most of the way at 100 Hz and at
/// 10 kHz.
const LOUDNESS_BASS: f32 = 150.0;
const LOUDNESS_TREBLE: f32 = 6000.0;
/// By how much it raises the bass and the treble, in decibel.
const LOUDNESS_BASS_GAIN: f32 = 10.0;
const LOUDNESS_TREBLE_GAIN: f32 = 8.0;
/// How fast it fades towards full volume: whole up to 58 % of the volume, half at 79 %, nothing
/// at the top.
const LOUDNESS_FADE: f32 = 2.4;

/// What the listener chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub equalizer: bool,
    /// The gain of each band, in decibel: none at the top, down to `DEEPEST_CUT`.
    pub gains: [i8; BANDS.len()],
    pub loudness: bool,
}

impl Settings {
    /// The settings as text, to keep them: whether the equalizer is on, the gains and whether
    /// loudness is on, between commas.
    pub fn to_text(&self) -> String {
        let gains: Vec<String> = self.gains.iter().map(i8::to_string).collect();
        format!(
            "{},{},{}",
            u8::from(self.equalizer),
            gains.join(","),
            u8::from(self.loudness)
        )
    }

    /// The settings kept as `text`, or `None` if it does not hold any.
    pub fn from_text(text: &str) -> Option<Self> {
        let mut parts = text.split(',');
        let on = |part: &str| match part {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        };
        let equalizer = on(parts.next()?)?;
        let mut gains = [0i8; BANDS.len()];
        for gain in &mut gains {
            *gain = parts.next()?.parse().ok()?;
            if !(DEEPEST_CUT..=0).contains(gain) {
                return None;
            }
        }
        let loudness = on(parts.next()?)?;
        parts.next().is_none().then_some(Self {
            equalizer,
            gains,
            loudness,
        })
    }
}

#[derive(Clone, Copy)]
enum Shape {
    /// Raises or lowers what is around a frequency.
    Band,
    /// Raises or lowers what is below a frequency.
    Bass,
    /// Raises or lowers what is above a frequency.
    Treble,
}

/// A second-order filter for two channels, as in the Audio EQ Cookbook of Robert
/// Bristow-Johnson. Its past lives apart from its coefficients, so that these can change while
/// audio flows.
#[derive(Clone, Copy)]
struct Filter {
    forward: [f32; 3],
    back: [f32; 2],
    past: [[f32; 2]; 2],
}

impl Filter {
    const FLAT: Self = Self {
        forward: [1.0, 0.0, 0.0],
        back: [0.0, 0.0],
        past: [[0.0; 2]; 2],
    };

    fn shape(&mut self, shape: Shape, hertz: f32, decibel: f32, sample_rate: f32) {
        let amplitude = 10f32.powf(decibel / 40.0);
        let angle = TAU * hertz / sample_rate;
        let (sine, cosine) = angle.sin_cos();
        let (forward, back) = match shape {
            Shape::Band => {
                let alpha = sine / (2.0 * BAND_SHARPNESS);
                (
                    [1.0 + alpha * amplitude, -2.0 * cosine, 1.0 - alpha * amplitude],
                    [1.0 + alpha / amplitude, -2.0 * cosine, 1.0 - alpha / amplitude],
                )
            }
            Shape::Bass | Shape::Treble => {
                // The treble shelf is the bass shelf with the cosine turned around.
                let turn = if matches!(shape, Shape::Bass) { 1.0 } else { -1.0 };
                let cosine = turn * cosine;
                let slope = 2.0 * amplitude.sqrt() * sine / (2.0 * SHELF_SHARPNESS);
                let (more, less) = (amplitude + 1.0, amplitude - 1.0);
                (
                    [
                        amplitude * (more - less * cosine + slope),
                        turn * 2.0 * amplitude * (less - more * cosine),
                        amplitude * (more - less * cosine - slope),
                    ],
                    [
                        more + less * cosine + slope,
                        turn * -2.0 * (less + more * cosine),
                        more + less * cosine - slope,
                    ],
                )
            }
        };
        self.forward = forward.map(|coefficient| coefficient / back[0]);
        self.back = [back[1] / back[0], back[2] / back[0]];
    }

    fn pass(&mut self, channel: usize, sample: f32) -> f32 {
        let past = &mut self.past[channel];
        let out = self.forward[0] * sample + past[0];
        past[0] = self.forward[1] * sample - self.back[0] * out + past[1];
        past[1] = self.forward[2] * sample - self.back[1] * out;
        out
    }
}

pub struct Tone {
    sample_rate: f32,
    /// The bands, then the bass and the treble of the loudness correction.
    filters: [Filter; FILTERS],
    /// Which filters do anything, first in the array, and how many they are: a band at the top
    /// and a loudness correction that has faded cost nothing, and with none the audio passes
    /// untouched.
    working: [usize; FILTERS],
    count: usize,
    /// By how much the audio is lowered before the filters, in decibel, to make room for what
    /// the loudness correction raises.
    room: f32,
}

impl Tone {
    /// A tone control for audio sampled at `sample_rate`, which passes it untouched until set.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate as f32,
            filters: [Filter::FLAT; FILTERS],
            working: [0; FILTERS],
            count: 0,
            room: 0.0,
        }
    }

    /// By how much the audio plays lower than it came in, in decibel, where nothing is raised:
    /// what the speaker's volume has to be raised by for the sound to keep its level.
    pub fn room(&self) -> f32 {
        self.room
    }

    /// Takes the listener's settings and the volume, from 0 to 1, that the audio plays at.
    pub fn set(&mut self, settings: Settings, volume: f32) {
        for ((filter, hertz), gain) in self.filters.iter_mut().zip(BANDS).zip(settings.gains) {
            filter.shape(Shape::Band, hertz, f32::from(gain), self.sample_rate);
        }
        let correction = if settings.loudness {
            (LOUDNESS_FADE * (1.0 - volume.clamp(0.0, 1.0))).min(1.0)
        } else {
            0.0
        };
        let bass = LOUDNESS_BASS_GAIN * correction;
        self.filters[BANDS.len()].shape(Shape::Bass, LOUDNESS_BASS, bass, self.sample_rate);
        self.filters[BANDS.len() + 1].shape(
            Shape::Treble,
            LOUDNESS_TREBLE,
            LOUDNESS_TREBLE_GAIN * correction,
            self.sample_rate,
        );

        self.count = 0;
        let bands = settings.gains.into_iter().map(|gain| settings.equalizer && gain != 0);
        let loudness = [correction > 0.0; 2];
        for (filter, works) in bands.chain(loudness).enumerate() {
            if works {
                self.working[self.count] = filter;
                self.count += 1;
            }
        }
        self.room = bass;
    }

    /// Changes `frames` in place: stereo frames of two little-endian 16-bit samples.
    pub fn process(&mut self, frames: &mut [u8]) {
        if self.count == 0 {
            return;
        }
        let lowered = 10f32.powf(-self.room / 20.0);
        for frame in frames.as_chunks_mut::<4>().0 {
            for (channel, bytes) in frame.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let mut sample = f32::from(i16::from_le_bytes(*bytes)) * lowered;
                for filter in &self.working[..self.count] {
                    sample = self.filters[*filter].pass(channel, sample);
                }
                // The conversion clips what overflows; half a step towards the sign rounds it.
                let rounded = (sample + 0.5f32.copysign(sample)) as i16;
                *bytes = rounded.to_le_bytes();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 44_100;

    /// A second of a tone of `hertz` on the left and of silence on the right, at a tenth of
    /// full scale.
    fn tone(hertz: f32) -> Vec<u8> {
        (0..SAMPLE_RATE)
            .flat_map(|index| {
                let angle = TAU * hertz * index as f32 / SAMPLE_RATE as f32;
                let left = (angle.sin() * 3276.7) as i16;
                [left.to_le_bytes(), 0i16.to_le_bytes()].concat()
            })
            .collect()
    }

    /// How loud the second half of the left channel is, in decibel against the tone of `tone`.
    fn level(frames: &[u8]) -> f32 {
        let half = &frames[frames.len() / 2..];
        let power: f32 = half
            .as_chunks::<4>()
            .0
            .iter()
            .map(|frame| f32::from(i16::from_le_bytes([frame[0], frame[1]])).powi(2))
            .sum::<f32>()
            / (half.len() / 4) as f32;
        10.0 * (power / (3276.7f32.powi(2) / 2.0)).log10()
    }

    /// The level at `hertz`, in decibel, that the settings give at a volume.
    fn gain_at(hertz: f32, settings: Settings, volume: f32) -> f32 {
        let mut control = Tone::new(SAMPLE_RATE);
        control.set(settings, volume);
        let mut frames = tone(hertz);
        control.process(&mut frames);
        level(&frames)
    }

    fn band(index: usize, gain: i8) -> Settings {
        let mut settings = Settings {
            equalizer: true,
            ..Settings::default()
        };
        settings.gains[index] = gain;
        settings
    }

    #[test]
    fn passes_the_audio_untouched_when_off_or_with_every_band_at_the_top() {
        let mut off = Settings::default();
        off.gains[5] = -24;
        for settings in [off, band(5, 0)] {
            let mut control = Tone::new(SAMPLE_RATE);
            control.set(settings, 0.2);
            let mut frames = tone(1000.0);
            control.process(&mut frames);
            assert_eq!(frames, tone(1000.0));
            assert_eq!(control.room(), 0.0);
        }
    }

    #[test]
    fn works_only_the_bands_that_were_moved() {
        let mut control = Tone::new(SAMPLE_RATE);
        let mut moved = band(2, -6);
        moved.gains[7] = -3;
        control.set(moved, 1.0);
        assert_eq!(control.working[..control.count], [2, 7]);

        moved.loudness = true;
        control.set(moved, 0.3);
        assert_eq!(control.working[..control.count], [2, 7, 10, 11]);

        moved.equalizer = false;
        control.set(moved, 0.3);
        assert_eq!(control.working[..control.count], [10, 11]);
        control.set(moved, 1.0);
        assert_eq!(control.count, 0);
    }

    #[test]
    fn lowers_a_band_by_its_gain() {
        for (index, hertz) in BANDS.into_iter().enumerate() {
            for gain in [-24, -12] {
                let level = gain_at(hertz, band(index, gain), 1.0);
                assert!((level - f32::from(gain)).abs() < 0.5, "{hertz} Hz at {level} dB");
            }
        }
    }

    #[test]
    fn never_makes_a_frequency_louder_than_it_came_in() {
        let mut uneven = band(0, -24);
        uneven.gains[3] = -7;
        uneven.gains[4] = -19;
        uneven.gains[8] = -12;
        let mut low = band(0, -24);
        low.gains = [-24; BANDS.len()];
        for settings in [uneven, low] {
            for step in 0..60 {
                let hertz = 20.0 * 2f32.powf(step as f32 / 6.0);
                let level = gain_at(hertz, settings, 1.0);
                assert!(level < 0.05, "{hertz} Hz at {level} dB");
            }
        }
    }

    #[test]
    fn leaves_the_far_bands_alone() {
        let far = gain_at(1000.0, band(0, -24), 1.0);
        assert!(far.abs() < 0.5, "{far} dB");
        let near = gain_at(62.0, band(0, -24), 1.0);
        assert!((-12.0..-1.0).contains(&near), "{near} dB");
    }

    #[test]
    fn loudness_raises_the_bass_and_the_treble_and_fades_towards_full_volume() {
        let on = Settings {
            loudness: true,
            ..Settings::default()
        };
        let over_middle = |hertz, volume| gain_at(hertz, on, volume) - gain_at(1000.0, on, volume);

        for volume in [0.0, 0.3, 0.58] {
            let deep = over_middle(40.0, volume);
            assert!((8.5..10.5).contains(&deep), "40 Hz at {deep} dB");
            let bass = over_middle(100.0, volume);
            assert!((5.5..8.5).contains(&bass), "100 Hz at {bass} dB");
            let treble = over_middle(10_000.0, volume);
            assert!((4.5..7.5).contains(&treble), "10 kHz at {treble} dB");
            let top = over_middle(16_000.0, volume);
            assert!((6.0..8.5).contains(&top), "16 kHz at {top} dB");
        }
        let half = over_middle(40.0, 0.79);
        assert!((4.0..5.5).contains(&half), "{half} dB");
        assert!(over_middle(40.0, 1.0).abs() < 0.1);
        assert!(over_middle(16_000.0, 1.0).abs() < 0.1);
    }

    #[test]
    fn loudness_tells_the_room_it_takes_and_never_more_than_the_speaker_has() {
        let on = Settings {
            loudness: true,
            ..Settings::default()
        };
        let mut control = Tone::new(SAMPLE_RATE);
        control.set(on, 0.3);
        assert!((control.room() - 10.0).abs() < 0.01, "{} dB", control.room());
        let middle = gain_at(1000.0, on, 0.3);
        assert!((-11.0..-9.0).contains(&middle), "{middle} dB");

        // A speaker plays from 30 dB below its loudest, evenly over the volume.
        for step in 1..=100 {
            let volume = step as f32 / 100.0;
            control.set(on, volume);
            assert!(control.room() <= 30.0 * (1.0 - volume) + 0.01, "{volume}");
        }
        assert_eq!(control.room(), 0.0);

        for step in 0..60 {
            let hertz = 20.0 * 2f32.powf(step as f32 / 6.0);
            let level = gain_at(hertz, on, 0.3);
            assert!(level < 0.1, "{hertz} Hz at {level} dB");
        }
    }

    #[test]
    fn keeps_the_channels_apart() {
        let mut control = Tone::new(SAMPLE_RATE);
        control.set(band(5, -12), 1.0);
        let mut frames = tone(1000.0);
        control.process(&mut frames);
        let (frames, _) = frames.as_chunks::<4>();
        assert!(frames.iter().all(|frame| frame[2..] == [0, 0]));
    }

    #[test]
    fn clips_a_peak_that_the_filters_lift() {
        // A square wave at full scale with its pitch lowered: what is left of it peaks higher.
        let mut control = Tone::new(SAMPLE_RATE);
        control.set(band(5, -24), 1.0);
        let mut square: Vec<u8> = (0..SAMPLE_RATE)
            .flat_map(|index| {
                let sample = if index / 22 % 2 == 0 { i16::MAX } else { i16::MIN };
                [sample.to_le_bytes(), sample.to_le_bytes()].concat()
            })
            .collect();
        control.process(&mut square);
        let loudest = square
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| i16::from_le_bytes(*bytes).unsigned_abs())
            .max()
            .unwrap();
        assert!(loudest <= i16::MIN.unsigned_abs());
    }

    #[test]
    fn settings_survive_being_kept_as_text() {
        let settings = Settings {
            equalizer: true,
            gains: [-24, -3, 0, -7, -12, 0, 0, -1, 0, -5],
            loudness: false,
        };
        assert_eq!(settings.to_text(), "1,-24,-3,0,-7,-12,0,0,-1,0,-5,0");
        assert_eq!(Settings::from_text(&settings.to_text()), Some(settings));
        assert_eq!(Settings::from_text("0,0,0,0,0,0,0,0,0,0,0,0"), Some(Settings::default()));
        let bad = [
            "",
            "0,0,0,0,0,0,0",
            "0,0,0,0,0,0,0,0,0,0,1,0",
            "0,0,0,0,0,0,0,0,0,0,-25,0",
            "0,0,0,0,0,0,0,0,0,0,0,2",
            "0,0,0,0,0,0,0,0,0,0,0,0,0",
            "0,a,0,0,0,0,0,0,0,0,0,0",
        ];
        for bad in bad {
            assert_eq!(Settings::from_text(bad), None, "{bad}");
        }
    }
}
