use tree_sitter::Node;

use super::{AnalyzeError, range, shape, text};
use crate::model::{CallSite, CallTarget, Export, FileArtifact, Language, LocalSymbol, SymbolKey};

pub(super) fn extract(
    path: &str,
    source: &str,
    root: Node<'_>,
) -> Result<FileArtifact, AnalyzeError> {
    let mut artifact = FileArtifact {
        language: Language::Rust,
        path: path.to_owned(),
        symbols: Vec::new(),
        calls: Vec::new(),
        imports: Vec::new(),
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
            let caller = artifact.symbols.len();
            artifact.symbols.push(LocalSymbol {
                key: SymbolKey {
                    language: Language::Rust,
                    file: path.to_owned(),
                    qualified_name,
                },
                module: modules.to_vec(),
                location: range(node),
                export: if has_child(node, "visibility_modifier") {
                    Export::Named
                } else {
                    Export::Private
                },
                shape: shape(path, source, node)?,
            });
            calls(path, source, body, caller, &mut artifact.calls)?;
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

fn calls(
    path: &str,
    source: &str,
    node: Node<'_>,
    caller: usize,
    output: &mut Vec<CallSite>,
) -> Result<(), AnalyzeError> {
    if matches!(node.kind(), "function_item" | "impl_item" | "mod_item") {
        return Ok(());
    }
    if node.kind() == "call_expression" {
        collect_call(path, source, node, caller, output)?;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        calls(path, source, child, caller, output)?;
    }
    Ok(())
}

fn collect_call(
    path: &str,
    source: &str,
    node: Node<'_>,
    caller: usize,
    output: &mut Vec<CallSite>,
) -> Result<(), AnalyzeError> {
    let Some(function) = node.child_by_field_name("function") else {
        return Ok(());
    };
    let Some(target) = call_target(path, source, function)? else {
        return Ok(());
    };
    output.push(CallSite {
        caller,
        target,
        location: range(node),
        order: node.start_byte() as u32,
    });
    Ok(())
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
            let value = text(path, source, value)?;
            let field = text(path, source, field)?.to_owned();
            if value == "self" {
                return Ok(Some(CallTarget::SelfMethod(field)));
            }
            let mut parts = value.split('.').map(str::to_owned).collect::<Vec<_>>();
            parts.push(field);
            Ok(Some(CallTarget::Path(parts)))
        }
        _ => Ok(None),
    }
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
