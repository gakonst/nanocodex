//! Streaming G.711 μ-law / 24 kHz PCM conversion for telephone media.
pub fn decode(byte: u8) -> i16 {
    let value = !byte;
    let magnitude = (((i32::from(value & 15) << 3) + 132) << ((value >> 4) & 7)) - 132;
    (if value & 128 != 0 {
        -magnitude
    } else {
        magnitude
    }) as i16
}
pub fn encode(sample: i16) -> u8 {
    let mut value = i32::from(sample);
    let sign = if value < 0 {
        value = -value;
        128
    } else {
        0
    };
    value = value.min(32635) + 132;
    let exponent = (0..=7).rev().find(|e| value & (128 << e) != 0).unwrap_or(0);
    !(sign | (exponent << 4) as u8 | ((value >> (exponent + 3)) & 15) as u8)
}
/// Linear interpolation preserves state across arbitrary input frame boundaries.
#[derive(Default)]
pub struct Upsampler {
    previous: Option<i16>,
}
impl Upsampler {
    pub fn convert(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len() * 6);
        for &byte in input {
            let current = decode(byte);
            let previous = self.previous.unwrap_or(current);
            for step in 1..=3 {
                let sample = (i32::from(previous)
                    + (i32::from(current) - i32::from(previous)) * step / 3)
                    as i16;
                output.extend_from_slice(&sample.to_le_bytes());
            }
            self.previous = Some(current);
        }
        output
    }
}
/// Low-pass FIR before 3:1 decimation keeps wideband speech from aliasing into
/// the telephone band. The 63-tap filter adds about 1.3 ms of group delay.
pub struct Downsampler {
    history: [f64; 63],
    position: usize,
    phase: u8,
}
impl Default for Downsampler {
    fn default() -> Self {
        Self {
            history: [0.0; 63],
            position: 0,
            phase: 0,
        }
    }
}
fn low_pass() -> &'static [f64; 63] {
    static COEFFICIENTS: std::sync::OnceLock<[f64; 63]> = std::sync::OnceLock::new();
    COEFFICIENTS.get_or_init(|| {
        let mut result = [0.0; 63];
        let cutoff = 3400.0 / 24_000.0;
        for (i, coefficient) in result.iter_mut().enumerate() {
            let offset = i as f64 - 31.0;
            let sinc = if offset == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * offset).sin()
                    / (std::f64::consts::PI * offset)
            };
            let window = 0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / 62.0).cos();
            *coefficient = sinc * window;
        }
        let sum: f64 = result.iter().sum();
        for coefficient in &mut result {
            *coefficient /= sum;
        }
        result
    })
}
impl Downsampler {
    /// The Realtime PCM contract supplies complete little-endian i16 samples.
    pub fn convert(&mut self, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len() / 6);
        for bytes in input.chunks_exact(2) {
            self.history[self.position] = f64::from(i16::from_le_bytes([bytes[0], bytes[1]]));
            self.position = (self.position + 1) % self.history.len();
            self.phase += 1;
            if self.phase == 3 {
                let sample: f64 = low_pass()
                    .iter()
                    .enumerate()
                    .map(|(i, coefficient)| {
                        coefficient * self.history[(self.position + 62 - i) % 63]
                    })
                    .sum();
                output.push(encode(sample.round().clamp(-32768.0, 32767.0) as i16));
                self.phase = 0;
            }
        }
        output
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_vectors() {
        assert_eq!(decode(0xff), 0);
        assert_eq!(decode(0x7f), 0);
        assert_eq!(decode(0x00), -32124);
        assert_eq!(decode(0x80), 32124);
        assert_eq!(encode(0), 0xff);
        for byte in 0..=255 {
            assert_eq!(decode(encode(decode(byte))), decode(byte));
        }
    }
    #[test]
    fn rejects_frequencies_above_the_telephone_band() {
        let rms = |frequency: f64| {
            let pcm: Vec<u8> = (0..2400)
                .flat_map(|n| {
                    let sample = (10_000.0
                        * (2.0 * std::f64::consts::PI * frequency * n as f64 / 24_000.0).sin())
                        as i16;
                    sample.to_le_bytes()
                })
                .collect();
            let audio = Downsampler::default().convert(&pcm);
            let settled = &audio[100..];
            (settled
                .iter()
                .map(|&byte| f64::from(decode(byte)).powi(2))
                .sum::<f64>()
                / settled.len() as f64)
                .sqrt()
        };
        assert!(rms(1000.0) > 6000.0);
        assert!(rms(6000.0) < 100.0);
    }
    #[test]
    fn streaming_boundaries_preserve_samples() {
        let input: Vec<u8> = (0..=255).collect();
        let pcm = Upsampler::default().convert(&input);
        let mut up = Upsampler::default();
        let split: Vec<u8> = input
            .chunks(7)
            .flat_map(|chunk| up.convert(chunk))
            .collect();
        assert_eq!(pcm, split);
        assert_eq!(pcm.len(), input.len() * 6);
        let whole = Downsampler::default().convert(&pcm);
        let mut down = Downsampler::default();
        let split: Vec<u8> = pcm
            .chunks(14)
            .flat_map(|chunk| down.convert(chunk))
            .collect();
        assert_eq!(whole, split);
        assert_eq!(whole.len(), input.len());
    }
}
