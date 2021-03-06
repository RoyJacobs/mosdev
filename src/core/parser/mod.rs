use crate::errors::{MosError, MosResult};
use codemap::Span;
use itertools::Itertools;
use nom::branch::alt;
use nom::bytes::complete::{is_a, is_not, tag, tag_no_case, take, take_till, take_till1};
use nom::character::complete::{alpha1, alphanumeric1, anychar, char, hex_digit1, none_of, space1};
use nom::combinator::{all_consuming, map, not, opt, recognize, rest};
use nom::multi::{many0, many1, separated_list1};
use nom::sequence::{pair, tuple};
use nom::InputTake;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

pub use ast::*;
pub use config_map::*;
pub use identifier::*;
pub use mnemonic::*;

/// Everything related to the syntax tree generated by the parser.
pub mod ast;
/// Config maps are key-value pair structures used in a few places, such as defining a segment.
pub mod config_map;
/// Everything related to identifiers (and paths).
pub mod identifier;
/// Mnemonics are the instructions the 6502 supports.
pub mod mnemonic;
/// Testing support
#[cfg(test)]
mod testing;

/// An error generated during parsing
#[derive(Debug)]
pub struct ParseError {
    span: Span,
    message: String,
}

impl ParseError {
    pub fn into_mos_error(self, tree: Arc<ParseTree>) -> MosError {
        MosError::Parser {
            tree,
            span: self.span,
            message: self.message,
        }
    }
}

/// Allows a fixed value to be returned in a parsing step
fn value<T: Clone>(value: T) -> impl FnMut(LocatedSpan) -> IResult<T> {
    move |input| Ok((input, value.clone()))
}

/// Similar to nom's internal [map] command, but uses a [FnOnce] instead of [FnMut]
fn map_once<I, O1, O2, E, F, G>(mut first: F, second: G) -> impl FnOnce(I) -> nom::IResult<I, O2, E>
where
    F: nom::Parser<I, O1, E>,
    G: FnOnce(O1) -> O2,
{
    move |input: I| {
        let (input, o1) = first.parse(input)?;
        Ok((input, second(o1)))
    }
}

/// If parsing fails, try to continue but log the error in the parser's [State]
fn expect<'a, F, E, T>(
    mut parser: F,
    error_msg: E,
) -> impl FnMut(LocatedSpan<'a>) -> IResult<'a, Option<T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<'a, T>,
    E: ToString,
{
    move |input| {
        let begin = input.location_offset();
        let i = input.clone();
        match parser(input) {
            Ok((remaining, out)) => Ok((remaining, Some(out))),
            Err(nom::Err::Error(_)) | Err(nom::Err::Failure(_)) => {
                let message = error_msg.to_string();
                if message.is_empty() {
                    // We're eating this error, assuming a more descriptive error
                    // is generated downstream
                } else {
                    let end = i.location_offset();
                    let span = to_span(&i, begin, end);
                    let err = ParseError { span, message };
                    i.extra.report_error(err);
                }
                Ok((i, None))
            }
            Err(err) => Err(err),
        }
    }
}

/// Handles a comment in the C++ style, e.g. `// foo"`
fn cpp_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    recognize(pair(tag("//"), is_not("\n\r")))(input)
}

/// Handles a comment in the C style, e.g. `/* hello */`. Deals with nested comments.
fn c_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    let original_input = input.clone();
    let (mut input, _) = tag("/*")(input)?;

    // We've already eaten 2 chars of the /* tag
    let mut offset = 2;

    // We've opened 1 comment
    let mut count = 1;

    while count > 0 {
        let (mut new_input, tag) = take_till(|c| c == '/' || c == '*')(input)?;
        offset += tag.len();
        if new_input.fragment().starts_with("/*") {
            count += 1;
            offset += 2;
            let (ni, _) = take(2usize)(new_input)?;
            new_input = ni;
        } else if new_input.fragment().starts_with("*/") {
            count -= 1;
            offset += 2;
            let (ni, _) = take(2usize)(new_input)?;
            new_input = ni;
        } else {
            // This wasn't a /* or */, so eat one char and continue.
            // If we can't continue we're at the end of the file and so the comment was unterminated.
            offset += 1;
            let (ni, tag) = expect(take(1usize), "unterminated block comment")(new_input)?;
            if tag.is_none() {
                // Just eat the rest of the input and ignore the following 'unexpected...' error
                ni.extra.ignore_next_error();
                return rest(ni);
            }
            new_input = ni;
        }
        input = new_input;
    }

    Ok(original_input.take_split(offset))
}

#[doc(hidden)]
fn trivia_impl() -> impl FnMut(LocatedSpan) -> IResult<Trivia> {
    move |input: LocatedSpan| {
        let (input, comment) = alt((
            map(space1, |span: LocatedSpan| {
                Trivia::Whitespace(span.fragment().to_owned().into())
            }),
            map(c_comment, |span| {
                Trivia::CStyle(span.fragment().to_owned().into())
            }),
            map(cpp_comment, |span| {
                Trivia::CppStyle(span.fragment().to_owned().into())
            }),
        ))(input)?;

        Ok((input, comment))
    }
}

/// Tries to parse trivia, including newlines
fn multiline_trivia(input: LocatedSpan) -> IResult<Box<Located<Vec<Trivia>>>> {
    map_once(
        located(|input| {
            many1(alt((
                trivia_impl(),
                map(tuple((opt(char('\r')), char('\n'))), |_| Trivia::NewLine),
            )))(input)
        }),
        Box::new,
    )(input)
}

/// Tries to parse multiline trivia
fn ws<'a, T, F>(mut inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<'a, Located<T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<'a, T>,
{
    move |input: LocatedSpan<'a>| {
        let (input, trivia) = opt(multiline_trivia)(input)?;
        let (input, result) = located_with_trivia(input, trivia, |i| inner(i))?;
        Ok((input, result))
    }
}

fn located<'a, T, F>(mut inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<'a, Located<T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<'a, T>,
{
    move |input: LocatedSpan<'a>| {
        let begin = input.location_offset();
        let (input, data) = inner(input)?;
        let end = input.location_offset();
        let span = to_span(&input, begin, end);
        Ok((input, Located::new(span, data)))
    }
}

/// Wraps whatever is being parsed in a `Located<T>`, including trivia, so that span information is preserved
pub fn located_with_trivia<'a, F: FnOnce(LocatedSpan<'a>) -> IResult<'a, T>, T>(
    input: LocatedSpan<'a>,
    trivia: Option<Box<Located<Vec<Trivia>>>>,
    inner: F,
) -> IResult<'a, Located<T>> {
    let begin = input.location_offset();
    let (input, data) = inner(input)?;
    let end = input.location_offset();
    let span = to_span(&input, begin, end);
    Ok((input, Located::new_with_trivia(span, data, trivia)))
}

fn to_span(input: &LocatedSpan, begin: usize, end: usize) -> Span {
    input.extra.file.span.subspan(begin as u64, end as u64)
}

/// Tries to parse a Rust-style identifier
fn identifier_name(input: LocatedSpan) -> IResult<Identifier> {
    map_once(
        recognize(pair(
            alt((alpha1, tag("_"))),
            many0(alt((alphanumeric1, tag("_")))),
        )),
        move |id: LocatedSpan| Identifier::new(id.fragment().to_string()),
    )(input)
}

/// Tries to parse a scope identifier ('-' or '+', not followed by any other alphanumeric)
fn identifier_scope(input: LocatedSpan) -> IResult<Identifier> {
    map_once(
        tuple((alt((char('-'), char('+'))), not(alphanumeric1))),
        move |(id, _)| Identifier::new(id.to_string()),
    )(input)
}

/// Tries to parse an full identifier path (e.g. `foo.bar.baz`) that may also include address modifiers
fn identifier_value(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    located(|input| {
        let id = tuple((
            opt(ws(map(alt((char('<'), char('>'))), move |m| match m {
                '<' => AddressModifier::LowByte,
                '>' => AddressModifier::HighByte,
                _ => panic!(),
            }))),
            ws(separated_list1(
                char('.'),
                alt((identifier_scope, identifier_name)),
            )),
        ));

        map_once(id, move |(modifier, identifier_path)| {
            let span = identifier_path.span;
            let identifier_path = identifier_path.map(|ids| {
                let path = ids.iter().cloned().collect_vec();
                Located::new(span, IdentifierPath::new(&path))
            });

            ExpressionFactor::IdentifierValue {
                path: identifier_path.flatten(),
                modifier,
            }
        })(input)
    })(input)
}

#[doc(hidden)]
fn register_suffix<'a>(
    input: LocatedSpan<'a>,
    reg: &'a str,
    map_to: IndexRegister,
) -> IResult<'a, RegisterSuffix> {
    map_once(
        tuple((ws(char(',')), ws(tag_no_case(reg)))),
        move |(comma, register)| {
            let register = register.map(|_r| map_to);
            RegisterSuffix { comma, register }
        },
    )(input)
}

/// Tries to parse a ", x" register suffix
fn register_x_suffix(input: LocatedSpan) -> IResult<RegisterSuffix> {
    register_suffix(input, "x", IndexRegister::X)
}

/// Tries to parse a ", y" register suffix
fn register_y_suffix(input: LocatedSpan) -> IResult<RegisterSuffix> {
    register_suffix(input, "y", IndexRegister::Y)
}

/// Tries to parse the operand of a 6502 instruction
fn operand(input: LocatedSpan) -> IResult<Operand> {
    let am_imm = map(tuple((ws(char('#')), expression)), move |(imm, expr)| {
        let lchar = Some(imm.map_into(|_| '#'));
        Operand {
            expr,
            lchar,
            rchar: None,
            addressing_mode: AddressingMode::Immediate,
            suffix: None,
        }
    });

    let optional_suffix = || opt(alt((register_x_suffix, register_y_suffix)));

    let am_abs = map(
        tuple((expression, optional_suffix())),
        move |(expr, suffix)| Operand {
            expr,
            lchar: None,
            rchar: None,
            addressing_mode: AddressingMode::AbsoluteOrZP,
            suffix,
        },
    );

    let am_ind = map(
        tuple((
            ws(char('(')),
            ws(expression),
            ws(char(')')),
            optional_suffix(),
        )),
        move |(lchar, expr, rchar, suffix)| Operand {
            expr: expr.flatten(),
            lchar: Some(lchar),
            rchar: Some(rchar),
            addressing_mode: AddressingMode::OuterIndirect,
            suffix,
        },
    );

    let am_outer_ind = map(
        tuple((
            ws(char('(')),
            ws(expression),
            optional_suffix(),
            ws(char(')')),
        )),
        move |(lchar, expr, suffix, rchar)| Operand {
            expr: expr.flatten(),
            lchar: Some(lchar),
            rchar: Some(rchar),
            addressing_mode: AddressingMode::Indirect,
            suffix,
        },
    );

    alt((am_imm, am_abs, am_ind, am_outer_ind))(input)
}

/// Tries to parse a bare block
fn braces(input: LocatedSpan) -> IResult<Token> {
    let scope = input.extra.new_anonymous_scope();
    map_once(block, move |block| Token::Braces { block, scope })(input)
}

/// Tries to parse a 6502 instruction consisting of a mnemonic and optionally an operand (e.g. `LDA #123`)
fn instruction(input: LocatedSpan) -> IResult<Token> {
    alt((
        map(
            tuple((ws(mnemonic), operand)),
            move |(mnemonic, operand)| {
                let instruction = Instruction {
                    mnemonic,
                    operand: Some(operand),
                };
                Token::Instruction(instruction)
            },
        ),
        map(
            tuple((
                ws(implied_mnemonic),
                expect(
                    not(operand),
                    "", // Eating the error since the unexpected operand itself will also generate an error
                ),
            )),
            move |(mnemonic, _)| {
                let instruction = Instruction {
                    mnemonic,
                    operand: None,
                };
                Token::Instruction(instruction)
            },
        ),
    ))(input)
}

/// When encountering an error, try to eat enough characters so that parsing may continue from a relatively clean state again
fn error(input: LocatedSpan) -> IResult<Token> {
    map_once(
        tuple((
            ws(recognize(take_till1(|c| {
                c == ')' || c == '}' || c == '\n' || c == '\r'
            }))),
            opt(anychar),
        )),
        move |(input, char)| {
            let char = char.map(|c| c.to_string()).unwrap_or_default();
            let err = ParseError {
                span: input.span,
                message: format!("unexpected '{}'", input.data.fragment()),
            };
            input.data.extra.report_error(err);
            let input = input.map_into(|i| format!("{}{}", i.fragment(), char));
            Token::Error(input)
        },
    )(input)
}

/// Tries to parse a label in the form of `foo:`
fn label(input: LocatedSpan) -> IResult<Token> {
    map_once(
        tuple((ws(identifier_name), ws(char(':')), opt(block))),
        move |(id, colon, block)| Token::Label { id, colon, block },
    )(input)
}

/// Tries to parse a data statement such as `.byte 1, 2, 3`
fn data(input: LocatedSpan) -> IResult<Token> {
    map_once(
        tuple((
            alt((
                map(ws(tag_no_case(".byte")), |t| t.map(|_| DataSize::Byte)),
                map(ws(tag_no_case(".word")), |t| t.map(|_| DataSize::Word)),
                map(ws(tag_no_case(".dword")), |t| t.map(|_| DataSize::Dword)),
            )),
            expect(arg_list, "expected expression"),
        )),
        move |(size, values)| Token::Data {
            values: values.unwrap_or_default(),
            size,
        },
    )(input)
}

#[doc(hidden)]
fn varconst_impl<'a, 'b>(
    input: LocatedSpan<'a>,
    tag: &'b str,
    ty: VariableType,
) -> IResult<'a, Token> {
    map_once(
        tuple((
            ws(tag_no_case(tag)),
            ws(identifier_name),
            ws(char('=')),
            expression,
        )),
        move |(tag, id, eq, value)| {
            let ty = tag.map(|_| ty);
            Token::VariableDefinition { ty, id, eq, value }
        },
    )(input)
}

/// Tries to parse a variable definition of the form `.var foo = 1`
fn variable_definition(input: LocatedSpan) -> IResult<Token> {
    varconst_impl(input, ".var", VariableType::Variable)
}

/// Tries to parse a constant definition of the form `.const foo = 1`
fn const_definition(input: LocatedSpan) -> IResult<Token> {
    varconst_impl(input, ".const", VariableType::Constant)
}

/// Tries to parse a program counter definition of the form `* = $2000`
fn pc_definition(input: LocatedSpan) -> IResult<Token> {
    map_once(
        tuple((ws(char('*')), ws(char('=')), ws(expression))),
        move |(star, eq, value)| Token::ProgramCounterDefinition {
            star,
            eq,
            value: value.flatten(),
        },
    )(input)
}

/// Tries to parse a configuration map definition
fn config_definition(input: LocatedSpan) -> IResult<Token> {
    map_once(
        tuple((
            ws(tag_no_case(".define")),
            ws(identifier_name),
            expect(
                config_map::config_map,
                "unable to parse configuration object",
            ),
        )),
        move |(tag, id, cfg)| Token::Definition {
            tag: tag.map_into(|_| ".define".into()),
            id,
            value: cfg.map(Box::new),
        },
    )(input)
}

/// Tries to parse tokens enclosed in braces, e.g. `{ ... }`
fn block(input: LocatedSpan) -> IResult<Block> {
    map_once(
        tuple((ws(char('{')), many0(statement), ws(char('}')))),
        move |(lparen, inner, rparen)| Block {
            lparen,
            inner,
            rparen,
        },
    )(input)
}

/// Tries to parse a segment definition
fn segment(input: LocatedSpan) -> IResult<Token> {
    map_once(
        tuple((ws(tag_no_case(".segment")), ws(identifier_name), opt(block))),
        move |(tag, id, block)| Token::Segment {
            tag: tag.map_into(|_| ".segment".into()),
            id,
            block,
        },
    )(input)
}

/// Tries to parse an if/else statement
fn if_(input: LocatedSpan) -> IResult<Token> {
    let if_scope = Box::new(input.extra.new_anonymous_scope());
    let else_scope = Box::new(input.extra.new_anonymous_scope());
    map_once(
        tuple((
            ws(tag_no_case(".if")),
            expression,
            block,
            opt(tuple((ws(tag_no_case("else")), block))),
        )),
        move |(tag_if, value, if_, else_)| {
            let tag_if = tag_if.map_into(|_| ".if".into());
            let (tag_else, else_) = match else_ {
                Some((tag_else, else_)) => {
                    (Some(tag_else.map_into(|_| "else".into())), Some(else_))
                }
                None => (None, None),
            };
            Token::If {
                tag_if,
                value,
                if_,
                if_scope,
                tag_else,
                else_,
                else_scope,
            }
        },
    )(input)
}

/// Tries to parse an align directive, of the form `.align 16`
fn align(input: LocatedSpan) -> IResult<Token> {
    map_once(
        tuple((ws(tag_no_case(".align")), ws(expression))),
        move |(tag, value)| {
            let tag = tag.map_into(|_| ".align".into());
            let value = value.flatten();
            Token::Align { tag, value }
        },
    )(input)
}

/// Tries to parse an include directive, of the form `.include "foo.bin"`
fn include(input: LocatedSpan) -> IResult<Token> {
    let filename = recognize(many1(none_of("\"\r\n")));

    map_once(
        tuple((
            ws(tag_no_case(".include")),
            ws(char('"')),
            located(filename),
            char('"'),
        )),
        move |(tag, lquote, filename, _)| {
            let tag = tag.map_into(|_| ".include".into());
            let filename = filename.map(|v| v.fragment().to_string());
            Token::Include {
                tag,
                lquote,
                filename,
            }
        },
    )(input)
}

/// Tries to parse all valid statement types
fn statement(input: LocatedSpan) -> IResult<Token> {
    alt((
        braces,
        instruction,
        variable_definition,
        const_definition,
        pc_definition,
        config_definition,
        label,
        data,
        segment,
        if_,
        align,
        include,
    ))(input)
}

/// Tries to parse any valid statement but will fallback to [error] in case of trouble
fn statement_or_error(input: LocatedSpan) -> IResult<Token> {
    alt((statement, error))(input)
}

/// Tries to eat the remaining characters
fn eof(input: LocatedSpan) -> IResult<Token> {
    map(ws(rest), move |rest| Token::Eof(rest.map_into(|_| ())))(input)
}

/// Parses an entire file
fn source_file(input: LocatedSpan) -> IResult<Vec<Token>> {
    map(tuple((many0(statement_or_error), eof)), |(tokens, eof)| {
        let mut result = tokens;
        result.push(eof);
        result
    })(input)
}

/// Parses a number of any possible [NumberType]
fn number(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    located(|input| {
        map_once(
            alt((
                tuple((
                    map(ws(char('$')), |ty| ty.map_into(|_| NumberType::Hex)),
                    ws(recognize(many1(hex_digit1))),
                )),
                tuple((
                    map(ws(char('%')), |ty| ty.map_into(|_| NumberType::Bin)),
                    ws(recognize(many1(is_a("01")))),
                )),
                tuple((
                    located(|input| value(NumberType::Dec)(input)),
                    ws(recognize(many1(is_a("0123456789")))),
                )),
            )),
            move |(ty, value)| {
                let loc = value.span;
                let trv = value.trivia;
                let num = Number::from_type(ty.data.clone(), value.data.fragment());
                let value = Located::new_with_trivia(loc, num, trv);
                ExpressionFactor::Number { ty, value }
            },
        )(input)
    })(input)
}

/// Deals with parentheses encountered in expressions
fn expression_parens(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    located(|input| {
        map_once(
            tuple((ws(char('[')), ws(expression), ws(char(']')))),
            move |(lparen, inner, rparen)| {
                let inner = Box::new(inner.flatten());
                ExpressionFactor::ExprParens {
                    lparen,
                    inner,
                    rparen,
                }
            },
        )(input)
    })(input)
}

/// Parses the star character when used as a placeholder for the current program counter
fn current_pc(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    located(|input| {
        map_once(ws(char('*')), move |star| {
            ExpressionFactor::CurrentProgramCounter(star)
        })(input)
    })(input)
}

/// Parses a comma-separated list
fn arg_list(input: LocatedSpan) -> IResult<Vec<ArgItem>> {
    map(
        tuple((
            many0(tuple((ws(expression), ws(char(','))))),
            ws(expression),
        )),
        |(list, last)| {
            let list = list
                .into_iter()
                .map(|(expr, comma)| (expr.flatten(), Some(comma)))
                .collect::<Vec<ArgItem>>();
            let mut result: Vec<ArgItem> = vec![];
            result.extend(list);
            result.push((last.flatten(), None));
            result
        },
    )(input)
}

/// Parses a function call when invoked in an expression
fn fn_call(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    located(|input| {
        map_once(
            tuple((
                ws(identifier_name),
                ws(char('(')),
                opt(arg_list),
                ws(char(')')),
            )),
            move |(name, lparen, args, rparen)| {
                let args = args.unwrap_or_else(Vec::new);
                ExpressionFactor::FunctionCall {
                    name,
                    lparen,
                    args,
                    rparen,
                }
            },
        )(input)
    })(input)
}

/// A factor in an expression, without any flags
fn expression_factor_inner(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    alt((
        number,
        fn_call,
        identifier_value,
        current_pc,
        expression_parens,
    ))(input)
}

/// Parses a factor used in an expression, such as a number or a function call
fn expression_factor(input: LocatedSpan) -> IResult<Located<Expression>> {
    // See if we can parse the term without any flags. If we can't, there must be flags, so let's try again
    located(|input| {
        alt((
            map(expression_factor_inner, move |factor| Expression::Factor {
                factor: Box::new(factor),
                flags: ExpressionFactorFlags::empty(),
                tag_not: None,
                tag_neg: None,
            }),
            map(
                tuple((
                    opt(ws(char('!'))),
                    opt(ws(char('-'))),
                    expression_factor_inner,
                )),
                move |(tag_not, tag_neg, factor)| {
                    let mut flags = ExpressionFactorFlags::empty();
                    if tag_not.is_some() {
                        flags.set(ExpressionFactorFlags::NOT, true);
                    }
                    if tag_neg.is_some() {
                        flags.set(ExpressionFactorFlags::NEG, true);
                    }
                    Expression::Factor {
                        factor: Box::new(factor),
                        flags,
                        tag_not,
                        tag_neg,
                    }
                },
            ),
        ))(input)
    })(input)
}

/// Folds back a list of expressions and operations into a single [Expression] token
fn fold_expressions(
    initial: Located<Expression>,
    remainder: Vec<(Located<BinaryOp>, Located<Expression>)>,
) -> Located<Expression> {
    remainder.into_iter().fold(initial, |acc, pair| {
        let (op, expr) = pair;

        Located::new(
            acc.span,
            Expression::BinaryExpression(BinaryExpression {
                op,
                lhs: Box::new(acc),
                rhs: Box::new(expr),
            }),
        )
    })
}

/// Parses a term in an expression, containing operators of the highest precedence, e.g. a multiplication
fn expression_term(input: LocatedSpan) -> IResult<Located<Expression>> {
    let (input, initial) = expression_factor(input)?;

    let (input, remainder) = many0(tuple((
        ws(alt((
            map(tag("*"), |_| BinaryOp::Mul),
            map(tag("/"), |_| BinaryOp::Div),
            map(tag("<<"), |_| BinaryOp::Shl),
            map(tag(">>"), |_| BinaryOp::Shr),
            map(tag("^"), |_| BinaryOp::Xor),
        ))),
        expression_factor,
    )))(input)?;

    Ok((input, fold_expressions(initial, remainder)))
}

/// Parses a (sub)expression containing operators of the lowest precedence
pub fn expression(input: LocatedSpan) -> IResult<Located<Expression>> {
    let (input, initial) = expression_term(input)?;

    let (input, remainder) = many0(tuple((
        ws(alt((
            map(tag("+"), |_| BinaryOp::Add),
            map(tag("-"), |_| BinaryOp::Sub),
            map(tag("=="), |_| BinaryOp::Eq),
            map(tag("!="), |_| BinaryOp::Ne),
            map(tag(">="), |_| BinaryOp::GtEq),
            map(tag("<="), |_| BinaryOp::LtEq),
            map(tag(">"), |_| BinaryOp::Gt),
            map(tag("<"), |_| BinaryOp::Lt),
            map(tag("&&"), |_| BinaryOp::And),
            map(tag("||"), |_| BinaryOp::Or),
        ))),
        expression_term,
    )))(input)?;

    Ok((input, fold_expressions(initial, remainder)))
}

/// Parses an input file and returns a hopefully parsed file
pub fn parse<'a>(filename: &'a Path, source: &'a str) -> (Arc<ParseTree>, Option<MosError>) {
    let state = State::new(filename.as_os_str().to_string_lossy(), source);
    let code_map = state.code_map.clone();
    let files = state.files.clone();
    let errors = state.errors.clone();
    let input = LocatedSpan::new_extra(source, state);
    let (_, tokens) = all_consuming(source_file)(input).expect("parser cannot fail");

    let code_map = Rc::try_unwrap(code_map).ok().unwrap().into_inner();
    let files = Rc::try_unwrap(files).ok().unwrap().into_inner();
    let tree = Arc::new(ParseTree::new(code_map, files, tokens));

    let errors = Rc::try_unwrap(errors).ok().unwrap().into_inner();
    if errors.is_empty() {
        (tree, None)
    } else {
        let errors = errors
            .into_iter()
            .map(|e| e.into_mos_error(tree.clone()))
            .collect_vec();
        (tree, Some(MosError::Multiple(errors)))
    }
}

pub fn parse_or_err<'a>(filename: &'a Path, source: &'a str) -> MosResult<Arc<ParseTree>> {
    let (tree, error) = parse(filename, source);
    match error {
        Some(e) => Err(e),
        None => Ok(tree),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_instruction() {
        check("lda #123", "LDA #123");
    }

    #[test]
    fn parse_multiple_lines() {
        check("lda #123\nsta $d020", "LDA #123\nSTA $d020");
        check("nop\n  nop", "NOP\n  NOP");
    }

    #[test]
    fn parse_expression() {
        check("lda #1 + 2", "LDA #1 + 2");
        check("lda #1     +    2", "LDA #1     +    2");
        check("lda #[1   +   2]", "LDA #[1   +   2]");
        check("lda #[1   +   2   ]", "LDA #[1   +   2   ]");
        check("lda #[   1   +   2   ]", "LDA #[   1   +   2   ]");
        check("lda #1 ^ 4", "LDA #1 ^ 4");
        check("lda #1 << 4", "LDA #1 << 4");
        check("lda #1 >> 4", "LDA #1 >> 4");
        check("lda #1 || 2", "LDA #1 || 2");
        check("lda #1 && 2", "LDA #1 && 2");
        check("lda  %11101", "LDA  %11101");
        check(
            "lda  %11101   +   [  $ff  * -12367 ] / foo",
            "LDA  %11101   +   [  $ff  * -12367 ] / foo",
        );
    }

    #[test]
    fn parse_expression_factor_flags() {
        check("lda #-foo", "LDA #-foo");
        check("lda #!foo", "LDA #!foo");
    }

    #[test]
    fn parse_equality() {
        check("lda #1 == 2", "LDA #1 == 2");
        check("lda #1 != 2", "LDA #1 != 2");
        check("lda #1 < 2", "LDA #1 < 2");
        check("lda #1 > 2", "LDA #1 > 2");
        check("lda #1 <= 2", "LDA #1 <= 2");
        check("lda #1 >= 2", "LDA #1 >= 2");
    }

    #[test]
    fn parse_identifier_paths() {
        check("lda a", "LDA a");
        check("lda   super.a", "LDA   super.a");
    }

    #[test]
    fn parse_identifier_scopes() {
        check("lda -", "LDA -");
        check("lda   super.+", "LDA   super.+");
    }

    #[test]
    fn can_handle_leading_trailing_whitespace() {
        check("   lda #123", "   LDA #123");
        check("   \nlda #123", "   \nLDA #123");
        check("   \n\nlda #123", "   \n\nLDA #123");
        check("\n\n  ", "\n\n  ");
        check("lda #123\n\n   ", "LDA #123\n\n   ");
        check("   \nlda #123\n\n   ", "   \nLDA #123\n\n   ");
        check("   \n   ", "   \n   ");
        check("lda #123   \n   ", "LDA #123   \n   ");
    }

    #[test]
    fn parse_braces() {
        check("{  }", "{  }");
        check("{  }    ", "{  }    ");
        check("{   lda #123   }", "{   LDA #123   }");
        check(
            r"
            {
                lda #123
                lda #234
            }
        ",
            r"
            {
                LDA #123
                LDA #234
            }
        ",
        );
    }

    #[test]
    fn parse_if() {
        check("  .if   foo { nop }", "  .IF   foo { NOP }");
        check("  .if   foo\n{\nnop\n}", "  .IF   foo\n{\nNOP\n}");
        check(
            "   .if   foo { nop }   else { brk }",
            "   .IF   foo { NOP }   ELSE { BRK }",
        );
        check(".if defined(foo) { nop }", ".IF defined(foo) { NOP }");
        check(".if !defined(foo) { nop }", ".IF !defined(foo) { NOP }");
    }

    #[test]
    fn parse_align() {
        check("   .align   123", "   .ALIGN   123");
    }

    #[test]
    fn parse_addressing_modes() {
        check("lda #123", "LDA #123");
        check("lda 12345", "LDA 12345");
        check("lda 12345, x", "LDA 12345, X");
        check("lda 12345 ,  x", "LDA 12345 ,  X");
        check("lda 12345, y", "LDA 12345, Y");
        check("lda (123), x", "LDA (123), X");
        check("lda (123, x)", "LDA (123, X)");
        check("lda (   123   ,   x   )", "LDA (   123   ,   X   )");
    }

    #[test]
    fn parse_variable_definitions() {
        check("  .var foo   = 123", "  .VAR foo   = 123");
        check("  .const foo   = 123", "  .CONST foo   = 123");
    }

    #[test]
    fn parse_segment_definitions() {
        check(
            r"  .define   segment
            {
            name = hello
            start = 4096
            }",
            r"  .DEFINE   segment
            {
            name = hello
            start = 4096
            }",
        );
    }

    #[test]
    fn use_segment() {
        check(".segment   foo", ".SEGMENT   foo");
        check("  .segment   foo   { nop }", "  .SEGMENT   foo   { NOP }");
        check_ignore_err(
            "  .segment   foo   {invalid}\nnop",
            "  .SEGMENT   foo   {invalid}\nNOP",
        );
    }

    #[test]
    fn parse_current_pc() {
        check("lda *", "LDA *");
        check("lda * - 3", "LDA * - 3");
    }

    #[test]
    fn set_current_pc() {
        check("  *   =   $1000", "  *   =   $1000");
    }

    #[test]
    fn parse_address_modifiers() {
        check("lda #<foo", "LDA #<foo");
        check("lda #>foo", "LDA #>foo");
    }

    #[test]
    fn parse_data() {
        check(
            ".byte 123\n.word foo\n.dword 12345678\n.word 1 + 2,   3, 4 * 4",
            ".BYTE 123\n.WORD foo\n.DWORD 12345678\n.WORD 1 + 2,   3, 4 * 4",
        );
    }

    #[test]
    fn parse_label() {
        check("   foo:   nop", "   foo:   NOP");
        check("   foo:   {nop }", "   foo:   {NOP }");
    }

    #[test]
    fn parse_include() {
        check("   .include    \"foo.bin\"", "   .INCLUDE    \"foo.bin\"");
    }

    #[test]
    fn parse_fn_call() {
        let factor = invoke("func()", fn_call);
        assert_eq!(factor.to_string(), "func()");

        let factor = invoke("func   (   a)", fn_call);
        assert_eq!(factor.to_string(), "func   (   a)");

        let factor = invoke("func   (a   ,   b   )", fn_call);
        assert_eq!(factor.to_string(), "func   (a   ,   b   )");
    }

    #[test]
    fn error_when_using_operand_with_implied_mnemonic() {
        check_err("inx $1234", "test.asm:1:5: error: unexpected '$1234'");
        check_err_span("inx $1234", 5, 10);
    }

    #[test]
    fn parse_comments() {
        check(
            ".const test /* test value */ = 1\n.segment default {nop /* nice*/}\nfoo: {/* here it is */}// hi",
            ".CONST test /* test value */ = 1\n.SEGMENT default {NOP /* nice*/}\nfoo: {/* here it is */}// hi"
        );

        // nested comment
        check(
            ".const test /* a /* b /* c */ */ */ = 1",
            ".CONST test /* a /* b /* c */ */ */ = 1",
        );

        // unbalanced comment
        check_err(
            ".const test /* a = 1",
            "test.asm:1:21: error: unterminated block comment",
        );
    }

    fn check(src: &str, expected: &str) {
        let (tree, error) = parse(&Path::new("test.asm"), src);
        if let Some(e) = error {
            panic!(e.to_string());
        }
        let actual = tree.tokens().iter().map(|e| format!("{}", e)).join("");
        assert_eq!(actual, expected.to_string());
    }

    fn check_ignore_err(src: &str, expected: &str) {
        let (tree, _) = parse(&Path::new("test.asm"), src);
        let actual = tree.tokens().iter().map(|e| format!("{}", e)).join("");
        assert_eq!(actual, expected.to_string());
    }

    fn check_err(src: &str, expected: &str) {
        let (_tree, error) = parse(&Path::new("test.asm"), src);
        assert!(error.is_some());
        let actual = error.unwrap().to_string();
        assert_eq!(actual, expected.to_string());
    }

    fn check_err_span(src: &str, start_column: usize, end_column: usize) {
        let (_tree, error) = parse(&Path::new("test.asm"), src);
        match error.unwrap() {
            MosError::Multiple(errors) => {
                assert_eq!(errors.len(), 1);
                match errors.first().unwrap() {
                    MosError::Parser { tree, span, .. } => {
                        let loc = tree.code_map().look_up_span(*span);
                        assert_eq!(loc.begin.column + 1, start_column);
                        assert_eq!(loc.end.column + 1, end_column);
                    }
                    _ => panic!(),
                }
            }
            _ => panic!(),
        }
    }

    fn invoke<S: Into<String>, O, F: FnOnce(LocatedSpan) -> IResult<Located<O>>>(
        src: S,
        parser: F,
    ) -> O {
        let src = src.into();
        let state = State::new("test.asm", src.clone());
        let input = LocatedSpan::new_extra(&src, state);
        let result = parser(input);
        if result.is_err() {
            panic!(format!("{}", result.err().unwrap()));
        }
        result.ok().unwrap().1.data
    }
}
