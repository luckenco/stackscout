use std::collections::BTreeSet;

use tree_sitter::Node;

use super::{AnalyzeError, compact, first_named, range, shape, text};
use crate::model::{
    CallSite, CallTarget, ExportBinding, ExportName, FileArtifact, Import, ImportTarget, Language,
    LocalStep, LocalSymbol, SymbolKey, Visibility,
};

#[derive(Clone, Copy)]
struct Member<'a> {
    class: &'a str,
    is_static: bool,
}

pub(super) fn extract(
    path: &str,
    source: &str,
    root: Node<'_>,
) -> Result<FileArtifact, AnalyzeError> {
    let mut artifact = FileArtifact {
        language: Language::TypeScript,
        path: path.to_owned(),
        symbols: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
    };
    imports(path, source, root, &mut artifact.imports)?;
    exports(path, source, root, &mut artifact.exports)?;
    definitions(path, source, root, None, &mut artifact)?;
    apply_exports(&mut artifact);
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

fn exports(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut Vec<ExportBinding>,
) -> Result<(), AnalyzeError> {
    if node.kind() == "export_statement" {
        collect_exports(path, source, node, output)?;
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        exports(path, source, child, output)?;
    }
    Ok(())
}

fn collect_exports(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut Vec<ExportBinding>,
) -> Result<(), AnalyzeError> {
    let is_default = text(path, source, node)?.starts_with("export default");
    if let Some(declaration) = node.child_by_field_name("declaration") {
        if is_default {
            if let Some(name) = declaration.child_by_field_name("name") {
                output.push(ExportBinding {
                    local: text(path, source, name)?.to_owned(),
                    exported: ExportName::Default,
                });
            }
        } else {
            collect_exported_declarations(path, source, declaration, output)?;
        }
        return Ok(());
    }
    if let Some(value) = node.child_by_field_name("value") {
        if is_default {
            let local = if is_function_value(value) {
                "default"
            } else if let Some(name) = value.child_by_field_name("name") {
                text(path, source, name)?
            } else {
                text(path, source, value)?
            };
            output.push(ExportBinding {
                local: local.to_owned(),
                exported: ExportName::Default,
            });
        }
        return Ok(());
    }

    if node.child_by_field_name("source").is_some() {
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "export_clause" {
            continue;
        }
        let mut clause = child.walk();
        for specifier in child.named_children(&mut clause) {
            if specifier.kind() != "export_specifier" {
                continue;
            }
            let Some(name) = specifier.child_by_field_name("name") else {
                continue;
            };
            let local = text(path, source, name)?.trim_matches(['\'', '"']);
            let exported = specifier
                .child_by_field_name("alias")
                .map(|alias| text(path, source, alias))
                .transpose()?
                .unwrap_or(local)
                .trim_matches(['\'', '"']);
            output.push(ExportBinding {
                local: local.to_owned(),
                exported: ExportName::Named(exported.to_owned()),
            });
        }
    }
    Ok(())
}

fn collect_exported_declarations(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut Vec<ExportBinding>,
) -> Result<(), AnalyzeError> {
    if matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "class_declaration"
            | "abstract_class_declaration"
    ) {
        if let Some(name) = node.child_by_field_name("name") {
            let name = text(path, source, name)?.to_owned();
            output.push(ExportBinding {
                local: name.clone(),
                exported: ExportName::Named(name),
            });
        }
        return Ok(());
    }
    if node.kind() == "variable_declarator" {
        if let Some(name) = node.child_by_field_name("name")
            && name.kind() == "identifier"
        {
            let name = text(path, source, name)?.to_owned();
            output.push(ExportBinding {
                local: name.clone(),
                exported: ExportName::Named(name),
            });
        }
        return Ok(());
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_exported_declarations(path, source, child, output)?;
    }
    Ok(())
}

fn apply_exports(artifact: &mut FileArtifact) {
    for symbol in &mut artifact.symbols {
        let Some(local) = symbol.key.qualified_name.first() else {
            continue;
        };
        if artifact
            .exports
            .iter()
            .any(|binding| binding.local == *local)
        {
            symbol.visibility = Visibility::Public;
        }
    }
}

fn definitions(
    path: &str,
    source: &str,
    node: Node<'_>,
    class_name: Option<&str>,
    artifact: &mut FileArtifact,
) -> Result<(), AnalyzeError> {
    match node.kind() {
        "export_statement" => {
            if let Some(value) = node.child_by_field_name("value")
                && is_function_value(value)
            {
                add_function(path, source, value, Some("default"), None, artifact)?;
                return Ok(());
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                definitions(path, source, child, class_name, artifact)?;
            }
            return Ok(());
        }
        "function_declaration" | "generator_function_declaration" => {
            add_function(path, source, node, None, None, artifact)?;
            return Ok(());
        }
        "variable_declarator" => {
            let Some(name) = node.child_by_field_name("name") else {
                return Ok(());
            };
            let Some(value) = node.child_by_field_name("value") else {
                return Ok(());
            };
            if name.kind() == "identifier" && is_function_value(value) {
                add_function(
                    path,
                    source,
                    value,
                    Some(text(path, source, name)?),
                    class_name.map(|class| Member {
                        class,
                        is_static: false,
                    }),
                    artifact,
                )?;
            }
            return Ok(());
        }
        "method_definition" => {
            if class_name.is_some() && !is_accessor(node) {
                add_function(
                    path,
                    source,
                    node,
                    None,
                    class_name.map(|class| Member {
                        class,
                        is_static: is_static(node),
                    }),
                    artifact,
                )?;
            }
            return Ok(());
        }
        "public_field_definition" => {
            let (Some(class_name), Some(name), Some(value)) = (
                class_name,
                node.child_by_field_name("name"),
                node.child_by_field_name("value"),
            ) else {
                return Ok(());
            };
            if !matches!(
                name.kind(),
                "property_identifier" | "private_property_identifier"
            ) || !is_function_value(value)
            {
                return Ok(());
            }
            add_function(
                path,
                source,
                value,
                Some(text(path, source, name)?),
                Some(Member {
                    class: class_name,
                    is_static: is_static(node),
                }),
                artifact,
            )?;
            return Ok(());
        }
        "class_declaration" | "abstract_class_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return Ok(());
            };
            let name = text(path, source, name_node)?;
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    definitions(path, source, child, Some(name), artifact)?;
                }
            }
            return Ok(());
        }
        "arrow_function" | "function_expression" | "generator_function" => return Ok(()),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        definitions(path, source, child, class_name, artifact)?;
    }
    Ok(())
}

fn add_function(
    path: &str,
    source: &str,
    node: Node<'_>,
    explicit_name: Option<&str>,
    member: Option<Member<'_>>,
    artifact: &mut FileArtifact,
) -> Result<(), AnalyzeError> {
    let name = if let Some(name) = explicit_name {
        name
    } else {
        let name_node =
            node.child_by_field_name("name")
                .ok_or_else(|| AnalyzeError::Unsupported {
                    path: path.to_owned(),
                    message: format!("{} has no name", node.kind()),
                })?;
        text(path, source, name_node)?
    };
    let body = node
        .child_by_field_name("body")
        .ok_or_else(|| AnalyzeError::Unsupported {
            path: path.to_owned(),
            message: format!("function {name} has no body"),
        })?;
    let mut qualified_name = Vec::new();
    if let Some(member) = member {
        qualified_name.push(member.class.to_owned());
        if member.is_static {
            qualified_name.push("[static]".to_owned());
        }
    }
    qualified_name.push(name.to_owned());
    let bindings = bindings(path, source, node, body)?;
    artifact.symbols.push(LocalSymbol {
        key: SymbolKey {
            language: Language::TypeScript,
            file: path.to_owned(),
            qualified_name,
        },
        module: Vec::new(),
        location: range(node),
        visibility: Visibility::Private,
        shape: shape(path, source, definition_node(node))?,
        steps: steps(path, source, body, &bindings)?,
    });
    Ok(())
}

fn steps(
    path: &str,
    source: &str,
    node: Node<'_>,
    bindings: &BTreeSet<String>,
) -> Result<Vec<LocalStep>, AnalyzeError> {
    let mut output = Vec::new();
    collect_steps(path, source, node, bindings, &mut output)?;
    Ok(output)
}

fn collect_steps(
    path: &str,
    source: &str,
    node: Node<'_>,
    bindings: &BTreeSet<String>,
    output: &mut Vec<LocalStep>,
) -> Result<(), AnalyzeError> {
    if is_nested_function(node.kind()) {
        return Ok(());
    }

    let mut scoped = None;
    if is_lexical_scope(node.kind()) {
        let mut names = bindings.clone();
        scope_bindings(path, source, node, &mut names)?;
        scoped = Some(names);
    }
    let bindings = scoped.as_ref().unwrap_or(bindings);
    if node.kind() == "if_statement" {
        collect_if(path, source, node, false, bindings, output)?;
        return Ok(());
    }
    if node.kind() == "call_expression" {
        if let Some(call) = collect_call(path, source, node, bindings)? {
            output.push(LocalStep::Call(call));
        }
    } else if node.kind() == "new_expression"
        && let Some(call) = collect_constructor(path, source, node, bindings)?
    {
        output.push(LocalStep::Call(call));
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_steps(path, source, child, bindings, output)?;
    }
    Ok(())
}

fn collect_if(
    path: &str,
    source: &str,
    node: Node<'_>,
    else_if: bool,
    bindings: &BTreeSet<String>,
    output: &mut Vec<LocalStep>,
) -> Result<(), AnalyzeError> {
    let condition =
        node.child_by_field_name("condition")
            .ok_or_else(|| AnalyzeError::Unsupported {
                path: path.to_owned(),
                message: "if statement has no condition".to_owned(),
            })?;
    collect_steps(path, source, condition, bindings, output)?;
    let condition_text = compact(text(path, source, condition)?);
    let kind = if else_if { "else-if" } else { "if" };
    let label = if else_if { "else if" } else { "if" };
    let consequence =
        node.child_by_field_name("consequence")
            .ok_or_else(|| AnalyzeError::Unsupported {
                path: path.to_owned(),
                message: "if statement has no consequence".to_owned(),
            })?;
    output.push(LocalStep::Branch {
        key: format!("{kind}:{condition_text}"),
        label: format!("{label} {condition_text}"),
        location: range(condition),
        children: steps(path, source, consequence, bindings)?,
    });

    let Some(alternative) = node.child_by_field_name("alternative") else {
        return Ok(());
    };
    let Some(inner) = first_named(alternative) else {
        return Ok(());
    };
    if inner.kind() == "if_statement" {
        collect_if(path, source, inner, true, bindings, output)?;
    } else {
        output.push(LocalStep::Branch {
            key: "else".to_owned(),
            label: "else".to_owned(),
            location: range(alternative),
            children: steps(path, source, inner, bindings)?,
        });
    }
    Ok(())
}

fn collect_call(
    path: &str,
    source: &str,
    node: Node<'_>,
    bindings: &BTreeSet<String>,
) -> Result<Option<CallSite>, AnalyzeError> {
    let Some(function) = node.child_by_field_name("function") else {
        return Ok(None);
    };
    let Some(target) = call_target(path, source, function, bindings)? else {
        return Ok(None);
    };
    Ok(Some(CallSite {
        target,
        location: range(node),
    }))
}

fn collect_constructor(
    path: &str,
    source: &str,
    node: Node<'_>,
    bindings: &BTreeSet<String>,
) -> Result<Option<CallSite>, AnalyzeError> {
    let Some(constructor) = node.child_by_field_name("constructor") else {
        return Ok(None);
    };
    let Some(parts) = target_parts(path, source, constructor)? else {
        return Ok(None);
    };
    let target = if parts.len() == 1 && bindings.contains(&parts[0]) {
        CallTarget::Bound(parts[0].clone())
    } else {
        CallTarget::Constructor(parts)
    };
    Ok(Some(CallSite {
        target,
        location: range(node),
    }))
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

fn target_parts(
    path: &str,
    source: &str,
    node: Node<'_>,
) -> Result<Option<Vec<String>>, AnalyzeError> {
    match node.kind() {
        "identifier" => Ok(Some(vec![text(path, source, node)?.to_owned()])),
        "member_expression" => Ok(Some(
            text(path, source, node)?
                .split('.')
                .map(str::to_owned)
                .collect(),
        )),
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
    if let Some(parameter) = function.child_by_field_name("parameter") {
        binding_names(path, source, parameter, &mut output)?;
    }
    function_bindings(path, source, body, &mut output)?;
    Ok(output)
}

fn function_bindings(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut BTreeSet<String>,
) -> Result<(), AnalyzeError> {
    if is_nested_function(node.kind()) {
        return Ok(());
    }
    if node.kind() == "variable_declaration" {
        declaration_bindings(path, source, node, output)?;
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        function_bindings(path, source, child, output)?;
    }
    Ok(())
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
    if matches!(node.kind(), "identifier" | "rest_pattern") {
        binding_names(path, source, node, output)?;
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        parameter_bindings(path, source, child, output)?;
    }
    Ok(())
}

fn scope_bindings(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut BTreeSet<String>,
) -> Result<(), AnalyzeError> {
    if node.kind() == "catch_clause"
        && let Some(parameter) = node.child_by_field_name("parameter")
    {
        binding_names(path, source, parameter, output)?;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "lexical_declaration" | "variable_declaration" => {
                declaration_bindings(path, source, child, output)?;
            }
            "function_declaration" | "generator_function_declaration" | "class_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    binding_names(path, source, name, output)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn declaration_bindings(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut BTreeSet<String>,
) -> Result<(), AnalyzeError> {
    if node.kind() == "variable_declarator" {
        if let Some(name) = node.child_by_field_name("name") {
            binding_names(path, source, name, output)?;
        }
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        declaration_bindings(path, source, child, output)?;
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

fn definition_node(node: Node<'_>) -> Node<'_> {
    node.parent()
        .filter(|parent| {
            matches!(
                parent.kind(),
                "variable_declarator" | "public_field_definition" | "export_statement"
            )
        })
        .unwrap_or(node)
}

fn is_function_value(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "arrow_function" | "function_expression" | "generator_function"
    )
}

fn is_lexical_scope(kind: &str) -> bool {
    matches!(
        kind,
        "statement_block" | "catch_clause" | "switch_body" | "for_statement" | "for_in_statement"
    )
}

fn is_nested_function(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "generator_function_declaration"
            | "arrow_function"
            | "function_expression"
            | "generator_function"
            | "method_definition"
    )
}

fn is_accessor(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| matches!(child.kind(), "get" | "set"))
}

fn is_static(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "static")
}
