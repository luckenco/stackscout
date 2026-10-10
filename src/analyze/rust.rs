#[cfg(test)]
mod tests;

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
        rust_modules: Vec::new(),
        exports: Vec::new(),
        unsupported: Default::default(),
    };
    imports(path, source, root, &[], root, &mut artifact.rust_imports)?;
    modules(path, source, root, &[], &mut artifact.rust_modules)?;
    definitions(path, source, root, &[], &[], &[], &mut artifact)?;
    unsupported(path, source, root, &mut artifact.unsupported)?;
    for shapes in artifact.unsupported.values_mut() {
        shapes.sort();
    }
    Ok(artifact)
}

fn modules(
    path: &str,
    source: &str,
    node: Node<'_>,
    current: &[String],
    output: &mut Vec<(Vec<String>, String)>,
) -> Result<(), AnalyzeError> {
    if node.kind() == "mod_item" {
        let name = required_field(path, source, node, "name")?.to_owned();
        if let Some(body) = node.child_by_field_name("body") {
            let mut nested = current.to_vec();
            nested.push(name);
            return modules(path, source, body, &nested, output);
        }
        // `#[path]` and macro-expanded declarations need separate handling.
        if node.prev_named_sibling().is_some_and(|previous| {
            previous.kind() == "attribute_item"
                && text(path, source, previous).is_ok_and(|value| {
                    value.trim_start().starts_with("#[path")
                        || value.trim_start().starts_with("#[cfg_attr")
                })
        }) {
            return Ok(());
        }
        output.push((current.to_vec(), name));
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        modules(path, source, child, current, output)?;
    }
    Ok(())
}

fn unsupported(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut std::collections::BTreeMap<&'static str, Vec<String>>,
) -> Result<(), AnalyzeError> {
    let category = match node.kind() {
        "const_item" => Some("const"),
        "macro_definition" => Some("macro"),
        "struct_item" => Some("struct"),
        _ => None,
    };
    if let Some(category) = category {
        let target = if category == "struct" {
            node.child_by_field_name("body")
        } else {
            Some(node)
        };
        if let Some(target) = target {
            let mut value = String::new();
            if category == "struct"
                && let Some(name) = node.child_by_field_name("name")
            {
                item_shape(path, source, name, &mut value)?;
            }
            item_shape(path, source, target, &mut value)?;
            output.entry(category).or_default().push(value);
        }
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        unsupported(path, source, child, output)?;
    }
    Ok(())
}

fn item_shape(
    path: &str,
    source: &str,
    node: Node<'_>,
    output: &mut String,
) -> Result<(), AnalyzeError> {
    if matches!(node.kind(), "line_comment" | "block_comment") {
        return Ok(());
    }
    if node.child_count() == 0 {
        output.push_str(node.kind());
        output.push(':');
        output.push_str(text(path, source, node)?);
        output.push('|');
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        item_shape(path, source, child, output)?;
    }
    Ok(())
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
                spelling(path, source, alias)?
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
    if matches!(node.kind(), "scoped_identifier" | "scoped_type_identifier") {
        let mut parts = if let Some(parent) = node.child_by_field_name("path") {
            use_path(path, source, parent)?
        } else {
            vec![String::new()]
        };
        parts.push(required_field(path, source, node, "name")?.to_owned());
        return Ok(parts);
    }
    if node.kind() == "generic_type"
        && let (Some(base), Some(arguments)) = (
            node.child_by_field_name("type"),
            node.child_by_field_name("type_arguments"),
        )
    {
        let mut parts = use_path(path, source, base)?;
        parts
            .last_mut()
            .expect("type path has a name")
            .push_str(&spelling(path, source, arguments)?);
        return Ok(parts);
    }
    Ok(vec![spelling(path, source, node)?])
}

// Token spelling, not whitespace deletion: literals retain their contents and
// adjacent words (notably `as` in UFCS) must remain separate tokens.
fn spelling(path: &str, source: &str, node: Node<'_>) -> Result<String, AnalyzeError> {
    fn visit(
        path: &str,
        source: &str,
        node: Node<'_>,
        output: &mut String,
    ) -> Result<(), AnalyzeError> {
        if matches!(node.kind(), "line_comment" | "block_comment") {
            return Ok(());
        }
        if node.child_count() == 0 || node.kind().ends_with("_literal") {
            let token = text(path, source, node)?;
            let token = if matches!(
                node.kind(),
                "identifier" | "type_identifier" | "field_identifier"
            ) {
                token.strip_prefix("r#").unwrap_or(token)
            } else {
                token
            };
            let word = |ch: char| ch.is_alphanumeric() || ch == '_';
            if output.ends_with(word) && token.starts_with(word) {
                output.push(' ');
            }
            output.push_str(token);
            return Ok(());
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(path, source, child, output)?;
        }
        Ok(())
    }
    let mut output = String::new();
    visit(path, source, node, &mut output)?;
    Ok(output)
}

fn conditions(path: &str, source: &str, node: Node<'_>) -> Result<Vec<String>, AnalyzeError> {
    let mut conditions = Vec::new();
    let mut previous = node.prev_named_sibling();
    while let Some(attribute) = previous {
        if !matches!(
            attribute.kind(),
            "attribute_item" | "line_comment" | "block_comment"
        ) {
            break;
        }
        if attribute.kind() == "attribute_item" {
            let value = spelling(path, source, attribute)?;
            if value.starts_with("#[cfg(") || value.starts_with("#[cfg_attr(") {
                conditions.push(value);
            }
        }
        previous = attribute.prev_named_sibling();
    }
    conditions.reverse();
    Ok(conditions)
}

fn definitions(
    path: &str,
    source: &str,
    node: Node<'_>,
    modules: &[String],
    owner: &[String],
    inherited_conditions: &[String],
    artifact: &mut FileArtifact,
) -> Result<(), AnalyzeError> {
    let mut conditions = inherited_conditions.to_vec();
    conditions.extend(self::conditions(path, source, node)?);
    match node.kind() {
        "trait_item" => {
            let name = required_field(path, source, node, "name")?;
            if let Some(body) = node.child_by_field_name("body") {
                definitions(
                    path,
                    source,
                    body,
                    modules,
                    &[format!("<trait {name}>")],
                    &conditions,
                    artifact,
                )?;
            }
            return Ok(());
        }
        "function_item" => {
            let name = required_field(path, source, node, "name")?;
            let body =
                node.child_by_field_name("body")
                    .ok_or_else(|| AnalyzeError::Unsupported {
                        path: path.to_owned(),
                        message: format!("function {name} has no body"),
                    })?;
            let mut qualified_name = modules.to_vec();
            qualified_name.extend_from_slice(owner);
            qualified_name.push(name.to_owned());
            artifact.symbols.push(LocalSymbol {
                key: SymbolKey {
                    language: Language::Rust,
                    file: path.to_owned(),
                    qualified_name,
                    conditions,
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
            let owner = if let Some(trait_node) = node.child_by_field_name("trait") {
                let type_name = spelling(path, source, type_node)?;
                let trait_name = spelling(path, source, trait_node)?;
                vec![format!("<{type_name} as {trait_name}>")]
            } else {
                use_path(path, source, type_node)?
            };
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    definitions(path, source, child, modules, &owner, &conditions, artifact)?;
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
                nested.push(spelling(path, source, name)?);
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    definitions(path, source, child, &nested, owner, &conditions, artifact)?;
                }
                return Ok(());
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        definitions(path, source, child, modules, owner, &conditions, artifact)?;
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
    let boundary = match node.kind() {
        "closure_expression" => Some("closure body omitted".to_owned()),
        "async_block" => Some("async body deferred".to_owned()),
        "function_item" | "impl_item" | "mod_item" => Some(format!(
            "nested {} body omitted",
            node.kind().trim_end_matches("_item")
        )),
        "macro_invocation" => Some(format!(
            "macro {}! unexpanded",
            required_field(path, source, node, "macro")?
        )),
        _ => None,
    };
    if let Some(boundary) = boundary {
        output.push(LocalStep::Call(CallSite {
            target: CallTarget::Opaque(format!("[{boundary}]")),
            location: range(node),
        }));
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
        CallTarget::Method(parts) if parts.len() == 2 => {
            if let Some(owner) = receiver_type(path, source, node, &parts[0])?
                && !bound(path, source, node, &owner[0], true)?
            {
                target = CallTarget::TypedMethod {
                    receiver: parts[0].clone(),
                    owner,
                    method: parts[1].clone(),
                };
            }
        }
        _ => {}
    }
    Ok(Some(CallSite {
        target,
        location: range(node),
    }))
}

// Only direct identifiers with explicit nominal annotations are inferred. A nearer
// untyped binding blocks an outer typed one; expression inference is left unknown.
fn receiver_type(
    path: &str,
    source: &str,
    call: Node<'_>,
    name: &str,
) -> Result<Option<Vec<String>>, AnalyzeError> {
    let mut ancestor = call.parent();
    while let Some(node) = ancestor {
        if node.kind() == "match_arm"
            && let Some(pattern) = node.child_by_field_name("pattern")
            && pattern_binds(path, source, pattern, name)?
        {
            return Ok(None);
        }
        if node.kind() == "for_expression"
            && node
                .child_by_field_name("body")
                .is_some_and(|body| call.start_byte() >= body.start_byte())
            && let Some(pattern) = node.child_by_field_name("pattern")
            && pattern_binds(path, source, pattern, name)?
        {
            return Ok(None);
        }
        if matches!(node.kind(), "if_expression" | "while_expression")
            && node
                .child_by_field_name("alternative")
                .is_none_or(|body| call.start_byte() < body.start_byte())
            && let Some(condition) = node.child_by_field_name("condition")
            && let_binds(path, source, condition, call.start_byte(), name)?
        {
            return Ok(None);
        }
        if node.kind() == "closure_expression"
            && let Some(parameters) = node.child_by_field_name("parameters")
        {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                if pattern_binds(path, source, parameter, name)? {
                    return Ok(None);
                }
            }
        }
        if node.kind() == "block" {
            let mut cursor = node.walk();
            let items = node.named_children(&mut cursor).collect::<Vec<_>>();
            for item in items.into_iter().rev() {
                if matches!(item.kind(), "const_item" | "static_item")
                    && required_field(path, source, item, "name")? == name
                {
                    return Ok(None);
                }
                if item.kind() != "let_declaration" || item.end_byte() > call.start_byte() {
                    continue;
                }
                if let Some(pattern) = item.child_by_field_name("pattern")
                    && pattern_binds(path, source, pattern, name)?
                {
                    if pattern.kind() == "identifier"
                        && let Some(annotation) = item.child_by_field_name("type")
                    {
                        return annotated_type(path, source, annotation);
                    }
                    return Ok(None);
                }
            }
        }
        if node.kind() == "function_item" {
            if let Some(parameters) = node.child_by_field_name("parameters") {
                let mut cursor = parameters.walk();
                for parameter in parameters.named_children(&mut cursor) {
                    if let Some(pattern) = parameter.child_by_field_name("pattern")
                        && pattern_binds(path, source, pattern, name)?
                    {
                        if pattern.kind() == "identifier"
                            && let Some(annotation) = parameter.child_by_field_name("type")
                        {
                            return annotated_type(path, source, annotation);
                        }
                        return Ok(None);
                    }
                }
            }
            return Ok(None);
        }
        ancestor = node.parent();
    }
    Ok(None)
}

fn annotated_type(
    path: &str,
    source: &str,
    mut node: Node<'_>,
) -> Result<Option<Vec<String>>, AnalyzeError> {
    if node.kind() == "reference_type" {
        let Some(inner) = node.child_by_field_name("type") else {
            return Ok(None);
        };
        node = inner;
    }
    if matches!(node.kind(), "type_identifier" | "scoped_type_identifier") {
        let parts = use_path(path, source, node)?;
        if parts.iter().all(|part| !part.contains('<')) {
            return Ok(Some(parts));
        }
    }
    Ok(None)
}

fn call_target(
    path: &str,
    source: &str,
    mut node: Node<'_>,
) -> Result<Option<CallTarget>, AnalyzeError> {
    loop {
        let inner = match node.kind() {
            "generic_function" => node.child_by_field_name("function"),
            "parenthesized_expression" => {
                let mut cursor = node.walk();
                node.named_children(&mut cursor)
                    .find(|child| !matches!(child.kind(), "line_comment" | "block_comment"))
            }
            _ => None,
        };
        let Some(inner) = inner else { break };
        node = inner;
    }
    match node.kind() {
        "identifier" => Ok(Some(CallTarget::Bare(spelling(path, source, node)?))),
        "scoped_identifier" => {
            let parts = use_path(path, source, node)?;
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
            let field = spelling(path, source, field)?;
            if text(path, source, value)? == "self" {
                return Ok(Some(CallTarget::SelfMethod(field)));
            }
            let mut parts = field_parts(path, source, value)?;
            parts.push(field);
            Ok(Some(CallTarget::Method(parts)))
        }
        _ => Ok(Some(CallTarget::Opaque(format!(
            "[callee {}]",
            text(path, source, node)?
        )))),
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
                && matches!(owner.kind(), "impl_item" | "trait_item")
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
        let value = text(path, source, node)?;
        return Ok(value.strip_prefix("r#").unwrap_or(value) == name);
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
    let value = text(path, source, child)?;
    Ok(value.strip_prefix("r#").unwrap_or(value))
}

// tree-sitter-rust 0.24 treats the stable `safe` foreign-item modifier as an
// ERROR token. Accept only that token in an otherwise checked unsafe extern
// declaration; other parser errors still invalidate the file.
pub(super) fn safe_foreign(node: Node<'_>, source: &str) -> bool {
    if !node.is_error() || node.utf8_text(source.as_bytes()) != Ok("safe") {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let declaration = if parent.kind() == "declaration_list" {
        let Some(next) = node.next_named_sibling() else {
            return false;
        };
        if !next
            .child(0)
            .is_some_and(|first| matches!(first.kind(), "fn" | "static"))
        {
            return false;
        }
        next
    } else {
        if !node
            .next_sibling()
            .is_some_and(|next| matches!(next.kind(), "fn" | "static"))
        {
            return false;
        }
        parent
    };
    if !matches!(
        declaration.kind(),
        "function_signature_item" | "static_item"
    ) {
        return false;
    }
    declaration
        .parent()
        .and_then(|list| list.parent())
        .is_some_and(|foreign| {
            foreign.kind() == "foreign_mod_item"
                && foreign
                    .child(0)
                    .is_some_and(|first| first.kind() == "unsafe")
        })
}

fn has_child(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| child.kind() == kind)
}
