use super::*;
impl Parser {
    pub(super) fn private_use(&mut self) -> Result<JsString, JsError> {
        let token = self.current();
        let TokenKind::PrivateIdentifier(name) = &token.kind else {
            return Err(self.error("Expected private identifier"));
        };
        let name = name.clone();
        let location = (token.line, token.column);
        self.record_private_use(name.clone(), location)?;
        self.advance(1);
        Ok(name)
    }

    fn record_private_use(
        &mut self,
        name: JsString,
        (line, column): (usize, usize),
    ) -> Result<(), JsError> {
        if let Some(scope) = self.private_scopes.last_mut() {
            scope.uses.entry(name).or_insert((line, column));
        } else if !self.eval_private_names.contains(&name) {
            return Err(JsError::syntax(
                format!("Undeclared private name {name}"),
                line,
                column,
            ));
        }
        Ok(())
    }

    fn declare_private(
        &mut self,
        name: JsString,
        static_element: bool,
        kind: MethodKind,
    ) -> Result<(), JsError> {
        use std::collections::btree_map::Entry;
        // Fields and methods reserve both accessor halves.
        let declaration = PrivateDeclaration {
            static_element,
            getter: kind != MethodKind::Setter,
            setter: kind != MethodKind::Getter,
        };
        let scope = self.private_scopes.last_mut().unwrap();
        match scope.declarations.entry(name) {
            Entry::Vacant(entry) => {
                scope.names.push(entry.key().clone());
                entry.insert(declaration);
            }
            Entry::Occupied(mut entry) => {
                let prior = entry.get_mut();
                if prior.static_element != static_element
                    || (prior.getter && declaration.getter)
                    || (prior.setter && declaration.setter)
                {
                    return Err(self.error("Duplicate private name"));
                }
                prior.getter |= declaration.getter;
                prior.setter |= declaration.setter;
            }
        }
        Ok(())
    }

    pub(super) fn method_function(
        &mut self,
        name: Option<JsString>,
        derived_constructor: bool,
        kind: FunctionKind,
        method: MethodKind,
    ) -> Result<Function, JsError> {
        let old = (
            self.super_property,
            self.super_call,
            self.new_target,
            self.class_initializer,
            self.static_block,
            self.await_context,
            self.await_count,
            self.yield_context,
            self.yield_count,
        );
        self.super_property = true;
        self.super_call = derived_constructor;
        self.new_target = true;
        self.class_initializer = false;
        self.static_block = false;
        self.await_context = kind.await_context(KeywordContext::Parameters);
        self.yield_context = kind.yield_context(KeywordContext::Parameters);
        let result = (|| {
            self.expect("(")?;
            let params = match method {
                MethodKind::Method => self.parameters()?,
                MethodKind::Getter => {
                    self.expect(")")?;
                    Vec::new()
                }
                MethodKind::Setter => {
                    if self.at("...") {
                        return Err(self.error("Setter requires a single non-rest parameter"));
                    }
                    let param = self.parameter(false)?;
                    self.expect(")")?;
                    vec![param]
                }
            };
            let mut f = self.function_body(name, params, false, kind)?;
            f.method = true;
            Ok(f)
        })();
        (
            self.super_property,
            self.super_call,
            self.new_target,
            self.class_initializer,
            self.static_block,
            self.await_context,
            self.await_count,
            self.yield_context,
            self.yield_count,
        ) = old;
        result
    }
    pub(super) fn class(&mut self, declaration: bool) -> Result<Class, JsError> {
        if self.depth >= MAX_PARSER_DEPTH {
            return Err(self.error("Class nesting limit exceeded"));
        }
        self.depth += 1;
        let result = self.class_inner(declaration);
        self.depth -= 1;
        let class = result?;
        self.check_ast(Ast::Class(&class))?;
        Ok(class)
    }
    fn class_inner(&mut self, declaration: bool) -> Result<Class, JsError> {
        let name = if !self.at("{") && !self.at("extends") {
            Some(self.name()?)
        } else if declaration {
            return Err(self.error("Class declaration requires a name"));
        } else {
            None
        };
        let heritage = if self.eat("extends") {
            let value = self.expression(15)?;
            if matches!(value, Expr::Unary(..) | Expr::Update(..)) {
                return Err(self.error("Invalid class heritage expression"));
            }
            Some(Box::new(value))
        } else {
            None
        };
        self.expect("{")?;
        self.private_scopes.push(PrivateScope::default());
        let mut elements = Vec::new();
        let mut constructor = false;
        while !self.eat("}") {
            if self.is_eof() {
                return Err(self.error("Unterminated class body"));
            }
            if self.eat(";") {
                continue;
            }
            let mut is_static = false;
            if self.at("static")
                && self
                    .lookahead(1)
                    .is_some_and(|t| !matches!(t.kind, TokenKind::Punct("(" | "=" | ";" | "}")))
            {
                self.advance(1);
                is_static = true;
                if self.eat("{") {
                    let old_labels = std::mem::take(&mut self.labels);
                    let old = (
                        self.super_property,
                        self.super_call,
                        self.new_target,
                        self.class_initializer,
                        self.function_depth,
                        self.loop_depth,
                        self.switch_depth,
                        self.static_block,
                        self.yield_context,
                    );
                    self.super_property = true;
                    self.super_call = false;
                    self.new_target = true;
                    self.class_initializer = true;
                    self.function_depth = 0;
                    self.loop_depth = 0;
                    self.switch_depth = 0;
                    self.static_block = true;
                    self.yield_context = KeywordContext::Parameters;
                    let body = self.block_contents();
                    (
                        self.super_property,
                        self.super_call,
                        self.new_target,
                        self.class_initializer,
                        self.function_depth,
                        self.loop_depth,
                        self.switch_depth,
                        self.static_block,
                        self.yield_context,
                    ) = old;
                    self.labels = old_labels;
                    elements.push(ClassElement::StaticBlock(body?));
                    continue;
                }
            }
            let is_async = self.async_method_ahead();
            if is_async {
                self.advance(1);
            }
            let function_kind = self.function_kind(is_async);
            let mut kind = MethodKind::Method;
            if function_kind == FunctionKind::Ordinary
                && let Some(accessor) = self.accessor_ahead(true)
            {
                self.advance(1);
                kind = accessor;
            }
            let computed = self.eat("[");
            let key = if computed {
                let key = self.expression_in(2)?;
                self.expect("]")?;
                ClassKey::Public(key)
            } else if let TokenKind::PrivateIdentifier(name) = &self.current().kind {
                let name = name.clone();
                if name == JsString::from("#constructor") {
                    return Err(self.error("A private element cannot be named #constructor"));
                }
                self.advance(1);
                self.declare_private(name.clone(), is_static, kind)?;
                ClassKey::Private(name)
            } else {
                ClassKey::Public(Expr::Literal(Value::String(self.property_name()?)))
            };
            let literal = if let ClassKey::Public(Expr::Literal(Value::String(name))) = &key {
                Some(name.clone())
            } else {
                None
            };
            let is_constructor =
                !is_static && !computed && literal.as_ref() == Some(&JsString::from("constructor"));
            if self.at("(") {
                if is_constructor
                    && (function_kind != FunctionKind::Ordinary
                        || kind != MethodKind::Method
                        || constructor)
                {
                    return Err(self.error("Invalid or duplicate class constructor"));
                }
                if is_static && !computed && literal.as_ref() == Some(&JsString::from("prototype"))
                {
                    return Err(self.error("Static method cannot be named prototype"));
                }
                let function = self.method_function(
                    None,
                    is_constructor && heritage.is_some(),
                    function_kind,
                    kind,
                )?;
                if is_constructor {
                    constructor = true;
                }
                elements.push(ClassElement::Method {
                    key,
                    function,
                    kind,
                    is_static,
                    constructor: is_constructor,
                });
            } else {
                if function_kind != FunctionKind::Ordinary || kind != MethodKind::Method {
                    return Err(self.error("Method requires a body"));
                }
                if !computed
                    && (literal.as_ref() == Some(&JsString::from("constructor"))
                        || (is_static && literal.as_ref() == Some(&JsString::from("prototype"))))
                {
                    return Err(self.error("Invalid class field name"));
                }
                let old = (
                    self.super_property,
                    self.super_call,
                    self.new_target,
                    self.class_initializer,
                    self.await_context,
                    self.await_count,
                    self.yield_context,
                );
                if self.await_context == KeywordContext::Body {
                    self.await_context = KeywordContext::Parameters;
                }
                self.yield_context = KeywordContext::Parameters;
                self.super_property = true;
                self.super_call = false;
                self.new_target = true;
                self.class_initializer = true;
                let value = if self.eat("=") {
                    self.expression_in(2).map(Some)
                } else {
                    Ok(None)
                };
                (
                    self.super_property,
                    self.super_call,
                    self.new_target,
                    self.class_initializer,
                    self.await_context,
                    self.await_count,
                    self.yield_context,
                ) = old;
                let value = value?;
                if !self.eat(";") && !self.at("}") && !self.current().newline {
                    return Err(self.error("Expected class field terminator"));
                }
                elements.push(ClassElement::Field {
                    key,
                    value,
                    is_static,
                });
            }
        }
        let scope = self.private_scopes.pop().unwrap();
        for (name, location) in scope.uses {
            if !scope.declarations.contains_key(&name) {
                self.record_private_use(name, location)?;
            }
        }
        Ok(Class {
            name,
            heritage,
            elements,
            private_names: scope.names,
        })
    }
}
