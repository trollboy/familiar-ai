//! PRD-072 targeted-edit forms for the `apply-edit` capability.
//!
//! Whole-file replacement (the PRD-058 default) survives unchanged. This
//! module adds two additional edit forms — search/replace blocks and
//! unified-diff hunks — applied atomically against a file's current content
//! by locating an anchor (the pre-edit text) and swapping in the post-edit
//! text. A hunk whose anchor no longer matches is a named
//! [`EditError::AnchorDivergence`], never a silent misapply. If the anchor
//! is absent but the *replacement* text is already present at that anchor's
//! expected shape, the edit is treated as already applied — a resumed loop
//! replaying the identical tool call after a crash reaches the identical
//! file state instead of failing closed on its own prior write.
//!
//! Pure and storage-agnostic: no filesystem access happens here. The host
//! executor (`familiar_ai_daemon::agent_runtime::SandboxedToolExecutor`)
//! reads current content, calls [`resolve_edit`], and writes the result.

/// The three write forms `apply-edit` accepts, keyed by the tool call's
/// optional `change_kind` argument. Absent `change_kind` means
/// [`EditForm::WholeFile`] — the PRD-058 default, byte-for-byte unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditForm {
    #[default]
    WholeFile,
    SearchReplace,
    UnifiedDiff,
}

impl EditForm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WholeFile => "whole-file",
            Self::SearchReplace => "search-replace",
            Self::UnifiedDiff => "unified-diff",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "whole-file" => Some(Self::WholeFile),
            "search-replace" => Some(Self::SearchReplace),
            "unified-diff" => Some(Self::UnifiedDiff),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchReplaceBlock {
    pub search: String,
    pub replace: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// A block's anchor text does not match the current content, and the
    /// replacement text is not already present either — a genuinely stale
    /// hunk, named rather than silently misapplied.
    AnchorDivergence { detail: String },
    /// The payload itself did not parse as the declared edit form.
    MalformedEdit { detail: String },
}

/// Parses the `content` argument as a JSON array of `{"search":...,
/// "replace":...}` objects.
pub fn parse_search_replace_blocks(payload: &str) -> Result<Vec<SearchReplaceBlock>, EditError> {
    let parsed: serde_json::Value =
        serde_json::from_str(payload).map_err(|error| EditError::MalformedEdit {
            detail: format!("search-replace payload is not valid JSON: {error}"),
        })?;
    let serde_json::Value::Array(items) = parsed else {
        return Err(EditError::MalformedEdit {
            detail: "search-replace payload must be a JSON array of blocks".into(),
        });
    };
    if items.is_empty() {
        return Err(EditError::MalformedEdit {
            detail: "search-replace payload contains no blocks".into(),
        });
    }
    items
        .into_iter()
        .map(|item| {
            let search = item
                .get("search")
                .and_then(|v| v.as_str())
                .ok_or_else(|| EditError::MalformedEdit {
                    detail: "search-replace block missing string \"search\"".into(),
                })?
                .to_string();
            let replace = item
                .get("replace")
                .and_then(|v| v.as_str())
                .ok_or_else(|| EditError::MalformedEdit {
                    detail: "search-replace block missing string \"replace\"".into(),
                })?
                .to_string();
            Ok(SearchReplaceBlock { search, replace })
        })
        .collect()
}

/// Parses the `content` argument as unified-diff hunks and derives one
/// anchor/replacement pair per hunk: the anchor is the hunk's context+
/// removed lines, the replacement is its context+added lines. Application
/// is anchor-text matching, the same primitive search/replace blocks use —
/// hunks are a second serialization of the same edit, not a second
/// application strategy.
pub fn parse_unified_diff_blocks(payload: &str) -> Result<Vec<SearchReplaceBlock>, EditError> {
    let mut blocks = Vec::new();
    let mut lines = payload.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("@@") {
            continue;
        }
        let mut anchor_lines: Vec<&str> = Vec::new();
        let mut replacement_lines: Vec<&str> = Vec::new();
        while let Some(next) = lines.peek() {
            if next.starts_with("@@") {
                break;
            }
            let body = lines.next().unwrap();
            if let Some(rest) = body.strip_prefix('-') {
                anchor_lines.push(rest);
            } else if let Some(rest) = body.strip_prefix('+') {
                replacement_lines.push(rest);
            } else if let Some(rest) = body.strip_prefix(' ') {
                anchor_lines.push(rest);
                replacement_lines.push(rest);
            } else if body.is_empty() {
                anchor_lines.push(body);
                replacement_lines.push(body);
            }
        }
        if anchor_lines.is_empty() && replacement_lines.is_empty() {
            return Err(EditError::MalformedEdit {
                detail: "unified-diff hunk has no context, removed, or added lines".into(),
            });
        }
        blocks.push(SearchReplaceBlock {
            search: anchor_lines.join("\n"),
            replace: replacement_lines.join("\n"),
        });
    }
    if blocks.is_empty() {
        return Err(EditError::MalformedEdit {
            detail: "unified-diff payload contains no @@ hunk headers".into(),
        });
    }
    Ok(blocks)
}

/// The result of [`apply_targeted_edit`]: the resolved content plus whether
/// every block in the call was an idempotent replay (its anchor was already
/// gone because the edit had already landed), meaning `content` is
/// byte-identical to the input and no write is actually needed. A caller
/// MUST surface `already_applied` distinctly from a genuine write — never
/// report a replayed no-op the same way as `wrote N bytes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEdit {
    pub content: String,
    pub already_applied: bool,
}

/// Applies every block atomically against `current`: each block's anchor is
/// located and swapped for its replacement in sequence against a working
/// copy. If any block's anchor is missing (and its replacement is not
/// already present, the idempotent-replay case) the whole operation fails
/// and `current` is returned untouched by the caller — never a partial
/// apply. An anchor matching more than once is refused as ambiguous rather
/// than guessing which occurrence the model meant.
///
/// The idempotent-replay check is a content heuristic, not a positional or
/// journal-based one, so it is deliberately conservative: a missing anchor
/// is only treated as "already applied" when its `replace` text occurs
/// **exactly once** in the working copy. A `replace` string that occurs
/// zero times, or more than once (e.g. a common token like `}` that already
/// exists elsewhere in the file for unrelated reasons), cannot be
/// distinguished from a genuinely stale hunk and is refused as
/// `AnchorDivergence` — never silently skipped and reported as a
/// successful write.
pub fn apply_targeted_edit(
    current: &str,
    blocks: &[SearchReplaceBlock],
) -> Result<AppliedEdit, EditError> {
    let mut working = current.to_string();
    let mut wrote_any_block = false;
    for (index, block) in blocks.iter().enumerate() {
        if block.search.is_empty() {
            return Err(EditError::MalformedEdit {
                detail: format!("edit {index}: empty search anchor is not permitted"),
            });
        }
        let occurrences = working.matches(block.search.as_str()).count();
        match occurrences {
            1 => {
                working = working.replacen(block.search.as_str(), &block.replace, 1);
                wrote_any_block = true;
            }
            0 => {
                let replacement_occurrences = if block.replace.is_empty() {
                    0
                } else {
                    working.matches(block.replace.as_str()).count()
                };
                if replacement_occurrences != 1 {
                    return Err(EditError::AnchorDivergence {
                        detail: format!(
                            "edit {index}: anchor text not found in current content (search: {:?})",
                            truncate_for_diagnostic(&block.search)
                        ),
                    });
                }
                // Idempotent replay: a crash between this write landing on
                // disk and its journal result being recorded means the
                // model may reissue the identical call. The anchor is gone
                // because this edit already happened, and the replacement
                // occurs exactly once, so this block is (as far as a
                // content check can confirm) a replay of its own prior
                // write; leave `working` untouched and continue so replay
                // reproduces the same final file state.
            }
            n => {
                return Err(EditError::AnchorDivergence {
                    detail: format!(
                        "edit {index}: anchor matched {n} locations, expected exactly 1 (search: {:?})",
                        truncate_for_diagnostic(&block.search)
                    ),
                });
            }
        }
    }
    Ok(AppliedEdit {
        content: working,
        already_applied: !wrote_any_block,
    })
}

fn truncate_for_diagnostic(text: &str) -> String {
    const LIMIT: usize = 80;
    if text.len() <= LIMIT {
        text.to_string()
    } else {
        format!("{}…", &text[..LIMIT])
    }
}

/// Resolves the `apply-edit` tool call's `content` payload against the
/// file's current content (`None` for a file that does not yet exist) into
/// the bytes to write, plus whether the call was an idempotent replay that
/// needs no write at all. Whole-file resolution is a pure passthrough —
/// byte-for-byte identical to PRD-058 behavior when `change_kind` is absent
/// — and is never treated as a replay: it always applies.
pub fn resolve_edit(
    current: Option<&str>,
    form: EditForm,
    payload: &str,
) -> Result<AppliedEdit, EditError> {
    match form {
        EditForm::WholeFile => Ok(AppliedEdit {
            content: payload.to_string(),
            already_applied: false,
        }),
        EditForm::SearchReplace => {
            let blocks = parse_search_replace_blocks(payload)?;
            apply_targeted_edit(current.unwrap_or_default(), &blocks)
        }
        EditForm::UnifiedDiff => {
            let blocks = parse_unified_diff_blocks(payload)?;
            apply_targeted_edit(current.unwrap_or_default(), &blocks)
        }
    }
}

/// Worker-instruction text stating the targeted-edit preference for files
/// at or above `threshold_bytes`. Enforcement is the tool's shape — both
/// forms are always accepted — this text is the nudge PRD-072 requires the
/// runtime to state, not a gate.
pub fn targeted_edit_worker_instruction(threshold_bytes: usize) -> String {
    format!(
        "apply-edit: for a file at or above {threshold_bytes} bytes, prefer a targeted \
         edit (change_kind \"search-replace\" or \"unified-diff\") over a whole-file \
         rewrite. Whole-file replacement (change_kind \"whole-file\", the default) stays \
         available for new files and genuine full rewrites."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_file_is_pure_passthrough() {
        let result = resolve_edit(Some("old"), EditForm::WholeFile, "new").unwrap();
        assert_eq!(result.content, "new");
        assert!(!result.already_applied);
        // Even with no current content (new file), whole-file passes through.
        let result = resolve_edit(None, EditForm::WholeFile, "new").unwrap();
        assert_eq!(result.content, "new");
        assert!(!result.already_applied);
    }

    #[test]
    fn search_replace_applies_single_match() {
        let current = "fn main() {\n    old();\n}\n";
        let payload = r#"[{"search":"old();","replace":"new();"}]"#;
        let result = resolve_edit(Some(current), EditForm::SearchReplace, payload).unwrap();
        assert_eq!(result.content, "fn main() {\n    new();\n}\n");
        assert!(!result.already_applied);
    }

    #[test]
    fn search_replace_rejects_stale_anchor_with_named_divergence() {
        let current = "fn main() {\n    current();\n}\n";
        let payload = r#"[{"search":"stale_call();","replace":"new();"}]"#;
        let error = resolve_edit(Some(current), EditForm::SearchReplace, payload).unwrap_err();
        assert!(matches!(error, EditError::AnchorDivergence { .. }));
    }

    #[test]
    fn search_replace_rejects_ambiguous_multiple_matches() {
        let current = "dup();\ndup();\n";
        let payload = r#"[{"search":"dup();","replace":"single();"}]"#;
        let error = resolve_edit(Some(current), EditForm::SearchReplace, payload).unwrap_err();
        assert!(matches!(error, EditError::AnchorDivergence { .. }));
    }

    #[test]
    fn search_replace_is_idempotent_on_replay() {
        // Simulates a resumed loop replaying the identical call after the
        // write already landed on disk: the anchor is gone, but the
        // replacement is present exactly once, so replay reproduces the
        // same content rather than failing closed, and is reported as such.
        let already_applied = "fn main() {\n    new();\n}\n";
        let payload = r#"[{"search":"old();","replace":"new();"}]"#;
        let result = resolve_edit(Some(already_applied), EditForm::SearchReplace, payload).unwrap();
        assert_eq!(result.content, already_applied);
        assert!(result.already_applied);
    }

    #[test]
    fn search_replace_rejects_stale_anchor_when_replacement_occurs_elsewhere() {
        // The anchor ("stale();") is genuinely absent — this hunk never
        // applied to this file at all — but its replacement text ("}")
        // already occurs (twice, here) for reasons unrelated to this edit.
        // A heuristic that only checks "does this text appear anywhere"
        // would misidentify this as an already-applied replay and silently
        // skip a genuinely stale hunk; requiring exactly one occurrence
        // catches it instead.
        let current = "fn a() {\n}\nfn b() {\n}\n";
        let payload = r#"[{"search":"stale();","replace":"}"}]"#;
        let error = resolve_edit(Some(current), EditForm::SearchReplace, payload).unwrap_err();
        assert!(matches!(error, EditError::AnchorDivergence { .. }));
        // Never a silent misapply: content is untouched by the caller since
        // resolve_edit returned an error rather than a resolved edit.
    }

    #[test]
    fn search_replace_atomic_apply_rejects_whole_batch_on_one_divergence() {
        let current = "a();\nb();\n";
        let payload =
            r#"[{"search":"a();","replace":"a2();"},{"search":"missing();","replace":"x();"}]"#;
        let error = resolve_edit(Some(current), EditForm::SearchReplace, payload).unwrap_err();
        assert!(matches!(error, EditError::AnchorDivergence { .. }));
    }

    #[test]
    fn unified_diff_hunk_applies() {
        let current = "line1\nold_line\nline3\n";
        let diff = "@@ -1,3 +1,3 @@\n line1\n-old_line\n+new_line\n line3\n";
        let result = resolve_edit(Some(current), EditForm::UnifiedDiff, diff).unwrap();
        assert_eq!(result.content, "line1\nnew_line\nline3\n");
        assert!(!result.already_applied);
    }

    #[test]
    fn unified_diff_rejects_stale_hunk() {
        let current = "line1\nsomething_else\nline3\n";
        let diff = "@@ -1,3 +1,3 @@\n line1\n-old_line\n+new_line\n line3\n";
        let error = resolve_edit(Some(current), EditForm::UnifiedDiff, diff).unwrap_err();
        assert!(matches!(error, EditError::AnchorDivergence { .. }));
    }

    #[test]
    fn malformed_search_replace_payload_is_named_not_misapplied() {
        let error = resolve_edit(Some("x"), EditForm::SearchReplace, "not json").unwrap_err();
        assert!(matches!(error, EditError::MalformedEdit { .. }));
    }

    #[test]
    fn edit_form_round_trips_through_as_str_and_parse() {
        for form in [
            EditForm::WholeFile,
            EditForm::SearchReplace,
            EditForm::UnifiedDiff,
        ] {
            assert_eq!(EditForm::parse(form.as_str()), Some(form));
        }
        assert_eq!(EditForm::parse("bogus"), None);
        assert_eq!(EditForm::default(), EditForm::WholeFile);
    }

    #[test]
    fn worker_instruction_states_threshold_and_preference() {
        let instruction = targeted_edit_worker_instruction(4096);
        assert!(instruction.contains("4096"));
        assert!(instruction.contains("search-replace"));
        assert!(instruction.contains("unified-diff"));
    }
}
