//! Timestamped stereo mixing. Audio clocks share the capture QPC epoch; gaps are silence.
//! Each source's packets are laid end to end. Their device timestamps jitter by up to a third
//! of a millisecond (microphones especially), and placing every packet at its own timestamp
//! left a gap or an overlap at most packet boundaries, heard as crackling. The timestamps only
//! steer the run: a drift beyond SLACK repeats or drops one frame per packet, and a jump beyond
//! RESYNC (game audio pausing, a device glitch) restarts the run at the timestamp.
use std::collections::VecDeque;
pub const RATE: u64 = 48_000;
pub const TICKS: u64 = 10_000_000;
const SLACK: i64 = RATE as i64 / 500; // 2 ms
const RESYNC: i64 = RATE as i64 / 50; // 20 ms
struct Packet {
    start: u64,
    samples: Vec<f32>,
}
#[derive(Default)]
pub struct Mixer {
    sources: [VecDeque<Packet>; 2],
    next: [Option<i64>; 2],
    consumed: u64,
}
impl Mixer {
    /// The next packet of `source` starts at its own timestamp instead of continuing the run.
    pub fn restart(&mut self, source: usize) {
        self.next[source] = None;
    }
    /// `stamp` is the packet's first frame relative to the epoch, possibly negative. A silent
    /// packet (`None`) still advances the run.
    pub fn push(&mut self, source: usize, stamp: i64, frames: usize, samples: Option<&[f32]>) {
        let start = match self.next[source] {
            Some(next) if (stamp - next).abs() <= RESYNC => next,
            _ => stamp,
        };
        let (skip, repeat) = match stamp - start {
            _ if frames == 0 => (0, 0),
            lag if lag > SLACK => (0, 1),
            lag if lag < -SLACK => (1, 0),
            _ => (0, 0),
        };
        let length = (frames - skip + repeat) as i64;
        self.next[source] = Some(start + length);
        let Some(samples) = samples else { return };
        let first = start.max(self.consumed as i64);
        let last = (start + length).min((self.consumed + RATE * 5) as i64);
        if first >= last {
            return;
        }
        let mut run = Vec::with_capacity(length as usize * 2);
        run.extend_from_slice(&samples[..repeat * 2]);
        run.extend_from_slice(&samples[skip * 2..]);
        let from = (first - start) as usize * 2;
        let to = (last - start) as usize * 2;
        run.truncate(to);
        run.drain(..from);
        self.sources[source].push_back(Packet {
            start: first as u64,
            samples: run,
        });
    }
    pub fn take_pcm(&mut self, start: u64, count: usize) -> Vec<u8> {
        let end = start + count as u64;
        let mut mixed = vec![0.0f32; count * 2];
        for (source, packets) in self.sources.iter_mut().enumerate() {
            let gain = if source == 0 { 0.55 } else { 1.0 };
            for packet in packets.iter() {
                let first = start.max(packet.start);
                let last = end.min(packet.start + packet.samples.len() as u64 / 2);
                if first >= last {
                    continue;
                }
                let offset = (first - packet.start) as usize * 2;
                let target = (first - start) as usize * 2;
                for i in 0..(last - first) as usize * 2 {
                    mixed[target + i] += packet.samples[offset + i] * gain;
                }
            }
            packets.retain(|p| p.start + p.samples.len() as u64 / 2 > end);
        }
        self.consumed = end;
        let mut output = Vec::with_capacity(count * 4);
        for sample in mixed {
            output.extend_from_slice(
                &((sample.clamp(-1., 1.) * i16::MAX as f32) as i16).to_le_bytes(),
            );
        }
        output
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn left(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks_exact(4)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect()
    }
    /// Stereo frames whose left channel converts to the PCM values from, from + 1, ...
    fn ramp(from: usize, frames: usize) -> Vec<f32> {
        (from..from + frames)
            .flat_map(|i| [(i as f32 + 0.5) / i16::MAX as f32, 0.])
            .collect()
    }
    #[test]
    fn aligns_sources_and_fills_silence() {
        let mut m = Mixer::default();
        m.push(0, 1, 2, Some(&[1., -1., 0.5, 0.5]));
        m.push(1, 2, 1, Some(&[0.5, -0.5]));
        let b = m.take_pcm(0, 4);
        let v: Vec<_> = b
            .chunks_exact(2)
            .map(|v| i16::from_le_bytes([v[0], v[1]]))
            .collect();
        assert_eq!(&v[..2], &[0, 0]);
        assert_eq!(v[2], (32767. * 0.55) as i16);
        assert_eq!(v[4], (32767. * 0.775) as i16);
        assert_eq!(&v[6..], &[0, 0]);
    }
    #[test]
    fn clips_mix_and_rejects_already_consumed_packets() {
        let mut m = Mixer::default();
        m.push(0, 0, 1, Some(&[1., -1.]));
        m.push(1, 0, 1, Some(&[1., -1.]));
        assert_eq!(m.take_pcm(0, 1), [255, 127, 1, 128]);
        m.restart(0);
        m.push(0, 0, 1, Some(&[1., 1.]));
        assert!(m.sources[0].is_empty());
    }
    #[test]
    fn future_queue_is_bounded() {
        let mut m = Mixer::default();
        m.push(0, RATE as i64 * 5, 1, Some(&[1., 1.]));
        assert!(m.sources[0].is_empty());
    }
    #[test]
    fn jittering_timestamps_keep_packets_end_to_end() {
        // Recorded microphone packets: 480 frames, timestamps off by up to 16 frames either way.
        let mut m = Mixer::default();
        let jitter = [0, 14, -15, 1, -1, 13, 0, -16, 15, 2];
        for (i, offset) in jitter.iter().enumerate() {
            m.push(1, (i * 480) as i64 + offset, 480, Some(&ramp(i * 480 + 1, 480)));
        }
        let expected: Vec<i16> = (1..=4800).collect();
        assert_eq!(left(&m.take_pcm(0, 4800)), expected);
    }
    #[test]
    fn silent_packets_advance_the_run() {
        let mut m = Mixer::default();
        m.push(1, 0, 480, Some(&ramp(1, 480)));
        m.push(1, 485, 480, None);
        m.push(1, 955, 480, Some(&ramp(961, 480)));
        let v = left(&m.take_pcm(0, 1440));
        assert_eq!(v[479], 480);
        assert!(v[480..960].iter().all(|&s| s == 0));
        assert_eq!(v[960], 961);
    }
    #[test]
    fn drift_is_corrected_one_frame_per_packet() {
        // A device clock 0.1% slow against QPC (ten times a poor one) stays within SLACK.
        let mut m = Mixer::default();
        let mut lag = 0;
        for i in 0..400i64 {
            let stamp = i * 480 + i / 2;
            m.push(1, stamp, 480, None);
            lag = stamp + 480 - m.next[1].unwrap();
            assert!(lag <= SLACK, "packet {i} lags {lag} frames");
        }
        assert!(lag >= SLACK - 1, "the lag {lag} was corrected, not restarted");
        // A lagging run repeats the packet's first frame; a leading one drops it.
        let mut m = Mixer::default();
        m.push(1, 0, 3, Some(&ramp(1, 3)));
        m.push(1, 3 + SLACK + 1, 3, Some(&ramp(4, 3)));
        m.push(1, 7 - SLACK - 1, 3, Some(&ramp(7, 3)));
        assert_eq!(left(&m.take_pcm(0, 10)), [1, 2, 3, 4, 4, 5, 6, 8, 9, 0]);
    }
    #[test]
    fn a_jump_beyond_resync_restarts_at_the_timestamp() {
        let mut m = Mixer::default();
        m.push(1, 0, 480, Some(&ramp(1, 480)));
        m.push(1, 480 + RESYNC + 1, 480, Some(&ramp(1, 480)));
        let v = left(&m.take_pcm(0, 2000));
        assert_eq!(v[480 + RESYNC as usize], 0);
        assert_eq!(v[481 + RESYNC as usize], 1);
    }
}
