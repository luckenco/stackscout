use std::fmt::Write;

use similar::{Algorithm, DiffOp, capture_diff_slices_by_key};

use crate::diff::{GraphDiff, TreeNode};

pub(crate) fn render(diff: &GraphDiff) -> String {
    render_with_locations(diff, false)
}

fn render_with_locations(diff: &GraphDiff, locations: bool) -> String {
    let mut output = String::new();
    for (index, root) in diff.roots.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        match (&root.before, &root.after) {
            (Some(before), Some(after)) => pair(&mut output, before, after, 0, locations),
            (Some(before), None) => full(&mut output, '-', before, 0, locations),
            (None, Some(after)) => full(&mut output, '+', after, 0, locations),
            (None, None) => unreachable!("a diff root exists in at least one graph"),
        }
    }
    output
}

fn pair(output: &mut String, before: &TreeNode, after: &TreeNode, depth: usize, locations: bool) {
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
                        line(output, ' ', depth + 1, new, locations);
                    } else {
                        pair(output, old, new, depth + 1, locations);
                    }
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for node in &before.children[old_index..old_index + old_len] {
                    full(output, '-', node, depth + 1, locations);
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for node in &after.children[new_index..new_index + new_len] {
                    full(output, '+', node, depth + 1, locations);
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                for node in &before.children[old_index..old_index + old_len] {
                    full(output, '-', node, depth + 1, locations);
                }
                for node in &after.children[new_index..new_index + new_len] {
                    full(output, '+', node, depth + 1, locations);
                }
            }
        }
    }
}

fn full(output: &mut String, marker: char, node: &TreeNode, depth: usize, locations: bool) {
    line(output, marker, depth, node, locations);
    for child in &node.children {
        full(output, marker, child, depth + 1, locations);
    }
}

fn line(output: &mut String, marker: char, depth: usize, node: &TreeNode, locations: bool) {
    output.push(marker);
    output.push(' ');
    output.push_str(&"  ".repeat(depth));
    output.push_str(&node.label);
    if locations && let Some(location) = &node.location {
        write!(
            output,
            " @ {}:{}-{}:{}",
            location.start_line, location.start_column, location.end_line, location.end_column
        )
        .expect("writing to a string cannot fail");
    }
    output.push('\n');
}
