use std::collections::BTreeSet;
use std::fmt::Write;

use similar::{Algorithm, DiffOp, capture_diff_slices_by_key};

use crate::diff::{GraphDiff, TreeNode};

const MAX_BYTES: usize = 48 * 1024;
pub(crate) const TRUNCATED: &str = "\n[truncated: diff output incomplete]\n";

pub(crate) struct Output {
    text: String,
    pub(crate) truncated: bool,
}

impl Output {
    pub(crate) fn new() -> Self {
        Self {
            text: String::new(),
            truncated: false,
        }
    }

    pub(crate) fn finish(mut self, truncated: bool) -> String {
        if self.truncated || truncated {
            self.text.push_str(TRUNCATED);
        }
        self.text
    }

    pub(crate) fn push(&mut self, ch: char) {
        let _ = self.write_char(ch);
    }

    pub(crate) fn push_str(&mut self, text: &str) {
        let _ = self.write_str(text);
    }
}

impl Write for Output {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.truncated {
            return Err(std::fmt::Error);
        }
        let remaining = MAX_BYTES - self.text.len();
        if text.len() <= remaining {
            self.text.push_str(text);
            return Ok(());
        }
        let mut end = remaining;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&text[..end]);
        self.truncated = true;
        Err(std::fmt::Error)
    }
}

pub(crate) fn render(diff: &GraphDiff) -> String {
    render_with_locations(diff, false)
}

pub(crate) fn render_with_locations(diff: &GraphDiff, locations: bool) -> String {
    let mut output = Output::new();
    let unchanged = &diff.unchanged;
    for (index, root) in diff.roots.iter().enumerate() {
        if output.truncated {
            break;
        }
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
    output.finish(diff.truncated)
}

fn pair(
    output: &mut Output,
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
    if output.truncated || before.fingerprint() == after.fingerprint() {
        return;
    }

    let operations = capture_diff_slices_by_key(
        Algorithm::Myers,
        &before.children,
        &after.children,
        |node| node.identity.clone(),
    );
    for operation in operations {
        if output.truncated {
            return;
        }
        match operation {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for offset in 0..len {
                    if output.truncated {
                        return;
                    }
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
    output: &mut Output,
    marker: char,
    node: &TreeNode,
    depth: usize,
    locations: bool,
    unchanged: &BTreeSet<String>,
) {
    if output.truncated {
        return;
    }
    line(output, marker, depth, node, locations);
    if output.truncated {
        return;
    }
    if !node.children.is_empty()
        && unchanged.contains(&node.identity)
        && !node.contains("[truncated]")
    {
        let _ = writeln!(
            output,
            "{marker} {}[unchanged body omitted]",
            "  ".repeat(depth + 1)
        );
        return;
    }
    for child in &node.children {
        if output.truncated {
            return;
        }
        full(output, marker, child, depth + 1, locations, unchanged);
    }
}

fn line(output: &mut Output, marker: char, depth: usize, node: &TreeNode, locations: bool) {
    if output.truncated {
        return;
    }
    output.push(marker);
    output.push(' ');
    output.push_str(&"  ".repeat(depth));
    if node.label.chars().any(char::is_control) {
        let _ = write!(output, "{:?}", node.label);
    } else {
        output.push_str(&node.label);
    }
    let location = node
        .call_site_location
        .as_ref()
        .or(node.definition_location.as_ref());
    if locations && let Some(location) = location {
        let _ = write!(
            output,
            " @ {:?}:{}:{}-{}:{}",
            node.location_file
                .as_ref()
                .expect("a source range has a file"),
            location.start_line,
            location.start_column,
            location.end_line,
            location.end_column
        );
    }
    output.push('\n');
}

#[cfg(test)]
mod tests {
    #[test]
    fn rendering_bounds_many_roots_and_large_escaped_utf8_labels() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/compact-labels");
        let mut analyzers = crate::analyze::Analyzers::new().unwrap();
        let artifacts = analyzers.directory(&root.join("after")).unwrap();
        let graph = crate::build(&artifacts.artifacts).unwrap();
        let empty = crate::graph::Graph::default();
        let mut diff = crate::diff::compare(&empty, &graph);
        let root = diff.roots[0].clone();
        diff.roots = vec![root.clone(); 10_000];
        let output = super::render(&diff);
        assert!(output.ends_with(super::TRUNCATED));
        assert!(output.len() <= super::MAX_BYTES + super::TRUNCATED.len());

        for label in [
            "é".repeat(super::MAX_BYTES),
            "\n界".repeat(super::MAX_BYTES),
        ] {
            diff.roots = vec![root.clone()];
            diff.roots[0].after.as_mut().unwrap().label = label;
            let output = super::render_with_locations(&diff, true);
            assert!(output.ends_with(super::TRUNCATED));
            assert!(output.len() <= super::MAX_BYTES + super::TRUNCATED.len());
            assert_eq!(output.matches(super::TRUNCATED).count(), 1);
        }

        diff.roots.clear();
        diff.truncated = true;
        assert_eq!(super::render(&diff), super::TRUNCATED);
        diff.truncated = false;
        assert_eq!(super::render(&diff), "");
    }

    #[test]
    fn incomplete_resolutions_have_bounded_labels_and_witnesses() {
        use crate::{diff, model};

        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/compact-labels");
        let mut analyzers = crate::analyze::Analyzers::new().unwrap();
        let artifacts = analyzers.directory(&root.join("after")).unwrap();
        let mut graph = crate::build(&artifacts.artifacts).unwrap();
        let key = graph.symbols.keys().next().unwrap().clone();
        let template = graph.symbols[&key].clone();
        graph.symbols.retain(|candidate, _| candidate == &key);
        graph.incoming.clear();
        let mut candidates = Vec::new();
        for index in 0..100 {
            let mut witness = template.clone();
            witness.symbol.key.qualified_name = vec![format!("witness{index:03}")];
            witness.symbol.visibility = model::Visibility::Private;
            witness.steps.clear();
            candidates.push(witness.symbol.key.clone());
            graph.symbols.insert(witness.symbol.key.clone(), witness);
        }
        let location = template.symbol.location;
        let target = model::CallTarget::Path(vec!["target".to_owned()]);
        for resolution in [
            model::Resolution::Limited {
                target: target.clone(),
                candidates: Vec::new(),
            },
            model::Resolution::Limited {
                target: target.clone(),
                candidates: candidates.clone(),
            },
            model::Resolution::Ambiguous { target, candidates },
        ] {
            let kind = if matches!(resolution, model::Resolution::Limited { .. }) {
                "limited"
            } else {
                "ambiguous"
            };
            let node = graph.symbols.get_mut(&key).unwrap();
            node.steps = vec![model::ResolvedStep::Branch {
                key: "branch".to_owned(),
                label: "branch".to_owned(),
                location: location.clone(),
                children: vec![model::ResolvedStep::Call(model::ResolvedCall {
                    location: location.clone(),
                    resolution,
                })],
            }];
            let unchanged = diff::compare(&graph, &graph);
            assert!(unchanged.roots.is_empty());
            assert!(unchanged.truncated);
            assert_eq!(super::render(&unchanged), super::TRUNCATED);

            let empty = crate::graph::Graph::default();
            let added = diff::compare(&empty, &graph);
            let output = super::render(&added);
            assert!(output.contains(&format!("[{kind}: target ->")), "{output}");
            assert!(output.contains("+       [truncated]"), "{output}");
            assert!(output.ends_with(super::TRUNCATED));
            let tree = added.roots[0].after.as_ref().unwrap();
            assert_eq!(tree.identity, key.to_string());
            let limited = &tree.children[0].children[0];
            assert!(limited.children.len() <= 9);
            assert!(!limited.label.contains("witness008"));
        }
    }

    #[test]
    fn escaped_labels_keep_original_source_ranges() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/compact-labels");
        let mut analyzers = crate::analyze::Analyzers::new().unwrap();
        let before = analyzers.directory(&root.join("before")).unwrap();
        let after = analyzers.directory(&root.join("after")).unwrap();
        let before = crate::build(&before.artifacts).unwrap();
        let after = crate::build(&after.artifacts).unwrap();
        let diff = crate::diff::compare(&before, &after);
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
