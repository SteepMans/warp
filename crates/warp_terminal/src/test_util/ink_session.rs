//! Byte stream of a synthetic Claude Code session for performance tests.
//!
//! Static transcript output is appended above a dynamic region (spinner,
//! status line, input box) that is erased and redrawn on every frame with
//! `ESC[2K` / `ESC[1A` / `ESC[G`, wrapped in synchronized output
//! (`ESC[?2026h` .. `ESC[?2026l`). This is the Ink log-update pattern. Claude
//! Code 2.x diffs cells instead and writes fewer bytes per frame, so this is an
//! upper bound for the per-frame work. [`InkSession::full_redraw`] is the worst
//! case of a reset: `ESC[2J ESC[3J ESC[H` and a replay of the whole transcript.

pub const ROWS: usize = 50;
pub const COLS: usize = 120;

/// Generates the byte stream of a synthetic Claude Code session.
pub struct InkSession {
    prev_dynamic_height: usize,
    pub static_lines: usize,
    frame: usize,
    sync_output: bool,
}

impl InkSession {
    pub fn new(sync_output: bool) -> Self {
        Self {
            prev_dynamic_height: 0,
            static_lines: 0,
            frame: 0,
            sync_output,
        }
    }

    /// A transcript line: markdown-ish text, diffs, box drawing, with SGR colors.
    pub fn static_line(i: usize) -> String {
        match i % 12 {
            0 => format!("\x1b[1m\x1b[38;5;174m●\x1b[39m\x1b[22m Read(src/module_{i}.rs)"),
            1 => format!("  \x1b[2m⎿\x1b[22m  Read \x1b[1m{}\x1b[22m lines", i % 300),
            2 => format!(
                "\x1b[48;2;34;92;43m\x1b[38;2;255;255;255m{:>5} +    let value_{i} = compute(&state, {i});\x1b[39m\x1b[49m",
                i % 1000
            ),
            3 => format!(
                "\x1b[48;2;122;41;54m\x1b[38;2;255;255;255m{:>5} -    let value_{i} = compute_old(&state);\x1b[39m\x1b[49m",
                i % 1000
            ),
            4 => "╭──────────────────────────────────────────────────────────────╮".to_owned(),
            5 => format!(
                "│ \x1b[36mfn\x1b[39m example_{i}() -> \x1b[33mResult\x1b[39m<()> {{ … }}{:>20}│",
                ""
            ),
            6 => "╰──────────────────────────────────────────────────────────────╯".to_owned(),
            7 => format!(
                "Here is an explanation of step {i}: the \x1b[1mgrid\x1b[22m stores rows in flat storage once they scroll off, and the renderer reads only the visible window."
            ),
            8 => format!(
                "  - item {i}: \x1b[3mitalic\x1b[23m and `code_{i}` and https://example.com/path/{i}"
            ),
            9 => String::new(),
            10 => format!("\x1b[38;5;246m> user prompt number {i} with some typing\x1b[39m"),
            _ => format!("\x1b[32m✓\x1b[39m Tests passed ({i} total)"),
        }
    }

    fn dynamic_frame(&self, height: usize) -> Vec<String> {
        const SPINNER: [&str; 6] = ["·", "✢", "✳", "∗", "✻", "✽"];
        let mut lines = Vec::with_capacity(height);
        lines.push(format!(
            "\x1b[38;5;174m{}\x1b[39m Thinking… \x1b[2m({}s · ↑ {} tokens · esc to interrupt)\x1b[22m",
            SPINNER[self.frame % SPINNER.len()],
            self.frame / 10,
            self.frame * 7
        ));
        lines.push(String::new());
        let box_inner = height.saturating_sub(5).max(1);
        lines.push(format!("\x1b[2m╭{}╮\x1b[22m", "─".repeat(COLS - 4)));
        for i in 0..box_inner {
            let text = if i == 0 { "> typing a long prompt" } else { "" };
            lines.push(format!(
                "\x1b[2m│\x1b[22m {text:<width$}\x1b[2m│\x1b[22m",
                width = COLS - 5
            ));
        }
        lines.push(format!("\x1b[2m╰{}╯\x1b[22m", "─".repeat(COLS - 4)));
        lines.push("  \x1b[2m? for shortcuts\x1b[22m                       \x1b[38;5;174m⏵⏵ auto-accept edits on\x1b[39m".to_owned());
        lines.truncate(height);
        lines
    }

    fn erase_previous(&self, out: &mut String) {
        // Mirrors ansi-escapes `eraseLines(n)` used by Ink's log-update.
        for i in 0..self.prev_dynamic_height {
            out.push_str("\x1b[2K");
            if i + 1 < self.prev_dynamic_height {
                out.push_str("\x1b[1A");
            }
        }
        if self.prev_dynamic_height > 0 {
            out.push_str("\x1b[G");
        }
    }

    /// One Ink frame: erase the dynamic region, append `new_static` transcript
    /// lines, redraw a dynamic region of `dynamic_height` lines.
    pub fn frame(&mut self, new_static: usize, dynamic_height: usize) -> Vec<u8> {
        let mut out = String::new();
        if self.sync_output {
            out.push_str("\x1b[?2026h");
        }
        self.erase_previous(&mut out);
        for _ in 0..new_static {
            out.push_str(&Self::static_line(self.static_lines));
            out.push_str("\r\n");
            self.static_lines += 1;
        }
        let lines = self.dynamic_frame(dynamic_height);
        out.push_str(&lines.join("\r\n"));
        if self.sync_output {
            out.push_str("\x1b[?2026l");
        }
        self.prev_dynamic_height = lines.len();
        self.frame += 1;
        out.into_bytes()
    }

    /// Full clear of screen and scrollback followed by a replay of the whole
    /// transcript and the dynamic region.
    pub fn full_redraw(&mut self, dynamic_height: usize) -> Vec<u8> {
        let mut out = String::new();
        if self.sync_output {
            out.push_str("\x1b[?2026h");
        }
        out.push_str("\x1b[2J\x1b[3J\x1b[H");
        for i in 0..self.static_lines {
            out.push_str(&Self::static_line(i));
            out.push_str("\r\n");
        }
        let lines = self.dynamic_frame(dynamic_height);
        out.push_str(&lines.join("\r\n"));
        if self.sync_output {
            out.push_str("\x1b[?2026l");
        }
        self.prev_dynamic_height = lines.len();
        self.frame += 1;
        out.into_bytes()
    }
}
