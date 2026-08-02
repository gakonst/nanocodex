/// Streaming linear PCM sample-rate converter.
pub struct LinearResampler {
    step: f64,
    position: f64,
    source: Vec<f32>,
}

impl LinearResampler {
    /// Creates a converter between fixed source and destination sample rates.
    pub fn new(source_rate: u32, destination_rate: u32) -> Self {
        Self {
            step: f64::from(source_rate) / f64::from(destination_rate),
            position: 0.0,
            source: Vec::new(),
        }
    }

    /// Converts the next contiguous source chunk into the reusable destination buffer.
    pub fn push_into(&mut self, input: impl IntoIterator<Item = f32>, output: &mut Vec<f32>) {
        self.source.extend(input);
        output.clear();
        while self.position + 1.0 < self.source.len() as f64 {
            let index = self.position.floor() as usize;
            let fraction = (self.position - index as f64) as f32;
            output.push(
                self.source[index] + (self.source[index + 1] - self.source[index]) * fraction,
            );
            self.position += self.step;
        }
        let consumed = self.position.floor() as usize;
        if consumed > 0 {
            self.source.drain(..consumed.min(self.source.len()));
            self.position -= consumed as f64;
        }
    }

    /// Clears retained interpolation state.
    pub fn clear(&mut self) {
        self.position = 0.0;
        self.source.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::LinearResampler;

    #[test]
    fn interpolates_across_chunk_boundaries_without_reallocating_output() {
        let mut resampler = LinearResampler::new(48_000, 24_000);
        let mut output = Vec::with_capacity(4);
        resampler.push_into([0.0, 0.25], &mut output);
        assert_eq!(output, vec![0.0]);
        let capacity = output.capacity();
        resampler.push_into([0.5, 0.75, 1.0], &mut output);
        assert_eq!(output, vec![0.5]);
        assert_eq!(output.capacity(), capacity);

        let mut resampler = LinearResampler::new(24_000, 48_000);
        resampler.push_into([0.0], &mut output);
        assert!(output.is_empty());
        resampler.push_into([1.0], &mut output);
        assert_eq!(output, vec![0.0, 0.5]);
        resampler.push_into([2.0], &mut output);
        assert_eq!(output, vec![1.0, 1.5]);
    }

    #[test]
    fn output_is_invariant_to_source_chunk_boundaries() {
        let source = (0..4_410)
            .map(|index| (index as f32 / 100.0).sin())
            .collect::<Vec<_>>();
        let mut contiguous = LinearResampler::new(44_100, 24_000);
        let mut expected = Vec::new();
        contiguous.push_into(source.iter().copied(), &mut expected);

        let mut chunked = LinearResampler::new(44_100, 24_000);
        let mut actual = Vec::new();
        let mut output = Vec::new();
        for chunk in source.chunks(127) {
            chunked.push_into(chunk.iter().copied(), &mut output);
            actual.extend_from_slice(&output);
        }
        assert_eq!(actual, expected);
    }
}
