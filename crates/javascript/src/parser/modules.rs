//! The Module goal produces import/export records, not a function wrapper.
use super::*;
use crate::modules::ModuleRequest;
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub(crate) struct Module {
    pub body: Vec<Stmt>,
    pub requests: Vec<ModuleRequest>,
    pub imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    requested: BTreeMap<ModuleRequest, usize>,
}
#[derive(Clone, Debug)]
pub(crate) struct ImportEntry {
    pub request: usize,
    pub imported: Option<JsString>,
    pub local: JsString,
}
#[derive(Clone, Debug)]
pub(crate) enum ExportEntry {
    Local {
        exported: JsString,
        local: JsString,
    },
    Indirect {
        exported: JsString,
        request: usize,
        imported: JsString,
    },
    Namespace {
        exported: JsString,
        request: usize,
    },
    Star {
        request: usize,
    },
}
impl ExportEntry {
    pub fn exported(&self) -> Option<&JsString> {
        match self {
            Self::Local { exported, .. }
            | Self::Indirect { exported, .. }
            | Self::Namespace { exported, .. } => Some(exported),
            Self::Star { .. } => None,
        }
    }
}
pub(crate) fn parse(source: &str, compilation_bytes: usize) -> Result<Module, JsError> {
    let mut p = Parser {
        tokens: VecDeque::with_capacity(TOKEN_WINDOW),
        lexer: Lexer::new(source, compilation_bytes, false),
        function_depth: 0,
        legacy_literals: 0,
        last_end: 0,
        loop_depth: 0,
        switch_depth: 0,
        labels: Vec::new(),
        next_label: 1,
        depth: 0,
        allow_in: true,
        super_property: false,
        super_call: false,
        new_target: false,
        class_initializer: false,
        eval_private_names: BTreeSet::new(),
        private_scopes: Vec::new(),
        module_goal: true,
        static_block: false,
        await_context: KeywordContext::Body,
        await_count: 0,
        yield_context: KeywordContext::None,
        yield_count: 0,
    };
    p.fill();
    let mut module = Module {
        body: Vec::new(),
        requests: Vec::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        requested: BTreeMap::new(),
    };
    while !p.is_eof() {
        if p.at("import")
            && !p
                .lookahead(1)
                .is_some_and(|t| matches!(t.kind, TokenKind::Punct("." | "(")))
        {
            p.advance(1);
            p.import_declaration(&mut module)?;
        } else if p.eat("export") {
            p.export_declaration(&mut module)?;
        } else {
            module.body.push(p.statement()?);
        }
    }
    if p.legacy_literals != 0 {
        return Err(p.error("Legacy literal in module code"));
    }
    let mut exports = std::collections::BTreeSet::new();
    for export in &module.exports {
        if let Some(name) = export.exported()
            && !exports.insert(name.clone())
        {
            return Err(p.error("Duplicate module export"));
        }
    }
    let mut imports = std::collections::BTreeSet::new();
    for import in &module.imports {
        if !imports.insert(import.local.clone()) {
            return Err(p.error("Duplicate import binding"));
        }
    }
    Ok(module)
}
impl Parser {
    fn module_name(&mut self) -> Result<JsString, JsError> {
        match self.current().kind.clone() {
            TokenKind::Word(name) | TokenKind::EscapedWord(name) => {
                self.advance(1);
                Ok(name.into())
            }
            TokenKind::String(name) => {
                if char::decode_utf16(name.as_utf16().iter().copied()).any(|unit| unit.is_err()) {
                    return Err(self.error("Module export name must be well-formed Unicode"));
                }
                self.advance(1);
                Ok(name)
            }
            _ => Err(self.error("Expected a module import/export name")),
        }
    }
    fn module_specifier(&mut self, module: &mut Module) -> Result<usize, JsError> {
        let TokenKind::String(specifier) = self.current().kind.clone() else {
            return Err(self.error("Module specifier must be a string literal"));
        };
        self.advance(1);
        let mut attributes = BTreeMap::new();
        if self.eat("with") {
            self.expect("{")?;
            while !self.eat("}") {
                let key = match self.current().kind.clone() {
                    TokenKind::String(key) => key,
                    TokenKind::Word(key) | TokenKind::EscapedWord(key) => key.into(),
                    _ => return Err(self.error("Invalid import attribute key")),
                };
                self.advance(1);
                self.expect(":")?;
                let TokenKind::String(value) = self.current().kind.clone() else {
                    return Err(self.error("Import attribute value must be a string literal"));
                };
                self.advance(1);
                if attributes.insert(key, value).is_some() {
                    return Err(self.error("Duplicate import attribute"));
                }
                if self.eat("}") {
                    break;
                }
                self.expect(",")?;
            }
        }
        let request = ModuleRequest {
            specifier,
            attributes,
        };
        if let Some(index) = module.requested.get(&request) {
            return Ok(*index);
        }
        let index = module.requests.len();
        module.requested.insert(request.clone(), index);
        module.requests.push(request);
        Ok(index)
    }
    fn import_declaration(&mut self, module: &mut Module) -> Result<(), JsError> {
        if (self.at("source") || self.at("defer"))
            && self.lookahead(1).is_some_and(|token| {
                matches!(&token.kind,TokenKind::Word(word) if word!="from")
                    || token.kind == TokenKind::Punct("*")
            })
        {
            return Err(self.error("Source-phase and deferred imports are not implemented"));
        }
        if matches!(self.current().kind, TokenKind::String(_)) {
            self.module_specifier(module)?;
            return self.semi();
        }
        let mut entries = Vec::<(Option<JsString>, JsString)>::new();
        if !self.at("*") && !self.at("{") {
            entries.push((Some("default".into()), self.name()?));
            if !self.eat(",") {
                self.expect("from")?;
                let request = self.module_specifier(module)?;
                for (imported, local) in entries {
                    module.imports.push(ImportEntry {
                        request,
                        imported,
                        local,
                    });
                }
                return self.semi();
            }
        }
        if self.eat("*") {
            self.expect("as")?;
            entries.push((None, self.name()?));
        } else {
            self.expect("{")?;
            while !self.eat("}") {
                let string_name = matches!(self.current().kind, TokenKind::String(_));
                let imported = self.module_name()?;
                let local = if self.eat("as") {
                    self.name()?
                } else {
                    if string_name || reserved(&imported.to_string_lossy()) {
                        return Err(self.error("Import name requires a local alias"));
                    }
                    imported.clone()
                };
                entries.push((Some(imported), local));
                if self.eat("}") {
                    break;
                }
                self.expect(",")?;
            }
        }
        self.expect("from")?;
        let request = self.module_specifier(module)?;
        for (imported, local) in entries {
            module.imports.push(ImportEntry {
                request,
                imported,
                local,
            });
        }
        self.semi()
    }
    fn export_declaration(&mut self, module: &mut Module) -> Result<(), JsError> {
        if self.eat("*") {
            let exported = if self.eat("as") {
                Some(self.module_name()?)
            } else {
                None
            };
            self.expect("from")?;
            let request = self.module_specifier(module)?;
            module.exports.push(if let Some(exported) = exported {
                ExportEntry::Namespace { exported, request }
            } else {
                ExportEntry::Star { request }
            });
            return self.semi();
        }
        if self.eat("{") {
            let mut names = Vec::new();
            while !self.eat("}") {
                let local_is_string = matches!(self.current().kind, TokenKind::String(_));
                let local = self.module_name()?;
                let exported = if self.eat("as") {
                    self.module_name()?
                } else {
                    local.clone()
                };
                names.push((local, exported, local_is_string));
                if self.eat("}") {
                    break;
                }
                self.expect(",")?;
            }
            let request = if self.eat("from") {
                Some(self.module_specifier(module)?)
            } else {
                None
            };
            for (local, exported, local_is_string) in names {
                if request.is_none() && (local_is_string || reserved(&local.to_string_lossy())) {
                    return Err(self.error("Local export requires an identifier reference"));
                }
                module.exports.push(if let Some(request) = &request {
                    ExportEntry::Indirect {
                        exported,
                        request: *request,
                        imported: local,
                    }
                } else {
                    ExportEntry::Local { exported, local }
                });
            }
            return self.semi();
        }
        if self.eat("default") {
            let start = self.current().start;
            let is_async = self.async_function_ahead();
            if is_async {
                self.advance(1);
            }
            let local = if self.eat("function") {
                let mut function = self.function(start, is_async, false)?;
                function.self_binding = false;
                if let Some(name) = &function.name {
                    let name = name.clone();
                    module.body.push(Stmt::Function(function));
                    name
                } else {
                    function.name = Some("default".into());
                    module.body.push(Stmt::DefaultFunction(function));
                    "*default*".into()
                }
            } else if self.eat("class") {
                let class = self.class(start, false)?;
                if let Some(name) = &class.name {
                    let name = name.clone();
                    module.body.push(Stmt::Class(name.clone(), class));
                    name
                } else {
                    module.body.push(Stmt::DefaultExport(Expr::Class(Box::new(class))));
                    "*default*".into()
                }
            } else {
                let expr = self.expression(2)?;
                self.semi()?;
                module.body.push(Stmt::DefaultExport(expr));
                "*default*".into()
            };
            module.exports.push(ExportEntry::Local {
                exported: "default".into(),
                local,
            });
            return Ok(());
        }
        let stmt = self.statement()?;
        let names = match &stmt {
            Stmt::Variable(_, decls) => decls
                .iter()
                .flat_map(|(p, _)| bound_names(p))
                .collect::<Vec<_>>(),
            Stmt::Function(f) => vec![f.name.clone().unwrap()],
            Stmt::Class(name, _) => vec![name.clone()],
            _ => return Err(self.error("Invalid exported declaration")),
        };
        module.body.push(stmt);
        module
            .exports
            .extend(names.into_iter().map(|name| ExportEntry::Local {
                exported: name.clone(),
                local: name,
            }));
        Ok(())
    }
}
