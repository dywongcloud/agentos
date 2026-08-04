//! Measures what the playout stall budget actually buys when a group arrives incomplete.
//!
//! `PLAYOUT_MAX_LATENCY` (60ms, `ios-bridge/src/lib.rs`) replaced the library's 150ms default.
//! The 60ms was DERIVED rather than guessed -- from the measured 64-172ms cost of resuming at a
//! group boundary, on the rule that tolerating a stall longer than skipping costs is strictly
//! worse than skipping. What that derivation never showed is the BENEFIT, because the budget only
//! does anything when a group is INCOMPLETE, and a lossless loopback never produces one.
//!
//! So this builds the incomplete group directly. No network, no packet loss to simulate: a
//! moq-lite track is produced in-process, one group is deliberately left open and blocking, newer
//! groups are appended past it, and moq-mux's ordered `Consumer` -- the same type the real
//! subscribe path uses -- is driven at each budget to see how long it withholds frames before
//! skipping ahead.
//!
//! One detail worth stating, because it is easy to get wrong: the skip rule compares MEDIA
//! timestamps, not wall-clock waiting (`max_timestamp.saturating_sub(oldest) >= self.latency` in
//! moq-mux's consumer). Media advances with wall clock on a live stream, so the two track each
//! other, but this probe drives the producer in real time rather than dumping frames instantly --
//! otherwise the skip would fire immediately and the wall-clock number would be meaningless.

use std::time::{Duration, Instant};

use moq_mux::container::{Consumer, Container, Frame, loc};
use tokio_util::bytes::Bytes;

/// How far apart the synthetic frames are in media time, matching the ~28fps the real pipeline
/// delivers so the timestamps this measures against are the ones production would produce.
const FRAME_INTERVAL: Duration = Duration::from_millis(36);
const FRAMES_PER_GROUP: usize = 8;

fn frame_at(timestamp: Duration, keyframe: bool) -> Frame {
    Frame {
        timestamp: moq_mux::container::Timestamp::from_micros(timestamp.as_micros() as u64)
            .expect("probe timestamps are small and cannot overflow the timescale"),
        payload: Bytes::from_static(&[0u8; 64]),
        keyframe,
    }
}

/// Drives one budget and returns how long the consumer withheld frames before delivering past the
/// blocked group.
async fn stall_for(budget: Duration) -> anyhow::Result<Duration> {
    let track = moq_net::Track {
        name: "video".to_string(),
        priority: 0,
    }
    .produce();
    let mut producer = track;
    let consumer_track = producer.consume();
    let mut consumer = Consumer::new(consumer_track, loc::Wire).with_latency(budget);

    let format = loc::Wire;
    let mut pts = Duration::ZERO;

    // Group 0: complete and finished. Playout can consume this normally.
    {
        let mut g = producer.append_group()?;
        for i in 0..FRAMES_PER_GROUP {
            format.write(&mut g, &[frame_at(pts, i == 0)])?;
            pts += FRAME_INTERVAL;
        }
        g.finish()?;
    }

    // Group 1: opened with its keyframe and then LEFT OPEN. This is the incomplete group -- the
    // consumer cannot finish it and cannot know more is coming, which is exactly the state a lost
    // packet leaves playout in.
    let blocked_oldest = pts;
    let mut blocked = producer.append_group()?;
    format.write(&mut blocked, &[frame_at(pts, true)])?;
    pts += FRAME_INTERVAL;

    // Drain group 0 AND the one frame group 1 managed to deliver before it stalled. Only after
    // that is the next read genuinely blocked -- reading group 1's available keyframe is normal
    // delivery, not a stall, and measuring it would have reported a skip that never happened.
    for _ in 0..(FRAMES_PER_GROUP + 1) {
        let _ = consumer.read().await?;
    }

    // Newer groups keep arriving in REAL TIME while group 1 stays stuck.
    let producing = tokio::spawn(async move {
        let mut pts = pts;
        for _ in 0..3 {
            let Ok(mut g) = producer.append_group() else {
                return;
            };
            for i in 0..FRAMES_PER_GROUP {
                if format.write(&mut g, &[frame_at(pts, i == 0)]).is_err() {
                    return;
                }
                pts += FRAME_INTERVAL;
                tokio::time::sleep(FRAME_INTERVAL).await;
            }
            let _ = g.finish();
        }
        // Hold the producer so the track never "finishes", which would let the consumer skip for
        // a different reason than the one being measured.
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(blocked);
    });

    let started = Instant::now();
    let next = tokio::time::timeout(Duration::from_secs(5), consumer.read()).await;
    let stalled = started.elapsed();
    producing.abort();

    match next {
        Ok(Ok(Some(frame))) => {
            anyhow::ensure!(
                Duration::from_micros(frame.timestamp.as_micros() as u64) > blocked_oldest,
                "consumer delivered a frame from the blocked group, so nothing was skipped"
            );
            Ok(stalled)
        }
        Ok(Ok(None)) => anyhow::bail!("track ended instead of skipping"),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => anyhow::bail!("consumer never skipped within 5s at a {budget:?} budget"),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    println!(
        "how long playout withholds frames when a group arrives incomplete\n\
         (synthetic blocked group, real-time newer groups, {FRAMES_PER_GROUP} frames per group at \
         {FRAME_INTERVAL:?})"
    );

    let shipped = Duration::from_millis(60);
    let library_default = Duration::from_millis(150);

    let with_shipped = stall_for(shipped).await?;
    println!("  budget {shipped:?} (what ships) -> stalled {with_shipped:.2?}");
    let with_default = stall_for(library_default).await?;
    println!("  budget {library_default:?} (library default) -> stalled {with_default:.2?}");

    anyhow::ensure!(
        with_shipped < with_default,
        "the lower budget did not shorten the stall ({with_shipped:.2?} vs {with_default:.2?}), \
         which would mean the change buys nothing on the one event it exists for"
    );

    println!(
        "\nVERDICT: OK -- on an incomplete group the shipped budget withholds frames for \
         {with_shipped:.2?} where the library default withholds {with_default:.2?}, a \
         {:.2?} shorter freeze per event. That is the benefit the derivation could not show, \
         because it only appears when a group is incomplete.",
        with_default - with_shipped
    );
    Ok(())
}
