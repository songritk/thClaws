//! Format thClaws output for LINE.
//!
//! LINE rendering constraints (plan-07):
//! - Single text message capped at 5_000 characters
//! - No formatting (markdown, code fences, headings render as
//!   literal text — fine, but extra-noisy compared to the GUI)
//! - No ANSI rendering — LINE strips the ESC byte but leaves the
//!   `[2m…` bracket payload visible as literal characters, which
//!   looks broken. Strip ANSI before sending.
//! - One reply per webhook; we send the **final** assistant text
//!   only, hiding intermediate `assistant` chunks between tool
//!   calls and any `thinking` blocks
//!
//! `filter_for_line` runs three passes:
//!   1. Strip ANSI CSI / OSC escape sequences (`\x1b[…m`). LINE
//!      preserves the ESC byte but renders the bracket payload as
//!      literal characters — looks like `[2m[tool: …]0m`. We
//!      DON'T try to strip ESC-less bracket payloads because they
//!      collide with user-typed prose about ANSI codes.
//!   2. Drop lines that look like tool-call narration (`⏺ …`,
//!      `[tool: …]`, lone `✓` markers).
//!   3. Trim surrounding whitespace + truncate at `TRUNCATE_AT`.

/// Hard ceiling — LINE rejects single text messages above this.
pub const LINE_MAX_CHARS: usize = 5_000;

/// We truncate well below the ceiling so the appended notice
/// always fits. `4_500 + notice` lands comfortably under 5_000.
pub const TRUNCATE_AT: usize = 4_500;

const NOTICE: &str = "\n\n…[response truncated — open thClaws to read in full]";

/// Full filter pipeline: ANSI strip → tool-narration strip → trim
/// → truncate. UTF-8 safe.
pub fn filter_for_line(body: &str) -> String {
    let no_ansi = strip_ansi(body);
    let no_narration = strip_tool_narration(&no_ansi);
    let trimmed = no_narration.trim();
    if trimmed.chars().count() <= TRUNCATE_AT {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(TRUNCATE_AT).collect();
    format!("{head}{NOTICE}")
}

/// ANSI strip + tool-narration strip, no truncate, no trim. Suitable
/// for per-chunk streaming text (plan-10 browser chat fan-out) where
/// each `AssistantTextDelta` is run through the cleaner before it
/// reaches the browser. UTF-8 safe.
///
/// Caveat: an ANSI escape sequence split across two delta chunks
/// would leak partial garbage. In practice models emit the whole
/// `\x1b[…m` in one token block, so this is rare. Tracked as a
/// known limitation.
pub fn clean_for_stream(chunk: &str) -> String {
    strip_tool_narration(&strip_ansi(chunk))
}

/// Drop CSI (`\x1b[…m`) and OSC (`\x1b]…\x07`) escape sequences.
/// Also handles the "ESC byte was stripped upstream" case where
/// only the `[<digits>m` tail remains visible — common when the
/// model echoes terminal output from a tool result.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        // Real CSI: ESC [ … final-byte-in-0x40..0x7E
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for nc in chars.by_ref() {
                    // Final byte ends the sequence; everything
                    // between is parameters / intermediates.
                    if ('@'..='~').contains(&nc) {
                        break;
                    }
                }
                continue;
            }
            if chars.peek() == Some(&']') {
                // OSC ends at BEL (0x07) or ST (`\x1b\\`). We
                // consume up to BEL — rarely matters in agent
                // output but keeps the strip robust.
                chars.next();
                for nc in chars.by_ref() {
                    if nc == '\x07' {
                        break;
                    }
                }
                continue;
            }
            // Unknown ESC sequence — drop the ESC, keep the rest
            // so we don't lose user-visible content.
            continue;
        }
        out.push(c);
    }
    out
}

/// Drop lines that are entirely tool-call narration. Conservative
/// patterns: only matches lines whose visible content is
/// recognisably structural, not user-visible answer text.
///
/// Tool-bullet lead characters the model sometimes echoes back
/// (because they're visible in the GUI Chat tab's rendered tool
/// indicator and the model has seen them in conversation history):
/// - `⏺` BLACK CIRCLE FOR RECORD (terminal-style)
/// - `🔧` WRENCH (chat-style — what frontend ChatView renders)
/// - `🛠️` HAMMER AND WRENCH (alternate chat tool icon)
/// - `🔨` HAMMER
///
/// Other tool-narration signals:
/// - `[tool: NAME …]` bracket marker (terminal-rendered indicator)
/// - Lone `✓` lines (tool-success checkmarks)
fn strip_tool_narration(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.split('\n') {
        let trimmed = line.trim_start();
        let starts_with_bullet = trimmed.starts_with('⏺')
            || trimmed.starts_with('🔧')
            || trimmed.starts_with("🛠️")
            || trimmed.starts_with('🛠')
            || trimmed.starts_with('🔨');
        let starts_with_tool_bracket =
            trimmed.starts_with("[tool:") || trimmed.starts_with("[tool ");
        let is_lone_check = trimmed == "✓" || trimmed.starts_with("✓ ×");
        if starts_with_bullet || starts_with_tool_bracket || is_lone_check {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_short() {
        assert_eq!(filter_for_line("hello"), "hello");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(filter_for_line("\n  hello\n\n"), "hello");
    }

    #[test]
    fn truncates_long_text_with_notice() {
        let body = "x".repeat(6_000);
        let out = filter_for_line(&body);
        assert!(out.ends_with("open thClaws to read in full]"));
        assert!(out.chars().count() < LINE_MAX_CHARS);
        assert!(out.chars().count() > TRUNCATE_AT);
    }

    #[test]
    fn strips_real_ansi_csi_sequences() {
        let input = "\x1b[2m[tool: Read /tmp/x]\x1b[0m\nThe answer is 42.";
        assert_eq!(filter_for_line(input), "The answer is 42.");
    }

    #[test]
    fn drops_tool_bullet_lines() {
        let input = "⏺ Read(/path/to/x.rs)\nThe file contains foo.";
        assert_eq!(filter_for_line(input), "The file contains foo.");
    }

    #[test]
    fn drops_wrench_emoji_tool_narration() {
        // Caught from a real LINE screenshot: the model echoed the
        // chat UI's `🔧 [Bash]` indicator into its text reply.
        let input = "🔧 [Bash]\nDone.";
        assert_eq!(filter_for_line(input), "Done.");
    }

    #[test]
    fn drops_multiple_tool_narration_lines() {
        // Two Bash calls in a turn — both lines should be stripped.
        let input = "🔧 [Bash]\n🔧 [Bash]\nลบแล้ว";
        assert_eq!(filter_for_line(input), "ลบแล้ว");
    }

    #[test]
    fn drops_lone_check_lines() {
        let input = "Looking at the code…\n✓\nIt all checks out.";
        // ✓ line is dropped; surrounding non-empty lines stay
        // adjacent (single `\n` between them after the drop).
        assert_eq!(
            filter_for_line(input),
            "Looking at the code…\nIt all checks out."
        );
    }

    #[test]
    fn preserves_user_typed_bracket_notation() {
        // ANSI strip only runs on real ESC sequences, so prose
        // about ANSI codes survives intact.
        let input = "The escape sequence is [2m] — read more here.";
        assert_eq!(
            filter_for_line(input),
            "The escape sequence is [2m] — read more here."
        );
    }

    #[test]
    fn ansi_strip_keeps_unicode_intact() {
        // Thai text inside an ANSI-styled run should survive.
        let input = "\x1b[33mสวัสดีครับ\x1b[0m — done.";
        assert_eq!(filter_for_line(input), "สวัสดีครับ — done.");
    }

    #[test]
    fn truncation_is_char_boundary_safe_for_thai() {
        // Thai chars are 3 bytes UTF-8; byte-truncating would
        // either panic on `.to_string()` of an invalid slice or
        // produce mojibake. `chars().take` makes the result valid.
        let body = "ก".repeat(5_000);
        let out = filter_for_line(&body);
        assert!(out.ends_with("open thClaws to read in full]"));
        // Round-trip through `is_char_boundary` would be tautological
        // since `String` always ends on one — instead check we
        // didn't lose all the Thai chars.
        let thai_count = out.chars().filter(|c| *c == 'ก').count();
        assert_eq!(thai_count, TRUNCATE_AT);
    }
}
