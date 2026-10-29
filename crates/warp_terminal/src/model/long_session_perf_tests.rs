//! Per-frame cost of a long Claude Code session in one long-running block, as
//! the transcript grows. The workload is [`InkSession`].
//!
//! The cheap test always runs. The measurement is `#[ignore]`d; run it with:
//!
//! ```text
//! cargo test -p warp_terminal --release --lib long_session -- --ignored --nocapture
//! ```

use std::io;
use std::time::{Duration, Instant};

use crate::SizeInfo;
use crate::event_listener::ChannelEventListener;
use crate::model::ansi::Processor;
use crate::model::blockgrid::BlockGrid;
use crate::model::grid::grid_handler::PerformResetGridChecks;
use crate::model::secrets::ObfuscateSecrets;
use crate::test_util::ink_session::{COLS, InkSession, ROWS};

/// A block grid configured the way the app configures a CLI agent block.
fn agent_block_grid(max_scroll_limit: usize, obfuscate: ObfuscateSecrets) -> BlockGrid {
    let mut grid = BlockGrid::new(
        SizeInfo::new_without_font_metrics(ROWS, COLS),
        max_scroll_limit,
        ChannelEventListener::new_for_test(),
        obfuscate,
        PerformResetGridChecks::default(),
    );
    grid.start();
    grid.enable_full_grid_clear_behavior();
    grid.set_trim_trailing_blank_rows(true);
    grid
}

/// What the renderer reads for one screen of the block: each row of the
/// window starting at `first_row` and every cell in it.
fn viewport_reads(grid: &BlockGrid, first_row: usize) -> usize {
    let handler = grid.grid_handler();
    let mut acc = 0;
    for row_idx in first_row..first_row + ROWS {
        if let Some(row) = handler.row(row_idx) {
            acc += row[..].iter().filter(|cell| cell.c != ' ').count();
        }
    }
    acc
}

/// Work the UI thread does for the active block on each wakeup, independent
/// of rendering: height bookkeeping and emptiness checks.
fn per_wakeup_reads(grid: &BlockGrid) -> usize {
    let mut acc = grid.len_displayed();
    acc += grid.should_show_as_empty_when_finished() as usize;
    acc += grid.has_visible_content() as usize;
    acc
}

struct Sample {
    transcript_lines: usize,
    parse: Duration,
    reads: Duration,
}

fn measure(
    target_lines: &[usize],
    frames: usize,
    max_scroll_limit: usize,
    obfuscate: ObfuscateSecrets,
    sync_output: bool,
) -> Vec<Sample> {
    let mut session = InkSession::new(sync_output);
    let mut grid = agent_block_grid(max_scroll_limit, obfuscate);
    let mut parser = Processor::new();
    let mut sink = io::sink();
    let mut samples = Vec::new();

    for &target in target_lines {
        // Grow the transcript the way a session does: a few lines per frame.
        while session.static_lines < target {
            let bytes = session.frame(4, 20);
            parser.parse_bytes(&mut grid, &bytes, &mut sink);
        }

        // Measure spinner-only frames with a varying dynamic height.
        let mut parse = Duration::ZERO;
        let mut reads = Duration::ZERO;
        let mut acc = 0;
        for i in 0..frames {
            let bytes = session.frame(0, 10 + (i % 30));
            let start = Instant::now();
            parser.parse_bytes(&mut grid, &bytes, &mut sink);
            parse += start.elapsed();

            let start = Instant::now();
            acc += per_wakeup_reads(&grid);
            reads += start.elapsed();
        }
        std::hint::black_box(acc);
        samples.push(Sample {
            transcript_lines: target,
            parse: parse / frames as u32,
            reads: reads / frames as u32,
        });
    }
    samples
}

#[test]
fn long_session_workload_smoke() {
    let mut session = InkSession::new(true);
    let mut grid = agent_block_grid(1000, ObfuscateSecrets::No);
    let mut parser = Processor::new();
    for _ in 0..200 {
        let bytes = session.frame(3, 20);
        parser.parse_bytes(&mut grid, &bytes, &mut io::sink());
    }
    let bytes = session.full_redraw(20);
    parser.parse_bytes(&mut grid, &bytes, &mut io::sink());
    assert_eq!(session.static_lines, 600);
    assert!(grid.len_displayed() >= 600);
    assert!(grid.has_visible_content());
}

#[test]
#[ignore = "perf measurement; run with --release -- --ignored --nocapture"]
fn long_session_per_frame_cost() {
    let targets = [1_000, 5_000, 20_000];
    let frames = 300;
    for (label, limit, obfuscate) in [
        ("limit=5000 redaction=off", 5_000, ObfuscateSecrets::No),
        ("limit=100000 redaction=off", 100_000, ObfuscateSecrets::No),
        (
            "limit=5000 redaction=strikethrough",
            5_000,
            ObfuscateSecrets::Strikethrough,
        ),
    ] {
        println!("== {label} ==");
        println!(
            "{:>10} {:>14} {:>14}",
            "lines", "parse us/frm", "reads us/frm"
        );
        for s in measure(&targets, frames, limit, obfuscate, true) {
            println!(
                "{:>10} {:>14.1} {:>14.1}",
                s.transcript_lines,
                s.parse.as_secs_f64() * 1e6,
                s.reads.as_secs_f64() * 1e6
            );
        }
    }

    for limit in [5_000, 100_000] {
        println!("== full redraw (2J 3J H + replay), limit={limit} ==");
        for target in [1_000, 5_000, 20_000] {
            let mut session = InkSession::new(true);
            let mut grid = agent_block_grid(limit, ObfuscateSecrets::No);
            let mut parser = Processor::new();
            while session.static_lines < target {
                let bytes = session.frame(4, 20);
                parser.parse_bytes(&mut grid, &bytes, &mut io::sink());
            }
            let bytes = session.full_redraw(20);
            let start = Instant::now();
            parser.parse_bytes(&mut grid, &bytes, &mut io::sink());
            println!(
                "{:>10} lines: {:>8.2} ms ({} KiB)",
                target,
                start.elapsed().as_secs_f64() * 1e3,
                bytes.len() / 1024
            );
        }
    }
}

#[test]
#[ignore = "perf measurement; run with --release -- --ignored --nocapture"]
fn long_session_viewport_read_cost() {
    println!("== viewport reads (one screen of rows), limit=5000 ==");
    println!(
        "{:>10} {:>10} {:>10} {:>10}",
        "lines", "top us", "middle us", "bottom us"
    );
    for target in [1_000, 5_000, 20_000] {
        let mut session = InkSession::new(true);
        let mut grid = agent_block_grid(5_000, ObfuscateSecrets::No);
        let mut parser = Processor::new();
        while session.static_lines < target {
            let bytes = session.frame(4, 20);
            parser.parse_bytes(&mut grid, &bytes, &mut io::sink());
        }
        let len = grid.len_displayed();
        let mut cols = Vec::new();
        for first_row in [0, len / 2, len.saturating_sub(ROWS)] {
            let iterations = 200;
            let mut acc = 0;
            let start = Instant::now();
            for _ in 0..iterations {
                acc += viewport_reads(&grid, first_row);
            }
            std::hint::black_box(acc);
            cols.push(start.elapsed().as_secs_f64() * 1e6 / iterations as f64);
        }
        println!(
            "{:>10} {:>10.1} {:>10.1} {:>10.1}",
            target, cols[0], cols[1], cols[2]
        );
    }
}
