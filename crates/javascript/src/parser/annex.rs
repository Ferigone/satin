//! Identify ordinary block functions eligible for the Annex B variable binding.
//! A hypothetical var declaration must cross every enclosing lexical scope.
use super::*;

pub(crate) fn mark(body: &mut [Stmt], excluded: impl IntoIterator<Item = JsString>) {
    let mut blocked: BTreeSet<_> = excluded.into_iter().collect();
    let names = lexical_names(body.iter(), false);
    blocked.extend(names);
    for statement in body {
        descend(statement, &mut blocked);
    }
}

fn lexical_names<'a>(statements: impl Iterator<Item = &'a Stmt>, block: bool) -> Vec<JsString> {
    let mut names = Vec::new();
    for statement in statements {
        match statement {
            Stmt::Variable(kind, decls) if *kind != VarKind::Var => {
                names.extend(decls.iter().flat_map(|(pattern, _)| bound_names(pattern)));
            }
            Stmt::Class(name, _) => names.push(name.clone()),
            Stmt::Function(f) if block => names.extend(f.name.iter().cloned()),
            _ => {}
        }
    }
    names
}

fn mark_declaration(statement: &mut Stmt, blocked: &BTreeSet<JsString>) {
    if let Stmt::Function(f) = statement {
        f.annex_binding = f.kind == FunctionKind::Ordinary
            && f.name.as_ref().is_some_and(|name| !blocked.contains(name));
    }
}

fn within(
    names: Vec<JsString>, blocked: &mut BTreeSet<JsString>,
    visit: impl FnOnce(&mut BTreeSet<JsString>),
) {
    let added: Vec<_> = names.into_iter().filter(|name| blocked.insert(name.clone())).collect();
    visit(blocked);
    for name in added { blocked.remove(&name); }
}

fn descend(statement: &mut Stmt, blocked: &mut BTreeSet<JsString>) {
    match statement {
        Stmt::Block(body) => {
            for child in &mut *body { mark_declaration(child, blocked); }
            within(lexical_names(body.iter(), true), blocked, |blocked| {
                for child in body { descend(child, blocked); }
            });
        }
        Stmt::Switch(_, cases) => {
            for child in cases.iter_mut().flat_map(|(_, body)| body) {
                mark_declaration(child, blocked);
            }
            let names = lexical_names(cases.iter().flat_map(|(_, body)| body), true);
            within(names, blocked, |blocked| {
                for child in cases.iter_mut().flat_map(|(_, body)| body) {
                    descend(child, blocked);
                }
            });
        }
        Stmt::If(_, yes, no) => {
            for child in std::iter::once(yes).chain(no) {
                mark_declaration(child, blocked);
                descend(child, blocked);
            }
        }
        Stmt::For(init, _, _, body) => {
            let names = lexical_names(init.as_deref().into_iter(), false);
            within(names, blocked, |b| descend(body, b));
        }
        Stmt::ForIn(kind, pattern, _, body, _) | Stmt::ForOf(kind, pattern, _, body, _) => {
            if matches!(kind, Some(VarKind::Let | VarKind::Const)) {
                within(bound_names(pattern), blocked, |b| descend(body, b));
            } else { descend(body, blocked); }
        }
        Stmt::Try(body, catch, finally) => {
            descend(body, blocked);
            if let Some((pattern, handler)) = catch {
                let names = match pattern {
                    Some(p) if !matches!(p, Pattern::Name(_)) => bound_names(p),
                    _ => Vec::new(),
                };
                within(names, blocked, |b| descend(handler, b));
            }
            if let Some(body) = finally { descend(body, blocked); }
        }
        Stmt::While(_, body) | Stmt::DoWhile(body, _) | Stmt::With(_, body) | Stmt::Label(_, _, body) => {
            descend(body, blocked);
        }
        _ => {}
    }
}
