//! Interleaving: packets of several encoders into one file, in time order.
//!
//! Each encoder hands packets over late, by its own delay, but always in its
//! own order. So one queue per track is enough: the earliest packet among the
//! queues' heads can go out once **every** unfinished track has something
//! queued, because nothing earlier can still arrive. That is libobs's rule in
//! `obs-output.c`, without its sorted global array (`docs/references/
//! audio-encoding.md` §2–3).
//!
//! Packets are lent by encoders, so queuing one copies it — into a buffer from a
//! pool that it returns to once written. In steady state nothing is allocated.

use std::collections::VecDeque;

use voltra_core::{Error, Result, Timestamp};

#[derive(Debug)]
struct Queued {
    pts: Timestamp,
    keyframe: bool,
    data: Vec<u8>,
}

/// Orders packets from several tracks by time.
///
/// # Examples
///
/// ```
/// use voltra_core::Timestamp;
/// use voltra_output::Interleaver;
///
/// let mut interleaver = Interleaver::new(2);
/// let at = Timestamp::from_millis;
/// interleaver.push(0, at(0), true, b"video 0")?;
/// interleaver.push(0, at(33), false, b"video 1")?;
/// let mut written = Vec::new();
/// // Nothing is safe to write until the audio track has something too.
/// interleaver.drain(|_, pts, _, _| { written.push(pts); Ok(()) })?;
/// assert!(written.is_empty());
///
/// interleaver.push(1, at(10), true, b"audio 0")?;
/// interleaver.drain(|_, pts, _, _| { written.push(pts); Ok(()) })?;
/// assert_eq!(written, [at(0), at(10)]);
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct Interleaver {
    queues: Vec<VecDeque<Queued>>,
    finished: Vec<bool>,
    pool: Vec<Vec<u8>>,
    queued: usize,
    peak: usize,
}

impl Interleaver {
    /// An interleaver for `tracks` tracks, numbered from 0.
    #[must_use]
    pub fn new(tracks: usize) -> Self {
        Self {
            queues: (0..tracks).map(|_| VecDeque::new()).collect(),
            finished: vec![false; tracks],
            pool: Vec::new(),
            queued: 0,
            peak: 0,
        }
    }

    /// Queue a packet of `track`, copying its bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an unknown or finished track, or a packet earlier
    /// than the track's previous one.
    pub fn push(
        &mut self,
        track: usize,
        pts: Timestamp,
        keyframe: bool,
        data: &[u8],
    ) -> Result<()> {
        if self.finished.get(track).copied().unwrap_or(true) {
            return Err(Error::config(format!(
                "track {track} is unknown or finished"
            )));
        }
        let Some(queue) = self.queues.get_mut(track) else {
            return Err(Error::config(format!("no track {track}")));
        };
        if queue.back().is_some_and(|last| pts < last.pts) {
            return Err(Error::config(format!(
                "track {track} went back in time to {pts}"
            )));
        }
        let mut buffer = self.pool.pop().unwrap_or_default();
        buffer.clear();
        buffer.extend_from_slice(data);
        queue.push_back(Queued {
            pts,
            keyframe,
            data: buffer,
        });
        self.queued += 1;
        self.peak = self.peak.max(self.queued);
        Ok(())
    }

    /// Declare that `track` will send nothing more, so it no longer holds the
    /// others back.
    pub fn finish(&mut self, track: usize) {
        if let Some(finished) = self.finished.get_mut(track) {
            *finished = true;
        }
    }

    /// Hand every packet that is safe to write to `write`, earliest first, as
    /// `(track, pts, keyframe, data)`. On a tie, the lower track goes first.
    ///
    /// # Errors
    ///
    /// Whatever `write` returns; the packet it refused is dropped.
    pub fn drain(
        &mut self,
        mut write: impl FnMut(usize, Timestamp, bool, &[u8]) -> Result<()>,
    ) -> Result<()> {
        loop {
            let waiting = self
                .queues
                .iter()
                .zip(&self.finished)
                .any(|(queue, &finished)| queue.is_empty() && !finished);
            if waiting {
                return Ok(());
            }
            let next = self
                .queues
                .iter()
                .enumerate()
                .filter_map(|(track, queue)| queue.front().map(|head| (head.pts, track)))
                .min();
            let Some((_, track)) = next else {
                return Ok(());
            };
            let Some(packet) = self.queues.get_mut(track).and_then(VecDeque::pop_front) else {
                return Ok(());
            };
            self.queued -= 1;
            let result = write(track, packet.pts, packet.keyframe, &packet.data);
            self.pool.push(packet.data);
            result?;
        }
    }

    /// Packets queued now.
    #[must_use]
    pub const fn queued(&self) -> usize {
        self.queued
    }

    /// The most packets ever queued at once: how far apart the encoders'
    /// delays are.
    #[must_use]
    pub const fn peak(&self) -> usize {
        self.peak
    }
}

#[cfg(test)]
mod tests {
    use voltra_core::Timestamp;

    use super::Interleaver;

    fn at(ms: u64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    fn drain(interleaver: &mut Interleaver) -> Vec<(usize, u64)> {
        let mut out = Vec::new();
        interleaver
            .drain(|track, pts, _, _| {
                out.push((track, pts.as_nanos() / 1_000_000));
                Ok(())
            })
            .unwrap();
        out
    }

    #[test]
    fn two_delayed_tracks_come_out_in_global_order() {
        let mut interleaver = Interleaver::new(2);
        let mut out = Vec::new();
        // Video at 0, 33, 66, 100 arrives two frames late; audio at 0, 21, …
        // arrives one packet late. Push in arrival order and drain each time.
        let video = [0, 33, 66, 100, 133];
        let audio = [0, 21, 43, 64, 85, 107, 128];
        let (mut v, mut a) = (0, 0);
        while v < video.len() || a < audio.len() {
            if v < video.len() {
                interleaver.push(0, at(video[v]), v == 0, &[0]).unwrap();
                v += 1;
            }
            for _ in 0..2 {
                if a < audio.len() {
                    interleaver.push(1, at(audio[a]), true, &[1]).unwrap();
                    a += 1;
                }
            }
            out.extend(drain(&mut interleaver));
        }
        interleaver.finish(0);
        interleaver.finish(1);
        out.extend(drain(&mut interleaver));
        let times: Vec<u64> = out.iter().map(|&(_, ms)| ms).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable();
        assert_eq!(times, sorted, "{out:?}");
        assert_eq!(out.len(), video.len() + audio.len(), "nothing left behind");
        assert_eq!(interleaver.queued(), 0);
    }

    #[test]
    fn a_finished_track_does_not_hold_the_other_back() {
        let mut interleaver = Interleaver::new(2);
        interleaver.push(0, at(0), true, &[0]).unwrap();
        interleaver.push(1, at(5), true, &[1]).unwrap();
        assert_eq!(drain(&mut interleaver), [(0, 0)]);
        interleaver.finish(0);
        interleaver.push(1, at(10), true, &[1]).unwrap();
        assert_eq!(drain(&mut interleaver), [(1, 5), (1, 10)]);
    }

    #[test]
    fn ties_go_to_the_lower_track() {
        let mut interleaver = Interleaver::new(2);
        interleaver.push(1, at(1_000), true, &[1]).unwrap();
        interleaver.push(0, at(1_000), true, &[0]).unwrap();
        interleaver.finish(0);
        interleaver.finish(1);
        assert_eq!(drain(&mut interleaver), [(0, 1_000), (1, 1_000)]);
    }

    #[test]
    fn buffers_come_back_to_the_pool() {
        let mut interleaver = Interleaver::new(1);
        for n in 0..100 {
            interleaver.push(0, at(n), true, &[0; 4096]).unwrap();
            drain(&mut interleaver);
        }
        assert_eq!(
            interleaver.pool.len(),
            1,
            "one buffer, reused a hundred times"
        );
        assert_eq!(interleaver.peak(), 1);
    }

    #[test]
    fn bad_pushes_are_refused() {
        let mut interleaver = Interleaver::new(1);
        assert!(interleaver.push(3, at(0), true, &[]).is_err());
        interleaver.push(0, at(10), true, &[]).unwrap();
        assert!(
            interleaver.push(0, at(9), true, &[]).is_err(),
            "back in time"
        );
        interleaver.finish(0);
        assert!(interleaver.push(0, at(20), true, &[]).is_err(), "finished");
    }
}
