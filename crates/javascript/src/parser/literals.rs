use super::{
    Expr, FunctionKind, JsError, JsString, KeywordContext, MAX_AST_DEPTH,
    MethodKind, ObjectEntry, Parser, TokenKind, Value, reserved,
};

enum Items {
    Array(Vec<Option<Expr>>),
    Object(Vec<ObjectEntry>),
}

struct Literal {
    items: Items,
    trailing: bool,
    closed: bool,
}

impl Literal {
    fn open(parser: &mut Parser) -> Result<Self, JsError> {
        let items = if parser.eat("[") {
            Items::Array(Vec::new())
        } else {
            parser.expect("{")?;
            Items::Object(Vec::new())
        };
        Ok(Self {
            items,
            trailing: false,
            closed: false,
        })
    }

    fn end(&self) -> &'static str {
        match self.items {
            Items::Array(_) => "]",
            Items::Object(_) => "}",
        }
    }

    fn finish(self) -> Expr {
        match self.items {
            Items::Array(items) => Expr::Array(items, self.trailing),
            Items::Object(items) => Expr::Object(items, self.trailing),
        }
    }
}

struct Property {
    key: Expr,
    prototype: bool,
    computed: bool,
}

impl Property {
    fn with_value(self, value: Expr, cover: bool, method: Option<MethodKind>) -> ObjectEntry {
        let infer_name = matches!(&value, Expr::Function(function) if function.name.is_none());
        ObjectEntry::Property {
            key: self.key,
            value,
            prototype: self.prototype,
            infer_name,
            cover,
            computed: self.computed,
            method,
        }
    }
}

enum Slot {
    Item { spread: bool },
    SpreadProperty,
    Property(Property),
}

enum Entry {
    Ready(ObjectEntry),
    Value(Slot),
}

impl Parser {
    pub(super) fn container_literal(&mut self) -> Result<Expr, JsError> {
        let mut literal = Literal::open(self)?;
        let mut parents = Vec::new();
        loop {
            if literal.closed || self.eat(literal.end()) {
                let primary = self.checked_expression(literal.finish())?;
                let Some((parent, slot)) = parents.pop() else {
                    return Ok(primary);
                };
                literal = parent;
                let value = self.initializer_tail(primary)?;
                self.push_literal_value(&mut literal, slot, value)?;
            } else {
                let slot = match &mut literal.items {
                    Items::Array(items) => {
                        if self.eat(",") {
                            items.push(None);
                            continue;
                        }
                        Slot::Item {
                            spread: self.eat("..."),
                        }
                    }
                    Items::Object(items) => match self.object_entry()? {
                        Entry::Ready(entry) => {
                            items.push(entry);
                            self.literal_separator(&mut literal)?;
                            continue;
                        }
                        Entry::Value(slot) => slot,
                    },
                };
                if self.at("[") || self.at("{") {
                    // Each saved container contributes at least one AST node.
                    // No native recursion or token lookahead is needed on this edge.
                    if parents.len() + 1 >= MAX_AST_DEPTH {
                        return Err(self.error("AST nesting limit exceeded"));
                    }
                    parents.push((literal, slot));
                    literal = Literal::open(self)?;
                    continue;
                }
                let value = self.initializer()?;
                self.push_literal_value(&mut literal, slot, value)?;
            }
            self.literal_separator(&mut literal)?;
        }
    }

    fn literal_separator(&mut self, literal: &mut Literal) -> Result<(), JsError> {
        if self.eat(literal.end()) {
            literal.closed = true;
        } else {
            self.expect(",")?;
            literal.trailing = self.at(literal.end());
        }
        Ok(())
    }

    fn push_literal_value(
        &self,
        literal: &mut Literal,
        slot: Slot,
        value: Expr,
    ) -> Result<(), JsError> {
        match (&mut literal.items, slot) {
            (Items::Array(items), Slot::Item { spread }) => {
                let value = if spread {
                    self.checked_expression(Expr::Spread(Box::new(value)))?
                } else {
                    value
                };
                items.push(Some(value));
            }
            (Items::Object(items), Slot::SpreadProperty) => items.push(ObjectEntry::Spread(value)),
            (Items::Object(items), Slot::Property(property)) => {
                items.push(property.with_value(value, false, None));
            }
            _ => unreachable!("literal slot belongs to its container"),
        }
        Ok(())
    }

    fn object_entry(&mut self) -> Result<Entry, JsError> {
        if self.eat("...") {
            return Ok(Entry::Value(Slot::SpreadProperty));
        }
        let method_start = self.current().start;
        let is_async = self.async_method_ahead();
        if is_async {
            self.advance(1);
        }
        let function_kind = self.function_kind(is_async);
        let accessor = if function_kind == FunctionKind::Ordinary {
            self.accessor_ahead(false)
        } else {
            None
        };
        if accessor.is_some() {
            self.advance(1);
        }
        let computed = self.eat("[");
        let shorthand = !computed
            && matches!(&self.current().kind,
                TokenKind::Word(word) | TokenKind::EscapedWord(word) if !reserved(word));
        let key = if computed {
            let key = self.expression_in(2)?;
            self.expect("]")?;
            key
        } else {
            Expr::Literal(Value::String(self.property_name()?))
        };
        let literal_key = if let Expr::Literal(Value::String(key)) = &key {
            Some(key.clone())
        } else {
            None
        };
        let mut property = Property {
            key,
            prototype: false,
            computed,
        };
        let method = accessor.or_else(|| self.at("(").then_some(MethodKind::Method));
        if function_kind != FunctionKind::Ordinary && !self.at("(") {
            return Err(self.error("Method requires a body"));
        }
        if let Some(method) = method {
            let function = self.method_function(
                method_start,
                if computed { None } else { literal_key },
                false,
                function_kind,
                method,
            )?;
            let value = self.checked_expression(Expr::Function(Box::new(function)))?;
            return Ok(Entry::Ready(property.with_value(
                value,
                false,
                Some(method),
            )));
        }
        if self.eat(":") {
            property.prototype = !computed
                && literal_key
                    .as_ref()
                    .is_some_and(|key| key == &JsString::from("__proto__"));
            return Ok(Entry::Value(Slot::Property(property)));
        }
        let Some(key) = literal_key else {
            return Err(self.error("Computed property requires a value or method"));
        };
        if !shorthand {
            return Err(self.error("Invalid shorthand property"));
        }
        if self.class_initializer && key == JsString::from("arguments") {
            return Err(self.error("arguments in a class initializer"));
        }
        if key == JsString::from("yield") && self.yield_context != KeywordContext::None {
            return Err(self.error("yield is not a shorthand identifier here"));
        }
        if key == JsString::from("await") {
            if self.static_block || self.module_goal || self.await_context != KeywordContext::None {
                return Err(self.error("await is not a shorthand identifier here"));
            }
            self.await_count += 1;
        }
        let value = Expr::Identifier(key);
        let cover = self.eat("=");
        let value = if cover {
            let default = self.expression(2)?;
            Expr::Assign("=", Box::new(value), Box::new(default))
        } else {
            value
        };
        Ok(Entry::Ready(property.with_value(value, cover, None)))
    }
}
