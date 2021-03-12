use crate::core::parser::{ArgItem, Block, Expression, ExpressionFactor, Token, VariableType};
use crate::errors::MosResult;
use crate::impl_request_handler;
use crate::lsp::{LspContext, RequestHandler};
use codemap::{CodeMap, Span};
use itertools::Itertools;
use lsp_types::request::SemanticTokensFullRequest;
use lsp_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult,
};
use once_cell::sync::OnceCell;
use std::collections::HashMap;
use strum::IntoEnumIterator;

#[derive(Clone, Copy, Debug, strum::EnumIter, Hash, PartialEq, Eq)]
enum TokenType {
    Identifier,
    Mnemonic,
    Number,
    Variable,
    Constant,
    Keyword,
}

static TOKEN_TYPE_LOOKUP: OnceCell<HashMap<TokenType, u32>> = OnceCell::new();
static TOKEN_MODIFIER_LOOKUP: OnceCell<HashMap<TokenType, u32>> = OnceCell::new();

impl TokenType {
    fn semantic_token_type(&self) -> SemanticTokenType {
        match &self {
            TokenType::Identifier => SemanticTokenType::METHOD,
            TokenType::Mnemonic => SemanticTokenType::PROPERTY,
            TokenType::Number => SemanticTokenType::NUMBER,
            TokenType::Variable => SemanticTokenType::VARIABLE,
            TokenType::Constant => SemanticTokenType::VARIABLE,
            TokenType::Keyword => SemanticTokenType::KEYWORD,
        }
    }

    fn semantic_token_modifiers(&self) -> Vec<SemanticTokenModifier> {
        match &self {
            TokenType::Constant => vec![SemanticTokenModifier::READONLY],
            _ => vec![],
        }
    }

    fn available_types() -> Vec<SemanticTokenType> {
        Self::iter()
            .map(|ty| ty.semantic_token_type())
            .unique()
            .collect_vec()
    }

    fn available_modifiers() -> Vec<SemanticTokenModifier> {
        Self::iter()
            .map(|ty| ty.semantic_token_modifiers())
            .flatten()
            .unique()
            .collect_vec()
    }

    fn type_to_u32(&self) -> u32 {
        let ty = self.semantic_token_type();
        let (val, _) = Self::available_types()
            .into_iter()
            .find_position(|available_ty| available_ty == &ty)
            .unwrap();
        val as u32
    }

    fn modifiers_to_u32(&self) -> u32 {
        let mut result = 0;
        for modifier in self.semantic_token_modifiers() {
            let (val, _) = Self::available_modifiers()
                .into_iter()
                .find_position(|available_mod| available_mod == &modifier)
                .unwrap();
            result += val as u32;
        }
        result
    }
}

pub fn caps() -> SemanticTokensOptions {
    let lookup = TokenType::iter().map(|ty| (ty, ty.type_to_u32())).collect();
    TOKEN_TYPE_LOOKUP.set(lookup).unwrap();
    let lookup = TokenType::iter()
        .map(|ty| (ty, ty.modifiers_to_u32()))
        .collect();
    TOKEN_MODIFIER_LOOKUP.set(lookup).unwrap();

    let legend = SemanticTokensLegend {
        token_types: TokenType::available_types(),
        token_modifiers: TokenType::available_modifiers(),
    };

    SemanticTokensOptions {
        legend,
        full: Some(SemanticTokensFullOptions::Bool(true)),
        ..Default::default()
    }
}

pub struct SemanticTokensFullRequestHandler {}

impl_request_handler!(SemanticTokensFullRequestHandler);

impl RequestHandler<SemanticTokensFullRequest> for SemanticTokensFullRequestHandler {
    fn handle(
        &self,
        ctx: &mut LspContext,
        _params: SemanticTokensParams,
    ) -> MosResult<Option<SemanticTokensResult>> {
        if let Some(analysis) = &ctx.analysis {
            let semtoks = emit_semantic_ast(analysis.tree.tokens());
            let data = to_deltas(analysis.tree.code_map(), semtoks);
            let tokens = SemanticTokens {
                result_id: None,
                data,
            };
            Ok(Some(SemanticTokensResult::Tokens(tokens)))
        } else {
            Ok(None)
        }
    }
}

fn to_deltas(code_map: &CodeMap, semtoks: Vec<SemTok>) -> Vec<SemanticToken> {
    let mut prev_line = 0;
    let mut prev_start = 0;

    let semtoks = semtoks
        .into_iter()
        .map(|st| (code_map.look_up_span(st.span), st.token_type))
        .collect_vec();

    let mut result = vec![];
    for (location, ty) in semtoks
        .iter()
        .sorted_by_key(|(location, _)| (location.begin.line, location.begin.column))
    {
        let cur_line = location.begin.line;
        let cur_start = location.begin.column;
        let delta_line = cur_line - prev_line;
        let delta_start = if cur_line == prev_line {
            cur_start - prev_start
        } else {
            cur_start
        };
        prev_line = cur_line;
        prev_start = cur_start;
        result.push(SemanticToken {
            delta_line: delta_line as u32,
            delta_start: delta_start as u32,
            length: (location.end.column - location.begin.column) as u32,
            token_type: *TOKEN_TYPE_LOOKUP.get().unwrap().get(&ty).unwrap(),
            token_modifiers_bitset: *TOKEN_MODIFIER_LOOKUP.get().unwrap().get(&ty).unwrap(),
        });
    }

    result
}

struct SemTok {
    span: Span,
    token_type: TokenType,
}

impl SemTok {
    fn new(span: Span, token_type: TokenType) -> Self {
        Self { span, token_type }
    }
}

struct SemTokBuilder {
    tokens: Vec<SemTok>,
}

impl SemTokBuilder {
    fn new() -> Self {
        Self { tokens: vec![] }
    }

    fn push<T: Into<Span>>(mut self, s: T, ty: TokenType) -> Self {
        self.tokens.push(SemTok::new(s.into(), ty));
        self
    }

    fn keyword<T: Into<Span>>(self, val: T) -> Self {
        self.push(val, TokenType::Keyword)
    }

    fn identifier<T: Into<Span>>(self, val: T) -> Self {
        self.push(val, TokenType::Identifier)
    }

    fn number<T: Into<Span>>(self, val: T) -> Self {
        self.push(val, TokenType::Number)
    }

    fn token(mut self, val: &Token) -> Self {
        self.tokens.extend(emit_semantic(&val).tokens);
        self
    }

    fn block(mut self, val: &Block) -> Self {
        self.tokens.extend(emit_semantic_ast(&val.inner));
        self
    }

    fn expression(mut self, val: &Expression) -> Self {
        self.tokens.extend(emit_expression_semantic(&val).tokens);
        self
    }

    fn args(mut self, args: &[ArgItem]) -> Self {
        for (expr, _) in args {
            self = self.expression(&expr.data);
        }
        self
    }
}

fn emit_semantic_ast(ast: &[Token]) -> Vec<SemTok> {
    ast.iter()
        .map(|tok| emit_semantic(tok).tokens)
        .flatten()
        .collect()
}

fn emit_semantic(token: &Token) -> SemTokBuilder {
    let b = SemTokBuilder::new();

    match &token {
        Token::Align { tag, value } => b.keyword(tag).expression(&value.data),
        Token::Braces { block, .. } | Token::Config(block) => b.block(block),
        Token::ConfigPair { key, value, .. } => b.push(key, TokenType::Keyword).token(&value.data),
        Token::Data { values, size } => b.push(size, TokenType::Keyword).args(values),
        Token::Definition { tag, id, value } => {
            let b = b.keyword(tag).identifier(id);
            match value {
                Some(v) => b.token(v),
                None => b,
            }
        }
        Token::Eof(_) => b,
        Token::Error(_) => b,
        Token::Expression(expr) => b.expression(&expr),
        Token::Include { tag, filename, .. } => b
            .push(tag, TokenType::Keyword)
            .push(filename, TokenType::Constant),
        Token::Label { id, block, .. } => {
            let b = b.identifier(id);
            match block {
                Some(block) => b.block(block),
                None => b,
            }
        }
        Token::If {
            tag_if,
            if_,
            value,
            tag_else,
            else_,
            ..
        } => {
            let b = b.keyword(tag_if).block(&if_).expression(&value.data);
            match else_ {
                Some(e) => b.keyword(tag_else.as_ref().unwrap()).block(e),
                None => b,
            }
        }
        Token::Instruction(i) => {
            let b = b.push(&i.mnemonic, TokenType::Mnemonic);
            match &i.operand {
                Some(op) => b.expression(&op.expr.data),
                None => b,
            }
        }
        Token::MacroDefinition {
            tag,
            id,
            args,
            block,
            ..
        } => b.keyword(tag).identifier(id).args(args).block(block),
        Token::MacroInvocation { name, args, .. } => {
            SemTokBuilder::new().identifier(name).args(args)
        }
        Token::ProgramCounterDefinition { star, value, .. } => {
            b.push(star, TokenType::Keyword).expression(&value.data)
        }
        Token::Segment { tag, id, block } => {
            let b = b.push(tag, TokenType::Keyword).identifier(id);
            match block {
                Some(block) => b.block(block),
                None => b,
            }
        }
        Token::VariableDefinition { ty, id, value, .. } => {
            let token_type = match &ty.data {
                VariableType::Constant => TokenType::Constant,
                VariableType::Variable => TokenType::Variable,
            };
            b.keyword(ty).identifier(id).push(value, token_type)
        }
    }
}

fn emit_expression_semantic(expression: &Expression) -> SemTokBuilder {
    match &expression {
        Expression::BinaryExpression(bin) => SemTokBuilder::new()
            .expression(&bin.lhs.data)
            .expression(&bin.rhs.data),
        Expression::Factor { factor, .. } => match &factor.data {
            ExpressionFactor::ExprParens { inner, .. } => emit_expression_semantic(&inner.data),
            ExpressionFactor::Number { value, .. } => SemTokBuilder::new().number(value),
            ExpressionFactor::IdentifierValue { path, .. } => SemTokBuilder::new().identifier(path),
            ExpressionFactor::CurrentProgramCounter(pc) => {
                SemTokBuilder::new().push(pc, TokenType::Constant)
            }
            ExpressionFactor::FunctionCall { name, args, .. } => {
                SemTokBuilder::new().identifier(name).args(args)
            }
        },
    }
}
