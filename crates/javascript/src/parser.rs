pub(crate) mod annex;
mod classes;
mod cold_functions;
mod directives;
mod literals;
pub(crate) mod modules;
use crate::{
    ErrorKind, JsError, JsString, Value,
    lexer::{Lexer, Token, TokenKind},
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

// These are independent budgets: recursive grammar calls and the resulting AST
// can have different depths (e.g. a flat left-associative operator chain). Keep
// both bounded before an unsafe-to-drop tree can be constructed. The limits are
// verified together with compilation and destruction on a 1 MiB native stack.
const MAX_PARSER_DEPTH: usize = 24;
const MAX_AST_DEPTH: usize = 64;
const TOKEN_WINDOW: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImportPhase {
    Evaluation,
    Defer,
    Source,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum VarKind {
    Var,
    Let,
    Const,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FunctionKind {
    Ordinary,
    Async,
    Generator,
    AsyncGenerator,
}

impl FunctionKind {
    pub(crate) fn is_generator(self) -> bool {
        matches!(self, Self::Generator | Self::AsyncGenerator)
    }

    fn await_context(self, context: KeywordContext) -> KeywordContext {
        if matches!(self, Self::Async | Self::AsyncGenerator) {
            context
        } else {
            KeywordContext::None
        }
    }

    fn yield_context(self, context: KeywordContext) -> KeywordContext {
        if self.is_generator() {
            context
        } else {
            KeywordContext::None
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Function {
    pub source: std::rc::Rc<crate::lexer::SourceSpan>,
    pub name: Option<JsString>,
    pub params: Vec<Parameter>,
    pub body: Vec<Stmt>,
    pub validated: Option<std::rc::Rc<crate::bytecode::Code>>,
    pub arrow: bool,
    pub kind: FunctionKind,
    pub strict: bool,
    pub self_binding: bool,
    pub annex_binding: bool,
    pub method: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Class {
    pub source: std::rc::Rc<crate::lexer::SourceSpan>,
    pub name: Option<JsString>,
    pub heritage: Option<Box<Expr>>,
    pub elements: Vec<ClassElement>,
    pub private_names: Vec<JsString>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MethodKind {
    Method,
    Getter,
    Setter,
}
#[derive(Clone, Debug)]
pub(crate) enum ClassKey {
    Public(Expr),
    Private(JsString),
}
#[derive(Clone, Debug)]
pub(crate) enum ClassElement {
    Method {
        key: ClassKey,
        function: Function,
        kind: MethodKind,
        is_static: bool,
        constructor: bool,
    },
    Field {
        key: ClassKey,
        value: Option<Expr>,
        is_static: bool,
    },
    StaticBlock(Vec<Stmt>),
}
#[derive(Clone, Debug)]
pub(crate) enum Pattern {
    Name(JsString),
    Target(Box<Expr>),
    Array(Vec<Option<Pattern>>, Option<Box<Pattern>>),
    // Property key, target, computed: ContainsExpression must distinguish a
    // computed literal key from an otherwise identical static property name.
    Object(Vec<(Expr, Pattern, bool)>, Option<Box<Pattern>>),
    Default(Box<Pattern>, Box<Expr>),
}
#[derive(Clone, Debug)]
pub(crate) struct Parameter {
    pub pattern: Pattern,
    pub rest: bool,
}
pub(crate) fn bound_names(pattern: &Pattern) -> Vec<JsString> {
    let mut out = Vec::new();
    let mut pending = vec![pattern];
    while let Some(pattern) = pending.pop() {
        match pattern {
            Pattern::Name(name) => out.push(name.clone()),
            Pattern::Target(_) => {}
            Pattern::Array(items, rest) => {
                pending.extend(rest.iter().map(Box::as_ref));
                pending.extend(items.iter().rev().flatten());
            }
            Pattern::Object(properties, rest) => {
                pending.extend(rest.iter().map(Box::as_ref));
                pending.extend(properties.iter().rev().map(|(_, p, _)| p));
            }
            Pattern::Default(p, _) => pending.push(p),
        }
    }
    out
}
pub(crate) fn simple_parameter(parameter: &Parameter) -> bool {
    !parameter.rest && matches!(parameter.pattern, Pattern::Name(_))
}
#[derive(Clone, Debug)]
pub(crate) enum ObjectEntry {
    Property {
        key: Expr,
        value: Expr,
        prototype: bool,
        infer_name: bool,
        cover: bool,
        computed: bool,
        method: Option<MethodKind>,
    },
    Spread(Expr),
}
#[derive(Clone, Debug)]
pub(crate) struct TemplatePart {
    pub raw: JsString,
    pub cooked: Option<JsString>,
}
#[derive(Clone, Debug)]
pub(crate) enum ChainLink {
    Member(Expr, bool),
    PrivateMember(JsString, bool),
    Call(Vec<Expr>, bool),
}
#[derive(Clone, Debug)]
pub(crate) enum Expr {
    Literal(Value),
    // Fold successive string-literal additions in linear space without making a directive.
    JoinedString(Vec<u16>),
    RegExp(std::rc::Rc<crate::regexp::Program>),
    Identifier(JsString),
    This,
    Super,
    NewTarget,
    ImportMeta,
    DynamicImport(ImportPhase, Box<Expr>, Option<Box<Expr>>),
    Await(Box<Expr>),
    Yield {
        value: Option<Box<Expr>>,
        delegate: bool,
        asynchronous: bool,
    },
    Class(Box<Class>),
    Array(Vec<Option<Expr>>, bool),
    Object(Vec<ObjectEntry>, bool),
    Destructure(Pattern, Box<Expr>),
    Template(Vec<TemplatePart>, Vec<Expr>, Option<Box<Expr>>),
    Chain(Box<Expr>, Vec<ChainLink>),
    Group(Box<Expr>),
    Spread(Box<Expr>),
    Function(Box<Function>),
    Member(Box<Expr>, Box<Expr>),
    PrivateMember(Box<Expr>, JsString),
    PrivateIn(JsString, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    NestedCalls(Box<Expr>, Vec<(JsString, Vec<Expr>)>),
    New(Box<Expr>, Vec<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Assign(&'static str, Box<Expr>, Box<Expr>),
    Update(&'static str, Box<Expr>, bool),
    Conditional(Vec<(Expr, Expr)>, Box<Expr>),
    Sequence(Vec<Expr>),
}
pub(crate) type LabelId = std::num::NonZeroU32;

#[derive(Clone, Debug)]
pub(crate) enum Stmt {
    Empty,
    EscapedStringStatement(JsString),
    Expression(Expr),
    Block(Vec<Stmt>),
    Variable(VarKind, Vec<(Pattern, Option<Expr>)>),
    Function(Function),
    Class(JsString, Class),
    DefaultExport(Expr),
    DefaultFunction(Function),
    If(Vec<(Expr, Stmt)>, Option<Box<Stmt>>),
    With(Expr, Box<Stmt>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    ForIn(Option<VarKind>, Pattern, Expr, Box<Stmt>, Option<Box<Expr>>),
    ForOf(Option<VarKind>, Pattern, Expr, Box<Stmt>, bool),
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Label(LabelId, Vec<JsString>, Box<Stmt>),
    Return(Option<Expr>),
    Break(Option<LabelId>),
    Continue(Option<LabelId>),
    Throw(Expr),
    Try(
        Box<Stmt>,
        Option<(Option<Pattern>, Box<Stmt>)>,
        Option<Box<Stmt>>,
    ),
}
#[derive(Default)]
pub(crate) struct EvalContext {
    pub super_property: bool,
    pub super_call: bool,
    pub new_target: bool,
    pub class_initializer: bool,
    pub private_names: BTreeSet<JsString>,
}
pub(crate) fn parse(source: &str) -> Result<Vec<Stmt>, JsError> {
    // Function constructor callers reserve the combined wrapped sources before
    // parsing their separate parameter and body goals.
    parse_script(Lexer::new(source, usize::MAX, true), source.len(), EvalContext::default(), false)
}
pub(crate) fn parse_script(
    lexer: Lexer,
    source_bytes: usize,
    context: EvalContext,
    inherited_strict: bool,
) -> Result<Vec<Stmt>, JsError> {
    let mut p = script_parser(lexer, context);
    // A function that is never called must still pass all early errors.
    p.defer_large_functions = source_bytes >= 64 * 1024;
    p.known_strict_context = inherited_strict;
    let mut in_prologue = true;
    let mut body = Vec::new();
    while !p.is_eof() {
        let statement = p.statement()?;
        p.known_strict_context |= directives::visit(&statement, &mut in_prologue);
        body.push(statement);
    }
    if p.legacy_literals != 0 && p.known_strict_context {
        return Err(p.error("Legacy literal in strict code"));
    }
    annex::mark(&mut body, []);
    Ok(body)
}

pub(crate) fn reparse_ordinary_function(
    source: &crate::lexer::SourceSpan,
    compilation_bytes: usize,
) -> Result<Function, JsError> {
    let lexer = Lexer::from_span(source.clone(), compilation_bytes, true);
    let mut p = script_parser(lexer, EvalContext::default());
    p.expect("function")?;
    let function = p.function(0, false, false)?;
    if !p.is_eof() {
        return Err(p.error("Unexpected token after function"));
    }
    Ok(function)
}

fn script_parser(lexer: Lexer, context: EvalContext) -> Parser {
    let mut p = Parser {
        tokens: VecDeque::with_capacity(TOKEN_WINDOW),
        lexer,
        defer_large_functions: false,
        known_strict_context: false,
        function_depth: 0,
        legacy_literals: 0,
        last_end: 0,
        loop_depth: 0,
        switch_depth: 0,
        labels: Vec::new(),
        next_label: 1,
        depth: 0,
        allow_in: true,
        super_property: context.super_property,
        super_call: context.super_call,
        new_target: context.new_target,
        class_initializer: context.class_initializer,
        eval_private_names: context.private_names,
        private_scopes: Vec::new(),
        module_goal: false,
        static_block: false,
        await_context: KeywordContext::None,
        await_count: 0,
        yield_context: KeywordContext::None,
        yield_count: 0,
    };
    p.fill();
    p
}
pub(crate) fn has_strict_directive(body: &[Stmt]) -> bool {
    let mut in_prologue = true;
    for stmt in body {
        if directives::visit(stmt, &mut in_prologue) {
            return true;
        }
        if !in_prologue {
            break;
        }
    }
    false
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum KeywordContext {
    None,
    Parameters,
    Body,
}

#[derive(Default)]
struct PrivateScope {
    declarations: BTreeMap<JsString, PrivateDeclaration>,
    uses: BTreeMap<JsString, (usize, usize)>,
    names: Vec<JsString>,
}

struct PrivateDeclaration {
    static_element: bool,
    getter: bool,
    setter: bool,
}

struct ActiveLabel {
    id: LabelId,
    names: Vec<JsString>,
    iteration: bool,
}

struct Parser {
    // Current token followed by up to two lookahead tokens.
    tokens: VecDeque<Token>,
    lexer: Lexer,
    defer_large_functions: bool,
    /// Strictness established by an enclosing script or function prologue.
    known_strict_context: bool,
    function_depth: usize,
    legacy_literals: usize,
    last_end: usize,
    loop_depth: usize,
    switch_depth: usize,
    labels: Vec<ActiveLabel>,
    next_label: u32,
    depth: usize,
    allow_in: bool,
    super_property: bool,
    super_call: bool,
    new_target: bool,
    class_initializer: bool,
    eval_private_names: BTreeSet<JsString>,
    private_scopes: Vec<PrivateScope>,
    module_goal: bool,
    static_block: bool,
    await_context: KeywordContext,
    // Cover grammar must remember `await` before a following arrow selects
    // the formal-parameter goal. Property names and nested function bodies
    // do not contribute to this count.
    await_count: usize,
    yield_context: KeywordContext,
    yield_count: usize,
}
impl Parser {
    fn fill(&mut self) {
        while self.tokens.len() < TOKEN_WINDOW {
            // The parser chooses the lexical goal at a slash before lexing past it.
            // Otherwise apparent comments or strings inside a pattern could scan
            // arbitrarily far ahead, then be rescanned from the slash.
            if self.tokens.back().is_some_and(|token| {
                matches!(
                    token.kind,
                    TokenKind::Eof | TokenKind::Invalid | TokenKind::Punct("/" | "/=")
                )
            }) {
                break;
            }
            self.tokens.push_back(self.lexer.next());
        }
    }

    fn advance(&mut self, count: usize) {
        if count != 0 { self.last_end = self.tokens[count - 1].end; }
        self.legacy_literals += self.tokens.drain(..count).filter(|t| t.legacy_literal).count();
        self.fill();
    }

    fn current(&self) -> &Token {
        &self.tokens[0]
    }
    fn lookahead(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(offset)
    }
    fn is_eof(&self) -> bool {
        self.current().kind == TokenKind::Eof
    }
    fn at(&self, s: &str) -> bool {
        match &self.current().kind {
            TokenKind::Word(w) => w == s,
            TokenKind::Punct(p) => *p == s,
            _ => false,
        }
    }
    fn let_declaration(&self) -> bool {
        self.at("let")
            && self.lookahead(1).is_some_and(|token| match &token.kind {
                TokenKind::Word(word) | TokenKind::EscapedWord(word) => !reserved(word),
                TokenKind::Punct("[" | "{") => true,
                _ => false,
            })
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.at(s) {
            self.advance(1);
            true
        } else {
            false
        }
    }
    fn error(&self, message: impl Into<String>) -> JsError {
        if let Some(error) = &self.current().error {
            return (**error).clone();
        }
        let message = message.into();
        let mut error = JsError::syntax(&message, self.current().line, self.current().column);
        if message.contains("not implemented") || message.contains("unsupported") {
            error.kind = ErrorKind::UnsupportedFeature;
        }
        if message.contains("nesting limit") {
            error.kind = ErrorKind::ExecutionLimit;
        }
        error
    }
    fn expect(&mut self, s: &str) -> Result<(), JsError> {
        if self.eat(s) {
            Ok(())
        } else {
            Err(self.error(format!("Expected '{s}'")))
        }
    }
    fn identifier_allowed(&self, word: &str) -> bool {
        !reserved(word)
            && !(word == "yield" && self.yield_context != KeywordContext::None)
            && !(word == "await"
                && (self.module_goal
                    || self.static_block
                    || self.await_context != KeywordContext::None))
    }
    fn name(&mut self) -> Result<JsString, JsError> {
        if self.at("{") || self.at("[") || self.at("...") {
            return Err(self.error("Destructuring/rest bindings are not implemented"));
        }
        match self.current().kind.clone() {
            TokenKind::Word(w) | TokenKind::EscapedWord(w) if self.identifier_allowed(&w) => {
                self.await_count += usize::from(w == "await");
                self.advance(1);
                Ok(w.into())
            }
            _ => Err(self.error("Expected an identifier")),
        }
    }
    fn property_name(&mut self) -> Result<JsString, JsError> {
        if self.at("[") {
            return Err(self.error("Computed property names are not implemented"));
        }
        if self.at("...") {
            return Err(self.error("Object spread syntax is not implemented"));
        }
        match self.current().kind.clone() {
            TokenKind::Word(w) | TokenKind::EscapedWord(w) => {
                self.advance(1);
                Ok(w.into())
            }
            TokenKind::String(s) => {
                self.advance(1);
                Ok(s)
            }
            TokenKind::Number(n) => {
                self.advance(1);
                Ok(number_key(n).into())
            }
            TokenKind::BigInt(value) => {
                let key = self.lexer.bigint_property_name(&value)?;
                self.advance(1);
                Ok(key)
            }
            _ => Err(self.error("Expected a property name")),
        }
    }
    fn member_name(&mut self) -> Result<JsString, JsError> {
        match self.current().kind.clone() {
            TokenKind::Word(name) | TokenKind::EscapedWord(name) => {
                self.advance(1);
                Ok(name.into())
            }
            _ => Err(self.error("Expected an identifier name after '.'")),
        }
    }
    fn semi(&mut self) -> Result<(), JsError> {
        if self.eat(";") || self.at("}") || self.is_eof() || self.current().newline {
            Ok(())
        } else {
            Err(self.error("Expected ';' or a line terminator"))
        }
    }
    fn statement(&mut self) -> Result<Stmt, JsError> {
        // Neither a return nor an expression statement has a statement body.
        // Its expression charges the recursive grammar edge when needed.
        let result = if self.eat("return") {
            self.return_statement()
        } else if matches!(self.current().kind,
            TokenKind::Punct("(" | "[" | "!" | "~" | "+" | "-" | "++" | "--" | "/" | "/=")
                | TokenKind::Number(_) | TokenKind::BigInt(_) | TokenKind::String(_)
                | TokenKind::Template { head: true, .. })
            || (matches!(&self.current().kind, TokenKind::Word(word) | TokenKind::EscapedWord(word)
                if !reserved(word) && !matches!(word.as_str(), "let" | "async"))
                && !self.label_ahead())
        {
            self.expression_statement()
        } else {
            if self.depth >= MAX_PARSER_DEPTH {
                return Err(self.error("Parser nesting limit exceeded"));
            }
            self.depth += 1;
            let result = self.statement_inner();
            self.depth -= 1;
            result
        };
        let statement = result?;
        self.check_ast(Ast::Statement(&statement))?;
        Ok(statement)
    }
    fn statement_inner(&mut self) -> Result<Stmt, JsError> {
        let start = self.current().start;
        if self.label_ahead() {
            return self.labelled_statement();
        }
        if self.eat(";") {
            return Ok(Stmt::Empty);
        }
        if self.eat("{") {
            return Ok(Stmt::Block(self.block_contents()?));
        }
        if self.eat("var") {
            let d = self.declarations(VarKind::Var)?;
            self.semi()?;
            return Ok(Stmt::Variable(VarKind::Var, d));
        }
        if self.let_declaration() {
            self.advance(1);
            let d = self.declarations(VarKind::Let)?;
            self.semi()?;
            return Ok(Stmt::Variable(VarKind::Let, d));
        }
        if self.eat("const") {
            let d = self.declarations(VarKind::Const)?;
            self.semi()?;
            return Ok(Stmt::Variable(VarKind::Const, d));
        }
        if self.eat("class") {
            let class = self.class(start, true)?;
            return Ok(Stmt::Class(class.name.clone().unwrap(), class));
        }
        if self.async_function_ahead() {
            self.advance(2);
            return self.function(start, true, true).map(Stmt::Function);
        }
        if self.eat("function") {
            return self.function(start, false, true).map(Stmt::Function);
        }
        if self.eat("if") {
            return self.if_statement();
        }
        if self.eat("while") {
            return self.while_statement();
        }
        if self.eat("do") {
            return self.do_statement();
        }
        if self.eat("for") {
            return self.for_statement();
        }
        if self.eat("break") {
            let target = self.jump_target(false)?;
            self.semi()?;
            return Ok(Stmt::Break(target));
        }
        if self.eat("continue") {
            let target = self.jump_target(true)?;
            self.semi()?;
            return Ok(Stmt::Continue(target));
        }
        if self.eat("throw") {
            if self.current().newline {
                return Err(self.error("Line terminator after throw"));
            }
            let expr = self.expression(1)?;
            self.semi()?;
            return Ok(Stmt::Throw(expr));
        }
        if self.eat("try") {
            return self.try_statement();
        }
        if self.eat("switch") {
            return self.switch_statement();
        }
        if self.eat("debugger") {
            self.semi()?;
            return Ok(Stmt::Empty);
        }
        if self.module_goal && self.at("with") {
            return Err(self.error("with is forbidden in strict module code"));
        }
        if self.module_goal
            && (self.at("export")
                || (self.at("import")
                    && !self
                        .lookahead(1)
                        .is_some_and(|t| matches!(t.kind, TokenKind::Punct("." | "(")))))
        {
            return Err(self.error("Module declaration outside module top level"));
        }
        if self.eat("with") {
            self.expect("(")?;
            let object = self.expression(1)?;
            self.expect(")")?;
            return Ok(Stmt::With(object, Box::new(self.controlled_statement()?)));
        }
        if self.at("export") {
            return Err(self.error("An export declaration requires module code"));
        }
        self.expression_statement()
    }

    fn expression_statement(&mut self) -> Result<Stmt, JsError> {
        let escaped = self.current().escaped_string;
        let e = self.expression(1)?;
        self.semi()?;
        if escaped && let Expr::Literal(Value::String(value)) = e {
            return Ok(Stmt::EscapedStringStatement(value));
        }
        Ok(Stmt::Expression(e))
    }

    fn if_statement(&mut self) -> Result<Stmt, JsError> {
        // Else-if clauses share a parser frame. Their conditions and bodies
        // retain their own ordinary grammar and syntax-tree depth checks.
        let mut arms = Vec::new();
        let fallback = loop {
            self.expect("(")?;
            let test = self.expression(1)?;
            self.expect(")")?;
            arms.push((test, self.if_clause()?));
            if !self.eat("else") {
                break None;
            }
            if !self.eat("if") {
                break Some(Box::new(self.if_clause()?));
            }
        };
        Ok(Stmt::If(arms, fallback))
    }
    fn while_statement(&mut self) -> Result<Stmt, JsError> {
        self.expect("(")?;
        let test = self.expression(1)?;
        self.expect(")")?;
        self.loop_depth += 1;
        let body = self.controlled_statement();
        self.loop_depth -= 1;
        Ok(Stmt::While(test, Box::new(body?)))
    }
    fn do_statement(&mut self) -> Result<Stmt, JsError> {
        self.loop_depth += 1;
        let body = self.controlled_statement();
        self.loop_depth -= 1;
        let body = Box::new(body?);
        self.expect("while")?;
        self.expect("(")?;
        let test = self.expression(1)?;
        self.expect(")")?;
        self.eat(";");
        Ok(Stmt::DoWhile(body, test))
    }
    fn return_statement(&mut self) -> Result<Stmt, JsError> {
        if self.function_depth == 0 {
            return Err(self.error("Return outside a function"));
        }
        let expr = if self.at(";") || self.at("}") || self.is_eof() || self.current().newline {
            None
        } else {
            let value = self.expression(1)?;
            let value = if self.yield_context == KeywordContext::Body
                && self.await_context == KeywordContext::Body
            {
                self.checked_expression(Expr::Await(Box::new(value)))?
            } else {
                value
            };
            Some(value)
        };
        self.semi()?;
        Ok(Stmt::Return(expr))
    }
    fn try_statement(&mut self) -> Result<Stmt, JsError> {
        self.expect("{")?;
        let body = Box::new(Stmt::Block(self.block_contents()?));
        let catch = if self.eat("catch") {
            let binding = if self.eat("(") {
                let name = self.binding_pattern()?;
                self.expect(")")?;
                Some(name)
            } else {
                None
            };
            self.expect("{")?;
            Some((binding, Box::new(Stmt::Block(self.block_contents()?))))
        } else {
            None
        };
        let finally = if self.eat("finally") {
            self.expect("{")?;
            Some(Box::new(Stmt::Block(self.block_contents()?)))
        } else {
            None
        };
        if catch.is_none() && finally.is_none() {
            return Err(self.error("Try requires catch or finally"));
        }
        Ok(Stmt::Try(body, catch, finally))
    }
    fn switch_statement(&mut self) -> Result<Stmt, JsError> {
        self.expect("(")?;
        let test = self.expression(1)?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        let mut default = false;
        self.switch_depth += 1;
        while !self.eat("}") {
            let test = if self.eat("case") {
                Some(self.expression(1)?)
            } else if self.eat("default") {
                if default {
                    return Err(self.error("Duplicate default clause"));
                }
                default = true;
                None
            } else {
                return Err(self.error("Expected switch case or default"));
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.at("case") && !self.at("default") && !self.at("}") {
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        self.switch_depth -= 1;
        Ok(Stmt::Switch(test, cases))
    }
    fn label_ahead(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Word(_) | TokenKind::EscapedWord(_)
        ) && self
            .lookahead(1)
            .is_some_and(|token| token.kind == TokenKind::Punct(":"))
    }

    fn labelled_statement(&mut self) -> Result<Stmt, JsError> {
        let mut names = Vec::new();
        while self.label_ahead() {
            if self.depth + names.len() >= MAX_PARSER_DEPTH {
                return Err(self.error("Parser nesting limit exceeded"));
            }
            let name = self.name()?;
            if names.contains(&name) || self.labels.iter().any(|label| label.names.contains(&name))
            {
                return Err(self.error(format!("Duplicate label '{name}'")));
            }
            names.push(name);
            self.expect(":")?;
        }

        // The lexer token budget is smaller than the label id space.
        let id = LabelId::new(self.next_label).unwrap();
        self.next_label += 1;
        self.labels.push(ActiveLabel {
            id,
            names,
            iteration: self.at("while") || self.at("do") || self.at("for"),
        });
        let item = self.statement();
        let ActiveLabel { names, .. } = self.labels.pop().unwrap();
        let item = item?;
        if matches!(
            item,
            Stmt::Variable(VarKind::Let | VarKind::Const, _) | Stmt::Class(..)
        ) || matches!(&item, Stmt::Function(f) if f.kind != FunctionKind::Ordinary)
        {
            return Err(self.error("Invalid labelled declaration"));
        }
        Ok(Stmt::Label(id, names, Box::new(item)))
    }

    #[inline(always)]
    fn if_clause(&mut self) -> Result<Stmt, JsError> {
        // Annex B permits only an ordinary FunctionDeclaration in an unbraced
        // if/else clause. The compiler rejects it in inherited strict code.
        if self.at("function")
            && !self
                .lookahead(1)
                .is_some_and(|t| t.kind == TokenKind::Punct("*"))
        {
            return self.statement();
        }
        self.controlled_statement()
    }

    fn controlled_statement(&mut self) -> Result<Stmt, JsError> {
        if self.eat("{") {
            // The controlling statement already occupies a parser frame.
            // Parse its braced body directly instead of reentering the
            // statement dispatcher; child statements and the AST stay bounded.
            let body = Stmt::Block(self.block_contents()?);
            self.check_ast(Ast::Statement(&body))?;
            return Ok(body);
        }
        if self.at("const")
            || self.at("class")
            || self.at("function")
            || self.async_function_ahead()
        {
            return Err(self.error("Declaration requires a block in statement position"));
        }
        // In a Statement, sloppy `let` can begin an expression or a label.
        // The ExpressionStatement grammar explicitly excludes the `let [` prefix.
        if self.at("let") && !self.label_ahead() {
            if self
                .lookahead(1)
                .is_some_and(|t| t.kind == TokenKind::Punct("["))
            {
                return Err(self.error("let [ cannot start an expression statement"));
            }
            let expression = self.expression(1)?;
            self.semi()?;
            return Ok(Stmt::Expression(expression));
        }
        let statement = self.statement()?;
        if matches!(&statement, Stmt::Label(_, _, item) if matches!(**item, Stmt::Function(_))) {
            return Err(self.error("Labelled function in a controlled statement"));
        }
        Ok(statement)
    }

    fn jump_target(&mut self, continuing: bool) -> Result<Option<LabelId>, JsError> {
        if self.current().newline || self.at(";") || self.at("}") || self.is_eof() {
            if self.loop_depth == 0 && (continuing || self.switch_depth == 0) {
                return Err(self.error(if continuing {
                    "Continue outside a loop"
                } else {
                    "Break outside a loop or switch"
                }));
            }
            return Ok(None);
        }

        if !matches!(
            self.current().kind,
            TokenKind::Word(_) | TokenKind::EscapedWord(_)
        ) {
            return Err(self.error("Expected a label identifier"));
        }
        let name = self.name()?;
        let label = self
            .labels
            .iter()
            .rev()
            .find(|label| label.names.contains(&name))
            .ok_or_else(|| self.error(format!("Unknown label '{name}'")))?;
        if continuing && !label.iteration {
            return Err(self.error(format!("Continue target '{name}' is not an iteration")));
        }
        Ok(Some(label.id))
    }

    fn block_contents(&mut self) -> Result<Vec<Stmt>, JsError> {
        self.block_contents_with_directives(false)
    }
    fn block_contents_with_directives(&mut self, mut in_prologue: bool) -> Result<Vec<Stmt>, JsError> {
        let mut stmts = Vec::new();
        while !self.eat("}") {
            if self.is_eof() {
                return Err(self.error("Unterminated block"));
            }
            let statement = self.statement()?;
            self.known_strict_context |= directives::visit(&statement, &mut in_prologue);
            stmts.push(statement);
        }
        Ok(stmts)
    }
    fn declarations(&mut self, kind: VarKind) -> Result<Vec<(Pattern, Option<Expr>)>, JsError> {
        let mut values = Vec::new();
        loop {
            let name = self.binding_pattern()?;
            if kind != VarKind::Var
                && bound_names(&name)
                    .iter()
                    .any(|name| name == &JsString::from("let"))
            {
                return Err(self.error("let cannot be a lexical binding name"));
            }
            let init = if self.eat("=") {
                Some(self.initializer()?)
            } else {
                None
            };
            if kind == VarKind::Const && init.is_none() && !self.at("in") && !self.at("of") {
                return Err(self.error("Const requires an initializer"));
            }
            if !matches!(name, Pattern::Name(_))
                && init.is_none()
                && !self.at("in")
                && !self.at("of")
            {
                return Err(self.error("Destructuring declaration requires an initializer"));
            }
            values.push((name, init));
            if !self.eat(",") {
                break;
            }
        }
        Ok(values)
    }
    fn for_statement(&mut self) -> Result<Stmt, JsError> {
        let asynchronous = self.eat("await");
        if asynchronous && (self.await_context != KeywordContext::Body || self.static_block) {
            return Err(self.error("for await requires an asynchronous body or a module"));
        }
        self.expect("(")?;
        let disallowed_of_target = self.at("let")
            || (!asynchronous
                && self.at("async")
                && self.lookahead(1).is_some_and(
                    |token| matches!(&token.kind, TokenKind::Word(word) if word == "of"),
                ));
        let previous = self.allow_in;
        self.allow_in = false;
        let kind = if self.eat("var") {
            Some(VarKind::Var)
        } else if self.let_declaration() {
            self.advance(1);
            Some(VarKind::Let)
        } else if self.eat("const") {
            Some(VarKind::Const)
        } else {
            None
        };
        let init = if let Some(k) = kind {
            Some(Box::new(Stmt::Variable(k, self.declarations(k)?)))
        } else if self.at(";") {
            None
        } else {
            Some(Box::new(Stmt::Expression(self.expression(1)?)))
        };
        self.allow_in = previous;
        let enumerate = self.eat("in");
        if enumerate || self.eat("of") {
            if enumerate && asynchronous {
                return Err(self.error("for await requires of"));
            }
            let mut initializer = None;
            let (kind, target) = match init.map(|stmt| *stmt) {
                Some(Stmt::Variable(kind, mut declarations))
                    if declarations.len() == 1 && (declarations[0].1.is_none() ||
                        (enumerate && kind == VarKind::Var && matches!(declarations[0].0, Pattern::Name(_)))) =>
                {
                    let (target, value) = declarations.remove(0);
                    initializer = value.map(Box::new);
                    (Some(kind), target)
                }
                Some(Stmt::Expression(target))
                    if (enumerate || !disallowed_of_target)
                        && (assignable(&target)
                            || matches!(target, Expr::Array(..) | Expr::Object(..))) =>
                {
                    // Bare array and object literals must also parse as assignment patterns.
                    let pattern = if call_assignment_target(&target) {
                        Pattern::Target(Box::new(target))
                    } else {
                        self.pattern(target, false)?
                    };
                    (None, pattern)
                }
                Some(Stmt::Expression(_)) => {
                    return Err(self.error("Invalid iteration assignment target"));
                }
                _ => {
                    return Err(self.error("Expected a single uninitialized iteration binding"));
                }
            };
            let source = self.expression(if enumerate { 1 } else { 2 })?;
            self.expect(")")?;
            self.loop_depth += 1;
            let body = self.controlled_statement();
            self.loop_depth -= 1;
            let body = Box::new(body?);
            return Ok(if enumerate {
                Stmt::ForIn(kind, target, source, body, initializer)
            } else {
                Stmt::ForOf(kind, target, source, body, asynchronous)
            });
        }
        if asynchronous {
            return Err(self.error("for await requires of"));
        }
        self.expect(";")?;
        let test = if self.at(";") {
            None
        } else {
            Some(self.expression(1)?)
        };
        self.expect(";")?;
        let update = if self.at(")") {
            None
        } else {
            Some(self.expression(1)?)
        };
        self.expect(")")?;
        self.loop_depth += 1;
        let body = self.controlled_statement();
        self.loop_depth -= 1;
        Ok(Stmt::For(init, test, update, Box::new(body?)))
    }
    fn function_kind(&mut self, is_async: bool) -> FunctionKind {
        match (is_async, self.eat("*")) {
            (true, true) => FunctionKind::AsyncGenerator,
            (true, false) => FunctionKind::Async,
            (false, true) => FunctionKind::Generator,
            (false, false) => FunctionKind::Ordinary,
        }
    }
    fn function(&mut self, start: usize, is_async: bool, require_name: bool) -> Result<Function, JsError> {
        let before = self.lexer.remaining_compilation_bytes();
        let kind = self.function_kind(is_async);
        let old_counts = (self.await_count, self.yield_count);
        let name = if !self.at("(") {
            let outer = (self.await_context, self.yield_context, self.static_block);
            if !require_name {
                self.static_block = false;
                self.await_context = kind.await_context(KeywordContext::Parameters);
                self.yield_context = kind.yield_context(KeywordContext::Parameters);
            }
            let name = self.name();
            (self.await_context, self.yield_context, self.static_block) = outer;
            Some(name?)
        } else if require_name {
            return Err(self.error("Function declaration requires a name"));
        } else {
            None
        };
        let old = (
            self.super_property,
            self.super_call,
            self.new_target,
            self.class_initializer,
            self.static_block,
            self.await_context,
            self.yield_context,
        );
        self.super_property = false;
        self.super_call = false;
        self.new_target = true;
        self.class_initializer = false;
        self.static_block = false;
        self.await_context = kind.await_context(KeywordContext::Parameters);
        self.yield_context = kind.yield_context(KeywordContext::Parameters);
        let result = (|| {
            self.expect("(")?;
            let params = self.parameters()?;
            let mut f = self.function_body(start, name, params, false, kind)?;
            f.self_binding = !require_name && f.name.is_some();
            self.compact_cold_function(&mut f, before)?;
            Ok(f)
        })();
        (
            self.super_property,
            self.super_call,
            self.new_target,
            self.class_initializer,
            self.static_block,
            self.await_context,
            self.yield_context,
        ) = old;
        (self.await_count, self.yield_count) = old_counts;
        result
    }
    fn function_body(
        &mut self,
        start: usize,
        name: Option<JsString>,
        params: Vec<Parameter>,
        arrow: bool,
        kind: FunctionKind,
    ) -> Result<Function, JsError> {
        let old_strict_context = self.known_strict_context;
        let legacy_before = self.legacy_literals;
        let old_loop = self.loop_depth;
        let old_switch = self.switch_depth;
        let old_labels = std::mem::take(&mut self.labels);
        let old_static_block = self.static_block;
        let old_await_context = self.await_context;
        let old_await_count = self.await_count;
        let old_yield_context = self.yield_context;
        let old_yield_count = self.yield_count;
        self.loop_depth = 0;
        self.switch_depth = 0;
        self.static_block = false;
        self.await_context = kind.await_context(KeywordContext::Body);
        self.yield_context = kind.yield_context(KeywordContext::Body);
        self.function_depth += 1;
        let result = if self.eat("{") {
            let previous = self.allow_in;
            self.allow_in = true;
            let result = self.block_contents_with_directives(true);
            self.allow_in = previous;
            result
        } else if arrow {
            self.expression(2)
                .map(|expr| vec![Stmt::Return(Some(expr))])
        } else {
            Err(self.error("Expected function body"))
        };
        self.function_depth -= 1;
        self.loop_depth = old_loop;
        self.switch_depth = old_switch;
        self.labels = old_labels;
        self.static_block = old_static_block;
        self.await_context = old_await_context;
        self.await_count = old_await_count;
        self.yield_context = old_yield_context;
        self.yield_count = old_yield_count;
        self.known_strict_context = old_strict_context;
        let mut body = result?;
        let strict = has_strict_directive(&body);
        if strict && self.legacy_literals != legacy_before {
            return Err(self.error("Legacy literal in strict function"));
        }
        if !strict {
            let excluded = params.iter().flat_map(|p| bound_names(&p.pattern)).chain([JsString::from("arguments")]);
            annex::mark(&mut body, excluded);
        }
        if strict && !params.iter().all(simple_parameter) {
            return Err(self.error("Use strict directive with a non-simple parameter list"));
        }
        if strict {
            let mut seen = std::collections::BTreeSet::new();
            for p in params.iter().flat_map(|p| bound_names(&p.pattern)) {
                if !seen.insert(p.clone())
                    || matches!(p.to_string_lossy().as_str(), "eval" | "arguments")
                {
                    return Err(self.error("Invalid strict-mode function parameter"));
                }
            }
        }
        Ok(Function {
            source: self.lexer.span(start, self.last_end),
            name,
            params,
            body,
            validated: None,
            arrow,
            kind,
            strict,
            self_binding: false,
            annex_binding: false,
            method: false,
        })
    }
    fn identifier_arrow_at(&self, offset: usize) -> bool {
        self.lookahead(offset).is_some_and(|token| {
            matches!(&token.kind,
                TokenKind::Word(word) | TokenKind::EscapedWord(word) if !reserved(word))
        }) && self
            .lookahead(offset + 1)
            .is_some_and(|token| token.kind == TokenKind::Punct("=>") && !token.newline)
    }

    /// Parse the shared grouped-expression, call, and arrow-parameter grammar
    /// once. Slash literals inside it are read at their expression site.
    fn cover(&mut self) -> Result<(Vec<Expr>, bool), JsError> {
        let mut parts = Vec::new();
        if self.eat(")") {
            return Ok((parts, false));
        }
        loop {
            let spread = self.eat("...");
            let previous = self.allow_in;
            self.allow_in = true;
            let value = self.initializer();
            self.allow_in = previous;
            let value = value?;
            parts.push(if spread {
                self.checked_expression(Expr::Spread(Box::new(value)))?
            } else {
                value
            });
            if self.eat(")") {
                return Ok((parts, false));
            }
            self.expect(",")?;
            if self.eat(")") {
                return Ok((parts, true));
            }
        }
    }

    fn arrow_from_cover(
        &mut self,
        start: usize,
        parts: Vec<Expr>,
        trailing: bool,
        is_async: bool,
    ) -> Result<Expr, JsError> {
        let count = parts.len();
        let mut params = Vec::with_capacity(count);
        let mut seen = std::collections::BTreeSet::new();
        for (index, value) in parts.into_iter().enumerate() {
            let (value, rest) = match value {
                Expr::Spread(value) => {
                    if index + 1 != count || trailing {
                        return Err(
                            self.error("Rest parameter must be last without a trailing comma")
                        );
                    }
                    (*value, true)
                }
                value => (value, false),
            };
            let pattern = self.pattern(value, true)?;
            if rest && matches!(pattern, Pattern::Default(..)) {
                return Err(self.error("Rest parameter cannot have a default"));
            }
            for name in bound_names(&pattern) {
                if name == JsString::from("await")
                    && (is_async
                        || self.module_goal
                        || self.static_block
                        || self.await_context != KeywordContext::None)
                {
                    return Err(self.error("Invalid await binding in arrow parameters"));
                }
                if !seen.insert(name) {
                    return Err(self.error("Duplicate arrow parameter"));
                }
            }
            params.push(Parameter { pattern, rest });
        }
        let kind = if is_async {
            FunctionKind::Async
        } else {
            FunctionKind::Ordinary
        };
        let function = self.function_body(start, None, params, true, kind)?;
        self.checked_expression(Expr::Function(Box::new(function)))
    }

    fn parameter(&mut self, rest: bool) -> Result<Parameter, JsError> {
        let expression = self.expression_in(2)?;
        let pattern = self.pattern(expression, true)?;
        if rest && matches!(pattern, Pattern::Default(..)) {
            return Err(self.error("Rest parameter cannot have a default"));
        }
        Ok(Parameter { pattern, rest })
    }

    fn parameters(&mut self) -> Result<Vec<Parameter>, JsError> {
        let mut params = Vec::new();
        if self.eat(")") {
            return Ok(params);
        }
        loop {
            let rest = self.eat("...");
            params.push(self.parameter(rest)?);
            if self.eat(")") {
                break;
            }
            self.expect(",")?;
            if rest {
                return Err(self.error("Rest parameter must be last without a trailing comma"));
            }
            if self.eat(")") {
                break;
            }
        }
        Ok(params)
    }
    fn binding_pattern(&mut self) -> Result<Pattern, JsError> {
        if self.at("[") || self.at("{") {
            let expression = self.primary()?;
            self.pattern(expression, true)
        } else {
            Ok(Pattern::Name(self.name()?))
        }
    }
    fn pattern(&self, e: Expr, binding: bool) -> Result<Pattern, JsError> {
        let pattern = match e {
            Expr::Identifier(name) => Pattern::Name(name),
            Expr::Member(..) | Expr::PrivateMember(..) if !binding => Pattern::Target(Box::new(e)),
            Expr::Array(items, trailing) => {
                let count = items.len();
                let mut out = Vec::new();
                let mut rest = None;
                for (index, item) in items.into_iter().enumerate() {
                    match item {
                        Some(Expr::Spread(value)) => {
                            if index + 1 != count || trailing || matches!(*value, Expr::Assign(..))
                            {
                                return Err(self.error("Rest element must be last without an initializer or trailing comma"));
                            }
                            let value = self.pattern(*value, binding)?;
                            if matches!(value, Pattern::Default(..)) {
                                return Err(self.error("Rest element cannot have a default"));
                            }
                            rest = Some(Box::new(value));
                        }
                        Some(value) => out.push(Some(self.pattern(value, binding)?)),
                        None => out.push(None),
                    }
                }
                Pattern::Array(out, rest)
            }
            Expr::Object(entries, trailing) => {
                let count = entries.len();
                let mut properties = Vec::new();
                let mut rest = None;
                for (index, entry) in entries.into_iter().enumerate() {
                    match entry {
                        ObjectEntry::Spread(value) => {
                            if index + 1 != count || trailing {
                                return Err(self
                                    .error("Rest property must be last without a trailing comma"));
                            }
                            let value = self.pattern(value, binding)?;
                            if !matches!(value, Pattern::Name(_))
                                && !(matches!(value, Pattern::Target(_)) && !binding)
                            {
                                return Err(self.error("Invalid object rest target"));
                            }
                            rest = Some(Box::new(value));
                        }
                        ObjectEntry::Property {
                            key,
                            value,
                            computed,
                            method,
                            ..
                        } => {
                            if method.is_some() {
                                return Err(self.error("Invalid method in a destructuring target"));
                            }
                            properties.push((key, self.pattern(value, binding)?, computed));
                        }
                    }
                }
                Pattern::Object(properties, rest)
            }
            Expr::Assign("=", target, value) => {
                Pattern::Default(Box::new(self.pattern(*target, binding)?), value)
            }
            Expr::Destructure(pattern, value) => Pattern::Default(Box::new(pattern), value),
            Expr::Group(value) if !binding && assignable(&value) => self.pattern(*value, false)?,
            _ => return Err(self.error("Invalid destructuring target")),
        };
        if binding {
            let mut pending = vec![&pattern];
            while let Some(p) = pending.pop() {
                match p {
                    Pattern::Target(_) => {
                        return Err(self.error("Member expression is not a binding target"));
                    }
                    Pattern::Array(items, rest) => {
                        pending.extend(items.iter().flatten());
                        pending.extend(rest.iter().map(Box::as_ref));
                    }
                    Pattern::Object(properties, rest) => {
                        pending.extend(properties.iter().map(|(_, p, _)| p));
                        pending.extend(rest.iter().map(Box::as_ref));
                    }
                    Pattern::Default(p, _) => pending.push(p),
                    _ => {}
                }
            }
        }
        Ok(pattern)
    }
    // These leading tokens identify a complete primary expression. Parse
    // container entries or function bodies directly and keep only their
    // real recursive grammar edges live while reading an initializer.
    #[inline(always)]
    fn initializer(&mut self) -> Result<Expr, JsError> {
        if self.at("[") || self.at("{") {
            self.literal_initializer()
        } else if self.at("function") || self.async_function_ahead() {
            self.function_initializer()
        } else {
            self.expression(2)
        }
    }
    fn literal_initializer(&mut self) -> Result<Expr, JsError> {
        let previous = self.allow_in;
        self.allow_in = true;
        let literal = self.container_literal();
        self.allow_in = previous;
        self.initializer_tail(literal?)
    }
    fn function_initializer(&mut self) -> Result<Expr, JsError> {
        let start = self.current().start;
        let is_async = self.eat("async");
        self.expect("function")?;
        let previous = self.allow_in;
        self.allow_in = true;
        let function = self.function(start, is_async, false);
        self.allow_in = previous;
        let primary = self.checked_expression(Expr::Function(Box::new(function?)))?;
        self.initializer_tail(primary)
    }
    fn initializer_tail(&mut self, primary: Expr) -> Result<Expr, JsError> {
        // The primary expression has already passed the prefix portion of parsing.
        // Its postfix/infix suffix may recurse and still uses the grammar limit.
        if self.depth >= MAX_PARSER_DEPTH {
            return Err(self.error("Expression nesting limit exceeded"));
        }
        self.depth += 1;
        let result = self
            .finish_unary(primary)
            .and_then(|left| self.expression_tail(2, left, false));
        self.depth -= 1;
        self.checked_expression(result?)
    }

    fn expression(&mut self, min: u8) -> Result<Expr, JsError> {
        if self.depth >= MAX_PARSER_DEPTH {
            return Err(self.error("Expression nesting limit exceeded"));
        }
        self.depth += 1;
        let previous = self.allow_in;
        let result = self
            .expression_prefix(min)
            .and_then(|(left, arrow)| self.expression_tail(min, left, arrow));
        self.allow_in = previous;
        self.depth -= 1;
        self.checked_expression(result?)
    }
    fn check_ast(&self, root: Ast<'_>) -> Result<(), JsError> {
        if ast_within_budget(root) {
            Ok(())
        } else {
            Err(self.error("AST nesting limit exceeded"))
        }
    }
    fn checked_expression(&self, expression: Expr) -> Result<Expr, JsError> {
        self.check_ast(Ast::Expression(&expression))?;
        Ok(expression)
    }
    // A bare arrow is an AssignmentExpression. Only a comma can follow it
    // directly, whereas an arrow inside parentheses can take normal operators.
    fn expression_prefix(&mut self, min: u8) -> Result<(Expr, bool), JsError> {
        let start = self.current().start;
        let async_prefix =
            min <= 2 && self.at("async") && self.lookahead(1).is_some_and(|next| !next.newline);
        let async_word_arrow = async_prefix && self.identifier_arrow_at(1);
        if min <= 2 && (async_word_arrow || self.identifier_arrow_at(0)) {
            if async_word_arrow {
                self.advance(1);
            }
            let name = self.name()?;
            self.expect("=>")?;
            return self
                .arrow_from_cover(start, vec![Expr::Identifier(name)], false, async_word_arrow)
                .map(|arrow| (arrow, true));
        }

        let async_parens = async_prefix
            && self
                .lookahead(1)
                .is_some_and(|next| next.kind == TokenKind::Punct("("));
        let left = if matches!(self.current().kind, TokenKind::PrivateIdentifier(_)) {
            if min > 10 || !self.allow_in {
                return Err(self.error("Unexpected private identifier"));
            }
            let name = self.private_use()?;
            self.expect("in")?;
            let object = self.expression(11)?;
            self.checked_expression(Expr::PrivateIn(name, Box::new(object)))?
        } else if min <= 2 && self.yield_context == KeywordContext::Body && self.at("yield") {
            self.yield_expression()?
        } else if min <= 2 && (self.at("(") || async_parens) {
            self.advance(if async_parens { 2 } else { 1 });
            let awaits = self.await_count;
            let yields = self.yield_count;
            let (mut parts, trailing) = self.cover()?;
            if self.at("=>") && !self.current().newline {
                if self.yield_count != yields {
                    return Err(self.error("yield is not allowed in arrow parameters"));
                }
                if self.await_count != awaits
                    && (async_parens || self.await_context != KeywordContext::None)
                {
                    return Err(self.error("await is not allowed in arrow parameters"));
                }
                self.advance(1);
                return self
                    .arrow_from_cover(start, parts, trailing, async_parens)
                    .map(|arrow| (arrow, true));
            }
            let primary = if async_parens {
                Expr::Call(Box::new(Expr::Identifier("async".into())), parts)
            } else {
                if parts.is_empty()
                    || trailing
                    || parts.iter().any(|part| matches!(part, Expr::Spread(_)))
                {
                    return Err(self.error("Invalid parenthesized expression"));
                }
                let inner = if parts.len() == 1 {
                    parts.pop().unwrap()
                } else {
                    self.checked_expression(Expr::Sequence(parts))?
                };
                Expr::Group(Box::new(inner))
            };
            self.finish_unary(self.checked_expression(primary)?)?
        } else {
            self.unary()?
        };
        Ok((left, false))
    }
    fn expression_tail(
        &mut self,
        mut min: u8,
        mut left: Expr,
        mut arrow: bool,
    ) -> Result<Expr, JsError> {
        enum Pending {
            Operator {
                op: &'static str,
                left: Expr,
                assignment: bool,
            },
            Consequent {
                test: Expr,
                allow_in: bool,
            },
            Alternate(Vec<(Expr, Expr)>),
            Sequence(Vec<Expr>),
        }
        let mut pending = Vec::new();
        loop {
            let assignment_only = arrow || matches!(left, Expr::Yield { .. });
            let (frame, next_min) = if min <= 3 && !assignment_only && self.eat("?") {
                let allow_in = self.allow_in;
                self.allow_in = true;
                (
                    Pending::Consequent {
                        test: left,
                        allow_in,
                    },
                    2,
                )
            } else if let Some((op, prec, right_assoc)) = self.operator()
                && prec >= min
                && (!assignment_only || op == ",")
            {
                if op == "**" && matches!(left, Expr::Await(_) | Expr::Unary(..)) {
                    return Err(self.error("A unary expression on the left side of exponentiation requires parentheses"));
                }
                self.advance(1);
                if op == "," {
                    (Pending::Sequence(vec![left]), 2)
                } else {
                    (
                        Pending::Operator {
                            op,
                            left,
                            assignment: prec == 2,
                        },
                        if right_assoc { prec } else { prec + 1 },
                    )
                }
            } else {
                let Some((frame, outer_min)) = pending.pop() else {
                    return Ok(left);
                };
                arrow = false;
                min = outer_min;
                match frame {
                    Pending::Operator {
                        op,
                        left: lhs,
                        assignment,
                    } => {
                        left = self.combine_operator(op, lhs, left, assignment)?;
                        continue;
                    }
                    Pending::Consequent { test, allow_in } => {
                        self.allow_in = allow_in;
                        self.expect(":")?;
                        // A conditional in the alternate is the next branch of
                        // the same chain. Preserve the outer binding power when
                        // replacing that pending frame with the extended chain.
                        let mut branches = Vec::new();
                        if matches!(pending.last(), Some((Pending::Alternate(_), _))) {
                            let Some((Pending::Alternate(previous), outer)) = pending.pop() else {
                                unreachable!()
                            };
                            branches = previous;
                            min = outer;
                        }
                        branches.push((test, left));
                        (Pending::Alternate(branches), 2)
                    }
                    Pending::Alternate(branches) => {
                        left = self.checked_expression(Expr::Conditional(branches, Box::new(left)))?;
                        continue;
                    }
                    Pending::Sequence(mut values) => {
                        values.push(left);
                        if self.eat(",") {
                            (Pending::Sequence(values), 2)
                        } else {
                            left = self.checked_expression(Expr::Sequence(values))?;
                            continue;
                        }
                    }
                }
            };
            // Every unresolved operator or list contributes a node to the
            // resulting tree. Parsing its RHS does not recurse on the native stack.
            if pending.len() + 1 >= MAX_AST_DEPTH {
                return Err(self.error("AST nesting limit exceeded"));
            }
            pending.push((frame, min));
            min = next_min;
            (left, arrow) = self.expression_prefix(min)?;
        }
    }
    fn combine_operator(
        &self,
        op: &'static str,
        left: Expr,
        right: Expr,
        assignment: bool,
    ) -> Result<Expr, JsError> {
        let mixing = |value: &Expr, operators: &[&str]| matches!(value,Expr::Binary(operator,..)if operators.contains(operator));
        if (op == "??" && (mixing(&left, &["&&", "||"]) || mixing(&right, &["&&", "||"])))
            || (matches!(op, "&&" | "||") && (mixing(&left, &["??"]) || mixing(&right, &["??"])))
        {
            return Err(self.error(
                "Nullish coalescing cannot mix with logical operators without parentheses",
            ));
        }
        let expression = if op == "+" {
            match (left, right) {
                (Expr::Literal(Value::String(a)), Expr::Literal(Value::String(b))) => {
                    let mut units = Vec::with_capacity(a.len() + b.len());
                    units.extend_from_slice(a.as_utf16());
                    units.extend_from_slice(b.as_utf16());
                    Expr::JoinedString(units)
                }
                (Expr::JoinedString(mut units), Expr::Literal(Value::String(b))) => {
                    units.extend_from_slice(b.as_utf16());
                    Expr::JoinedString(units)
                }
                (left, right) => Expr::Binary(op, Box::new(left), Box::new(right)),
            }
        } else if !assignment {
            Expr::Binary(op, Box::new(left), Box::new(right))
        } else if op == "=" && matches!(left, Expr::Array(..) | Expr::Object(..)) {
            Expr::Destructure(self.pattern(left, false)?, Box::new(right))
        } else {
            if !assignable(&left) {
                return Err(self.error("Invalid assignment target"));
            }
            Expr::Assign(op, Box::new(left), Box::new(right))
        };
        self.checked_expression(expression)
    }
    fn yield_expression(&mut self) -> Result<Expr, JsError> {
        self.advance(1);
        self.yield_count += 1;
        let asynchronous = self.await_context == KeywordContext::Body;
        if self.current().newline {
            return Ok(Expr::Yield {
                value: None,
                delegate: false,
                asynchronous,
            });
        }
        let delegate = self.eat("*");
        let has_operand = delegate
            || !matches!(
                self.current().kind,
                TokenKind::Eof
                    | TokenKind::Punct(";" | "," | ":" | ")" | "]" | "}")
                    | TokenKind::Template { head: false, .. }
            );
        let value = has_operand
            .then(|| self.expression(2).map(Box::new))
            .transpose()?;
        self.checked_expression(Expr::Yield {
            value,
            delegate,
            asynchronous,
        })
    }
    fn expression_in(&mut self, min: u8) -> Result<Expr, JsError> {
        let previous = self.allow_in;
        self.allow_in = true;
        let result = self.expression(min);
        self.allow_in = previous;
        result
    }
    fn operator(&self) -> Option<(&'static str, u8, bool)> {
        let s = match &self.current().kind {
            TokenKind::Punct(s) => *s,
            TokenKind::Word(w) if w == "in" && self.allow_in => "in",
            TokenKind::Word(w) if w == "instanceof" => "instanceof",
            _ => return None,
        };
        Some(match s {
            "," => (s, 1, false),
            "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "**=" | "<<=" | ">>=" | ">>>=" | "&="
            | "|=" | "^=" | "&&=" | "||=" | "??=" => (s, 2, true),
            "??" | "||" => (s, 4, false),
            "&&" => (s, 5, false),
            "|" => (s, 6, false),
            "^" => (s, 7, false),
            "&" => (s, 8, false),
            "==" | "!=" | "===" | "!==" => (s, 9, false),
            "<" | ">" | "<=" | ">=" | "in" | "instanceof" => (s, 10, false),
            "<<" | ">>" | ">>>" => (s, 11, false),
            "+" | "-" => (s, 12, false),
            "*" | "/" | "%" => (s, 13, false),
            "**" => (s, 14, true),
            _ => return None,
        })
    }
    fn unary(&mut self) -> Result<Expr, JsError> {
        if self.at("await") {
            match self.await_context {
                KeywordContext::None => {}
                KeywordContext::Parameters => {
                    return Err(
                        self.error("await is not allowed in this initializer or parameter list")
                    );
                }
                KeywordContext::Body if !self.static_block => {
                    self.await_count += 1;
                    self.advance(1);
                    let value = self.expression(15)?;
                    return self.checked_expression(Expr::Await(Box::new(value)));
                }
                KeywordContext::Body => {}
            }
        }
        for op in ["!", "~", "+", "-", "typeof", "void", "delete", "++", "--"] {
            if self.eat(op) {
                let e = self.expression(15)?;
                if matches!(op, "++" | "--") {
                    if !assignable(&e) {
                        return Err(self.error("Invalid update target"));
                    }
                    return self.checked_expression(Expr::Update(op, Box::new(e), true));
                }
                if op == "delete" && private_reference(&e) {
                    return Err(self.error("Cannot delete a private element"));
                }
                return self.checked_expression(Expr::Unary(op, Box::new(e)));
            }
        }
        let mut constructors = 0;
        let mut e = loop {
            if !self.eat("new") { break self.primary()?; }
            if self.eat(".") {
                self.expect("target")?;
                if !self.new_target {
                    return Err(self.error("new.target outside a function"));
                }
                break Expr::NewTarget;
            }
            constructors += 1;
            if constructors > MAX_AST_DEPTH {
                return Err(self.error("Constructor nesting limit exceeded"));
            }
        };
        if constructors > 0 && matches!(e, Expr::DynamicImport(..)) {
            return Err(self.error("A dynamic import call cannot follow new"));
        }
        for _ in 0..constructors {
            let constructor = self.postfix(e, false)?;
            let args = if self.eat("(") { self.arguments()? } else { Vec::new() };
            e = self.checked_expression(Expr::New(Box::new(constructor), args))?;
        }
        self.finish_unary(e)
    }
    fn finish_unary(&mut self, primary: Expr) -> Result<Expr, JsError> {
        let e = self.postfix(primary, true)?;
        if !self.current().newline {
            for op in ["++", "--"] {
                if self.eat(op) {
                    if !assignable(&e) {
                        return Err(self.error("Invalid update target"));
                    }
                    return self.checked_expression(Expr::Update(op, Box::new(e), false));
                }
            }
        }
        Ok(e)
    }
    fn postfix(&mut self, mut e: Expr, calls: bool) -> Result<Expr, JsError> {
        loop {
            if self.eat("?.") {
                if matches!(e, Expr::Super) {
                    return Err(self.error("Optional super chain"));
                }
                if !calls {
                    return Err(self.error("Optional chain cannot be the constructor of new"));
                }
                let link = if self.eat("[") {
                    let key = self.expression_in(1)?;
                    self.expect("]")?;
                    ChainLink::Member(key, true)
                } else if self.eat("(") {
                    ChainLink::Call(self.arguments()?, true)
                } else if matches!(self.current().kind, TokenKind::PrivateIdentifier(_)) {
                    ChainLink::PrivateMember(self.private_use()?, true)
                } else {
                    ChainLink::Member(Expr::Literal(Value::String(self.member_name()?)), true)
                };
                e = self.chain_link(e, link)?;
            } else if matches!(self.current().kind, TokenKind::Template { head: true, .. }) {
                if matches!(e, Expr::Chain(..)) {
                    return Err(self.error("An optional chain cannot be a template tag"));
                }
                e = self.template(Some(e))?;
            } else if self.eat(".") {
                e = if matches!(self.current().kind, TokenKind::PrivateIdentifier(_)) {
                    if matches!(e, Expr::Super) {
                        return Err(self.error("Private names cannot be used with super"));
                    }
                    let name = self.private_use()?;
                    if matches!(e, Expr::Chain(..)) {
                        self.chain_link(e, ChainLink::PrivateMember(name, false))?
                    } else {
                        self.checked_expression(Expr::PrivateMember(Box::new(e), name))?
                    }
                } else {
                    let key = Expr::Literal(Value::String(self.member_name()?));
                    if matches!(e, Expr::Chain(..)) {
                        self.chain_link(e, ChainLink::Member(key, false))?
                    } else {
                        let member =
                            self.checked_expression(Expr::Member(Box::new(e), Box::new(key)))?;
                        if calls && self.eat("(") {
                            let args = self.arguments()?;
                            self.method_calls(member, args)?
                        } else {
                            member
                        }
                    }
                };
            } else if self.eat("[") {
                let key = self.expression_in(1)?;
                self.expect("]")?;
                e = if matches!(e, Expr::Chain(..)) {
                    self.chain_link(e, ChainLink::Member(key, false))?
                } else {
                    self.checked_expression(Expr::Member(Box::new(e), Box::new(key)))?
                };
            } else if calls && self.eat("(") {
                let args = self.arguments()?;
                e = if matches!(e, Expr::Chain(..)) {
                    self.chain_link(e, ChainLink::Call(args, false))?
                } else {
                    self.checked_expression(Expr::Call(Box::new(e), args))?
                };
            } else {
                break;
            }
        }
        Ok(e)
    }
    fn method_calls(&mut self, first_member: Expr, mut args: Vec<Expr>) -> Result<Expr, JsError> {
        let mut links = Vec::new();
        while self.at(".")
            && self.lookahead(1).is_some_and(|t| {
                matches!(t.kind, TokenKind::Word(_) | TokenKind::EscapedWord(_))
            })
            && self.lookahead(2).is_some_and(|t| matches!(t.kind, TokenKind::Punct("(")))
        {
            self.advance(1);
            let key = Expr::Literal(Value::String(self.member_name()?));
            self.expect("(")?;
            links.push(ChainLink::Call(args, false));
            links.push(ChainLink::Member(key, false));
            args = self.arguments()?;
        }
        // Keep the outer Call for assignment-target and tagged-template grammar.
        // The existing chain emitter keeps every intermediate property reference
        // (including its receiver) until the subsequent call consumes it.
        let callee = if links.is_empty() {
            first_member
        } else {
            self.checked_expression(Expr::Chain(Box::new(first_member), links))?
        };
        self.checked_expression(Expr::Call(Box::new(callee), args))
    }
    fn chain_link(&self, e: Expr, link: ChainLink) -> Result<Expr, JsError> {
        let e = if let Expr::Chain(base, mut links) = e {
            if links.len() >= MAX_AST_DEPTH {
                return Err(self.error("Optional chain nesting limit exceeded"));
            }
            links.push(link);
            Expr::Chain(base, links)
        } else {
            Expr::Chain(Box::new(e), vec![link])
        };
        self.checked_expression(e)
    }
    fn template(&mut self, tag: Option<Expr>) -> Result<Expr, JsError> {
        let mut parts = Vec::new();
        let mut substitutions = Vec::new();
        loop {
            let TokenKind::Template {
                raw, cooked, tail, ..
            } = self.current().kind.clone()
            else {
                return Err(self.error("Expected template continuation"));
            };
            if tag.is_none() && cooked.is_none() {
                return Err(self.error("Invalid escape in untagged template"));
            }
            parts.push(TemplatePart { raw, cooked });
            self.advance(1);
            if tail {
                break;
            }
            substitutions.push(self.expression_in(1)?);
        }
        self.checked_expression(Expr::Template(parts, substitutions, tag.map(Box::new)))
    }
    fn arguments(&mut self) -> Result<Vec<Expr>, JsError> {
        // Collect ordinary calls that immediately begin the preceding call's
        // first argument. Handle context-sensitive names with the normal grammar.
        let mut names = Vec::new();
        while matches!(&self.current().kind, TokenKind::Word(name) | TokenKind::EscapedWord(name)
                if !matches!(name.as_str(), "async" | "arguments" | "yield" | "await")
                    && self.identifier_allowed(name))
            && self.lookahead(1).is_some_and(|t| t.kind == TokenKind::Punct("("))
        {
            names.push(self.name()?);
            self.advance(1);
        }
        let (mut args, _) = self.cover()?;
        for name in names.into_iter().rev() {
            let call = if !args.is_empty() && !matches!(args[0], Expr::Spread(_)) {
                let rest = args.split_off(1);
                let first = args.pop().unwrap();
                let (base, mut levels) = match first {
                    Expr::NestedCalls(base, levels) => (base, levels),
                    first => (Box::new(first), Vec::new()),
                };
                levels.push((name, rest));
                Expr::NestedCalls(base, levels)
            } else {
                Expr::Call(Box::new(Expr::Identifier(name)), args)
            };
            args = if self.at(")") || self.at(",") {
                vec![call]
            } else {
                // A suffix belongs to this first argument, just as if it had
                // been parsed by cover() in its own recursive invocation.
                let previous = self.allow_in;
                self.allow_in = true;
                let first = self.initializer_tail(call);
                self.allow_in = previous;
                vec![first?]
            };
            if self.eat(",") {
                let (rest, _) = self.cover()?;
                args.extend(rest);
            } else {
                self.expect(")")?;
            }
        }
        Ok(args)
    }
    fn async_function_ahead(&self) -> bool {
        self.at("async")
            && self.lookahead(1).is_some_and(|next| {
                !next.newline && matches!(&next.kind, TokenKind::Word(word) if word == "function")
            })
    }

    fn async_method_ahead(&self) -> bool {
        self.at("async")
            && self.lookahead(1).is_some_and(|next| {
                !next.newline
                    && matches!(
                        next.kind,
                        TokenKind::Word(_)
                            | TokenKind::EscapedWord(_)
                            | TokenKind::String(_)
                            | TokenKind::Number(_)
                            | TokenKind::BigInt(_)
                            | TokenKind::PrivateIdentifier(_)
                            | TokenKind::Punct("[" | "*")
                    )
            })
    }

    fn accessor_ahead(&self, allow_private: bool) -> Option<MethodKind> {
        let kind = if self.at("get") {
            MethodKind::Getter
        } else if self.at("set") {
            MethodKind::Setter
        } else {
            return None;
        };
        match &self.lookahead(1)?.kind {
            TokenKind::Word(_)
            | TokenKind::EscapedWord(_)
            | TokenKind::String(_)
            | TokenKind::Number(_)
            | TokenKind::BigInt(_)
            | TokenKind::Punct("[") => Some(kind),
            TokenKind::PrivateIdentifier(_) if allow_private => Some(kind),
            _ => None,
        }
    }

    fn primary(&mut self) -> Result<Expr, JsError> {
        let previous = self.allow_in;
        self.allow_in = true;
        let result = self.primary_inner();
        self.allow_in = previous;
        result
    }
    fn primary_inner(&mut self) -> Result<Expr, JsError> {
        let start = self.current().start;
        if self.at("yield") && self.yield_context != KeywordContext::None {
            return Err(self.error(
                "yield is only allowed here as an assignment expression in a generator body",
            ));
        }
        if matches!(self.current().kind, TokenKind::Template { head: true, .. }) {
            return self.template(None);
        }
        if self.at("/") || self.at("/=") {
            let slash = &self.tokens[0];
            let (source, flags) = self.lexer.read_regexp(slash)?;
            let program = crate::regexp::compile(source, flags).map_err(|mut error| {
                error.line = Some(slash.line);
                error.column = Some(slash.column);
                error
            })?;
            self.last_end = self.lexer.position();
            self.tokens.clear();
            self.fill();
            return Ok(Expr::RegExp(program));
        }
        if self.at("...") {
            // List productions consume spread/rest before parsing their operand.
            return Err(self.error("Unexpected '...' in an expression"));
        }
        if self.eat("class") {
            return self.class(start, false).map(|class| Expr::Class(Box::new(class)));
        }
        if self.eat("super") {
            if !self.super_property && !self.super_call {
                return Err(self.error("super outside a method"));
            }
            if self.at("(") && !self.super_call {
                return Err(self.error("super call outside a derived constructor"));
            }
            if !self.at("(") && !self.at(".") && !self.at("[") {
                return Err(self.error("Invalid super expression"));
            }
            return Ok(Expr::Super);
        }
        if self.eat("import") {
            let phase = if self.at(".") && self.lookahead(1).is_some_and(|token| {
                matches!(&token.kind, TokenKind::Word(name) if name == "defer" || name == "source")
            }) {
                self.expect(".")?;
                if self.eat("defer") { Some(ImportPhase::Defer) }
                else { self.expect("source")?; Some(ImportPhase::Source) }
            } else if self.at("(") { Some(ImportPhase::Evaluation) } else { None };
            if let Some(phase) = phase {
                self.expect("(")?;
                let specifier = Box::new(self.expression_in(2)?);
                let options = if self.eat(",") && !self.at(")") {
                    let options = self.expression_in(2)?;
                    self.eat(",");
                    Some(Box::new(options))
                } else {
                    None
                };
                self.expect(")")?;
                return self.checked_expression(Expr::DynamicImport(phase, specifier, options));
            }
            if !self.module_goal {
                return Err(self.error("import.meta outside a module"));
            }
            self.expect(".")?;
            self.expect("meta")?;
            return Ok(Expr::ImportMeta);
        }
        if self.static_block && self.at("await") {
            return Err(self.error("await in a class static block"));
        }
        if self.module_goal && self.at("await") {
            return Err(self.error("await is reserved in module code"));
        }
        if self.eat("(") {
            let e = self.expression(1)?;
            self.expect(")")?;
            return self.checked_expression(Expr::Group(Box::new(e)));
        }
        if self.at("[") || self.at("{") {
            return self.container_literal();
        }
        if self.async_function_ahead() {
            self.advance(2);
            let function = self.function(start, true, false)?;
            return self.checked_expression(Expr::Function(Box::new(function)));
        }
        if self.eat("function") {
            let function = self.function(start, false, false)?;
            return self.checked_expression(Expr::Function(Box::new(function)));
        }
        if self.eat("this") {
            return Ok(Expr::This);
        }
        if self.eat("true") {
            return Ok(Expr::Literal(Value::Bool(true)));
        }
        if self.eat("false") {
            return Ok(Expr::Literal(Value::Bool(false)));
        }
        if self.eat("null") {
            return Ok(Expr::Literal(Value::Null));
        }
        match self.current().kind.clone() {
            TokenKind::Number(n) => {
                self.advance(1);
                Ok(Expr::Literal(Value::Number(n)))
            }
            TokenKind::BigInt(value) => {
                self.advance(1);
                Ok(Expr::Literal(Value::BigInt(value)))
            }
            TokenKind::String(s) => {
                self.advance(1);
                Ok(Expr::Literal(Value::String(s)))
            }
            TokenKind::Word(w) | TokenKind::EscapedWord(w) if self.identifier_allowed(&w) => {
                if self.class_initializer && w == "arguments" {
                    return Err(self.error("arguments in a class initializer"));
                }
                self.await_count += usize::from(w == "await");
                self.advance(1);
                Ok(Expr::Identifier(w.into()))
            }
            _ => Err(self.error("Expected an expression")),
        }
    }
}
enum Ast<'a> {
    Expression(&'a Expr),
    Statement(&'a Stmt),
    Pattern(&'a Pattern),
    Class(&'a Class),
    Function(&'a Function),
}

/// Iterative inspection is intentional: a depth guard must not itself recurse.
/// Children were checked when constructed; each wrapper can exceed the budget
/// by only one node before it is rejected, keeping error-path destruction safe.
fn ast_within_budget(root: Ast<'_>) -> bool {
    let mut pending = vec![(root, 1usize)];
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_AST_DEPTH {
            return false;
        }
        let child_depth = depth + 1;
        match node {
            Ast::Class(class) => push_class_ast(class, child_depth, &mut pending),
            Ast::Function(function) => push_function_ast(function, child_depth, &mut pending),
            Ast::Pattern(pattern) => match pattern {
                Pattern::Name(_) => {}
                Pattern::Target(target) => pending.push((Ast::Expression(target), child_depth)),
                Pattern::Default(pattern, value) => {
                    pending.push((Ast::Pattern(pattern), child_depth));
                    pending.push((Ast::Expression(value), child_depth));
                }
                Pattern::Array(items, rest) => {
                    pending.extend(
                        items
                            .iter()
                            .flatten()
                            .map(|p| (Ast::Pattern(p), child_depth)),
                    );
                    pending.extend(rest.iter().map(|p| (Ast::Pattern(p), child_depth)));
                }
                Pattern::Object(properties, rest) => {
                    for (key, pattern, _) in properties {
                        pending.push((Ast::Expression(key), child_depth));
                        pending.push((Ast::Pattern(pattern), child_depth));
                    }
                    pending.extend(rest.iter().map(|p| (Ast::Pattern(p), child_depth)));
                }
            },
            Ast::Expression(expression) => match expression {
                Expr::Literal(_)
                | Expr::JoinedString(_)
                | Expr::RegExp(_)
                | Expr::Identifier(_)
                | Expr::This
                | Expr::Super
                | Expr::NewTarget
                | Expr::ImportMeta => {}
                Expr::Class(class) => push_class_ast(class, child_depth, &mut pending),
                Expr::Array(items, _) => pending.extend(
                    items
                        .iter()
                        .flatten()
                        .map(|e| (Ast::Expression(e), child_depth)),
                ),
                Expr::Object(items, _) => {
                    for entry in items {
                        match entry {
                            ObjectEntry::Property { key, value, .. } => {
                                pending.push((Ast::Expression(key), child_depth));
                                pending.push((Ast::Expression(value), child_depth));
                            }
                            ObjectEntry::Spread(value) => {
                                pending.push((Ast::Expression(value), child_depth))
                            }
                        }
                    }
                }
                Expr::Destructure(pattern, value) => {
                    pending.push((Ast::Pattern(pattern), child_depth));
                    pending.push((Ast::Expression(value), child_depth));
                }
                Expr::Template(_, substitutions, tag) => {
                    pending.extend(
                        substitutions
                            .iter()
                            .map(|e| (Ast::Expression(e), child_depth)),
                    );
                    pending.extend(tag.iter().map(|e| (Ast::Expression(e), child_depth)));
                }
                Expr::Chain(base, links) => {
                    pending.push((Ast::Expression(base), child_depth));
                    for link in links {
                        match link {
                            ChainLink::Member(key, _) => {
                                pending.push((Ast::Expression(key), child_depth))
                            }
                            ChainLink::Call(args, _) => pending
                                .extend(args.iter().map(|e| (Ast::Expression(e), child_depth))),
                            ChainLink::PrivateMember(..) => {}
                        }
                    }
                }
                Expr::Function(function) => {
                    push_function_ast(function, child_depth, &mut pending);
                }
                Expr::Member(a, b) | Expr::Binary(_, a, b) | Expr::Assign(_, a, b) => {
                    pending.push((Ast::Expression(a), child_depth));
                    pending.push((Ast::Expression(b), child_depth));
                }
                Expr::DynamicImport(_, value, options) => {
                    pending.push((Ast::Expression(value), child_depth));
                    pending.extend(options.iter().map(|e| (Ast::Expression(e), child_depth)));
                }
                Expr::Sequence(terms) => {
                    pending.extend(terms.iter().map(|e| (Ast::Expression(e), child_depth)));
                }
                Expr::NestedCalls(base, levels) => {
                    pending.push((Ast::Expression(base), child_depth));
                    for (_, args) in levels {
                        pending.extend(args.iter().map(|e| (Ast::Expression(e), child_depth)));
                    }
                }
                Expr::Call(target, args) | Expr::New(target, args) => {
                    pending.push((Ast::Expression(target), child_depth));
                    pending.extend(args.iter().map(|e| (Ast::Expression(e), child_depth)));
                }
                Expr::Unary(_, e)
                | Expr::Update(_, e, _)
                | Expr::Spread(e)
                | Expr::Group(e)
                | Expr::Await(e)
                | Expr::PrivateMember(e, _)
                | Expr::PrivateIn(_, e) => pending.push((Ast::Expression(e), child_depth)),
                Expr::Yield { value, .. } => {
                    pending.extend(value.iter().map(|e| (Ast::Expression(e), child_depth)));
                }
                Expr::Conditional(branches, fallback) => {
                    for (test, yes) in branches {
                        pending.push((Ast::Expression(test), child_depth));
                        pending.push((Ast::Expression(yes), child_depth));
                    }
                    pending.push((Ast::Expression(fallback), child_depth));
                }
            },
            Ast::Statement(statement) => match statement {
                Stmt::Empty | Stmt::EscapedStringStatement(_) | Stmt::Break(_) | Stmt::Continue(_) => {}
                Stmt::Label(_, _, item) => pending.push((Ast::Statement(item), child_depth)),
                Stmt::Class(_, class) => push_class_ast(class, child_depth, &mut pending),
                Stmt::DefaultExport(value) => pending.push((Ast::Expression(value), child_depth)),
                Stmt::Expression(e) | Stmt::Throw(e) => {
                    pending.push((Ast::Expression(e), child_depth))
                }
                Stmt::Block(body) => {
                    pending.extend(body.iter().map(|s| (Ast::Statement(s), child_depth)))
                }
                Stmt::Function(function) | Stmt::DefaultFunction(function) => {
                    push_function_ast(function, child_depth, &mut pending);
                }
                Stmt::Variable(_, declarations) => {
                    for (pattern, value) in declarations {
                        pending.push((Ast::Pattern(pattern), child_depth));
                        pending.extend(value.iter().map(|e| (Ast::Expression(e), child_depth)));
                    }
                }
                Stmt::If(arms, no) => {
                    for (test, body) in arms {
                        pending.push((Ast::Expression(test), child_depth));
                        pending.push((Ast::Statement(body), child_depth));
                    }
                    pending.extend(no.iter().map(|s| (Ast::Statement(s), child_depth)));
                }
                Stmt::With(test, body) | Stmt::While(test, body) | Stmt::DoWhile(body, test) => {
                    pending.push((Ast::Expression(test), child_depth));
                    pending.push((Ast::Statement(body), child_depth));
                }
                Stmt::ForIn(_, pattern, test, body, _) | Stmt::ForOf(_, pattern, test, body, _) => {
                    if let Stmt::ForIn(_, _, _, _, Some(initializer)) = statement {
                        pending.push((Ast::Expression(initializer), child_depth));
                    }
                    pending.push((Ast::Pattern(pattern), child_depth));
                    pending.push((Ast::Expression(test), child_depth));
                    pending.push((Ast::Statement(body), child_depth));
                }
                Stmt::For(initial, test, update, body) => {
                    pending.extend(initial.iter().map(|s| (Ast::Statement(s), child_depth)));
                    pending.extend(
                        test.iter()
                            .chain(update.iter())
                            .map(|e| (Ast::Expression(e), child_depth)),
                    );
                    pending.push((Ast::Statement(body), child_depth));
                }
                Stmt::Switch(test, cases) => {
                    pending.push((Ast::Expression(test), child_depth));
                    for (test, body) in cases {
                        pending.extend(test.iter().map(|e| (Ast::Expression(e), child_depth)));
                        pending.extend(body.iter().map(|s| (Ast::Statement(s), child_depth)));
                    }
                }
                Stmt::Return(expression) => {
                    pending.extend(expression.iter().map(|e| (Ast::Expression(e), child_depth)))
                }
                Stmt::Try(body, catch, finally) => {
                    pending.push((Ast::Statement(body), child_depth));
                    pending.extend(catch.iter().map(|(_, s)| (Ast::Statement(s), child_depth)));
                    if let Some((Some(pattern), _)) = catch {
                        pending.push((Ast::Pattern(pattern), child_depth));
                    }
                    pending.extend(finally.iter().map(|s| (Ast::Statement(s), child_depth)));
                }
            },
        }
    }
    true
}

pub(crate) fn call_assignment_target(mut e: &Expr) -> bool {
    while let Expr::Group(inner) = e { e = inner; }
    matches!(e, Expr::NestedCalls(..))
        || matches!(e, Expr::Call(callee, _) if !matches!(callee.as_ref(), Expr::Super))
}

fn assignable(e: &Expr) -> bool {
    match e {
        Expr::Identifier(_) | Expr::Member(..) | Expr::PrivateMember(..) => true,
        Expr::Group(e) => assignable(e),
        _ if call_assignment_target(e) => true,
        _ => false,
    }
}
fn private_reference(mut expression: &Expr) -> bool {
    while let Expr::Group(inner) = expression {
        expression = inner;
    }
    match expression {
        Expr::PrivateMember(..) => true,
        Expr::Chain(_, links) => matches!(links.last(), Some(ChainLink::PrivateMember(..))),
        _ => false,
    }
}
fn number_key(n: f64) -> String {
    crate::vm::number_string(n)
}
fn reserved(s: &str) -> bool {
    matches!(
        s,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "enum"
    )
}

fn push_function_ast<'a>(function: &'a Function, depth: usize, pending: &mut Vec<(Ast<'a>, usize)>) {
    pending.extend(function.body.iter().map(|s| (Ast::Statement(s), depth)));
    pending.extend(function.params.iter().map(|p| (Ast::Pattern(&p.pattern), depth)));
}

fn push_class_ast<'a>(class: &'a Class, depth: usize, pending: &mut Vec<(Ast<'a>, usize)>) {
    pending.extend(class.heritage.iter().map(|e| (Ast::Expression(e), depth)));
    for element in &class.elements {
        match element {
            ClassElement::Method { key, function, .. } => {
                if let ClassKey::Public(key) = key {
                    pending.push((Ast::Expression(key), depth));
                }
                push_function_ast(function, depth, pending);
            }
            ClassElement::Field { key, value, .. } => {
                if let ClassKey::Public(key) = key {
                    pending.push((Ast::Expression(key), depth));
                }
                pending.extend(value.iter().map(|v| (Ast::Expression(v), depth)));
            }
            ClassElement::StaticBlock(body) => {
                pending.extend(body.iter().map(|s| (Ast::Statement(s), depth)))
            }
        }
    }
}
