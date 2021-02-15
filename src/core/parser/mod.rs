use std::path::Path;
use std::rc::Rc;

use itertools::Itertools;
use nom::branch::alt;
use nom::bytes::complete::{is_a, is_not, tag, tag_no_case, take_till1, take_until};
use nom::character::complete::{alpha1, alphanumeric1, char, hex_digit1, space1};
use nom::combinator::{all_consuming, map, opt, recognize, rest};
use nom::multi::{many0, many1, separated_list1};
use nom::sequence::{pair, tuple};

pub use ast::*;
pub use config_map::*;
pub use mnemonic::*;

use crate::core::parser::mnemonic::mnemonic;
use crate::errors::{MosError, MosResult};

/// Everything related to the syntax tree generated by the parser.
pub mod ast;
/// Config maps are key-value pair structures used in a few places, such as defining a segment.
pub mod config_map;
/// Mnemonics are the instructions the 6502 supports.
pub mod mnemonic;

/// An error generated during parsing
#[derive(thiserror::Error, Debug, PartialEq)]
pub enum ParseError<'a> {
    #[error("{message}")]
    ExpectedError {
        location: Location<'a>,
        message: String,
    },
    #[error("{message}")]
    UnexpectedError {
        location: Location<'a>,
        message: String,
    },
}

/// Converts a [ParseError] into a more generic [MosError]
impl<'a> From<ParseError<'a>> for MosError {
    fn from(err: ParseError<'a>) -> Self {
        match err {
            ParseError::ExpectedError { location, message } => Self::Parser {
                location: location.into(),
                message,
            },
            ParseError::UnexpectedError { location, message } => Self::Parser {
                location: location.into(),
                message,
            },
        }
    }
}

/// Allows a fixed value to be returned in a parsing step
fn value<'a, T: Clone>(value: T) -> impl FnMut(LocatedSpan<'a>) -> IResult<T> {
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
) -> impl FnMut(LocatedSpan<'a>) -> IResult<Option<T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<T>,
    E: ToString,
{
    move |input| {
        let i = input.clone();
        match parser(input) {
            Ok((remaining, out)) => Ok((remaining, Some(out))),
            Err(nom::Err::Error(_)) | Err(nom::Err::Failure(_)) => {
                let err = ParseError::ExpectedError {
                    location: Location::from(&i),
                    message: error_msg.to_string(),
                };
                i.extra.report_error(err);
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

/// Handles parsing inside of a C-style comment
fn inside_c_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    // Once we're inside a C comment, we don't care about anything except perhaps another /*
    let (input, _) = take_until("/*")(input)?;

    // Found another /*, so let's consume it
    let (input, _) = tag("/*")(input)?;

    // Found another /*, so now we either recurse or we go on until we're at the closing */
    let (input, _) = expect(
        pair(alt((inside_c_comment, take_until("*/"))), tag("*/")),
        "missing closing */",
    )(input)?;

    // Ignore any trailing characters until we're up to the next (one level up) */, so the outer function can deal with that
    take_until("*/")(input)
}

/// Handles a comment in the C style, e.g. `/* hello */`. Deals with nested comments via [inside_c_comment]
fn c_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    recognize(tuple((
        tag("/*"),
        expect(
            tuple((alt((inside_c_comment, take_until("*/"))), tag("*/"))),
            "missing closing */",
        ),
    )))(input)
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
    let location = Location::from(&input);

    map_once(
        many1(alt((
            trivia_impl(),
            map(tuple((opt(char('\r')), char('\n'))), |_| Trivia::NewLine),
        ))),
        move |comments| Box::new(Located::new(location, comments)),
    )(input)
}

/// Tries to parse trivia, without newlines
fn trivia(input: LocatedSpan) -> IResult<Box<Located<Vec<Trivia>>>> {
    let location = Location::from(&input);

    map_once(many1(trivia_impl()), move |comments| {
        Box::new(Located::new(location, comments))
    })(input)
}

#[doc(hidden)]
fn ws_impl<'a, T, F>(
    mut inner: F,
    multiline: bool,
) -> impl FnMut(LocatedSpan<'a>) -> IResult<Located<'a, T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<T>,
{
    move |input: LocatedSpan<'a>| {
        let location = Location::from(&input);

        let (input, trivia) = if multiline {
            opt(multiline_trivia)(input)
        } else {
            opt(trivia)(input)
        }?;
        let (input, data) = inner(input)?;
        let result = Located::new_with_trivia(location, data, trivia);

        Ok((input, result))
    }
}

/// Tries to parse multiline trivia
fn multiline_ws<'a, T, F>(inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<Located<'a, T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<T>,
{
    ws_impl(inner, true)
}

/// Tries to parse single line trivia
fn ws<'a, T, F>(inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<Located<'a, T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<T>,
{
    ws_impl(inner, false)
}

/// Wraps whatever is being parsed in a `Located<T>` so that location information is preserved
fn located<'a, T, F>(mut inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<Located<T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<T>,
{
    move |input: LocatedSpan<'a>| {
        let location = Location::from(&input);
        let (input, inner) = inner(input)?;
        Ok((input, Located::new(location, inner)))
    }
}

/// Tries to parse a Rust-style identifier
fn identifier_name(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        recognize(pair(
            alt((alpha1, tag("_"))),
            many0(alt((alphanumeric1, tag("_")))),
        )),
        move |id: LocatedSpan| {
            let id = Identifier(id.fragment());
            Located::new(location, Token::IdentifierName(id))
        },
    )(input)
}

/// Tries to parse an full identifier path (e.g. `foo.bar.baz`) that may also include address modifiers
fn identifier_value(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    let location = Location::from(&input);

    let id_location = location.clone();
    let id = tuple((
        opt(ws(map(alt((char('<'), char('>'))), move |m| {
            let modifier = match m {
                '<' => AddressModifier::LowByte,
                '>' => AddressModifier::HighByte,
                _ => panic!(),
            };
            Located::new(id_location.clone(), modifier)
        }))),
        ws(separated_list1(char('.'), identifier_name)),
    ));

    map_once(id, move |(modifier, identifier_path)| {
        let identifier_path = identifier_path.map(|ids| {
            let location = ids.first().unwrap().location.clone();
            let path = ids
                .iter()
                .map(|lt| lt.data.as_identifier().clone())
                .collect_vec();
            Located::new(location, IdentifierPath::new(&path))
        });
        let modifier = modifier.map(|m| m.flatten());

        Located::new(
            location,
            ExpressionFactor::IdentifierValue {
                path: identifier_path.flatten(),
                modifier,
            },
        )
    })(input)
}

#[doc(hidden)]
fn register_suffix<'a>(
    input: LocatedSpan<'a>,
    reg: &'a str,
    map_to: IndexRegister,
) -> IResult<'a, Located<'a, Token<'a>>> {
    let location = Location::from(&input);

    map_once(
        tuple((ws(char(',')), ws(tag_no_case(reg)))),
        move |(comma, register)| {
            let register = register.map(|_r| map_to);
            Located::new(location, Token::RegisterSuffix { comma, register })
        },
    )(input)
}

/// Tries to parse a ", x" register suffix
fn register_x_suffix(input: LocatedSpan) -> IResult<Located<Token>> {
    register_suffix(input, "x", IndexRegister::X)
}

/// Tries to parse a ", y" register suffix
fn register_y_suffix(input: LocatedSpan) -> IResult<Located<Token>> {
    register_suffix(input, "y", IndexRegister::Y)
}

/// Tries to parse the operand of a 6502 instruction
fn operand(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    let loc_imm = location.clone();
    let am_imm = map(tuple((ws(char('#')), expression)), move |(imm, expr)| {
        let lchar = Some(imm.map_into(|_| '#'));
        Located::new(
            loc_imm.clone(),
            Token::Operand(Operand {
                expr: Box::new(expr),
                lchar,
                rchar: None,
                addressing_mode: AddressingMode::Immediate,
                suffix: None,
            }),
        )
    });

    let optional_suffix = || opt(alt((register_x_suffix, register_y_suffix)));

    let loc_abs = location.clone();
    let am_abs = map(
        tuple((expression, optional_suffix())),
        move |(expr, suffix)| {
            Located::new(
                loc_abs.clone(),
                Token::Operand(Operand {
                    expr: Box::new(expr),
                    lchar: None,
                    rchar: None,
                    addressing_mode: AddressingMode::AbsoluteOrZP,
                    suffix: suffix.map(Box::new),
                }),
            )
        },
    );

    let loc_ind = location.clone();
    let am_ind = map(
        tuple((
            ws(char('(')),
            ws(expression),
            ws(char(')')),
            optional_suffix(),
        )),
        move |(lchar, expr, rchar, suffix)| {
            Located::new(
                loc_ind.clone(),
                Token::Operand(Operand {
                    expr: Box::new(expr.flatten()),
                    lchar: Some(lchar),
                    rchar: Some(rchar),
                    addressing_mode: AddressingMode::OuterIndirect,
                    suffix: suffix.map(Box::new),
                }),
            )
        },
    );

    let loc_outer_ind = location;
    let am_outer_ind = map(
        tuple((
            ws(char('(')),
            ws(expression),
            optional_suffix(),
            ws(char(')')),
        )),
        move |(lchar, expr, suffix, rchar)| {
            Located::new(
                loc_outer_ind.clone(),
                Token::Operand(Operand {
                    expr: Box::new(expr.flatten()),
                    lchar: Some(lchar),
                    rchar: Some(rchar),
                    addressing_mode: AddressingMode::Indirect,
                    suffix: suffix.map(Box::new),
                }),
            )
        },
    );

    alt((am_imm, am_abs, am_ind, am_outer_ind))(input)
}

/// Tries to parse a 6502 instruction consisting of a mnemonic and an operand (e.g. `LDA #123`)
fn instruction(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    let instruction = tuple((ws(mnemonic), opt(operand)));

    map_once(instruction, move |(mnemonic, operand)| {
        let instruction = Instruction {
            mnemonic,
            operand: operand.map(Box::new),
        };
        Located::new(location, Token::Instruction(instruction))
    })(input)
}

/// When encountering an error, try to eat enough characters so that parsing may continue from a relatively clean state again
fn error(input: LocatedSpan) -> IResult<Located<Token>> {
    map(
        ws(take_till1(|c| {
            c == ')' || c == '}' || c == '\n' || c == '\r'
        })),
        |span| {
            let err = ParseError::UnexpectedError {
                location: Location::from(&span.data),
                message: format!("unexpected '{}'", span.data.fragment()),
            };
            span.data.extra.report_error(err);
            Located::new(Location::from(&span.data), Token::Error)
        },
    )(input)
}

/// Tries to parse a label in the form of `foo:`
fn label(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            ws(identifier_name),
            expect(ws(char(':')), "labels should end with ':'"),
        )),
        move |(id, colon)| {
            let id = id.flatten().map_into(|i| i.into_identifier());
            Located::new(location, Token::Label { id, colon })
        },
    )(input)
}

/// Tries to parse a data statement such as `.byte 1, 2, 3`
fn data(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            alt((
                map(ws(tag_no_case(".byte")), |t| t.map(|_| DataSize::Byte)),
                map(ws(tag_no_case(".word")), |t| t.map(|_| DataSize::Word)),
                map(ws(tag_no_case(".dword")), |t| t.map(|_| DataSize::Dword)),
            )),
            expect(arg_list, "expected expression"),
        )),
        move |(size, values)| {
            Located::new(
                location,
                Token::Data {
                    values: values.unwrap_or_default(),
                    size,
                },
            )
        },
    )(input)
}

#[doc(hidden)]
fn varconst_impl<'a, 'b>(
    input: LocatedSpan<'a>,
    tag: &'b str,
    ty: VariableType,
) -> IResult<'a, Located<'a, Token<'a>>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            ws(tag_no_case(tag)),
            ws(identifier_name),
            ws(char('=')),
            expression,
        )),
        move |(tag, id, eq, value)| {
            let ty = tag.map(|_| ty);
            let id = id.flatten().map_into(|id| id.into_identifier());
            Located::new(
                location,
                Token::VariableDefinition {
                    ty,
                    id,
                    eq,
                    value: Box::new(value),
                },
            )
        },
    )(input)
}

/// Tries to parse a variable definition of the form `.var foo = 1`
fn variable_definition(input: LocatedSpan) -> IResult<Located<Token>> {
    varconst_impl(input, ".var", VariableType::Variable)
}

/// Tries to parse a constant definition of the form `.const foo = 1`
fn const_definition(input: LocatedSpan) -> IResult<Located<Token>> {
    varconst_impl(input, ".const", VariableType::Constant)
}

/// Tries to parse a program counter definition of the form `* = $2000`
fn pc_definition(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((ws(char('*')), ws(char('=')), ws(expression))),
        move |(star, eq, value)| {
            Located::new(
                location,
                Token::ProgramCounterDefinition {
                    star,
                    eq,
                    value: value.flatten(),
                },
            )
        },
    )(input)
}

/// Tries to parse a configuration map definition
fn config_definition(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            ws(tag_no_case(".define")),
            ws(identifier_name),
            expect(
                config_map::config_map,
                "unable to parse configuration object",
            ),
        )),
        move |(tag, id, cfg)| {
            let id = Box::new(id.flatten());
            Located::new(
                location,
                Token::Definition {
                    tag: tag.map_into(|_| ".define"),
                    id,
                    value: cfg.map(Box::new),
                },
            )
        },
    )(input)
}

/// Tries to parse tokens enclosed in braces, e.g. `{ ... }`
fn braces(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            multiline_ws(char('{')),
            located(opt(many0(statement))),
            ws(char('}')),
        )),
        move |(lparen, inner, rparen)| {
            let inner = inner.map_into(|vec| vec.unwrap_or_else(Vec::new));
            Located::new(
                location,
                Token::Braces {
                    lparen,
                    inner,
                    rparen,
                },
            )
        },
    )(input)
}

/// Tries to parse a segment definition
fn segment(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            ws(tag_no_case(".segment")),
            ws(identifier_name),
            opt(map(braces, Box::new)),
        )),
        move |(tag, id, inner)| {
            let id = id.flatten();
            Located::new(
                location.clone(),
                Token::Segment {
                    tag: tag.map_into(|_| ".segment"),
                    id: Box::new(id),
                    inner,
                },
            )
        },
    )(input)
}

/// Tries to parse an if/else statement
fn if_(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            ws(tag_no_case(".if")),
            expression,
            braces,
            opt(tuple((ws(tag_no_case("else")), braces))),
        )),
        move |(tag_if, value, if_, else_)| {
            let tag_if = tag_if.map_into(|_| ".if");
            let (tag_else, else_) = match else_ {
                Some((tag_else, else_)) => (Some(tag_else.map_into(|_| "else")), Some(else_)),
                None => (None, None),
            };
            Located::new(
                location,
                Token::If {
                    tag_if,
                    value,
                    if_: Box::new(if_),
                    tag_else,
                    else_: else_.map(Box::new),
                },
            )
        },
    )(input)
}

/// Tries to parse an align directive, of the form `.align 16`
fn align(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(
        tuple((ws(tag_no_case(".align")), ws(expression))),
        move |(tag, value)| {
            let tag = tag.map_into(|_| ".align");
            let value = value.flatten();
            Located::new(location, Token::Align { tag, value })
        },
    )(input)
}

/// Tries to parse all valid statement types
fn statement(input: LocatedSpan) -> IResult<Located<Token>> {
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
        end_of_line,
    ))(input)
}

/// Tries to parse any valid statement but will fallback to [error] in case of trouble
fn statement_or_error(input: LocatedSpan) -> IResult<Located<Token>> {
    alt((statement, error))(input)
}

/// Tries to parse a platform-independent end-of-line
fn end_of_line(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map_once(ws(tuple((opt(char('\r')), char('\n')))), move |triv| {
        let triv = triv.map(|_| EmptyDisplay);
        Located::new(location, Token::EolTrivia(triv))
    })(input)
}

/// Tries to eat the remaining characters
fn eof(input: LocatedSpan) -> IResult<Located<Token>> {
    map(ws(rest), move |rest| rest.map_into(|_| Token::Eof))(input)
}

/// Parses an entire file
fn source_file(input: LocatedSpan) -> IResult<Vec<Located<Token>>> {
    map(tuple((many1(statement_or_error), eof)), |(tokens, eof)| {
        let mut result = tokens;
        result.push(eof);
        result
    })(input)
}

/// Parses a number of any possible [NumberType]
fn number(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    let location = Location::from(&input);

    map(
        alt((
            tuple((
                map(ws(char('$')), |ty| ty.map_into(|_| NumberType::Hex)),
                ws(map(recognize(many1(hex_digit1)), |n| {
                    let location = Location::from(&n);
                    Located::new(
                        location,
                        i64::from_str_radix(n.fragment(), 16).ok().unwrap(),
                    )
                })),
            )),
            tuple((
                map(ws(char('%')), |ty| ty.map_into(|_| NumberType::Bin)),
                ws(map(recognize(many1(is_a("01"))), |n| {
                    let location = Location::from(&n);
                    Located::new(location, i64::from_str_radix(n.fragment(), 2).ok().unwrap())
                })),
            )),
            tuple((
                value(Located::new(location, NumberType::Dec)),
                ws(map(recognize(many1(is_a("0123456789"))), |n| {
                    let location = Location::from(&n);
                    Located::new(
                        location,
                        i64::from_str_radix(n.fragment(), 10).ok().unwrap(),
                    )
                })),
            )),
        )),
        |(ty, value)| value.map_into(move |value| ExpressionFactor::Number { ty, value }),
    )(input)
}

/// Deals with parentheses encountered in expressions
fn expression_parens(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    let location = Location::from(&input);

    map_once(
        tuple((ws(char('[')), ws(expression), ws(char(']')))),
        move |(lparen, inner, rparen)| {
            let inner = Box::new(inner.flatten());
            Located::new(
                location,
                ExpressionFactor::ExprParens {
                    lparen,
                    inner,
                    rparen,
                },
            )
        },
    )(input)
}

/// Parses the star character when used as a placeholder for the current program counter
fn current_pc(input: LocatedSpan) -> IResult<Located<ExpressionFactor>> {
    let location = Location::from(&input);

    map_once(ws(char('*')), move |star| {
        Located::new(location, ExpressionFactor::CurrentProgramCounter(star))
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
    let location = Location::from(&input);

    map_once(
        tuple((
            ws(identifier_name),
            ws(char('(')),
            opt(arg_list),
            ws(char(')')),
        )),
        move |(name, lparen, args, rparen)| {
            let name = Box::new(name.flatten());
            let args = args.unwrap_or_else(Vec::new);
            Located::new(
                location.clone(),
                ExpressionFactor::FunctionCall {
                    name,
                    lparen,
                    args,
                    rparen,
                },
            )
        },
    )(input)
}

/// Parses a factor used in an expression, such as a number or a function call
fn expression_factor(input: LocatedSpan) -> IResult<Located<Expression>> {
    let location = Location::from(&input);

    map_once(
        tuple((
            opt(ws(char('!'))),
            opt(ws(char('-'))),
            alt((
                number,
                fn_call,
                identifier_value,
                current_pc,
                expression_parens,
            )),
        )),
        move |(tag_not, tag_neg, factor)| {
            let mut flags = ExpressionFactorFlags::empty();
            if tag_not.is_some() {
                flags.set(ExpressionFactorFlags::NOT, true);
            }
            if tag_neg.is_some() {
                flags.set(ExpressionFactorFlags::NEG, true);
            }
            Located::new(
                location,
                Expression::Factor {
                    factor: Box::new(factor),
                    flags,
                    tag_not,
                    tag_neg,
                },
            )
        },
    )(input)
}

/// Folds back a list of expressions and operations into a single [Expression] token
fn fold_expressions<'a>(
    initial: Located<'a, Expression<'a>>,
    remainder: Vec<(Located<'a, BinaryOp>, Located<'a, Expression<'a>>)>,
) -> Located<'a, Expression<'a>> {
    remainder.into_iter().fold(initial, |acc, pair| {
        let (op, expr) = pair;

        Located::new(
            acc.location.clone(),
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
pub fn parse<'a>(filename: &'a Path, source: &'a str) -> MosResult<Vec<Located<'a, Token<'a>>>> {
    let state = State::new(filename);
    let errors = state.errors.clone();
    let input = LocatedSpan::new_extra(source, state);
    let (_, expr) = all_consuming(source_file)(input).expect("parser cannot fail");

    let errors = Rc::try_unwrap(errors).ok().unwrap().into_inner();
    if errors.is_empty() {
        Ok(expr)
    } else {
        Err(errors.into())
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
        check("lda #-foo", "LDA #-foo");
        check("lda #!foo", "LDA #!foo");
        check("lda  %11101", "LDA  %11101");
        check(
            "lda  %11101   +   [  $ff  * -12367 ] / foo",
            "LDA  %11101   +   [  $ff  * -12367 ] / foo",
        );
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
    fn parse_data() -> MosResult<()> {
        check(
            ".byte 123\n.word foo\n.dword 12345678\n.word 1 + 2,   3, 4 * 4",
            ".BYTE 123\n.WORD foo\n.DWORD 12345678\n.WORD 1 + 2,   3, 4 * 4",
        );
        Ok(())
    }

    #[test]
    fn parse_label() {
        check("   foo:   nop", "   foo:   NOP");
    }

    #[test]
    fn parse_fn_call() {
        let factor = invoke("func()", |span| fn_call(span));
        assert_eq!(factor.to_string(), "func()");

        let factor = invoke("func   (   a)", |span| fn_call(span));
        assert_eq!(factor.to_string(), "func   (   a)");

        let factor = invoke("func   (a   ,   b   )", |span| fn_call(span));
        assert_eq!(factor.to_string(), "func   (a   ,   b   )");
    }

    fn check(src: &str, expected: &str) {
        let expr = match parse(&Path::new("test.asm"), src) {
            Ok(expr) => expr,
            Err(e) => panic!("Errors: {:?}", e),
        };
        let actual = expr.into_iter().map(|e| format!("{}", e)).join("");
        assert_eq!(actual, expected.to_string());
    }

    fn invoke<'a, O: 'a, F: FnOnce(LocatedSpan<'a>) -> IResult<Located<'a, O>>>(
        src: &'a str,
        parser: F,
    ) -> O {
        let state = State::new(&Path::new("test.asm"));
        let input = LocatedSpan::new_extra(src, state);
        let result = parser(input);
        if result.is_err() {
            panic!(format!("{}", result.err().unwrap()));
        }
        result.ok().unwrap().1.data
    }
}
