//! Typed stack bytecode. Structured control instructions contain other compiled
//! chunks; expressions use a conventional operand/reference stack and jumps.
mod classes;
mod assignment_patterns;
pub(crate) use assignment_patterns::PatternStep;
use crate::{
    ErrorKind, JsError, JsString, Value,
    parser::{
        self, ChainLink, Expr, Function, FunctionKind, LabelId, MethodKind, ObjectEntry, Pattern,
        Stmt, TemplatePart, VarKind, bound_names, simple_parameter,
    },
};
pub(crate) use classes::{ClassCode, ClassElementCode, ClassKeyCode};
use std::{cell::RefCell, collections::BTreeSet, rc::Rc};
#[derive(Debug)]
pub(crate) struct TemplateSite {
    pub parts: Vec<TemplatePart>,
}
#[derive(Clone, Debug)]
pub(crate) enum PatternCode {
    Name(JsString),
    Target(Rc<Chunk>, Rc<Chunk>),
    PrivateTarget(Rc<Chunk>, JsString),
    SuperTarget(Rc<Chunk>),
    CallTarget(Rc<Chunk>),
    Array(Vec<Option<Rc<PatternCode>>>, Option<Rc<PatternCode>>),
    Object(Vec<(Rc<Chunk>, Rc<PatternCode>)>, Option<Rc<PatternCode>>),
    Default {
        target: Rc<PatternCode>,
        value: Rc<Chunk>,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct ParameterCode {
    pub pattern: Rc<PatternCode>,
    pub rest: bool,
}
pub(crate) fn pattern_names(pattern: &PatternCode) -> Vec<JsString> {
    let mut out = Vec::new();
    let mut pending = vec![pattern];
    while let Some(pattern) = pending.pop() {
        match pattern {
            PatternCode::Name(name) => out.push(name.clone()),
            PatternCode::Target(..)
            | PatternCode::PrivateTarget(..)
            | PatternCode::SuperTarget(_) | PatternCode::CallTarget(_) => {}
            PatternCode::Array(items, rest) => {
                pending.extend(rest.iter().map(Rc::as_ref));
                pending.extend(items.iter().rev().flatten().map(Rc::as_ref));
            }
            PatternCode::Object(properties, rest) => {
                pending.extend(rest.iter().map(Rc::as_ref));
                pending.extend(properties.iter().rev().map(|(_, p)| p.as_ref()));
            }
            PatternCode::Default { target, .. } => pending.push(target),
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ScopePlan {
    pub lexical: Vec<(JsString, bool)>,
    pub vars: Vec<JsString>,
    pub annex_vars: Vec<JsString>,
    pub functions: Vec<(JsString, Rc<Code>, bool)>,
}
impl ScopePlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.lexical.is_empty() && self.vars.is_empty() && self.functions.is_empty() && self.annex_vars.is_empty()
    }
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Chunk {
    pub ops: Vec<Op>,
    pub scope: ScopePlan,
    pub strict: bool,
    pub can_suspend: bool,
}
impl Chunk {
    fn finish(mut self) -> Rc<Self> {
        self.ops.shrink_to_fit();
        self.can_suspend = self.ops.iter().any(Op::can_suspend);
        Rc::new(self)
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Code {
    pub source: Rc<crate::lexer::SourceSpan>,
    pub name: Option<JsString>,
    pub params: Vec<ParameterCode>,
    pub param_names: Vec<JsString>,
    pub parameter_expressions: bool,
    pub mapped_arguments: bool,
    pub mapped_indices: Vec<usize>,
    pub length: usize,
    pub body: std::cell::OnceCell<Rc<Chunk>>,
    pub strict: bool,
    pub arrow: bool,
    pub kind: FunctionKind,
    pub self_binding: bool,
    pub method: bool,
}
impl Code {
    pub(crate) fn constructible(&self) -> bool {
        !self.arrow && !self.method && self.kind == FunctionKind::Ordinary
    }
}
#[derive(Clone, Debug)]
pub(crate) enum ClassName {
    Intrinsic,
    Binding(JsString),
    Property,
}
/// The full C-style loop header is cold data for every other stack opcode.
#[derive(Clone, Debug)]
pub(crate) struct ForCode {
    pub initial: Option<Rc<Chunk>>,
    pub test: Option<Rc<Chunk>>,
    pub update: Option<Rc<Chunk>>,
    pub body: Rc<Chunk>,
    pub lexical: Vec<(JsString, bool)>,
    pub label: Option<LabelId>,
}
#[derive(Clone, Debug)]
pub(crate) enum Op {
    Push(Value),
    RegExp(Rc<crate::regexp::Program>),
    Name(JsString),
    LoadName(JsString),
    This,
    NewTarget,
    CopyAnnexFunction(JsString),
    ImportMeta,
    DynamicImport(crate::parser::ImportPhase),
    Await,
    Yield {
        delegate: bool,
        asynchronous: bool,
    },
    SuperProperty,
    SuperConstructor,
    SuperConstruct,
    Class(Rc<ClassCode>, ClassName),
    Property,
    NamedProperty(JsString),
    LoadProperty(JsString),
    PrivateProperty(JsString),
    PrivateIn(JsString),
    Get,
    NormalizeReference,
    InvalidCallAssignment,
    Set,
    Delete,
    Update {
        increment: bool,
        prefix: bool,
    },
    Unary(&'static str),
    Binary(&'static str),
    TypeofReference,
    Pop,
    Dup,
    Swap,
    Jump(usize),
    JumpFalse(usize),
    JumpTrue(usize),
    JumpNotNullish(usize),
    Array,
    ArrayElement(bool),
    ArrayHole,
    Object,
    ObjectSpread,
    Destructure(Rc<PatternCode>),
    Pattern(PatternStep),
    TemplateObject(Rc<TemplateSite>),
    TemplateAppend(JsString),
    OptionalCheck(usize, bool),
    PropertyKey,
    ObjectProperty {
        prototype: bool,
        infer_name: bool,
        method: Option<MethodKind>,
    },
    Function(Rc<Code>),
    InferName(JsString),
    PrepareCall(bool),
    Arguments,
    Argument(bool),
    Call,
    Construct,
    /// Store the expression's GetValue as the statement-list completion.
    StatementValue,
    Initialize(VarKind, Rc<PatternCode>, Option<Rc<Chunk>>),
    Block(Rc<Chunk>),
    Label(LabelId, Rc<Chunk>),
    With(Rc<Chunk>, Rc<Chunk>),
    Branch(Rc<[(Rc<Chunk>, Rc<Chunk>)]>, Option<Rc<Chunk>>),
    While(Rc<Chunk>, Rc<Chunk>, Option<LabelId>),
    DoWhile(Rc<Chunk>, Rc<Chunk>, Option<LabelId>),
    For(Box<ForCode>),
    ForIn(
        Option<VarKind>,
        Rc<PatternCode>,
        Rc<Chunk>,
        Rc<Chunk>,
        Option<LabelId>,
    ),
    ForOf(
        Option<VarKind>,
        Rc<PatternCode>,
        Rc<Chunk>,
        Rc<Chunk>,
        bool,
        Option<LabelId>,
    ),
    Return(Option<Rc<Chunk>>),
    Break(Option<LabelId>),
    Continue(Option<LabelId>),
    Throw(Rc<Chunk>),
    Switch(Rc<Chunk>, Vec<(Option<Rc<Chunk>>, Rc<Chunk>)>, Box<ScopePlan>),
    Try(
        Rc<Chunk>,
        Option<(Option<Rc<PatternCode>>, Rc<Chunk>)>,
        Option<Rc<Chunk>>,
    ),
}
impl Op {
    pub(crate) fn loop_label(&self) -> Option<LabelId> {
        match self {
            Self::While(_, _, label)
            | Self::DoWhile(_, _, label)
            | Self::ForIn(_, _, _, _, label)
            | Self::ForOf(_, _, _, _, _, label) => *label,
            Self::For(code) => code.label,
            _ => unreachable!("loop opcode expected"),
        }
    }

    pub(crate) fn can_suspend(&self) -> bool {
        match self {
            Self::Await | Self::Yield { .. } | Self::ForOf(_, _, _, _, true, _) => true,
            Self::Throw(chunk)
            | Self::Block(chunk)
            | Self::Label(_, chunk) => chunk.can_suspend,
            Self::Initialize(_, _, chunk) | Self::Return(chunk) => {
                chunk.as_ref().is_some_and(|chunk| chunk.can_suspend)
            }
            Self::Branch(arms, no) => {
                arms.iter().any(|(test, body)| test.can_suspend || body.can_suspend)
                    || no.as_ref().is_some_and(|chunk| chunk.can_suspend)
            }
            Self::With(test, body)
            | Self::While(test, body, _)
            | Self::DoWhile(body, test, _)
            | Self::ForOf(_, _, test, body, false, _)
            | Self::ForIn(_, _, test, body, _) => test.can_suspend || body.can_suspend,
            Self::For(code) => {
                code.body.can_suspend
                    || [&code.initial, &code.test, &code.update]
                        .into_iter()
                        .flatten()
                        .any(|chunk| chunk.can_suspend)
            }
            Self::Switch(value, cases, _) => {
                value.can_suspend
                    || cases.iter().any(|(test, body)| {
                        body.can_suspend || test.as_ref().is_some_and(|chunk| chunk.can_suspend)
                    })
            }
            Self::Try(body, catch, finally) => {
                body.can_suspend
                    || catch.as_ref().is_some_and(|(_, chunk)| chunk.can_suspend)
                    || finally.as_ref().is_some_and(|chunk| chunk.can_suspend)
            }
            _ => false,
        }
    }
}

pub(crate) fn compile(source: &str, compilation_bytes: usize) -> Result<Rc<Chunk>, JsError> {
    compile_eval(
        crate::lexer::Lexer::new(source, compilation_bytes, true), source.len(),
        false, parser::EvalContext::default(),
    )
}
pub(crate) fn compile_eval(
    lexer: crate::lexer::Lexer,
    source_bytes: usize,
    inherited_strict: bool,
    context: parser::EvalContext,
) -> Result<Rc<Chunk>, JsError> {
    compile_with_phases(lexer, source_bytes, inherited_strict, context, |_| {})
}

pub(crate) fn compile_with_phases(
    lexer: crate::lexer::Lexer,
    source_bytes: usize,
    inherited_strict: bool,
    context: parser::EvalContext,
    mut phase: impl FnMut(&'static str),
) -> Result<Rc<Chunk>, JsError> {
    phase("source");
    let body = parser::parse_script(lexer, source_bytes, context, inherited_strict)?;
    phase("parsed");
    let strict = inherited_strict || parser::has_strict_directive(&body);
    let result = block(&body, true, strict);
    phase("emitted");
    drop(body);
    phase("retained");
    result
}
pub(crate) fn compile_dynamic_function(
    parameters: &str,
    body: &str,
    kind: FunctionKind,
) -> Result<Rc<Code>, JsError> {
    // Parse the two grammar goals separately. Concatenating both into one eval
    // would incorrectly allow parameter text to escape into executable code.
    let head = match kind {
        FunctionKind::Ordinary => "function",
        FunctionKind::Async => "async function",
        FunctionKind::Generator => "function*",
        FunctionKind::AsyncGenerator => "async function*",
    };
    let params = parser::parse(&format!("{head} __parameters({parameters}\n){{}}"))?;
    let [Stmt::Function(parameter_function)] = params.as_slice() else {
        return Err(syntax("Invalid Function constructor parameters"));
    };
    if !parameter_function.body.is_empty() {
        return Err(syntax("Invalid Function constructor parameters"));
    }
    let statements = parser::parse(&format!("{head} anonymous(){{\n{body}\n}}"))?;
    let [Stmt::Function(body_function)] = statements.as_slice() else {
        return Err(syntax("Invalid Function constructor body"));
    };
    let mut function = body_function.clone();
    function.params = parameter_function.params.clone();
    function.source = crate::lexer::SourceSpan::generated(&format!("{head} anonymous({parameters}\n) {{\n{body}\n}}"));
    if !function.strict {
        let excluded = function.params.iter().flat_map(|p| bound_names(&p.pattern)).chain([JsString::from("arguments")]);
        parser::annex::mark(&mut function.body, excluded);
    }
    compile_function(&function, false)
}
fn vector_storage<T>(values: &Vec<T>) -> usize {
    values.capacity() * std::mem::size_of::<T>()
}

fn scope_storage(scope: &ScopePlan) -> usize {
    vector_storage(&scope.lexical)
        + vector_storage(&scope.vars)
        + vector_storage(&scope.annex_vars)
        + vector_storage(&scope.functions)
        + scope.lexical.iter().map(|(name, _)| name)
            .chain(&scope.vars)
            .chain(&scope.annex_vars)
            .chain(scope.functions.iter().map(|(name, _, _)| name))
            .map(JsString::allocation_bytes).sum::<usize>()
}

/// Count the storage retained by compiled code, including spare vector capacity.
/// Code, chunks and source buffers may be reachable from more than one root.
pub(crate) fn retained_size_with_chunks(roots: Vec<Rc<Code>>, chunks: Vec<Rc<Chunk>>) -> usize {
    retained_size(roots, chunks, None)
}

pub(crate) fn retained_size(
    roots: Vec<Rc<Code>>,
    mut chunks: Vec<Rc<Chunk>>,
    shared_source: Option<usize>,
) -> usize {
    let mut functions = roots;
    let mut seen_functions = BTreeSet::new();
    let mut seen_sources: BTreeSet<usize> = shared_source.into_iter().collect();
    let mut seen_chunks = BTreeSet::new();
    const RC_COUNTS: usize = 2 * std::mem::size_of::<usize>();
    let mut bytes = 0usize;
    loop {
        while let Some(code) = functions.pop() {
            if !seen_functions.insert(Rc::as_ptr(&code) as usize) {
                continue;
            }
            let (address, size) = code.source.storage();
            if seen_sources.insert(address) { bytes = bytes.saturating_add(RC_COUNTS + size); }
            bytes = bytes.saturating_add(
                std::mem::size_of::<Code>() + std::mem::size_of::<crate::lexer::SourceSpan>()
                    + 2 * RC_COUNTS
                    + vector_storage(&code.params)
                    + vector_storage(&code.param_names)
                    + vector_storage(&code.mapped_indices)
                    + code.param_names.iter().map(JsString::allocation_bytes).sum::<usize>()
                    + code.name.as_ref().map_or(0, JsString::allocation_bytes),
            );
            chunks.extend(code.body.get().cloned());
            for parameter in &code.params {
                bytes =
                    bytes.saturating_add(pattern_retained_size(&parameter.pattern, &mut chunks));
            }
        }
        let Some(chunk) = chunks.pop() else {
            break;
        };
        if !seen_chunks.insert(Rc::as_ptr(&chunk) as usize) {
            continue;
        }
        bytes = bytes.saturating_add(
            std::mem::size_of::<Chunk>() + RC_COUNTS
                + vector_storage(&chunk.ops)
                + scope_storage(&chunk.scope),
        );
        functions.extend(chunk.scope.functions.iter().map(|(_, c, _)| c.clone()));
        for op in &chunk.ops {
            match op {
                Op::Push(Value::String(s))
                | Op::CopyAnnexFunction(s)
                | Op::Name(s)
                | Op::LoadName(s)
                | Op::NamedProperty(s)
                | Op::LoadProperty(s)
                | Op::PrivateProperty(s)
                | Op::PrivateIn(s)
                | Op::InferName(s)
                | Op::TemplateAppend(s) => bytes = bytes.saturating_add(s.allocation_bytes()),
                Op::Push(Value::BigInt(integer)) => {
                    bytes = bytes.saturating_add(integer.retained_bytes())
                }
                Op::RegExp(program) => bytes = bytes.saturating_add(program.retained_bytes()),
                Op::TemplateObject(site) => {
                    bytes = bytes.saturating_add(
                        std::mem::size_of::<TemplateSite>() + RC_COUNTS + vector_storage(&site.parts)
                            + site.parts.iter().map(|part| {
                                part.raw.allocation_bytes()
                                    + part.cooked.as_ref().map_or(0, JsString::allocation_bytes)
                            }).sum::<usize>(),
                    );
                }
                Op::Function(code) => functions.push(code.clone()),
                Op::Class(class, name) => {
                    let (address, size) = class.source.storage();
                    if seen_sources.insert(address) { bytes = bytes.saturating_add(RC_COUNTS + size); }
                    if let ClassName::Binding(name) = name {
                        bytes = bytes.saturating_add(name.len() * 2 + 64);
                    }
                    bytes = bytes.saturating_add(classes::retained_size(
                        class,
                        &mut functions,
                        &mut chunks,
                    ))
                }
                Op::Block(c) | Op::Label(_, c) | Op::Throw(c) => {
                    chunks.push(c.clone())
                }
                Op::Initialize(_, pattern, c) => {
                    bytes = bytes.saturating_add(pattern_retained_size(pattern, &mut chunks));
                    chunks.extend(c.iter().cloned());
                }
                Op::Branch(arms, no) => {
                    bytes = bytes.saturating_add(RC_COUNTS + std::mem::size_of_val(arms.as_ref()));
                    for (test, body) in arms.iter() {
                        chunks.push(test.clone());
                        chunks.push(body.clone());
                    }
                    chunks.extend(no.iter().cloned());
                }
                Op::With(a, b) | Op::While(a, b, _) | Op::DoWhile(a, b, _) => {
                    chunks.push(a.clone());
                    chunks.push(b.clone());
                }
                Op::For(code) => {
                    let ForCode { initial, test, update, body, lexical, .. } = code.as_ref();
                    bytes = bytes.saturating_add(size_of::<ForCode>() + vector_storage(lexical)
                        + lexical.iter().map(|(name, _)| name.allocation_bytes()).sum::<usize>());
                    chunks.extend(initial.iter().cloned());
                    chunks.extend(test.iter().cloned());
                    chunks.extend(update.iter().cloned());
                    chunks.push(body.clone());
                }
                Op::Destructure(pattern) => {
                    bytes = bytes.saturating_add(pattern_retained_size(pattern, &mut chunks))
                }
                Op::ForIn(_, pattern, a, b, _) | Op::ForOf(_, pattern, a, b, _, _) => {
                    bytes = bytes.saturating_add(pattern_retained_size(pattern, &mut chunks));
                    chunks.push(a.clone());
                    chunks.push(b.clone());
                }
                Op::Return(c) => chunks.extend(c.iter().cloned()),
                Op::Try(a, b, c) => {
                    chunks.push(a.clone());
                    if let Some((pattern, b)) = b {
                        if let Some(pattern) = pattern {
                            bytes =
                                bytes.saturating_add(pattern_retained_size(pattern, &mut chunks));
                        }
                        chunks.push(b.clone());
                    }
                    chunks.extend(c.iter().cloned());
                }
                Op::Switch(test, cases, scope) => {
                    bytes = bytes.saturating_add(vector_storage(cases)
                        + std::mem::size_of::<ScopePlan>() + scope_storage(scope));
                    chunks.push(test.clone());
                    for (test, body) in cases {
                        chunks.extend(test.iter().cloned());
                        chunks.push(body.clone());
                    }
                    functions.extend(scope.functions.iter().map(|(_, c, _)| c.clone()));
                }
                _ => {}
            }
        }
    }
    bytes
}

#[cfg(test)]
mod retained_tests;

fn syntax(message: impl Into<String>) -> JsError {
    JsError::new(ErrorKind::SyntaxError, message)
}
// yield is an IdentifierReference/BindingIdentifier outside generators in
// non-strict code. The parser accepts that cover grammar; strict validation
// belongs here, where inherited and directive strictness are both known.
fn strict_reserved(name: &JsString) -> bool {
    matches!(
        name.to_string_lossy().as_str(),
        "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
    )
}
fn check_identifier(name: &JsString, strict: bool) -> Result<(), JsError> {
    if strict && strict_reserved(name) {
        Err(syntax("Reserved identifier in strict mode"))
    } else {
        Ok(())
    }
}
pub(crate) fn invalid_strict_binding(name: &JsString) -> bool {
    strict_reserved(name) || matches!(name.to_string_lossy().as_str(), "eval" | "arguments")
}
fn unique_pattern(pattern: &Pattern) -> Result<(), JsError> {
    let mut seen = BTreeSet::new();
    if bound_names(pattern)
        .into_iter()
        .any(|name| !seen.insert(name))
    {
        Err(syntax("Duplicate pattern binding"))
    } else {
        Ok(())
    }
}
fn check_loop_lexical(names: Vec<JsString>, body: &Stmt) -> Result<(), JsError> {
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(syntax("Duplicate loop binding"));
        }
    }
    let mut vars = BTreeSet::new();
    collect_vars(std::slice::from_ref(body), &mut vars);
    if seen.iter().any(|name| vars.contains(name)) {
        return Err(syntax("Loop binding conflicts with var declaration"));
    }
    Ok(())
}
fn ungroup(mut e: &Expr) -> &Expr {
    while let Expr::Group(value) = e {
        e = value;
    }
    e
}
fn anonymous(e: &Expr) -> bool {
    match e {
        Expr::Function(function) => function.name.is_none(),
        Expr::Class(class) => class.name.is_none(),
        Expr::Group(e) => anonymous(e),
        _ => false,
    }
}
fn require_non_suspending(chunk: Rc<Chunk>, context: &str) -> Result<Rc<Chunk>, JsError> {
    if chunk.can_suspend {
        return Err(JsError::new(
            ErrorKind::UnsupportedFeature,
            format!("Suspension inside {context} is not implemented"),
        ));
    }
    Ok(chunk)
}

enum PatternSuspension {
    Reject,
    Track(bool),
}
impl PatternSuspension {
    fn expression(&mut self, chunk: Rc<Chunk>, context: &str) -> Result<Rc<Chunk>, JsError> {
        match self {
            Self::Reject => require_non_suspending(chunk, context),
            Self::Track(found) => {
                *found |= chunk.can_suspend;
                Ok(chunk)
            }
        }
    }
}

fn compile_pattern(pattern: &Pattern, strict: bool) -> Result<Rc<PatternCode>, JsError> {
    pattern_code(pattern, strict, &mut PatternSuspension::Reject)
}
fn pattern_code(pattern: &Pattern, strict: bool, suspension: &mut PatternSuspension) -> Result<Rc<PatternCode>, JsError> {
    Ok(Rc::new(match pattern {
        Pattern::Name(name) => {
            if strict && invalid_strict_binding(name) {
                return Err(syntax("Invalid strict-mode binding or assignment"));
            }
            PatternCode::Name(name.clone())
        }
        Pattern::Target(target) => match &**target {
            Expr::Member(object, key) => {
                let key =
                    suspension.expression(expression(key, strict)?, "a destructuring target")?;
                if matches!(object.as_ref(), Expr::Super) {
                    PatternCode::SuperTarget(key)
                } else {
                    let object = suspension.expression(
                        expression(object, strict)?,
                        "a destructuring target",
                    )?;
                    PatternCode::Target(object, key)
                }
            }
            Expr::PrivateMember(object, name) => PatternCode::PrivateTarget(
                suspension.expression(expression(object, strict)?, "a destructuring target")?,
                name.clone(),
            ),
            e if !strict && crate::parser::call_assignment_target(e) => PatternCode::CallTarget(suspension.expression(expression(e, strict)?, "an iteration target")?),
            _ => return Err(syntax("Invalid assignment target")),
        },
        Pattern::Default(target, value) => PatternCode::Default {
            target: pattern_code(target, strict, suspension)?,
            value: suspension.expression(
                if let Pattern::Name(name) = target.as_ref() {
                    named_expression(value, strict, name)?
                } else {
                    expression(value, strict)?
                },
                "a destructuring default",
            )?,
        },
        Pattern::Array(items, rest) => {
            let mut elements = Vec::with_capacity(items.len());
            for item in items {
                elements.push(match item {
                    Some(item) => Some(pattern_code(item, strict, suspension)?),
                    None => None,
                });
            }
            let rest = match rest {
                Some(rest) => Some(pattern_code(rest, strict, suspension)?),
                None => None,
            };
            PatternCode::Array(elements, rest)
        }
        Pattern::Object(properties, rest) => {
            let mut entries = Vec::with_capacity(properties.len());
            for (key, pattern, _) in properties {
                let key = suspension.expression(expression(key, strict)?, "a destructuring key")?;
                entries.push((key, pattern_code(pattern, strict, suspension)?));
            }
            let rest = match rest {
                Some(rest) => Some(pattern_code(rest, strict, suspension)?),
                None => None,
            };
            PatternCode::Object(entries, rest)
        }
    }))
}
fn pattern_retained_size(pattern: &Rc<PatternCode>, chunks: &mut Vec<Rc<Chunk>>) -> usize {
    let mut bytes = 0usize;
    let mut pending = vec![pattern.as_ref()];
    while let Some(pattern) = pending.pop() {
        bytes = bytes.saturating_add(std::mem::size_of::<PatternCode>() + 2 * std::mem::size_of::<usize>());
        match pattern {
            PatternCode::Name(name) => bytes = bytes.saturating_add(name.allocation_bytes()),
            PatternCode::Target(object, key) => {
                chunks.push(object.clone());
                chunks.push(key.clone());
            }
            PatternCode::PrivateTarget(object, name) => {
                chunks.push(object.clone());
                bytes = bytes.saturating_add(name.allocation_bytes());
            }
            PatternCode::SuperTarget(key) | PatternCode::CallTarget(key) => chunks.push(key.clone()),
            PatternCode::Default { target, value, .. } => {
                pending.push(target);
                chunks.push(value.clone());
            }
            PatternCode::Array(items, rest) => {
                bytes = bytes.saturating_add(vector_storage(items));
                pending.extend(items.iter().flatten().map(Rc::as_ref));
                pending.extend(rest.iter().map(Rc::as_ref));
            }
            PatternCode::Object(properties, rest) => {
                bytes = bytes.saturating_add(vector_storage(properties));
                for (key, p) in properties {
                    chunks.push(key.clone());
                    pending.push(p);
                }
                pending.extend(rest.iter().map(Rc::as_ref));
            }
        }
    }
    bytes
}
fn block(stmts: &[Stmt], function_scope: bool, strict: bool) -> Result<Rc<Chunk>, JsError> {
    block_scope(stmts, function_scope, strict, false)
}
fn block_scope(
    stmts: &[Stmt],
    function_scope: bool,
    strict: bool,
    module_scope: bool,
) -> Result<Rc<Chunk>, JsError> {
    let mut chunk = Chunk {
        scope: scope_plan(stmts, function_scope, strict, module_scope)?,
        strict,
        ..Chunk::default()
    };
    for stmt in stmts {
        statement(stmt, &mut chunk.ops, strict)?;
    }
    Ok(chunk.finish())
}

fn scope_plan(
    stmts: &[Stmt],
    function_scope: bool,
    strict: bool,
    module_scope: bool,
) -> Result<ScopePlan, JsError> {
    let mut scope = ScopePlan::default();
    let mut lexical = BTreeSet::new();
    let mut ordinary_block_functions = BTreeSet::new();
    let mut vars = BTreeSet::new();
    collect_vars(stmts, &mut vars);
    for stmt in stmts {
        let declaration = match stmt {
            Stmt::Label(_, _, item) if matches!(**item, Stmt::Function(_)) => item.as_ref(),
            _ => stmt,
        };
        match declaration {
            Stmt::Variable(kind, decls) if *kind != VarKind::Var => {
                for name in decls.iter().flat_map(|(pattern, _)| bound_names(pattern)) {
                    if !lexical.insert(name.clone()) {
                        return Err(syntax(format!("Duplicate lexical binding '{name}'")));
                    }
                    scope.lexical.push((name.clone(), *kind != VarKind::Const));
                }
            }
            Stmt::Class(name, _) => {
                if !lexical.insert(name.clone()) {
                    return Err(syntax("Duplicate class binding"));
                }
                scope.lexical.push((name.clone(), true));
            }
            Stmt::DefaultExport(_) => {
                if !lexical.insert("*default*".into()) {
                    return Err(syntax("Duplicate default export"));
                }
                scope.lexical.push(("*default*".into(), false));
            }
            Stmt::DefaultFunction(f) => {
                let name: JsString = "*default*".into();
                if !lexical.insert(name.clone()) {
                    return Err(syntax("Duplicate default export"));
                }
                scope.lexical.push((name.clone(), true));
                scope
                    .functions
                    .push((name, compile_function(f, true)?, false));
            }
            Stmt::Function(f) => {
                let name = f.name.clone().unwrap();
                let variable_function = function_scope && !module_scope;
                if variable_function {
                    vars.insert(name.clone());
                } else if lexical.insert(name.clone()) {
                    if !strict && f.kind == FunctionKind::Ordinary {
                        ordinary_block_functions.insert(name.clone());
                    }
                    scope.lexical.push((name.clone(), true));
                } else if f.kind != FunctionKind::Ordinary
                    || !ordinary_block_functions.contains(&name)
                {
                    return Err(syntax(format!("Duplicate lexical binding '{name}'")));
                }
                scope
                    .functions
                    .push((name, compile_function(f, strict)?, variable_function));
            }
            _ => {}
        }
    }
    for name in &lexical {
        if vars.contains(name) {
            return Err(syntax(format!(
                "Var conflicts with lexical binding '{name}'"
            )));
        }
    }
    if function_scope {
        if !strict {
            let mut names = BTreeSet::new();
            collect_declarations(stmts, &mut names, true);
            scope.annex_vars = names.into_iter().collect();
        }
        scope.vars = vars.into_iter().collect();
    }
    Ok(scope)
}
fn parameter_expressions(pattern: &Pattern) -> bool {
    let mut pending = vec![pattern];
    while let Some(pattern) = pending.pop() {
        match pattern {
            Pattern::Name(_) | Pattern::Target(_) => {}
            Pattern::Default(..) => return true,
            Pattern::Array(items, rest) => {
                pending.extend(items.iter().flatten());
                pending.extend(rest.iter().map(Box::as_ref));
            }
            Pattern::Object(properties, rest) => {
                if properties.iter().any(|(_, _, computed)| *computed) {
                    return true;
                }
                pending.extend(properties.iter().map(|(_, pattern, _)| pattern));
                pending.extend(rest.iter().map(Box::as_ref));
            }
        }
    }
    false
}
pub(crate) fn validate_deferred_function(
    f: &Function,
    inherited_strict: bool,
) -> Result<(Rc<Code>, usize), JsError> {
    let mut code = compile_function(f, inherited_strict)?;
    // Lower the entire body now for early errors. Until its first invocation,
    // only callable metadata and the original shared source slice are needed.
    Rc::get_mut(&mut code).unwrap().body.take();
    let retained = retained_size(vec![code.clone()], vec![], Some(f.source.storage().0))
        .saturating_add(std::mem::size_of::<Function>());
    Ok((code, retained))
}

pub(crate) fn recompile_body(code: &Code, compilation_bytes: usize) -> Result<Rc<Chunk>, JsError> {
    let f = parser::reparse_ordinary_function(&code.source, compilation_bytes)?;
    block(&f.body, true, code.strict)
}

fn compile_function(f: &Function, inherited_strict: bool) -> Result<Rc<Code>, JsError> {
    if let Some(code) = &f.validated {
        return Ok(code.clone());
    }
    let strict = inherited_strict || f.strict;
    if strict && !f.method && f.name.as_ref().is_some_and(invalid_strict_binding) {
        return Err(syntax("Invalid strict-mode function name"));
    }
    let body = block(&f.body, true, strict)?;
    let mut seen = BTreeSet::new();
    let names = f
        .params
        .iter()
        .flat_map(|p| bound_names(&p.pattern))
        .collect::<Vec<_>>();
    let simple = f.params.iter().all(simple_parameter);
    if f.strict && !simple {
        return Err(syntax(
            "Use strict directive with a non-simple parameter list",
        ));
    }
    for name in &names {
        if body.scope.lexical.iter().any(|(n, _)| n == name) {
            return Err(syntax("Parameter conflicts with lexical declaration"));
        }
        if (strict || f.arrow || f.method || !simple) && !seen.insert(name.clone()) {
            return Err(syntax("Duplicate function parameter"));
        }
        if strict && invalid_strict_binding(name) {
            return Err(syntax("Invalid strict-mode binding"));
        }
    }
    let mut mapped_indices = Vec::new();
    if !strict && simple {
        let mut seen = BTreeSet::new();
        for (index, name) in names.iter().enumerate().rev() {
            if seen.insert(name) { mapped_indices.push(index); }
        }
    }
    Ok(Rc::new(Code {
        mapped_indices,
        source: f.source.clone(),
        name: f.name.clone(),
        params: f
            .params
            .iter()
            .map(|p| {
                Ok(ParameterCode {
                    pattern: compile_pattern(&p.pattern, strict)?,
                    rest: p.rest,
                })
            })
            .collect::<Result<_, JsError>>()?,
        param_names: names,
        mapped_arguments: !strict && simple,
        parameter_expressions: f.params.iter().any(|p| parameter_expressions(&p.pattern)),
        length: f
            .params
            .iter()
            .take_while(|p| !p.rest && !matches!(p.pattern, Pattern::Default(..)))
            .count(),
        body: body.into(),
        strict,
        arrow: f.arrow,
        kind: f.kind,
        self_binding: f.self_binding,
        method: f.method,
    }))
}
fn collect_vars(stmts: &[Stmt], out: &mut BTreeSet<JsString>) {
    collect_declarations(stmts, out, false);
}
fn collect_declarations(stmts: &[Stmt], out: &mut BTreeSet<JsString>, annex: bool) {
    for stmt in stmts {
        match stmt {
            Stmt::Function(f) if annex && f.annex_binding => { out.extend(f.name.iter().cloned()); }
            Stmt::Variable(VarKind::Var, d) if !annex => {
                out.extend(d.iter().flat_map(|(pattern, _)| bound_names(pattern)));
            }
            Stmt::Block(body) => collect_declarations(body, out, annex),
            Stmt::If(arms, no) => {
                for body in arms.iter().map(|(_, body)| body).chain(no.as_deref()) {
                    collect_declarations(std::slice::from_ref(body), out, annex);
                }
            }
            Stmt::With(_, b) | Stmt::While(_, b) | Stmt::DoWhile(b, _) | Stmt::Label(_, _, b) => {
                collect_declarations(std::slice::from_ref(&**b), out, annex)
            }
            Stmt::For(init, _, _, body) => {
                if let Some(i) = init {
                    collect_declarations(std::slice::from_ref(&**i), out, annex);
                }
                collect_declarations(std::slice::from_ref(&**body), out, annex);
            }
            Stmt::ForIn(k, n, _, body, _) | Stmt::ForOf(k, n, _, body, _) => {
                if !annex && *k == Some(VarKind::Var) {
                    out.extend(bound_names(n));
                }
                collect_declarations(std::slice::from_ref(&**body), out, annex);
            }
            Stmt::Try(body, catch, finally) => {
                collect_declarations(std::slice::from_ref(&**body), out, annex);
                if let Some((_, c)) = catch {
                    collect_declarations(std::slice::from_ref(&**c), out, annex);
                }
                if let Some(f) = finally {
                    collect_declarations(std::slice::from_ref(&**f), out, annex);
                }
            }
            Stmt::Switch(_, cases) => {
                for (_, body) in cases {
                    collect_declarations(body, out, annex)
                }
            }
            _ => {}
        }
    }
}
fn stmt_chunk(stmt: &Stmt, strict: bool) -> Result<Rc<Chunk>, JsError> {
    let mut chunk = Chunk {
        strict,
        ..Chunk::default()
    };
    statement(stmt, &mut chunk.ops, strict)?;
    Ok(chunk.finish())
}
// Annex B interprets an ordinary function in an if clause as a single
// declaration inside a block. Inherited strictness is known at compile time.
fn if_clause(stmt: &Stmt, strict: bool) -> Result<Rc<Chunk>, JsError> {
    if let Stmt::Function(function) = stmt {
        if strict || function.kind != FunctionKind::Ordinary {
            return Err(syntax(
                "Function declaration in an if clause requires non-strict code",
            ));
        }
        let declarations = block(std::slice::from_ref(stmt), false, false)?;
        return Ok(Chunk {
            ops: vec![Op::Block(declarations)],
            ..Chunk::default()
        }
        .finish());
    }
    stmt_chunk(stmt, strict)
}

fn expression(e: &Expr, strict: bool) -> Result<Rc<Chunk>, JsError> {
    let mut c = Chunk {
        strict,
        ..Chunk::default()
    };
    expr(e, &mut c.ops, strict)?;
    Ok(c.finish())
}
fn named_expression(expr: &Expr, strict: bool, name: &JsString) -> Result<Rc<Chunk>, JsError> {
    if let Expr::Class(class) = ungroup(expr)
        && class.name.is_none()
    {
        return Ok(Rc::new(Chunk {
            ops: vec![Op::Class(
                classes::compile(class)?,
                ClassName::Binding(name.clone()),
            )],
            strict,
            ..Chunk::default()
        }));
    }
    let mut chunk = expression(expr, strict)?;
    if anonymous(expr) {
        Rc::get_mut(&mut chunk)
            .unwrap()
            .ops
            .push(Op::InferName(name.clone()));
    }
    Ok(chunk)
}
fn statement(s: &Stmt, out: &mut Vec<Op>, strict: bool) -> Result<(), JsError> {
    compile_statement(s, out, strict, None)
}

fn compile_statement(
    s: &Stmt,
    out: &mut Vec<Op>,
    strict: bool,
    label: Option<LabelId>,
) -> Result<(), JsError> {
    match s {
        Stmt::Function(f) if f.annex_binding && !strict => {
            out.push(Op::CopyAnnexFunction(f.name.clone().unwrap()));
        }
        Stmt::Empty | Stmt::Function(_) | Stmt::DefaultFunction(_) => {}
        Stmt::EscapedStringStatement(value) => {
            out.extend([Op::Push(Value::String(value.clone())), Op::StatementValue]);
        }
        Stmt::Expression(e) => {
            expr(e, out, strict)?;
            out.push(Op::StatementValue);
        }
        Stmt::Class(name, class) => {
            let chunk = Chunk {
                ops: vec![Op::Class(classes::compile(class)?, ClassName::Intrinsic)],
                strict: true,
                ..Chunk::default()
            };
            out.push(Op::Initialize(
                VarKind::Let,
                Rc::new(PatternCode::Name(name.clone())),
                Some(Rc::new(chunk)),
            ));
        }
        Stmt::DefaultExport(e) => {
            let chunk = named_expression(e, true, &"default".into())?;
            out.push(Op::Initialize(
                VarKind::Const,
                Rc::new(PatternCode::Name("*default*".into())),
                Some(chunk),
            ));
        }
        Stmt::Block(body) => out.push(Op::Block(block(body, false, strict)?)),
        Stmt::Label(id, names, item) => compile_label(*id, names, item, out, strict)?,
        Stmt::Variable(kind, decls) => compile_variables(*kind, decls, out, strict)?,
        Stmt::If(arms, no) => {
            let mut compiled = Vec::with_capacity(arms.len());
            for (test, body) in arms {
                compiled.push((expression(test, strict)?, if_clause(body, strict)?));
            }
            out.push(Op::Branch(
                compiled.into(),
                no.as_ref().map(|s| if_clause(s, strict)).transpose()?,
            ));
        }
        Stmt::With(object, body) => {
            if strict {
                return Err(syntax("with is forbidden in strict mode"));
            }
            out.push(Op::With(
                expression(object, strict)?,
                stmt_chunk(body, strict)?,
            ));
        }
        Stmt::While(test, body) => out.push(Op::While(
            expression(test, strict)?,
            stmt_chunk(body, strict)?,
            label,
        )),
        Stmt::DoWhile(body, test) => out.push(Op::DoWhile(
            stmt_chunk(body, strict)?,
            expression(test, strict)?,
            label,
        )),
        Stmt::For(initial, test, update, body) => compile_for(
            initial.as_deref(),
            test.as_ref(),
            update.as_ref(),
            body,
            out,
            strict,
            label,
        )?,
        Stmt::ForIn(kind, name, object, body, initializer) => compile_for_in(
            *kind,
            name,
            object,
            body,
            initializer.as_deref(),
            out,
            strict,
            label,
        )?,
        Stmt::ForOf(kind, name, iterable, body, asynchronous) => compile_for_of(
            *kind,
            name,
            iterable,
            body,
            *asynchronous,
            out,
            strict,
            label,
        )?,
        Stmt::Switch(test, cases) => compile_switch(test, cases, out, strict)?,
        Stmt::Return(e) => out.push(Op::Return(
            e.as_ref().map(|e| expression(e, strict)).transpose()?,
        )),
        Stmt::Break(target) => out.push(Op::Break(*target)),
        Stmt::Continue(target) => out.push(Op::Continue(*target)),
        Stmt::Throw(e) => out.push(Op::Throw(expression(e, strict)?)),
        Stmt::Try(body, catch, finally) => out.push(Op::Try(
            stmt_chunk(body, strict)?,
            catch
                .as_ref()
                .map(|(n, s)| {
                    if let Some(pattern) = n {
                        unique_pattern(pattern)?;
                        if let Stmt::Block(statements) = s.as_ref() {
                            let scope = scope_plan(statements, false, strict, false)?;
                            let names = bound_names(pattern);
                            if scope.lexical.iter().any(|(name, _)| names.contains(name)) {
                                return Err(syntax(
                                    "Catch binding conflicts with lexical declaration",
                                ));
                            }
                            if !matches!(pattern, Pattern::Name(_)) {
                                let mut vars = BTreeSet::new();
                                collect_vars(statements, &mut vars);
                                if names.iter().any(|name| vars.contains(name)) {
                                    return Err(syntax(
                                        "Catch pattern conflicts with var declaration",
                                    ));
                                }
                            }
                        }
                    }
                    Ok((
                        n.as_ref().map(|p| compile_pattern(p, strict)).transpose()?,
                        stmt_chunk(s, strict)?,
                    ))
                })
                .transpose()?,
            finally
                .as_ref()
                .map(|s| stmt_chunk(s, strict))
                .transpose()?,
        )),
    }
    Ok(())
}

fn compile_label(
    id: LabelId,
    names: &[JsString],
    item: &Stmt,
    out: &mut Vec<Op>,
    strict: bool,
) -> Result<(), JsError> {
    for name in names {
        check_identifier(name, strict)?;
    }
    match item {
        Stmt::While(..) | Stmt::DoWhile(..) | Stmt::For(..) | Stmt::ForIn(..) | Stmt::ForOf(..) => {
            compile_statement(item, out, strict, Some(id))?
        }
        Stmt::Function(_) if strict => {
            return Err(syntax("Labelled function in strict mode"));
        }
        Stmt::Function(_) => {}
        _ => out.push(Op::Label(id, stmt_chunk(item, strict)?)),
    }
    Ok(())
}

fn compile_variables(
    kind: VarKind,
    decls: &[(Pattern, Option<Expr>)],
    out: &mut Vec<Op>,
    strict: bool,
) -> Result<(), JsError> {
    for (pattern, init) in decls {
        if strict
            && bound_names(pattern)
                .iter()
                .any(|name| matches!(name.to_string_lossy().as_str(), "eval" | "arguments"))
        {
            return Err(syntax("Invalid strict-mode binding"));
        }
        out.push(Op::Initialize(
            kind,
            compile_pattern(pattern, strict)?,
            init.as_ref()
                .map(|expr| {
                    if let Pattern::Name(name) = pattern {
                        named_expression(expr, strict, name)
                    } else {
                        expression(expr, strict)
                    }
                })
                .transpose()?,
        ));
    }
    Ok(())
}

fn compile_for(
    initial: Option<&Stmt>,
    test: Option<&Expr>,
    update: Option<&Expr>,
    body: &Stmt,
    out: &mut Vec<Op>,
    strict: bool,
    label: Option<LabelId>,
) -> Result<(), JsError> {
    let lexical: Vec<(JsString, bool)> = match initial {
        Some(Stmt::Variable(k, d)) if *k != VarKind::Var => d
            .iter()
            .flat_map(|(p, _)| {
                bound_names(p)
                    .into_iter()
                    .map(|name| (name, *k != VarKind::Const))
            })
            .collect(),
        _ => Vec::new(),
    };
    check_loop_lexical(lexical.iter().map(|(name, _)| name.clone()).collect(), body)?;
    out.push(Op::For(Box::new(ForCode {
        initial: initial.map(|s| stmt_chunk(s, strict)).transpose()?,
        test: test.map(|e| expression(e, strict)).transpose()?,
        update: update.map(|e| expression(e, strict)).transpose()?,
        body: stmt_chunk(body, strict)?,
        lexical,
        label,
    })));
    Ok(())
}

fn compile_for_in(
    kind: Option<VarKind>,
    name: &Pattern,
    object: &Expr,
    body: &Stmt,
    initializer: Option<&Expr>,
    out: &mut Vec<Op>,
    strict: bool,
    label: Option<LabelId>,
) -> Result<(), JsError> {
    if let Some(initializer) = initializer {
        if strict {
            return Err(syntax(
                "A strict for-in declaration cannot have an initializer",
            ));
        }
        let Pattern::Name(binding) = name else {
            unreachable!()
        };
        out.push(Op::Initialize(
            VarKind::Var,
            compile_pattern(name, strict)?,
            Some(named_expression(initializer, strict, binding)?),
        ));
    }
    if matches!(kind, Some(VarKind::Let | VarKind::Const)) {
        check_loop_lexical(bound_names(name), body)?;
    }
    out.push(Op::ForIn(
        kind,
        compile_pattern(name, strict)?,
        expression(object, strict)?,
        stmt_chunk(body, strict)?,
        label,
    ));
    Ok(())
}

fn compile_for_of(
    kind: Option<VarKind>,
    name: &Pattern,
    iterable: &Expr,
    body: &Stmt,
    asynchronous: bool,
    out: &mut Vec<Op>,
    strict: bool,
    label: Option<LabelId>,
) -> Result<(), JsError> {
    if matches!(kind, Some(VarKind::Let | VarKind::Const)) {
        check_loop_lexical(bound_names(name), body)?;
    }
    if strict
        && bound_names(name)
            .iter()
            .any(|name| matches!(name.to_string_lossy().as_str(), "eval" | "arguments"))
    {
        return Err(syntax("Invalid strict-mode binding"));
    }
    out.push(Op::ForOf(
        kind,
        compile_pattern(name, strict)?,
        expression(iterable, strict)?,
        stmt_chunk(body, strict)?,
        asynchronous,
        label,
    ));
    Ok(())
}

fn compile_switch(
    test: &Expr,
    cases: &[(Option<Expr>, Vec<Stmt>)],
    out: &mut Vec<Op>,
    strict: bool,
) -> Result<(), JsError> {
    let all: Vec<_> = cases
        .iter()
        .flat_map(|(_, body)| body.iter().cloned())
        .collect();
    let plan = scope_plan(&all, false, strict, false)?;
    let mut compiled = Vec::new();
    for (test, body) in cases {
        let mut chunk = Chunk {
            strict,
            ..Chunk::default()
        };
        for stmt in body {
            statement(stmt, &mut chunk.ops, strict)?;
        }
        compiled.push((
            test.as_ref().map(|e| expression(e, strict)).transpose()?,
            chunk.finish(),
        ));
    }
    out.push(Op::Switch(
        expression(test, strict)?,
        compiled,
        Box::new(plan),
    ));
    Ok(())
}

enum Emit<'a> {
    Expression(&'a Expr),
    Named(&'a Expr, &'a JsString),
    Reference(&'a Expr, bool),
    Instruction(Op),
    Sequence(&'a [Expr]),
    PatternReference(&'a Pattern),
    PatternAssign(&'a Pattern),
    ShortCircuit(&'static str, &'a Expr),
    Patch(usize, &'static str),
    Conditional(&'a [(Expr, Expr)], &'a Expr),
    Consequent(&'a Expr, &'a [(Expr, Expr)], &'a Expr),
    Alternative(usize, &'a [(Expr, Expr)], &'a Expr),
    Logical(&'static str, &'a Expr, Option<&'a JsString>),
    LogicalKeep(usize, &'static str),
    Optional(Rc<RefCell<Vec<usize>>>, bool),
    ChainEnd(Rc<RefCell<Vec<usize>>>),
}
/// Explicit traversal keeps compiler frames independent of expression depth.
/// Function bodies retain the independent parser/AST nesting guards.
fn expr(e: &Expr, out: &mut Vec<Op>, strict: bool) -> Result<(), JsError> {
    use Emit::{Expression as E, Instruction as I, Reference as R};
    let mut pending = vec![E(e)];
    while let Some(task) = pending.pop() {
        match task {
            Emit::PatternReference(pattern) => {
                assignment_patterns::reference(pattern, &mut pending, out);
            }
            Emit::PatternAssign(pattern) => assignment_patterns::assign(pattern, &mut pending, out),
            Emit::Optional(branches, call) => {
                branches.borrow_mut().push(out.len());
                out.push(Op::OptionalCheck(0, call));
            }
            Emit::ChainEnd(branches) => {
                for &index in branches.borrow().iter() {
                    let Op::OptionalCheck(_, call) = out[index] else {
                        unreachable!()
                    };
                    out[index] = Op::OptionalCheck(out.len(), call);
                }
            }
            Emit::Logical(op, right, name) => {
                out.extend([Op::NormalizeReference, Op::Dup, Op::Get, Op::Dup]);
                let branch = out.len();
                out.push(Op::Jump(0));
                out.push(Op::Pop);
                pending.extend([Emit::LogicalKeep(branch, op), I(Op::Set)]);
                pending.push(if let Some(name) = name {
                    Emit::Named(right, name)
                } else {
                    E(right)
                });
            }
            Emit::LogicalKeep(branch, op) => {
                let jump = out.len();
                out.push(Op::Jump(0));
                out[branch] = match op {
                    "&&=" => Op::JumpFalse(out.len()),
                    "||=" => Op::JumpTrue(out.len()),
                    _ => Op::JumpNotNullish(out.len()),
                };
                out.extend([Op::Swap, Op::Pop]);
                out[jump] = Op::Jump(out.len());
            }
            Emit::Named(expr, name) => {
                if let Expr::Class(class) = ungroup(expr)
                    && class.name.is_none()
                {
                    out.push(Op::Class(
                        classes::compile(class)?,
                        ClassName::Binding(name.clone()),
                    ));
                } else {
                    pending.extend([I(Op::InferName(name.clone())), E(expr)]);
                }
            }
            I(op) => out.push(op),
            Emit::Sequence(terms) => {
                let (first, rest) = terms.split_first().expect("comma expression has operands");
                if !rest.is_empty() {
                    pending.extend([Emit::Sequence(rest), I(Op::Pop)]);
                }
                pending.push(E(first));
            }
            Emit::Patch(index, kind) => {
                out[index] = match kind {
                    "&&" => Op::JumpFalse(out.len()),
                    "||" => Op::JumpTrue(out.len()),
                    "??" => Op::JumpNotNullish(out.len()),
                    _ => Op::Jump(out.len()),
                }
            }
            Emit::ShortCircuit(op, right) => {
                out.push(Op::Dup);
                let branch = out.len();
                out.push(Op::Jump(0));
                out.push(Op::Pop);
                pending.push(Emit::Patch(branch, op));
                pending.push(E(right));
            }
            Emit::Conditional(branches, fallback) => {
                if let Some(((test, yes), rest)) = branches.split_first() {
                    pending.extend([Emit::Consequent(yes, rest, fallback), E(test)]);
                } else {
                    pending.push(E(fallback));
                }
            }
            Emit::Consequent(yes, rest, fallback) => {
                let branch = out.len();
                out.push(Op::JumpFalse(0));
                pending.push(Emit::Alternative(branch, rest, fallback));
                pending.push(E(yes));
            }
            Emit::Alternative(branch, rest, fallback) => {
                let jump = out.len();
                out.push(Op::Jump(0));
                out[branch] = Op::JumpFalse(out.len());
                pending.push(Emit::Patch(jump, "jump"));
                pending.push(Emit::Conditional(rest, fallback));
            }
            R(e, assignment) => match e {
                Expr::Group(value) => pending.push(R(value, assignment)),
                Expr::Chain(base, links) if !assignment => {
                    let branches = Rc::new(RefCell::new(Vec::new()));
                    pending.push(Emit::ChainEnd(branches.clone()));
                    for link in links.iter().rev() {
                        match link {
                            ChainLink::Member(key, optional) => {
                                if let Expr::Literal(Value::String(name)) = key {
                                    pending.push(I(Op::NamedProperty(name.clone())));
                                } else {
                                    pending.extend([I(Op::Property), E(key)]);
                                }
                                if *optional {
                                    pending.push(Emit::Optional(branches.clone(), false));
                                }
                                pending.push(I(Op::Get));
                            }
                            ChainLink::PrivateMember(name, optional) => {
                                pending.push(I(Op::PrivateProperty(name.clone())));
                                if *optional {
                                    pending.push(Emit::Optional(branches.clone(), false));
                                }
                                pending.push(I(Op::Get));
                            }
                            ChainLink::Call(args, optional) => {
                                pending.push(I(Op::Call));
                                for arg in args.iter().rev() {
                                    if let Expr::Spread(value) = arg {
                                        pending.extend([I(Op::Argument(true)), E(value)]);
                                    } else {
                                        pending.extend([I(Op::Argument(false)), E(arg)]);
                                    }
                                }
                                pending.push(I(Op::Arguments));
                                if *optional {
                                    pending.push(Emit::Optional(branches.clone(), true));
                                }
                                pending.push(I(Op::PrepareCall(!optional)));
                            }
                        }
                    }
                    pending.push(R(base, false));
                }
                Expr::Identifier(name) => {
                    check_identifier(name, strict)?;
                    if assignment
                        && strict
                        && matches!(name.to_string_lossy().as_str(), "eval" | "arguments")
                    {
                        return Err(syntax("Invalid strict-mode assignment"));
                    }
                    out.push(Op::Name(name.clone()));
                }
                Expr::Member(object, key) if matches!(object.as_ref(), Expr::Super) => {
                    pending.extend([I(Op::SuperProperty), E(key), I(Op::This)])
                }
                Expr::Member(object, key) => {
                    if let Expr::Literal(Value::String(name)) = key.as_ref() {
                        pending.push(I(Op::NamedProperty(name.clone())));
                    } else {
                        pending.extend([I(Op::Property), E(key)]);
                    }
                    pending.push(E(object));
                }
                Expr::PrivateMember(object, name) => {
                    pending.extend([I(Op::PrivateProperty(name.clone())), E(object)]);
                }
                _ if assignment && !strict && crate::parser::call_assignment_target(e) => {
                    pending.extend([I(Op::InvalidCallAssignment), E(e)]);
                }
                _ if assignment => return Err(syntax("Invalid assignment target")),
                _ => pending.push(E(e)),
            },
            E(e) => match e {
                Expr::Group(value) => pending.push(E(value)),
                Expr::Chain(..) => pending.extend([I(Op::Get), R(e, false)]),
                Expr::Template(parts, substitutions, tag) => {
                    if let Some(tag) = tag {
                        pending.push(I(Op::Call));
                        for value in substitutions.iter().rev() {
                            pending.extend([I(Op::Argument(false)), E(value)]);
                        }
                        pending.extend([
                            I(Op::Argument(false)),
                            I(Op::TemplateObject(Rc::new(TemplateSite {
                                parts: parts.clone(),
                            }))),
                            I(Op::Arguments),
                            I(Op::PrepareCall(true)),
                            R(tag, false),
                        ]);
                    } else {
                        for (value, part) in substitutions.iter().zip(parts.iter().skip(1)).rev() {
                            pending.extend([
                                I(Op::TemplateAppend(part.cooked.clone().unwrap())),
                                E(value),
                            ]);
                        }
                        out.push(Op::Push(Value::String(parts[0].cooked.clone().unwrap())));
                    }
                }
                Expr::Literal(value) => out.push(Op::Push(value.clone())),
                Expr::JoinedString(units) => out.push(Op::Push(Value::String(JsString::from_utf16(units.clone())))),
                Expr::RegExp(program) => out.push(Op::RegExp(program.clone())),
                Expr::Identifier(name) => {
                    check_identifier(name, strict)?;
                    out.push(Op::LoadName(name.clone()));
                }
                Expr::This => out.push(Op::This),
                Expr::NewTarget => out.push(Op::NewTarget),
                Expr::ImportMeta => out.push(Op::ImportMeta),
                Expr::Super => return Err(syntax("Bare super is not a value")),
                Expr::Class(class) => {
                    out.push(Op::Class(classes::compile(class)?, ClassName::Intrinsic))
                }
                Expr::Member(object, key) if matches!(object.as_ref(), Expr::Super) => {
                    pending.extend([I(Op::Get), I(Op::SuperProperty), E(key), I(Op::This)])
                }
                Expr::Member(object, key) => {
                    if let Expr::Literal(Value::String(name)) = key.as_ref() {
                        pending.extend([I(Op::LoadProperty(name.clone())), E(object)]);
                    } else {
                        pending.extend([I(Op::Get), I(Op::Property), E(key), E(object)]);
                    }
                }
                Expr::PrivateMember(..) => pending.extend([I(Op::Get), R(e, false)]),
                Expr::PrivateIn(name, object) => {
                    pending.extend([I(Op::PrivateIn(name.clone())), E(object)]);
                }
                Expr::Array(items, _) => {
                    out.push(Op::Array);
                    for item in items.iter().rev() {
                        match item {
                            Some(Expr::Spread(value)) => {
                                pending.extend([I(Op::ArrayElement(true)), E(value)])
                            }
                            Some(value) => pending.extend([I(Op::ArrayElement(false)), E(value)]),
                            None => pending.push(I(Op::ArrayHole)),
                        }
                    }
                }
                Expr::Object(pairs, _) => {
                    if pairs
                        .iter()
                        .filter(|p| {
                            matches!(
                                p,
                                ObjectEntry::Property {
                                    prototype: true,
                                    ..
                                }
                            )
                        })
                        .count()
                        > 1
                    {
                        return Err(syntax("Duplicate __proto__ property"));
                    }
                    out.push(Op::Object);
                    for entry in pairs.iter().rev() {
                        match entry {
                            ObjectEntry::Spread(value) => {
                                pending.extend([I(Op::ObjectSpread), E(value)])
                            }
                            ObjectEntry::Property {
                                key,
                                value,
                                prototype,
                                infer_name,
                                cover,
                                method,
                                ..
                            } => {
                                if *cover {
                                    return Err(syntax("Invalid initialized shorthand property"));
                                }
                                let named_class = match ungroup(value) {
                                    Expr::Class(class) if class.name.is_none() && !*prototype => {
                                        Some(class)
                                    }
                                    _ => None,
                                };
                                pending.push(I(Op::ObjectProperty {
                                    prototype: *prototype,
                                    infer_name: named_class.is_none()
                                        && (*infer_name || anonymous(value)),
                                    method: *method,
                                }));
                                if let Some(class) = named_class {
                                    pending.push(I(Op::Class(
                                        classes::compile(class)?,
                                        ClassName::Property,
                                    )));
                                } else {
                                    pending.push(E(value));
                                }
                                pending.extend([I(Op::PropertyKey), E(key), I(Op::Dup)]);
                            }
                        }
                    }
                }
                Expr::Spread(_) => {
                    return Err(syntax("Spread is only valid in an array or argument list"));
                }
                Expr::Destructure(pattern, value) => {
                    let mut suspension = PatternSuspension::Track(false);
                    let code = pattern_code(pattern, strict, &mut suspension)?;
                    if matches!(suspension, PatternSuspension::Track(true)) {
                        // Keep the original assignment result below the pattern's operands.
                        pending.extend([Emit::PatternAssign(pattern), I(Op::Dup), E(value)]);
                    } else {
                        pending.extend([I(Op::Destructure(code)), E(value)]);
                    }
                }
                Expr::Function(function) => {
                    out.push(Op::Function(compile_function(function, strict)?))
                }
                Expr::NestedCalls(base, levels) => {
                    // Capture each callee's reference, receiver and function
                    // before evaluating its first argument, from outside in.
                    // Finish calls and evaluate later arguments from inside out.
                    for (name, args) in levels.iter().rev() {
                        check_identifier(name, strict)?;
                        out.push(Op::Name(name.clone()));
                        out.push(Op::PrepareCall(true));
                        out.push(Op::Arguments);
                        pending.push(I(Op::Call));
                        for arg in args.iter().rev() {
                            if let Expr::Spread(value) = arg {
                                pending.extend([I(Op::Argument(true)), E(value)]);
                            } else {
                                pending.extend([I(Op::Argument(false)), E(arg)]);
                            }
                        }
                        pending.push(I(Op::Argument(false)));
                    }
                    pending.push(E(base));
                }
                Expr::Call(target, args) if matches!(target.as_ref(), Expr::Super) => {
                    pending.push(I(Op::SuperConstruct));
                    for arg in args.iter().rev() {
                        if let Expr::Spread(value) = arg {
                            pending.extend([I(Op::Argument(true)), E(value)]);
                        } else {
                            pending.extend([I(Op::Argument(false)), E(arg)]);
                        }
                    }
                    pending.extend([I(Op::Arguments), I(Op::SuperConstructor)]);
                }
                Expr::Call(target, args) | Expr::New(target, args) => {
                    let call = matches!(e, Expr::Call(..));
                    pending.push(I(if call { Op::Call } else { Op::Construct }));
                    for arg in args.iter().rev() {
                        if let Expr::Spread(value) = arg {
                            pending.extend([I(Op::Argument(true)), E(value)]);
                        } else {
                            pending.extend([I(Op::Argument(false)), E(arg)]);
                        }
                    }
                    pending.push(I(Op::Arguments));
                    if call {
                        pending.extend([I(Op::PrepareCall(true)), R(target, false)]);
                    } else {
                        pending.push(E(target));
                    }
                }
                Expr::Unary("typeof", value) if matches!(ungroup(value), Expr::Identifier(_)) => {
                    let Expr::Identifier(name) = ungroup(value) else {
                        unreachable!()
                    };
                    check_identifier(name, strict)?;
                    out.push(Op::Name(name.clone()));
                    out.push(Op::TypeofReference);
                }
                Expr::Unary("delete", value) => {
                    if matches!(
                        &**value,
                        Expr::Identifier(_) | Expr::Member(..) | Expr::Chain(..) | Expr::Group(_)
                    ) {
                        if strict && matches!(ungroup(value), Expr::Identifier(_)) {
                            return Err(syntax("Delete of an identifier in strict mode"));
                        }
                        pending.extend([I(Op::Delete), R(value, false)]);
                    } else {
                        pending.extend([I(Op::Push(Value::Bool(true))), I(Op::Pop), E(value)]);
                    }
                }
                Expr::Unary(op, value) => pending.extend([I(Op::Unary(op)), E(value)]),
                Expr::Await(value) => pending.extend([I(Op::Await), E(value)]),
                Expr::Yield {
                    value,
                    delegate,
                    asynchronous,
                } => {
                    pending.push(I(Op::Yield {
                        delegate: *delegate,
                        asynchronous: *asynchronous,
                    }));
                    if *asynchronous && !*delegate {
                        pending.push(I(Op::Await));
                    }
                    pending.push(value.as_deref().map_or(I(Op::Push(Value::Undefined)), E));
                }
                Expr::DynamicImport(phase, value, options) => pending.extend([
                    I(Op::DynamicImport(*phase)),
                    options.as_deref().map_or(I(Op::Push(Value::Undefined)), E),
                    E(value),
                ]),
                Expr::Binary(op, left, right) if matches!(*op, "&&" | "||" | "??") => {
                    pending.extend([Emit::ShortCircuit(op, right), E(left)])
                }
                Expr::Binary(op, left, right) => {
                    pending.extend([I(Op::Binary(op)), E(right), E(left)])
                }
                Expr::Assign(op, left, right) => {
                    if matches!(*op, "&&=" | "||=" | "??=") {
                        if crate::parser::call_assignment_target(left) {
                            return Err(syntax("Invalid logical assignment target"));
                        }
                        let name = if anonymous(right) {
                            if let Expr::Identifier(name) = left.as_ref() {
                                Some(name)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        pending.extend([Emit::Logical(op, right, name), R(left, true)]);
                        continue;
                    }
                    pending.push(I(Op::Set));
                    let right_evaluation = if *op == "="
                        && anonymous(right)
                        && let Expr::Identifier(name) = left.as_ref()
                    {
                        Emit::Named(right, name)
                    } else {
                        E(right)
                    };
                    if *op != "=" {
                        let binary = match *op {
                            "+=" => "+",
                            "-=" => "-",
                            "*=" => "*",
                            "/=" => "/",
                            "%=" => "%",
                            "**=" => "**",
                            "<<=" => "<<",
                            ">>=" => ">>",
                            ">>>=" => ">>>",
                            "&=" => "&",
                            "|=" => "|",
                            "^=" => "^",
                            _ => unreachable!(),
                        };
                        pending.push(I(Op::Binary(binary)));
                    }
                    pending.push(right_evaluation);
                    if *op != "=" {
                        pending.extend([I(Op::Get), I(Op::Dup), I(Op::NormalizeReference)]);
                    }
                    pending.push(R(left, true));
                }
                Expr::Update(op, value, prefix) => pending.extend([
                    I(Op::Update {
                        increment: *op == "++",
                        prefix: *prefix,
                    }),
                    R(value, true),
                ]),
                Expr::Conditional(branches, fallback) => {
                    pending.push(Emit::Conditional(branches, fallback));
                }
                Expr::Sequence(terms) => pending.push(Emit::Sequence(terms)),
            },
        }
    }
    Ok(())
}

pub(crate) fn compile_module_body(body: &[Stmt]) -> Result<Rc<Chunk>, JsError> {
    block_scope(body, true, true, true)
}
