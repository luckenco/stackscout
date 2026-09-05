use tree_sitter::Node;

use super::{AnalyzeError, compact, first_named, range, shape, text};
use crate::model::{
    CallSite, CallTarget, FileArtifact, Language, LocalStep, LocalSymbol, SymbolKey, Visibility,
};

pub(super) fn extract(
    path: &str,
    source: &str,
    root: Node<'_>,
) -> Result<FileArtifact, AnalyzeError> {
    let mut artifact = FileArtifact {
        language: Language::Rust,
        path: path.to_owned(),
        symbols: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
    };
    definitions(path, source, root, &[], None, &mut artifact)?;
    Ok(artifact)
}

fn definitions(
    path: &str,
    source: &str,
    node: Node<'_>,
    modules: &[String],
    impl_name: Option<&str>,
    artifact: &mut FileArtifact,
) -> Result<(), AnalyzeError> {
    match node.kind() {
        "trait_item" => return Ok(()),
        "function_item" => {
            let name = required_field(path, source, node, "name")?;
            let body =
                node.child_by_field_name("body")
                    .ok_or_else(|| AnalyzeError::Unsupported {
                        path: path.to_owned(),
                        message: format!("function {name} has no body"),
                    })?;
            let mut qualified_name = modules.to_vec();
            if let Some(name) = impl_name {
                qualified_name.push(name.to_owned());
            }
            qualified_name.push(name.to_owned());
            artifact.symbols.push(LocalSymbol {
                key: SymbolKey {
                    language: Language::Rust,
                    file: path.to_owned(),
                    qualified_name,
                },
                module: modules.to_vec(),
                location: range(node),
                visibility: if has_child(node, "visibility_modifier") {
                    Visibility::Public
                } else {
                    Visibility::Private
                },
                shape: shape(path, source, node)?,
                steps: steps(path, source, body)?,
            });
            return Ok(());
        }
        "impl_item" => {
            let type_node =
                node.child_by_field_name("type")
                    .ok_or_else(|| AnalyzeError::Unsupported {
                        path: path.to_owned(),
                        message: "impl has no type".to_owned(),
                    })?;
            let type_name = text(path, source, type_node)?.trim();
            let impl_name = if let Some(trait_node) = node.child_by_field_name("trait") {
                let trait_name = text(path, source, trait_node)?.trim();
                format!("<{type_name} as {trait_name}>")
            } else {
                type_name.to_owned()
            };
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    definitions(path, source, child, modules, Some(&impl_name), artifact)?;
                }
            }
            return Ok(());
        }
        "mod_item" => {
            if let (Some(name), Some(body)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("body"),
            ) {
                let mut nested = modules.to_vec();
                nested.push(text(path, source, name)?.to_owned());
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    definitions(path, source, child, &nested, impl_name, artifact)?;
                }
                return Ok(());
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        definitions(path, source, child, modules, impl_name, artifact)?;
    }
    Ok(())
}

fn steps(path: &str, source: &str, node: Node<'_>) -> Result<Vec<LocalStep>, AnalyzeError> {
    let mut output = Vec::new();
    collect_steps(path, source, node, &mut output)?;
    Ok(output)
}

fn collect_steps(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut Vec<LocalStep>,
) -> Result<(), AnalyzeError> {
    if matches!(
        node.kind(),
        "function_item" | "closure_expression" | "impl_item" | "mod_item"
    ) {
        return Ok(());
    }
    if node.kind() == "if_expression" {
        collect_if(path, source, node, false, output)?;
        return Ok(());
    }
    if node.kind() == "match_expression" {
        collect_match(path, source, node, output)?;
        return Ok(());
    }
    if node.kind() == "call_expression"
        && let Some(call) = collect_call(path, source, node)?
    {
        output.push(LocalStep::Call(call));
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_steps(path, source, child, output)?;
    }
    Ok(())
}

fn collect_if(
    path: &str,
    source: &str,
    node: Node<'_>,
    else_if: bool,
    output: &mut Vec<LocalStep>,
) -> Result<(), AnalyzeError> {
    let condition =
        node.child_by_field_name("condition")
            .ok_or_else(|| AnalyzeError::Unsupported {
                path: path.to_owned(),
                message: "if expression has no condition".to_owned(),
            })?;
    collect_steps(path, source, condition, output)?;
    let condition_text = compact(text(path, source, condition)?);
    let kind = if else_if { "else-if" } else { "if" };
    let label = if else_if { "else if" } else { "if" };
    let consequence =
        node.child_by_field_name("consequence")
            .ok_or_else(|| AnalyzeError::Unsupported {
                path: path.to_owned(),
                message: "if expression has no consequence".to_owned(),
            })?;
    output.push(LocalStep::Branch {
        key: format!("{kind}:{condition_text}"),
        label: format!("{label} {condition_text}"),
        location: range(condition),
        children: steps(path, source, consequence)?,
    });

    let Some(alternative) = node.child_by_field_name("alternative") else {
        return Ok(());
    };
    let Some(inner) = first_named(alternative) else {
        return Ok(());
    };
    if inner.kind() == "if_expression" {
        collect_if(path, source, inner, true, output)?;
    } else {
        output.push(LocalStep::Branch {
            key: "else".to_owned(),
            label: "else".to_owned(),
            location: range(alternative),
            children: steps(path, source, inner)?,
        });
    }
    Ok(())
}

fn collect_match(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut Vec<LocalStep>,
) -> Result<(), AnalyzeError> {
    if let Some(value) = node.child_by_field_name("value") {
        collect_steps(path, source, value, output)?;
    }
    let Some(body) = node.child_by_field_name("body") else {
        return Ok(());
    };
    let mut cursor = body.walk();
    for arm in body.named_children(&mut cursor) {
        if arm.kind() != "match_arm" {
            continue;
        }
        let Some(pattern) = arm.child_by_field_name("pattern") else {
            continue;
        };
        let Some(value) = arm.child_by_field_name("value") else {
            continue;
        };
        if let Some(condition) = pattern.child_by_field_name("condition") {
            collect_steps(path, source, condition, output)?;
        }
        let pattern_text = compact(text(path, source, pattern)?);
        output.push(LocalStep::Branch {
            key: format!("case:{pattern_text}"),
            label: format!("case {pattern_text}"),
            location: range(pattern),
            children: steps(path, source, value)?,
        });
    }
    Ok(())
}

fn collect_call(
    path: &str,
    source: &str,
    node: Node<'_>,
) -> Result<Option<CallSite>, AnalyzeError> {
    let Some(function) = node.child_by_field_name("function") else {
        return Ok(None);
    };
    let Some(target) = call_target(path, source, function)? else {
        return Ok(None);
    };
    Ok(Some(CallSite {
        target,
        location: range(node),
    }))
}

fn call_target(
    path: &str,
    source: &str,
    mut node: Node<'_>,
) -> Result<Option<CallTarget>, AnalyzeError> {
    if node.kind() == "generic_function" {
        node = node.child_by_field_name("function").unwrap_or(node);
    }
    match node.kind() {
        "identifier" => Ok(Some(CallTarget::Bare(text(path, source, node)?.to_owned()))),
        "scoped_identifier" => {
            let parts = text(path, source, node)?
                .split("::")
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if parts.len() == 2 && parts[0] == "Self" {
                return Ok(Some(CallTarget::SelfAssociated(parts[1].clone())));
            }
            Ok(Some(CallTarget::Path(parts)))
        }
        "field_expression" => {
            let Some(value) = node.child_by_field_name("value") else {
                return Ok(None);
            };
            let Some(field) = node.child_by_field_name("field") else {
                return Ok(None);
            };
            let field = text(path, source, field)?.to_owned();
            if text(path, source, value)? == "self" {
                return Ok(Some(CallTarget::SelfMethod(field)));
            }
            let mut parts = field_parts(path, source, value)?;
            parts.push(field);
            Ok(Some(CallTarget::Path(parts)))
        }
        _ => Ok(None),
    }
}

fn field_parts(path: &str, source: &str, node: Node<'_>) -> Result<Vec<String>, AnalyzeError> {
    if node.kind() == "field_expression"
        && let (Some(value), Some(field)) = (
            node.child_by_field_name("value"),
            node.child_by_field_name("field"),
        )
    {
        let mut parts = field_parts(path, source, value)?;
        parts.push(text(path, source, field)?.to_owned());
        return Ok(parts);
    }
    // Calls and other receiver expressions are opaque, including their literals.
    Ok(vec![text(path, source, node)?.to_owned()])
}

fn required_field<'a>(
    path: &str,
    source: &'a str,
    node: Node<'_>,
    field: &str,
) -> Result<&'a str, AnalyzeError> {
    let child = node
        .child_by_field_name(field)
        .ok_or_else(|| AnalyzeError::Unsupported {
            path: path.to_owned(),
            message: format!("{} has no {field}", node.kind()),
        })?;
    text(path, source, child)
}

fn has_child(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| child.kind() == kind)
}
