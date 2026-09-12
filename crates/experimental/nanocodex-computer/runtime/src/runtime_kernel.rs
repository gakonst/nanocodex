//! Parser-backed cell compilation. User declarations retain lexical semantics;
//! completed bindings are carried by value into the next ES module.
use crate::{Error, Result};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    AssignmentTarget, BindingPattern, Expression, ForStatementInit, ForStatementLeft,
    SimpleAssignmentTarget, Statement, VariableDeclaration, VariableDeclarationKind,
};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::{GetSpan, SourceType};
use std::collections::{BTreeMap, BTreeSet};

pub const MODULE: &str = "const k=globalThis.__skyreKernelInternals; export const read=k.read, probe=k.probe, commit=k.commit, start=k.start, finish=k.finish, initialize=k.initialize, resetOutput=k.resetOutput, drainOutput=k.drainOutput, warn=k.warn;";
// Unit tests can enter the actual engine before the installed banner equivalent.
// This control is absent from production builds and is never a Host option.
#[cfg(test)]
std::thread_local! {
    static WITHOUT_AUTOMATIC_SETUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
pub(crate) fn without_automatic_setup<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            WITHOUT_AUTOMATIC_SETUP.with(|value| value.set(self.0));
        }
    }
    let previous = WITHOUT_AUTOMATIC_SETUP.with(|value| value.replace(true));
    let _restore = Restore(previous);
    run()
}

pub struct Cell {
    pub source: String,
    pub result_name: String,
}
fn names(pattern: &BindingPattern<'_>, out: &mut Vec<String>) {
    match pattern {
        BindingPattern::BindingIdentifier(id) => out.push(id.name.to_string()),
        BindingPattern::AssignmentPattern(p) => names(&p.left, out),
        BindingPattern::ObjectPattern(p) => {
            for property in &p.properties {
                names(&property.value, out)
            }
            if let Some(rest) = &p.rest {
                names(&rest.argument, out)
            }
        }
        BindingPattern::ArrayPattern(p) => {
            for element in p.elements.iter().flatten() {
                names(element, out)
            }
            if let Some(rest) = &p.rest {
                names(&rest.argument, out)
            }
        }
    }
}
fn kind(declaration: &VariableDeclaration<'_>) -> &'static str {
    match declaration.kind {
        VariableDeclarationKind::Var => "var",
        VariableDeclarationKind::Const => "const",
        _ => "let",
    }
}
fn collect(declaration: &VariableDeclaration<'_>, bindings: &mut BTreeMap<String, String>) {
    for declarator in &declaration.declarations {
        let mut found = vec![];
        names(&declarator.id, &mut found);
        for name in found {
            bindings
                .entry(name)
                .or_insert_with(|| kind(declaration).into());
        }
    }
}
pub fn compile(code: &str, prior: &[(String, String)], cell: u64, salt: &str) -> Result<Cell> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(Error::new(-32004, error.to_string()));
    }
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&parsed.program);
    if let Some(error) = semantic.diagnostics.first() {
        return Err(Error::new(-32004, error.to_string()));
    }
    let mut current = BTreeMap::new();
    for statement in &parsed.program.body {
        match statement {
            Statement::VariableDeclaration(declaration) => collect(declaration, &mut current),
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = &function.id {
                    current.insert(id.name.to_string(), "function".into());
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = &class.id {
                    current.insert(id.name.to_string(), "class".into());
                }
            }
            Statement::ForStatement(loop_) => {
                if let Some(ForStatementInit::VariableDeclaration(declaration)) = &loop_.init
                    && declaration.kind == VariableDeclarationKind::Var
                {
                    collect(declaration, &mut current)
                }
            }
            Statement::ForInStatement(loop_) => {
                if let ForStatementLeft::VariableDeclaration(declaration) = &loop_.left
                    && declaration.kind == VariableDeclarationKind::Var
                {
                    collect(declaration, &mut current)
                }
            }
            Statement::ForOfStatement(loop_) => {
                if let ForStatementLeft::VariableDeclaration(declaration) = &loop_.left
                    && declaration.kind == VariableDeclarationKind::Var
                {
                    collect(declaration, &mut current)
                }
            }
            Statement::ImportDeclaration(_) => {
                return Err(Error::new(
                    -32004,
                    "Top-level static imports are unavailable; use await import(...)",
                ));
            }
            Statement::ExportNamedDeclaration(_)
            | Statement::ExportDefaultDeclaration(_)
            | Statement::ExportAllDeclaration(_) => {
                return Err(Error::new(
                    -32004,
                    "Cell exports are managed by the persistent runtime",
                ));
            }
            _ => (),
        }
    }
    if current.len() + prior.len() > 4096 {
        return Err(Error::new(
            -32004,
            "Persistent binding budget exceeded (4096 names)",
        ));
    }
    let scoping = semantic.semantic.scoping();
    let mut reassigned = BTreeSet::new();
    for (name, kind) in prior {
        if kind == "const"
            && !current.contains_key(name)
            && scoping
                .root_unresolved_references()
                .get(name.as_str())
                .is_some_and(|references| {
                    references
                        .iter()
                        .any(|id| scoping.get_reference(*id).is_write())
                })
        {
            reassigned.insert(name.clone());
        }
    }
    // The installed warning walker does not treat catch parameters as shadows.
    // Preserve its observed warning while leaving actual catch lexical behavior intact.
    for symbol in scoping.symbol_ids() {
        let name = scoping.symbol_name(symbol);
        if scoping.symbol_flags(symbol).is_catch_variable()
            && !current.contains_key(name)
            && prior
                .iter()
                .any(|(old, kind)| old == name && kind == "const")
            && scoping
                .get_resolved_references(symbol)
                .any(|reference| reference.is_write())
        {
            reassigned.insert(name.into());
        }
    }
    let prefix = format!("__skyre_{salt}_{cell}_");
    let api = format!("{prefix}api");
    let result_name = format!("{prefix}result");
    let commit = |name: &str| {
        format!(
            "{api}.commit({}, {}, ()=>{name}, {cell})",
            serde_json::to_string(name).unwrap(),
            serde_json::to_string(current.get(name).map(String::as_str).unwrap_or("let")).unwrap()
        )
    };
    let setup_prelude = format!("await {api}.initialize();\n");
    #[cfg(test)]
    let setup_prelude = if WITHOUT_AUTOMATIC_SETUP.with(std::cell::Cell::get) {
        String::new()
    } else {
        setup_prelude
    };
    let mut prelude = format!(
        "import * as {api} from 'skyre:kernel';\n{api}.start({cell}); {api}.resetOutput();\n{setup_prelude}let {result_name};\n"
    );
    for (name, kind) in prior {
        if current.contains_key(name) {
            continue;
        }
        let keyword = if kind == "var" {
            "var"
        } else if kind == "const" && !reassigned.contains(name) {
            "const"
        } else {
            "let"
        };
        let literal = serde_json::to_string(name).unwrap();
        prelude.push_str(&format!("{keyword} {name}={api}.read({literal});{api}.commit({literal},{},()=>{name},{cell});\n",serde_json::to_string(kind).unwrap()));
    }
    for (name, kind) in &current {
        prelude.push_str(&format!(
            "{api}.probe({},{},()=>{name},{cell});\n",
            serde_json::to_string(name).unwrap(),
            serde_json::to_string(kind).unwrap()
        ));
    }
    let mut warnings = reassigned;
    for (name, kind) in &current {
        if kind == "const"
            && prior
                .iter()
                .any(|(old, kind)| old == name && kind == "const")
        {
            warnings.insert(name.clone());
        }
    }
    for name in warnings {
        prelude.push_str(&format!(
            "{api}.warn({});\n",
            serde_json::to_string(&format!(
                "Warning: {name} was declared with const; use let for reassignable variables."
            ))
            .unwrap()
        ));
    }
    let mut edits: Vec<(usize, String)> = vec![];
    let mut declarations = vec![];
    for statement in &parsed.program.body {
        match statement {
            Statement::VariableDeclaration(declaration) => declarations.push(declaration.as_ref()),
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = &function.id {
                    edits.push((
                        function.span.end as usize,
                        format!("\n;{};", commit(id.name.as_str())),
                    ));
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = &class.id {
                    edits.push((
                        class.span.end as usize,
                        format!("\n;{};", commit(id.name.as_str())),
                    ));
                }
            }
            Statement::ForStatement(loop_) => {
                if let Some(ForStatementInit::VariableDeclaration(declaration)) = &loop_.init
                    && declaration.kind == VariableDeclarationKind::Var
                {
                    declarations.push(declaration.as_ref())
                }
            }
            Statement::ForInStatement(loop_) => {
                if let ForStatementLeft::VariableDeclaration(declaration) = &loop_.left
                    && declaration.kind == VariableDeclarationKind::Var
                {
                    let mut found = vec![];
                    for d in &declaration.declarations {
                        names(&d.id, &mut found)
                    }
                    edits.push((
                        loop_.body.span().start as usize,
                        format!(
                            "{{{};",
                            found
                                .iter()
                                .map(|n| commit(n))
                                .collect::<Vec<_>>()
                                .join(";")
                        ),
                    ));
                    edits.push((loop_.body.span().end as usize, "}".into()));
                }
            }
            Statement::ForOfStatement(loop_) => {
                if let ForStatementLeft::VariableDeclaration(declaration) = &loop_.left
                    && declaration.kind == VariableDeclarationKind::Var
                {
                    let mut found = vec![];
                    for d in &declaration.declarations {
                        names(&d.id, &mut found)
                    }
                    edits.push((
                        loop_.body.span().start as usize,
                        format!(
                            "{{{};",
                            found
                                .iter()
                                .map(|n| commit(n))
                                .collect::<Vec<_>>()
                                .join(";")
                        ),
                    ));
                    edits.push((loop_.body.span().end as usize, "}".into()));
                }
            }
            _ => (),
        }
    }
    let mut future_vars = BTreeMap::new();
    for declaration in &declarations {
        if declaration.kind == VariableDeclarationKind::Var {
            for declarator in &declaration.declarations {
                let mut found = vec![];
                names(&declarator.id, &mut found);
                for name in found {
                    future_vars.entry(name).or_insert(declarator.span.start);
                }
            }
        }
    }
    for statement in &parsed.program.body {
        if let Statement::ExpressionStatement(statement) = statement {
            let mut expression = &statement.expression;
            while let Expression::ParenthesizedExpression(parenthesized) = expression {
                expression = &parenthesized.expression;
            }
            match expression {
                Expression::AssignmentExpression(assignment) => {
                    if let AssignmentTarget::AssignmentTargetIdentifier(id) = &assignment.left
                        && future_vars
                            .get(id.name.as_str())
                            .is_some_and(|start| id.span.start < *start)
                    {
                        let name = id.name.as_str();
                        if assignment.operator.is_logical() {
                            let temp = format!("{prefix}logical_{}", assignment.span.start);
                            prelude.push_str(&format!("let {temp};\n"));
                            edits.push((
                                assignment.right.span().start as usize,
                                format!("({temp}=("),
                            ));
                            edits.push((
                                assignment.right.span().end as usize,
                                format!("),{},{temp})", commit(name)),
                            ));
                        } else {
                            edits.push((assignment.span.start as usize, "(".into()));
                            edits.push((
                                assignment.span.end as usize,
                                format!(",{},{name})", commit(name)),
                            ));
                        }
                    }
                }
                Expression::UpdateExpression(update) => {
                    if let SimpleAssignmentTarget::AssignmentTargetIdentifier(id) = &update.argument
                        && future_vars
                            .get(id.name.as_str())
                            .is_some_and(|start| id.span.start < *start)
                    {
                        edits.push((
                            update.span.start as usize,
                            format!("({},", commit(id.name.as_str())),
                        ));
                        edits.push((update.span.end as usize, ")".into()));
                    }
                }
                _ => (),
            }
        }
    }
    for declaration in declarations {
        for declarator in &declaration.declarations {
            let mut found = vec![];
            names(&declarator.id, &mut found);
            let markers = found
                .iter()
                .map(|n| commit(n))
                .collect::<Vec<_>>()
                .join(",");
            edits.push((
                declarator.span.end as usize,
                format!(
                    ", {prefix}commit_{}=({markers},undefined)",
                    declarator.span.start
                ),
            ));
        }
    }
    if let Some(Statement::ExpressionStatement(expression)) = parsed.program.body.last() {
        edits.push((
            expression.expression.span().start as usize,
            format!("{result_name}=("),
        ));
        edits.push((expression.expression.span().end as usize, ")".into()));
    }
    // A declaration ending where the next expression begins must commit before
    // that expression. Inserts are applied backwards at a shared source offset.
    edits.sort_by_key(|edit| {
        (
            std::cmp::Reverse(edit.0),
            edit.1.starts_with("\n;") || edit.1 == "}",
        )
    });
    let mut body = code.to_string();
    for (position, text) in edits {
        body.insert_str(position, &text)
    }
    let source =
        format!("{prelude}\n{body}\n;{api}.finish(true,{cell});\nexport {{{result_name}}};");
    Ok(Cell {
        source,
        result_name,
    })
}
