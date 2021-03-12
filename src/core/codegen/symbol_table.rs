use crate::core::codegen::{CodegenError, CodegenResult, DetailedCodegenError, ProgramCounter};
use crate::core::parser::{Block, Identifier, IdentifierPath};
use codemap::Span;
use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub enum Symbol {
    Label(ProgramCounter),
    Variable(i64),
    Constant(i64),
    System(i64),
    Macro(Block, Vec<Identifier>),
}

pub struct SymbolTable {
    symbols: HashMap<IdentifierPath, Symbol>,
    current: IdentifierPath,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
            current: IdentifierPath::empty(),
        }
    }

    pub fn symbols(&self) -> &HashMap<IdentifierPath, Symbol> {
        &self.symbols
    }

    pub fn register<'a, ID: Into<IdentifierPath>, S: Into<Option<&'a Span>>>(
        &mut self,
        path: ID,
        value: Symbol,
        span: S,
        allow_same_type_redefinition: bool,
    ) -> CodegenResult<()> {
        let path = path.into();
        let span = span.into();

        let path = self.current.join(path);
        if !allow_same_type_redefinition {
            let span = span.unwrap();
            if let Some(existing) = self.lookup(&path, false) {
                let is_same_type =
                    std::mem::discriminant(existing) == std::mem::discriminant(&value);

                if !is_same_type || existing != &value {
                    return Err(CodegenError::new(
                        *span,
                        DetailedCodegenError::SymbolRedefinition(path),
                    ));
                }
            }
        }

        self.symbols.insert(path, value);
        Ok(())
    }

    // Look up a symbol in the symbol table.
    // If `bubble_up` is set, the lookup will bubble up to parent scopes if the symbol is not found in the current scope.
    pub fn lookup<ID: Into<IdentifierPath>>(&self, path: ID, bubble_up: bool) -> Option<&Symbol> {
        let path = path.into();
        if path.is_empty() {
            return None;
        }

        let mut scope = self.current.clone();
        loop {
            let full_path = scope.join(&path).canonicalize();
            if let Some(symbol) = self.symbols.get(&full_path) {
                return Some(symbol);
            }

            if scope.is_empty() || !bubble_up {
                // Didn't find anything. We are at the root or not allowed to bubble up, so bail.
                return None;
            }

            scope.pop();
        }
    }

    pub fn value(&self, path: &IdentifierPath) -> CodegenResult<Option<i64>> {
        Ok(self
            .lookup(path, true)
            .map(|s| match s {
                Symbol::Label(pc) => Some(pc.as_i64()),
                Symbol::Variable(val) | Symbol::Constant(val) | Symbol::System(val) => Some(*val),
                Symbol::Macro(_, _) => None,
            })
            .flatten())
    }

    pub fn enter<ID: Into<Identifier>>(&mut self, id: ID) {
        let id = id.into();
        log::trace!("Entering scope: '{}'", id);
        self.current.push(&id);
    }

    pub fn leave(&mut self) {
        log::trace!("Leaving scope");
        assert!(!self.current.is_empty(), "Can't pop root scope");
        self.current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::{Symbol, SymbolTable};
    use crate::core::codegen::CodegenResult;
    use crate::testing::empty_span;

    type TestResult = CodegenResult<()>;

    #[test]
    fn can_lookup_symbols() -> TestResult {
        let mut st = SymbolTable::new();
        reg(&mut st, "A", 1)?;
        reg(&mut st, "B", 2)?;
        st.enter("foo");
        reg(&mut st, "A", 100)?;

        assert_eq!(st.lookup("A", true), Some(&Symbol::Constant(100)));

        st.enter("bar");
        reg(&mut st, "A", 555)?;
        assert_eq!(st.lookup("A", true), Some(&Symbol::Constant(555)));
        assert_eq!(st.lookup("B", true), Some(&Symbol::Constant(2)));
        assert_eq!(st.lookup("B", false), None);
        st.leave();

        assert_eq!(st.lookup("B", true), Some(&Symbol::Constant(2)));
        assert_eq!(st.lookup("super.A", true), Some(&Symbol::Constant(1)));
        assert_eq!(st.lookup("super.foo.A", true), Some(&Symbol::Constant(100)));

        st.leave();

        assert_eq!(st.lookup("foo.bar.A", true), Some(&Symbol::Constant(555)));

        assert_eq!(st.lookup("A", true), Some(&Symbol::Constant(1)));
        assert_eq!(st.lookup("foo.A", true), Some(&Symbol::Constant(100)));
        assert_eq!(st.lookup("super.A", true), Some(&Symbol::Constant(1)));

        assert_eq!(st.lookup("foo2.A", true), None);
        st.enter("foo2");
        reg(&mut st, "A", 200)?;
        assert_eq!(st.lookup("A", true), Some(&Symbol::Constant(200)));
        st.leave();
        assert_eq!(st.lookup("foo2.A", true), Some(&Symbol::Constant(200)));

        Ok(())
    }

    #[test]
    fn can_register_nested_symbols() -> TestResult {
        let mut st = SymbolTable::new();
        st.register("a.a", Symbol::Constant(1), &empty_span(), false)?;
        st.register("a.b", Symbol::Constant(2), &empty_span(), false)?;
        assert_eq!(st.lookup("a.a", true), Some(&Symbol::Constant(1)));
        assert_eq!(st.lookup("a.b", true), Some(&Symbol::Constant(2)));
        Ok(())
    }

    fn reg<'a>(st: &'a mut SymbolTable, id: &'a str, val: i64) -> TestResult {
        st.register(id, Symbol::Constant(val), &empty_span(), false)
    }
}
