use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Lock-free single-producer/single-consumer ring buffer.
///
/// This is designed for AURELIS's audio thread:
///
/// Decoder Thread  -> push()
/// Audio Callback  -> pop()
///
/// No mutexes are used.
pub struct AudioRingBuffer {
    buffer: Box<[UnsafeCell<f32>]>,
    capacity: usize,
    write: AtomicUsize,
    read: AtomicUsize,
}

// SAFETY: the samples live in `UnsafeCell`s and are only touched through
// `push`/`push_mono_downmix` (the single producer) and `pop` (the single
// consumer). The producer only writes slots the consumer has already released,
// and the consumer only reads slots the producer has already published; the
// hand-over is the Release store / Acquire load on the `write` and `read`
// indices. `clear` must only run while no callback is consuming.
unsafe impl Sync for AudioRingBuffer {}

impl AudioRingBuffer {
    /// Create a new ring buffer.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 1, "capacity must be greater than 1");

        Self {
            buffer: (0..capacity)
                .map(|_| UnsafeCell::new(0.0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            capacity,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        }
    }

    /// Maximum number of samples.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of samples currently stored.
    pub fn len(&self) -> usize {
        let w = self.write.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Acquire);

        if w >= r { w - r } else { self.capacity - r + w }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_full(&self) -> bool {
        self.len() == self.capacity - 1
    }

    /// Push samples into the buffer.
    ///
    /// Returns the number of samples successfully written.
    pub fn push(&self, input: &[f32]) -> usize {
        let mut written = 0;

        while written < input.len() {
            let w = self.write.load(Ordering::Relaxed);
            let r = self.read.load(Ordering::Acquire);

            let next = (w + 1) % self.capacity;

            if next == r {
                break;
            }

            // SAFETY: slot `w` is free (the consumer has released it) and is
            // not visible to the consumer until the `write` store below.
            unsafe {
                self.buffer[w].get().write(input[written]);
            }

            self.write.store(next, Ordering::Release);
            written += 1;
        }

        written
    }

    /// Downmix interleaved output frames and publish them to a mono analysis
    /// ring with one atomic index update. Returns the number of frames copied.
    pub(crate) fn push_mono_downmix(&self, input: &[f32], channels: usize) -> usize {
        if channels == 0 {
            return 0;
        }

        let requested_frames = input.len() / channels;
        if requested_frames == 0 {
            return 0;
        }

        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        let available = if write >= read {
            self.capacity - write + read - 1
        } else {
            read - write - 1
        };
        let frames = requested_frames.min(available);

        for frame_index in 0..frames {
            let frame_start = frame_index * channels;
            let mono = input[frame_start..frame_start + channels]
                .iter()
                .sum::<f32>()
                / channels as f32;
            let index = (write + frame_index) % self.capacity;
            // SAFETY: as in `push`: the slot is free and unpublished.
            unsafe {
                self.buffer[index].get().write(mono);
            }
        }

        if frames > 0 {
            self.write
                .store((write + frames) % self.capacity, Ordering::Release);
        }

        frames
    }

    /// Pop samples from the buffer.
    ///
    /// Returns the number of samples read.
    pub fn pop(&self, output: &mut [f32]) -> usize {
        let mut read_count = 0;

        while read_count < output.len() {
            let r = self.read.load(Ordering::Relaxed);
            let w = self.write.load(Ordering::Acquire);

            if r == w {
                break;
            }

            // SAFETY: slot `r` was published by the producer's Release store
            // and is not reused until the `read` store below.
            output[read_count] = unsafe { self.buffer[r].get().read() };

            self.read.store((r + 1) % self.capacity, Ordering::Release);

            read_count += 1;
        }

        read_count
    }

    /// Remove everything.
    pub fn clear(&self) {
        let w = self.write.load(Ordering::Acquire);
        self.read.store(w, Ordering::Release);
    }

    /// Remaining free space.
    pub fn available_space(&self) -> usize {
        self.capacity - 1 - self.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_empty() {
        let rb = AudioRingBuffer::new(8);

        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
        assert_eq!(rb.available_space(), 7);
    }

    #[test]
    fn push_and_pop_preserves_order() {
        let rb = AudioRingBuffer::new(8);

        let input = [1.0, 2.0, 3.0, 4.0];

        assert_eq!(rb.push(&input), 4);

        let mut out = [0.0; 4];

        assert_eq!(rb.pop(&mut out), 4);

        assert_eq!(input, out);
    }

    #[test]
    fn mono_downmix_publishes_interleaved_frames_in_order() {
        let rb = AudioRingBuffer::new(8);
        let stereo = [0.25, 0.75, -0.25, -0.75];

        assert_eq!(rb.push_mono_downmix(&stereo, 2), 2);
        let mut output = [0.0; 2];
        assert_eq!(rb.pop(&mut output), 2);
        assert_eq!(output, [0.5, -0.5]);
    }

    #[test]
    fn mono_downmix_drops_frames_when_the_analysis_ring_is_full() {
        let rb = AudioRingBuffer::new(4);
        let stereo = [0.1, 0.3, 0.2, 0.4, 0.3, 0.5, 0.4, 0.6];

        assert_eq!(rb.push_mono_downmix(&stereo, 2), 3);
        assert_eq!(rb.len(), 3);
    }

    #[test]
    fn wraparound_works() {
        let rb = AudioRingBuffer::new(8);

        let first = [1.0, 2.0, 3.0, 4.0];
        rb.push(&first);

        let mut temp = [0.0; 2];
        rb.pop(&mut temp);

        let second = [5.0, 6.0, 7.0];
        rb.push(&second);

        let mut out = [0.0; 5];
        rb.pop(&mut out);

        assert_eq!(out, [3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn clear_empties_buffer() {
        let rb = AudioRingBuffer::new(8);

        rb.push(&[1.0, 2.0, 3.0]);

        rb.clear();

        assert!(rb.is_empty());
        assert_eq!(rb.len(), 0);
    }

    #[test]
    fn full_buffer_rejects_extra_samples() {
        let rb = AudioRingBuffer::new(4);

        assert_eq!(rb.push(&[1.0, 2.0, 3.0]), 3);
        assert!(rb.is_full());

        assert_eq!(rb.push(&[4.0]), 0);
    }

    #[test]
    fn order_is_preserved_when_producer_and_consumer_run_concurrently() {
        use std::sync::Arc;

        const TOTAL: usize = 200_000;

        let rb = Arc::new(AudioRingBuffer::new(64));

        let producer_rb = Arc::clone(&rb);
        let producer = std::thread::spawn(move || {
            let mut next = 0usize;
            let mut chunk = [0.0f32; 37];

            while next < TOTAL {
                let n = chunk.len().min(TOTAL - next);

                for (i, slot) in chunk[..n].iter_mut().enumerate() {
                    *slot = (next + i) as f32;
                }

                let mut offset = 0;
                while offset < n {
                    let written = producer_rb.push(&chunk[offset..n]);

                    if written == 0 {
                        std::thread::yield_now();
                    }

                    offset += written;
                }

                next += n;
            }
        });

        let consumer_rb = Arc::clone(&rb);
        let consumer = std::thread::spawn(move || {
            let mut expected = 0usize;
            let mut out = [0.0f32; 53];

            while expected < TOTAL {
                let read = consumer_rb.pop(&mut out);

                if read == 0 {
                    std::thread::yield_now();
                    continue;
                }

                for &value in &out[..read] {
                    assert_eq!(value, expected as f32, "sample out of order");
                    expected += 1;
                }
            }
        });

        producer.join().unwrap();
        consumer.join().unwrap();
    }
}
