use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{AnalyzeError, range, shape, text};
use crate::model::{
    CallSite, CallTarget, Export, FileArtifact, Import, ImportTarget, Language, LocalSymbol,
    SymbolKey,
};

pub(super) fn extract(
    path: &str,
    source: &str,
    root: Node<'_>,
) -> Result<FileArtifact, AnalyzeError> {
    let mut artifact = FileArtifact {
        language: Language::TypeScript,
        path: path.to_owned(),
        symbols: Vec::new(),
        calls: Vec::new(),
        imports: Vec::new(),
    };
    imports(path, source, root, &mut artifact.imports)?;
    definitions(path, source, root, None, Export::Private, &mut artifact)?;
    Ok(artifact)
}

fn imports(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut Vec<Import>,
) -> Result<(), AnalyzeError> {
    if node.kind() == "import_statement" {
        let Some(source_node) = node.child_by_field_name("source") else {
            return Ok(());
        };
        let module = text(path, source, source_node)?
            .trim_matches(['\'', '"'])
            .to_owned();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_imports(path, source, child, &module, output)?;
        }
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        imports(path, source, child, output)?;
    }
    Ok(())
}

fn collect_imports(
    path: &str,
    source: &str,
    node: Node<'_>,
    module: &str,
    output: &mut Vec<Import>,
) -> Result<(), AnalyzeError> {
    match node.kind() {
        "import_specifier" => {
            let Some(name) = node.child_by_field_name("name") else {
                return Ok(());
            };
            let imported = text(path, source, name)?.trim_matches(['\'', '"']);
            let local = node
                .child_by_field_name("alias")
                .map(|alias| text(path, source, alias))
                .transpose()?
                .unwrap_or(imported);
            output.push(Import {
                local: local.to_owned(),
                target: ImportTarget::Named(imported.to_owned()),
                source: module.to_owned(),
            });
            return Ok(());
        }
        "namespace_import" => {
            let mut cursor = node.walk();
            if let Some(name) = node.named_children(&mut cursor).next() {
                output.push(Import {
                    local: text(path, source, name)?.to_owned(),
                    target: ImportTarget::Namespace,
                    source: module.to_owned(),
                });
            }
            return Ok(());
        }
        "import_clause" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    output.push(Import {
                        local: text(path, source, child)?.to_owned(),
                        target: ImportTarget::Default,
                        source: module.to_owned(),
                    });
                } else {
                    collect_imports(path, source, child, module, output)?;
                }
            }
            return Ok(());
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_imports(path, source, child, module, output)?;
    }
    Ok(())
}

fn definitions(
    path: &str,
    source: &str,
    node: Node<'_>,
    class_name: Option<&str>,
    class_export: Export,
    artifact: &mut FileArtifact,
) -> Result<(), AnalyzeError> {
    match node.kind() {
        "function_declaration" => {
            add_function(
                path,
                source,
                node,
                None,
                export(path, source, node)?,
                artifact,
            )?;
            return Ok(());
        }
        "method_definition" => {
            if class_name.is_some() && !is_accessor(node) {
                add_function(path, source, node, class_name, class_export, artifact)?;
            }
            return Ok(());
        }
        "class_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(());
            };
            let name = text(path, source, name_node)?;
            let class_export = export(path, source, node)?;
            let method_export = if class_export.is_public() {
                Export::Named
            } else {
                Export::Private
            };
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    definitions(path, source, child, Some(name), method_export, artifact)?;
                }
            }
            return Ok(());
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        definitions(path, source, child, class_name, class_export, artifact)?;
    }
    Ok(())
}

fn add_function(
    path: &str,
    source: &str,
    node: Node<'_>,
    class_name: Option<&str>,
    export: Export,
    artifact: &mut FileArtifact,
) -> Result<(), AnalyzeError> {
    let name_node = node
        .child_by_field_name("name")
        .ok_or_else(|| AnalyzeError::Unsupported {
            path: path.to_owned(),
            message: format!("{} has no name", node.kind()),
        })?;
    let name = text(path, source, name_node)?;
    let body = node
        .child_by_field_name("body")
        .ok_or_else(|| AnalyzeError::Unsupported {
            path: path.to_owned(),
            message: format!("function {name} has no body"),
        })?;
    let mut qualified_name = Vec::new();
    if let Some(class_name) = class_name {
        qualified_name.push(class_name.to_owned());
    }
    qualified_name.push(name.to_owned());
    let caller = artifact.symbols.len();
    artifact.symbols.push(LocalSymbol {
        key: SymbolKey {
            language: Language::TypeScript,
            file: path.to_owned(),
            qualified_name,
        },
        module: Vec::new(),
        location: range(node),
        export,
        shape: shape(path, source, node)?,
    });
    let bindings = bindings(path, source, node, body)?;
    calls(path, source, body, caller, &bindings, &mut artifact.calls)
}

fn calls(
    path: &str,
    source: &str,
    node: Node<'_>,
    caller: usize,
    bindings: &BTreeSet<String>,
    output: &mut Vec<CallSite>,
) -> Result<(), AnalyzeError> {
    if matches!(node.kind(), "function_declaration" | "method_definition") {
        return Ok(());
    }
    if node.kind() == "call_expression" {
        collect_call(path, source, node, caller, bindings, output)?;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        calls(path, source, child, caller, bindings, output)?;
    }
    Ok(())
}

fn collect_call(
    path: &str,
    source: &str,
    node: Node<'_>,
    caller: usize,
    bindings: &BTreeSet<String>,
    output: &mut Vec<CallSite>,
) -> Result<(), AnalyzeError> {
    let Some(function) = node.child_by_field_name("function") else {
        return Ok(());
    };
    let Some(target) = call_target(path, source, function, bindings)? else {
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
    node: Node<'_>,
    bindings: &BTreeSet<String>,
) -> Result<Option<CallTarget>, AnalyzeError> {
    match node.kind() {
        "identifier" => {
            let name = text(path, source, node)?.to_owned();
            if bindings.contains(&name) {
                return Ok(Some(CallTarget::Bound(name)));
            }
            Ok(Some(CallTarget::Bare(name)))
        }
        "member_expression" => {
            let Some(object) = node.child_by_field_name("object") else {
                return Ok(None);
            };
            let Some(property) = node.child_by_field_name("property") else {
                return Ok(None);
            };
            let object = text(path, source, object)?;
            let property = text(path, source, property)?.to_owned();
            if object == "this" {
                return Ok(Some(CallTarget::ThisMethod(property)));
            }
            let mut parts = object.split('.').map(str::to_owned).collect::<Vec<_>>();
            parts.push(property);
            Ok(Some(CallTarget::Path(parts)))
        }
        _ => Ok(None),
    }
}

fn bindings(
    path: &str,
    source: &str,
    function: Node<'_>,
    body: Node<'_>,
) -> Result<BTreeSet<String>, AnalyzeError> {
    let mut output = BTreeSet::new();
    if let Some(parameters) = function.child_by_field_name("parameters") {
        parameter_bindings(path, source, parameters, &mut output)?;
    }
    local_bindings(path, source, body, &mut output)?;
    Ok(output)
}

fn parameter_bindings(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut BTreeSet<String>,
) -> Result<(), AnalyzeError> {
    if matches!(node.kind(), "required_parameter" | "optional_parameter") {
        if let Some(binding) = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("pattern"))
        {
            binding_names(path, source, binding, output)?;
        }
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        parameter_bindings(path, source, child, output)?;
    }
    Ok(())
}

fn local_bindings(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut BTreeSet<String>,
) -> Result<(), AnalyzeError> {
    match node.kind() {
        "variable_declarator" => {
            if let Some(name) = node.child_by_field_name("name") {
                binding_names(path, source, name, output)?;
            }
            return Ok(());
        }
        "function_declaration" | "class_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                binding_names(path, source, name, output)?;
            }
            return Ok(());
        }
        "arrow_function" | "function_expression" | "method_definition" => return Ok(()),
        "catch_clause" => {
            if let Some(parameter) = node.child_by_field_name("parameter") {
                binding_names(path, source, parameter, output)?;
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        local_bindings(path, source, child, output)?;
    }
    Ok(())
}

fn binding_names(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut BTreeSet<String>,
) -> Result<(), AnalyzeError> {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) {
        output.insert(text(path, source, node)?.to_owned());
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        binding_names(path, source, child, output)?;
    }
    Ok(())
}

fn is_accessor(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| matches!(child.kind(), "get" | "set"))
}

fn export(path: &str, source: &str, node: Node<'_>) -> Result<Export, AnalyzeError> {
    let Some(parent) = node.parent() else {
        return Ok(Export::Private);
    };
    if parent.kind() != "export_statement" {
        return Ok(Export::Private);
    }
    if text(path, source, parent)?.starts_with("export default") {
        return Ok(Export::Default);
    }
    Ok(Export::Named)
}
