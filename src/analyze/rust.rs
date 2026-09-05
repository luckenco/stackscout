use tree_sitter::Node;

use super::{AnalyzeError, compact, first_named, range, shape, text};
use crate::model::{
    CallSite, CallTarget, FileArtifact, Language, LocalStep, LocalSymbol, RustImport, SymbolKey,
    Visibility,
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
        rust_imports: Vec::new(),
        exports: Vec::new(),
    };
    imports(path, source, root, &[], root, &mut artifact.rust_imports)?;
    definitions(path, source, root, &[], None, &mut artifact)?;
    Ok(artifact)
}

fn imports<'tree>(
    path: &str,
    source: &str,
    node: Node<'tree>,
    module: &[String],
    mut scope: Node<'tree>,
    output: &mut Vec<RustImport>,
) -> Result<(), AnalyzeError> {
    if node.kind() == "mod_item" {
        if let Some(body) = node.child_by_field_name("body") {
            let mut nested = module.to_vec();
            nested.push(required_field(path, source, node, "name")?.to_owned());
            imports(path, source, body, &nested, body, output)?;
        }
        return Ok(());
    }
    if node.kind() == "use_declaration" {
        if let Some(argument) = node.child_by_field_name("argument") {
            use_tree(path, source, argument, &[], module, scope, output)?;
        }
        return Ok(());
    }
    if matches!(node.kind(), "source_file" | "declaration_list" | "block") {
        scope = node;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        imports(path, source, child, module, scope, output)?;
    }
    Ok(())
}

fn use_tree(
    path: &str,
    source: &str,
    node: Node<'_>,
    prefix: &[String],
    module: &[String],
    scope: Node<'_>,
    output: &mut Vec<RustImport>,
) -> Result<(), AnalyzeError> {
    match node.kind() {
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                use_tree(path, source, child, prefix, module, scope, output)?;
            }
        }
        "scoped_use_list" => {
            let mut prefix = prefix.to_vec();
            if let Some(parent) = node.child_by_field_name("path") {
                prefix.extend(use_path(path, source, parent)?);
            } else {
                prefix.push(String::new()); // Leading `::` denotes an external path.
            }
            if let Some(list) = node.child_by_field_name("list") {
                use_tree(path, source, list, &prefix, module, scope, output)?;
            }
        }
        "identifier" | "scoped_identifier" | "self" | "crate" | "super" | "use_as_clause" => {
            let mut target = node;
            if node.kind() == "use_as_clause" {
                let Some(aliased) = node.child_by_field_name("path") else {
                    return Ok(());
                };
                target = aliased;
            }
            let mut parts = prefix.to_vec();
            parts.extend(use_path(path, source, target)?);
            if parts.last().is_some_and(|part| part == "self") && parts.len() > 1 {
                parts.pop();
            }
            let local = if let Some(alias) = node.child_by_field_name("alias") {
                text(path, source, alias)?.to_owned()
            } else {
                parts.last().cloned().unwrap_or_default()
            };
            if local != "_" && !local.is_empty() {
                output.push(RustImport {
                    module: module.to_vec(),
                    scope: range(scope),
                    local,
                    path: parts,
                });
            }
        }
        "use_wildcard" => output.push(RustImport {
            module: module.to_vec(),
            scope: range(scope),
            local: "*".to_owned(),
            path: Vec::new(),
        }),
        _ => {} // Macro-generated bindings are not expanded.
    }
    Ok(())
}

fn use_path(path: &str, source: &str, node: Node<'_>) -> Result<Vec<String>, AnalyzeError> {
    if node.kind() == "scoped_identifier" {
        let mut parts = if let Some(parent) = node.child_by_field_name("path") {
            use_path(path, source, parent)?
        } else {
            vec![String::new()]
        };
        parts.push(required_field(path, source, node, "name")?.to_owned());
        return Ok(parts);
    }
    Ok(vec![text(path, source, node)?.to_owned()])
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
    let Some(mut target) = call_target(path, source, function)? else {
        return Ok(None);
    };
    match &target {
        CallTarget::Bare(name) if bound(path, source, node, name, false)? => {
            target = CallTarget::Bound(name.clone());
        }
        CallTarget::Path(parts)
            if !parts.is_empty() && bound(path, source, node, &parts[0], true)? =>
        {
            target = CallTarget::Bound(parts.join("::"));
        }
        _ => {}
    }
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
            Ok(Some(CallTarget::Method(parts)))
        }
        _ => Ok(None),
    }
}

fn bound(
    path: &str,
    source: &str,
    call: Node<'_>,
    name: &str,
    type_path: bool,
) -> Result<bool, AnalyzeError> {
    let mut ancestor = call.parent();
    while let Some(node) = ancestor {
        if node.kind() == "function_item" {
            if type_path && type_binds(path, source, node, name)? {
                return Ok(true);
            }
            if type_path
                && let Some(owner) = node.parent().and_then(|body| body.parent())
                && owner.kind() == "impl_item"
                && type_binds(path, source, owner, name)?
            {
                return Ok(true);
            }
            if !type_path && let Some(parameters) = node.child_by_field_name("parameters") {
                let mut cursor = parameters.walk();
                for parameter in parameters.named_children(&mut cursor) {
                    if let Some(pattern) = parameter.child_by_field_name("pattern")
                        && pattern_binds(path, source, pattern, name)?
                    {
                        return Ok(true);
                    }
                }
            }
            break;
        }
        if node.kind() == "block" {
            let mut cursor = node.walk();
            for item in node.named_children(&mut cursor) {
                if !type_path
                    && item.kind() == "let_declaration"
                    && item.end_byte() <= call.start_byte()
                    && let Some(pattern) = item.child_by_field_name("pattern")
                    && pattern_binds(path, source, pattern, name)?
                {
                    return Ok(true);
                }
                let declaration = if type_path {
                    matches!(
                        item.kind(),
                        "struct_item"
                            | "enum_item"
                            | "union_item"
                            | "type_item"
                            | "mod_item"
                            | "trait_item"
                    )
                } else {
                    matches!(item.kind(), "function_item" | "const_item" | "static_item")
                };
                if declaration && required_field(path, source, item, "name")? == name {
                    return Ok(true);
                }
            }
        }
        if !type_path
            && node.kind() == "match_arm"
            && let Some(pattern) = node.child_by_field_name("pattern")
            && pattern_binds(path, source, pattern, name)?
        {
            return Ok(true);
        }
        if !type_path
            && node.kind() == "for_expression"
            && node
                .child_by_field_name("body")
                .is_some_and(|body| call.start_byte() >= body.start_byte())
            && let Some(pattern) = node.child_by_field_name("pattern")
            && pattern_binds(path, source, pattern, name)?
        {
            return Ok(true);
        }
        if !type_path
            && matches!(node.kind(), "if_expression" | "while_expression")
            && node
                .child_by_field_name("alternative")
                .is_none_or(|body| call.start_byte() < body.start_byte())
            && let Some(condition) = node.child_by_field_name("condition")
            && let_binds(path, source, condition, call.start_byte(), name)?
        {
            return Ok(true);
        }
        ancestor = node.parent();
    }
    Ok(false)
}

fn type_binds(path: &str, source: &str, node: Node<'_>, name: &str) -> Result<bool, AnalyzeError> {
    if let Some(parameters) = node.child_by_field_name("type_parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.named_children(&mut cursor) {
            if parameter.kind() == "type_parameter"
                && required_field(path, source, parameter, "name")? == name
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn let_binds(
    path: &str,
    source: &str,
    node: Node<'_>,
    position: usize,
    name: &str,
) -> Result<bool, AnalyzeError> {
    if node.kind() == "let_condition"
        && node.end_byte() <= position
        && let Some(pattern) = node.child_by_field_name("pattern")
        && pattern_binds(path, source, pattern, name)?
    {
        return Ok(true);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let_binds(path, source, child, position, name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn pattern_binds(
    path: &str,
    source: &str,
    node: Node<'_>,
    name: &str,
) -> Result<bool, AnalyzeError> {
    if matches!(node.kind(), "identifier" | "shorthand_field_identifier") {
        return Ok(text(path, source, node)? == name);
    }
    if matches!(
        node.kind(),
        "scoped_identifier" | "scoped_type_identifier" | "generic_type"
    ) {
        return Ok(false);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if Some(child) == node.child_by_field_name("condition")
            || Some(child) == node.child_by_field_name("type")
        {
            continue;
        }
        if pattern_binds(path, source, child, name)? {
            return Ok(true);
        }
    }
    Ok(false)
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
