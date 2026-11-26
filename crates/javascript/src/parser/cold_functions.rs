use super::{Ast, Function, FunctionKind, Parser, simple_parameter};
use crate::JsError;

impl Parser {
    /// Large bundles often register many ordinary functions in their outer
    /// wrapper. Validate each eligible function now, but leave only callable
    /// metadata and a shared source span until somebody actually invokes it.
    /// Defer one level only: re-parsing a callable never walks a growing chain
    /// of lazily emitted ancestors.
    pub(super) fn compact_cold_function(
        &mut self,
        function: &mut Function,
        before: usize,
    ) -> Result<(), JsError> {
        if self.defer_large_functions
            && self.function_depth == 1
            && function.kind == FunctionKind::Ordinary
            && self.private_scopes.is_empty()
            && self.eval_private_names.is_empty()
            && function.params.iter().all(simple_parameter)
        {
            self.check_ast(Ast::Function(function))?;
            let (code, retained) = crate::bytecode::validate_deferred_function(
                function,
                self.known_strict_context,
            )?;
            function.body = Vec::new();
            function.params = Vec::new();
            function.validated = Some(code);
            self.lexer.finish_deferred_function(before, retained)?;
        }
        Ok(())
    }
}
