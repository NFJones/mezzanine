//! Semantic Patch tests for parsing behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies shell-style apply_patch heredoc wrappers are stripped when they
/// accidentally appear inside the semantic action payload.
///
/// Models trained on command-line patch examples sometimes include
/// `apply_patch <<'PATCH'` around the patch text. The action parser should
/// treat that as a recoverable wrapper instead of dispatching or rejecting the
/// mutation, because the action itself already identifies the operation.
fn semantic_apply_patch_accepts_apply_patch_heredoc_wrapper_text() {
    let temp = test_temp_dir("semantic-codex-patch-shell-heredoc");
    std::fs::write(temp.join("note.txt"), "old\n").unwrap();
    let patch = "apply_patch <<'PATCH'\n*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch\nPATCH\n";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies blank hunk-body lines are interpreted as empty context lines.
///
/// Codex accepts empty body lines for patches that touch regions around blank
/// lines. Mezzanine should do the same so models do not need to manufacture a
/// single-space line to represent empty context.
fn semantic_apply_patch_accepts_blank_context_lines() {
    let temp = test_temp_dir("semantic-codex-patch-blank-context");
    std::fs::write(temp.join("note.txt"), "before\n\nold\n").unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: note.txt\n@@\n before\n\n-old\n+new\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "before\n\nnew\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies the semantic patch parser accepts the same lenient first-update
/// forms as Codex while still applying them through Mezzanine's checked
/// snapshot/write phases.
///
/// Models sometimes add whitespace around markers or omit the first `@@`
/// header in otherwise valid Mezzanine update patches. Accepting those forms
/// reduces correctable parse failures without weakening path or snapshot checks.
fn semantic_apply_patch_accepts_codex_lenient_first_update_hunk() {
    let temp = test_temp_dir("semantic-codex-patch-lenient-first-hunk");
    std::fs::write(temp.join("note.txt"), "old\ncontext\n").unwrap();
    let patch = "  *** Begin Patch  \n  *** Update File: note.txt  \n-old\n+new\n context\n  *** End Patch  ";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\ncontext\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies fenced patch strings are normalized before parsing.
///
/// Some non-native provider modes have historically placed the patch block in a
/// Markdown fence even when the action payload is already the structured
/// `apply_patch.patch` field. The runtime should recover from that wrapper,
/// while prompt guidance still asks models to emit the clean unwrapped block.
fn semantic_apply_patch_accepts_fenced_patch_text() {
    let temp = test_temp_dir("semantic-codex-patch-fenced");
    std::fs::write(temp.join("note.txt"), "old\n").unwrap();
    let patch = "```patch\n*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch\n```\n";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies fenced patch payloads preserve enough body indentation to dedent.
///
/// Wrapper normalization must remove surrounding Markdown syntax without
/// stripping only the first content line's indent; otherwise a fenced indented
/// payload would parse the marker but still reject hunk body lines as
/// over-indented text.
fn semantic_apply_patch_accepts_fenced_uniformly_indented_patch_text() {
    let temp = test_temp_dir("semantic-codex-patch-fenced-indented");
    std::fs::write(temp.join("note.txt"), "old\ncontext\n").unwrap();
    let patch = "```patch\n    *** Begin Patch\n    *** Update File: note.txt\n    @@\n    -old\n    +new\n     context\n    *** End Patch\n```\n";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\ncontext\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies heredoc-wrapped patch strings are normalized before parsing.
///
/// Codex keeps this compatibility path for models that wrap patch text in a
/// shell-looking heredoc even though the patch is passed as the tool argument.
/// Mezzanine strips the wrapper and still executes the semantic patch action,
/// not a shell `apply_patch` command.
fn semantic_apply_patch_accepts_heredoc_wrapped_patch_text() {
    let temp = test_temp_dir("semantic-codex-patch-heredoc");
    std::fs::write(temp.join("note.txt"), "old\n").unwrap();
    let patch = "<<'PATCH'\n*** Begin Patch\n*** Update File: note.txt\n@@\n-old\n+new\n*** End Patch\nPATCH\n";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies uniformly indented patch payloads are normalized before parsing.
///
/// Some provider/text-mode paths preserve surrounding indentation when a model
/// emits a patch block inside a list item, object literal, or fenced example.
/// The semantic action should recover from that wrapper indentation while still
/// requiring canonical hunk prefixes after the common indent is removed.
fn semantic_apply_patch_accepts_uniformly_indented_patch_text() {
    let temp = test_temp_dir("semantic-codex-patch-indented");
    std::fs::write(temp.join("note.txt"), "old\ncontext\n").unwrap();
    let patch = "    *** Begin Patch\n    *** Update File: note.txt\n    @@\n    -old\n    +new\n     context\n    *** End Patch\n";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.txt")).unwrap(),
        "new\ncontext\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies Mezzanine update hunks tolerate common unified-range metadata.
///
/// Models often include `@@ -old,+new @@` range text even when they are using
/// the Codex `*** Begin Patch` envelope. That range is not reliable once the
/// target file has changed, so Mezzanine ignores it and still applies the hunk
/// by body context plus any explicit anchor text after the closing marker.
fn semantic_apply_patch_ignores_unified_range_hunk_metadata() {
    let temp = test_temp_dir("semantic-codex-patch-unified-range");
    std::fs::write(
        temp.join("note.rs"),
        "fn first() {\n    old();\n}\n\nfn second() {\n    old();\n}\n",
    )
    .unwrap();
    let patch = "*** Begin Patch\n*** Update File: note.rs\n@@ -5,3 +5,3 @@ fn second\n-    old();\n+    new();\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("note.rs")).unwrap(),
        "fn first() {\n    old();\n}\n\nfn second() {\n    new();\n}\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies common copied path prefixes are normalized in patch headers.
///
/// Models often copy paths from shell output or git diff labels, producing
/// leading `./`, `a/`, `b/`, or interior `/.` segments even when the intended
/// target is a normal CWD-relative path. Accepting those safe normalizations
/// prevents correctable header-shape failures before hunk matching begins.
fn semantic_apply_patch_normalizes_common_patch_header_path_prefixes() {
    let temp = test_temp_dir("semantic-codex-patch-path-prefixes");
    std::fs::create_dir_all(temp.join("src")).unwrap();
    std::fs::write(temp.join("src/note.txt"), "old\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: a/./src/note.txt\n@@\n-old\n+new\n*** Add File: b/./generated.txt\n+created\n*** End Patch";

    let output = run_apply_patch_action(&temp, patch);

    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("src/note.txt")).unwrap(),
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.join("generated.txt")).unwrap(),
        "created\n"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
/// Verifies that a plain unified diff (without diff --git header) is also
/// accepted and converted.
fn unified_diff_conversion_accepts_minimal_unified_diff() {
    let diff = "--- a/file.txt\n+++ b/file.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";

    let converted = try_convert_unified_diff_to_mez_patch(diff).unwrap();

    assert!(converted.starts_with("*** Begin Patch"));
    assert!(converted.contains("*** Update File: file.txt"));
}

#[test]
/// Verifies unified diff conversion returns None for already-valid Mezzanine
/// patches so that no double-conversion occurs.
fn unified_diff_conversion_noop_for_mez_patch_format() {
    let mez = "*** Begin Patch\n*** Update File: lib.rs\n@@\n old\n+new\n*** End Patch\n";

    assert!(try_convert_unified_diff_to_mez_patch(mez).is_none());
}

#[test]
/// Verifies unified diff conversion produces valid Mezzanine patch blocks
/// that can be parsed and planned successfully.
fn unified_diff_conversion_produces_valid_mez_patch() {
    let diff = "--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,3 @@\n old line\n+new line\n context\n";

    let converted = try_convert_unified_diff_to_mez_patch(diff).unwrap();

    assert!(converted.starts_with("*** Begin Patch"));
    assert!(converted.ends_with("*** End Patch\n"));
    assert!(converted.contains("*** Update File: foo.rs"));
    assert!(converted.contains("old line"));
    assert!(converted.contains("+new line"));
    assert!(converted.contains(" context"));
}

#[test]
/// Verifies deleted-file unified diffs are not auto-converted.
///
/// Raw unified diff deletes carry old-side content expectations that a plain
/// `*** Delete File` operation cannot represent. Refusing conversion prevents a
/// stale delete diff from removing a file whose current contents no longer match
/// the diff's removed lines.
fn unified_diff_conversion_refuses_deleted_file_sections() {
    let diff = "diff --git a/file.txt b/file.txt\ndeleted file mode 100644\n--- a/file.txt\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-old\n";

    assert!(try_convert_unified_diff_to_mez_patch(diff).is_none());
}

#[test]
/// Verifies unified diff conversion returns None for non-diff, non-patch text.
fn unified_diff_conversion_rejects_non_diff_text() {
    assert!(try_convert_unified_diff_to_mez_patch("just some text").is_none());
    assert!(try_convert_unified_diff_to_mez_patch("").is_none());
}

#[test]
/// Verifies unified diff conversion handles the case where path prefixes
/// are stripped from `a/` and `b/` diff prefixes.
fn unified_diff_conversion_strips_path_prefixes() {
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n";

    let converted = try_convert_unified_diff_to_mez_patch(diff).unwrap();

    assert!(converted.contains("*** Update File: src/lib.rs"));
}
