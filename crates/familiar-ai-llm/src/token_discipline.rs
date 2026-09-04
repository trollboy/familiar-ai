//! PRD-072 bounded tool-result windowing: the wire-level shape of what a
//! tool result looks like once it is too large to hand the model whole.
//!
//! This module is pure and storage-agnostic — it never truncates the
//! record the host retains, only computes the head+tail view (with an
//! elided-line count and an optional paging handle) that becomes the
//! [`familiar_ai_llm::attempt::ToolResultPayload`] content. The host
//! executor is responsible for retaining the full, untruncated result
//! elsewhere (durable, but never inside a PRD-051 accounting row) and for
//! making the paging handle resolvable — this module only decides what the
//! model sees and reports how much was left out.

/// Line-count thresholds a tool result is bounded against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolResultWindow {
    /// Total line count at or below which a result passes through whole.
    pub max_lines: usize,
    /// Lines kept from the start of the result once bounded.
    pub head_lines: usize,
    /// Lines kept from the end of the result once bounded.
    pub tail_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedToolResult {
    /// What the model sees this turn.
    pub visible: String,
    pub truncated: bool,
    pub elided_lines: usize,
    /// Present only when `truncated` and the host supplied a handle the
    /// elided region can be retrieved through.
    pub paging_handle: Option<String>,
}

/// Bounds `full` to `window`, passing it through unchanged when it already
/// fits. `paging_handle`, when given, is echoed into the visible text and
/// the returned struct so the model can retrieve the elided region — the
/// caller (the host executor) is responsible for making that handle
/// resolvable and for retaining `full` durably regardless of the outcome.
pub fn bound_tool_result(
    full: &str,
    window: &ToolResultWindow,
    paging_handle: Option<String>,
) -> BoundedToolResult {
    let lines: Vec<&str> = full.lines().collect();
    if lines.len() <= window.max_lines {
        return BoundedToolResult {
            visible: full.to_string(),
            truncated: false,
            elided_lines: 0,
            paging_handle: None,
        };
    }
    let head_count = window.head_lines.min(lines.len());
    let tail_count = window.tail_lines.min(lines.len() - head_count);
    let head = &lines[..head_count];
    let tail_start = lines.len() - tail_count;
    let tail = &lines[tail_start..];
    let elided_lines = lines.len() - head_count - tail_count;

    let mut visible = head.join("\n");
    visible.push('\n');
    visible.push_str(&match &paging_handle {
        Some(handle) => format!(
            "... [{elided_lines} lines elided; full output retrievable via read-file at {handle}] ..."
        ),
        None => format!("... [{elided_lines} lines elided] ..."),
    });
    visible.push('\n');
    visible.push_str(&tail.join("\n"));

    BoundedToolResult {
        visible,
        truncated: true,
        elided_lines,
        paging_handle,
    }
}

/// Whether a file read may proceed with the whole file, or must be re-issued
/// with an explicit line range because the file exceeds the configured
/// span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReadRequirement {
    FullFileAllowed,
    ExplicitRangeRequired {
        total_lines: usize,
        max_lines: usize,
    },
}

/// A file at or under `max_lines` (or a call that already supplied an
/// explicit range) may read whole; otherwise the caller must specify one.
pub fn file_read_requirement(
    total_lines: usize,
    max_lines: usize,
    has_explicit_range: bool,
) -> FileReadRequirement {
    if has_explicit_range || total_lines <= max_lines {
        FileReadRequirement::FullFileAllowed
    } else {
        FileReadRequirement::ExplicitRangeRequired {
            total_lines,
            max_lines,
        }
    }
}

/// Extracts the 1-indexed, inclusive `[start_line, end_line]` span from
/// `full`. Out-of-range bounds clamp rather than panic.
pub fn slice_lines(full: &str, start_line: usize, end_line: usize) -> String {
    let start = start_line.max(1);
    let end = end_line.max(start);
    full.lines()
        .skip(start - 1)
        .take(end - start + 1)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> ToolResultWindow {
        ToolResultWindow {
            max_lines: 10,
            head_lines: 4,
            tail_lines: 4,
        }
    }

    #[test]
    fn result_within_window_passes_through_unbounded() {
        let full = (1..=10)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let bounded = bound_tool_result(&full, &window(), Some("handle".into()));
        assert!(!bounded.truncated);
        assert_eq!(bounded.elided_lines, 0);
        assert_eq!(bounded.paging_handle, None);
        assert_eq!(bounded.visible, full);
    }

    #[test]
    fn result_beyond_window_shows_head_tail_and_elided_count() {
        let full = (1..=100)
            .map(|n| format!("line{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let bounded = bound_tool_result(
            &full,
            &window(),
            Some(".familiar/tool-output/c1.txt".into()),
        );
        assert!(bounded.truncated);
        assert_eq!(bounded.elided_lines, 92);
        assert_eq!(
            bounded.paging_handle.as_deref(),
            Some(".familiar/tool-output/c1.txt")
        );
        assert!(bounded.visible.contains("line1\nline2\nline3\nline4"));
        assert!(bounded.visible.contains("line97\nline98\nline99\nline100"));
        assert!(bounded.visible.contains("92 lines elided"));
        assert!(bounded.visible.contains(".familiar/tool-output/c1.txt"));
    }

    #[test]
    fn file_read_requires_explicit_range_beyond_span() {
        assert_eq!(
            file_read_requirement(5_000, 2_000, false),
            FileReadRequirement::ExplicitRangeRequired {
                total_lines: 5_000,
                max_lines: 2_000
            }
        );
        assert_eq!(
            file_read_requirement(5_000, 2_000, true),
            FileReadRequirement::FullFileAllowed
        );
        assert_eq!(
            file_read_requirement(100, 2_000, false),
            FileReadRequirement::FullFileAllowed
        );
    }

    #[test]
    fn slice_lines_extracts_inclusive_one_indexed_range() {
        let full = "a\nb\nc\nd\ne";
        assert_eq!(slice_lines(full, 2, 4), "b\nc\nd");
        assert_eq!(slice_lines(full, 1, 1), "a");
        assert_eq!(slice_lines(full, 4, 100), "d\ne");
    }
}
