use std::collections::BTreeSet;
use std::fmt::Write;

use similar::{Algorithm, DiffOp, capture_diff_slices_by_key};

use crate::diff::{GraphDiff, TreeNode};

pub(crate) fn render(diff: &GraphDiff) -> String {
    render_with_locations(diff, false)
}

pub(crate) fn render_with_locations(diff: &GraphDiff, locations: bool) -> String {
    let mut output = String::new();
    let unchanged = &diff.unchanged;
    for (index, root) in diff.roots.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        match (&root.before, &root.after) {
            (Some(before), Some(after)) => {
                pair(&mut output, before, after, 0, locations, unchanged)
            }
            (Some(before), None) => full(&mut output, '-', before, 0, locations, unchanged),
            (None, Some(after)) => full(&mut output, '+', after, 0, locations, unchanged),
            (None, None) => unreachable!("a diff root exists in at least one graph"),
        }
    }
    output
}

fn pair(
    output: &mut String,
    before: &TreeNode,
    after: &TreeNode,
    depth: usize,
    locations: bool,
    unchanged: &BTreeSet<String>,
) {
    let marker = if before.local_shape != after.local_shape {
        '~'
    } else {
        ' '
    };
    line(output, marker, depth, after, locations);
    if before.fingerprint() == after.fingerprint() {
        return;
    }

    let operations = capture_diff_slices_by_key(
        Algorithm::Myers,
        &before.children,
        &after.children,
        |node| node.identity.clone(),
    );
    for operation in operations {
        match operation {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for offset in 0..len {
                    let old = &before.children[old_index + offset];
                    let new = &after.children[new_index + offset];
                    if old.fingerprint() == new.fingerprint() {
                        if !new.hide_when_unchanged {
                            line(output, ' ', depth + 1, new, locations);
                        }
                    } else {
                        pair(output, old, new, depth + 1, locations, unchanged);
                    }
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for node in &before.children[old_index..old_index + old_len] {
                    full(output, '-', node, depth + 1, locations, unchanged);
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for node in &after.children[new_index..new_index + new_len] {
                    full(output, '+', node, depth + 1, locations, unchanged);
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for node in &before.children[old_index..old_index + old_len] {
                    full(output, '-', node, depth + 1, locations, unchanged);
                }
                for node in &after.children[new_index..new_index + new_len] {
                    full(output, '+', node, depth + 1, locations, unchanged);
                }
            }
        }
    }
}

fn full(
    output: &mut String,
    marker: char,
    node: &TreeNode,
    depth: usize,
    locations: bool,
    unchanged: &BTreeSet<String>,
) {
    line(output, marker, depth, node, locations);
    if !node.children.is_empty()
        && unchanged.contains(&node.identity)
        && !node.contains("[truncated]")
    {
        writeln!(
            output,
            "{marker} {}[unchanged body omitted]",
            "  ".repeat(depth + 1)
        )
        .expect("writing to a string cannot fail");
        return;
    }
    for child in &node.children {
        full(output, marker, child, depth + 1, locations, unchanged);
    }
}

fn line(output: &mut String, marker: char, depth: usize, node: &TreeNode, locations: bool) {
    output.push(marker);
    output.push(' ');
    output.push_str(&"  ".repeat(depth));
    if node.label.chars().any(char::is_control) {
        write!(output, "{:?}", node.label).expect("writing to a string cannot fail");
    } else {
        output.push_str(&node.label);
    }
    let location = node
        .call_site_location
        .as_ref()
        .or(node.definition_location.as_ref());
    if locations && let Some(location) = location {
        write!(
            output,
            " @ {:?}:{}:{}-{}:{}",
            node.location_file
                .as_ref()
                .expect("a source range has a file"),
            location.start_line,
            location.start_column,
            location.end_line,
            location.end_column
        )
        .expect("writing to a string cannot fail");
    }
    output.push('\n');
}

#[cfg(test)]
mod tests {
    #[test]
    fn escaped_labels_keep_original_source_ranges() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/compact-labels");
        let mut analyzers = crate::analyze::Analyzers::new().unwrap();
        let before = analyzers.directory(&root.join("before")).unwrap();
        let after = analyzers.directory(&root.join("after")).unwrap();
        let diff = crate::diff::compare(
            &crate::build(&before.artifacts).unwrap(),
            &crate::build(&after.artifacts).unwrap(),
        );
        let output = super::render_with_locations(&diff, true);
        assert!(output.contains(" @ \"lib.rs\":2:5-3:13\n"), "{output}");
        assert!(output.contains(" @ \"lib.rs\":2:5-2:27\n"), "{output}");
        assert!(
            output.contains("rust:lib.rs::make @ \"lib.rs\":2:5-2:18\n"),
            "{output}"
        );
        assert_eq!(output.lines().count(), 4);
    }
}
