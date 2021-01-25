use std::cell::RefCell;
use std::rc::Rc;

use nom::bytes::complete::{is_not, tag, tag_no_case, take_till1, take_until};
use nom::character::complete::{alpha1, alphanumeric1, anychar, char, digit1, hex_digit1, space1};
use nom::combinator::{all_consuming, map, map_opt, not, opt, recognize, rest};
use nom::multi::many0;
use nom::sequence::{delimited, pair, preceded, terminated, tuple};
use nom::{branch::alt, character::complete::multispace0};

pub use ast::*;
pub use mnemonic::*;

use crate::core::parser::mnemonic::mnemonic;
use crate::errors::{MosError, MosResult};

mod ast;
mod mnemonic;

#[derive(thiserror::Error, Debug, PartialEq)]
pub enum ParseError<'a> {
    #[error("expected something, but got instead")]
    ExpectedError {
        location: Location<'a>,
        message: String,
    },
    #[error("unexpected")]
    UnexpectedError {
        location: Location<'a>,
        message: String,
    },
}

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

fn cpp_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    recognize(pair(tag("//"), is_not("\n\r")))(input)
}

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

fn c_comment(input: LocatedSpan) -> IResult<LocatedSpan> {
    recognize(tuple((
        tag("/*"),
        expect(
            tuple((alt((inside_c_comment, take_until("*/"))), tag("*/"))),
            "missing closing */",
        ),
    )))(input)
}

fn whitespace_or_comment<'a>() -> impl FnMut(LocatedSpan<'a>) -> IResult<Vec<Comment>> {
    map(
        many0(alt((
            map(space1, |_| None),
            map(c_comment, |span| {
                Some(Comment::CStyle(span.fragment().to_owned().into()))
            }),
            map(cpp_comment, |span| {
                Some(Comment::CppStyle(span.fragment().to_owned().into()))
            }),
        ))),
        |comments| comments.into_iter().flatten().collect::<Vec<_>>(),
    )
}

fn ws<'a, T: CanWrapWhitespace<'a>, F>(
    inner: F,
) -> impl FnMut(LocatedSpan<'a>) -> IResult<Located<T>>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<Located<T>>,
{
    map(
        tuple((whitespace_or_comment(), inner, whitespace_or_comment())),
        |(l, i, r)| {
            if l.is_empty() && r.is_empty() {
                i
            } else {
                Located::from(i.location.clone(), T::wrap_inner(l, Box::new(i), r))
            }
        },
    )
}

fn identifier_name(input: LocatedSpan) -> IResult<LocatedSpan> {
    recognize(pair(
        alt((alpha1, tag("_"))),
        many0(alt((alphanumeric1, tag("_")))),
    ))(input)
}

fn identifier(input: LocatedSpan) -> IResult<Located<Expression>> {
    let location = Location::from(&input);

    let id = tuple((
        opt(map(alt((char('<'), char('>'))), move |m| match m {
            '<' => Some(AddressModifier::LowByte),
            '>' => Some(AddressModifier::HighByte),
            _ => panic!(),
        })),
        identifier_name,
    ));

    map(id, move |(modifier, span)| {
        Located::from(
            location.clone(),
            Expression::Identifier(Identifier(span.fragment()), modifier.flatten()),
        )
    })(input)
}

fn register_suffix<'a>(
    input: LocatedSpan<'a>,
    reg: &'a str,
    map_to: Register,
) -> IResult<'a, Located<'a, Token<'a>>> {
    let location = Location::from(&input);

    preceded(
        char(','),
        ws(map(tag_no_case(reg), move |_| {
            Located::from(location.clone(), Token::RegisterSuffix(map_to))
        })),
    )(input)
}

fn register_x_suffix(input: LocatedSpan) -> IResult<Located<Token>> {
    register_suffix(input, "x", Register::X)
}

fn register_y_suffix(input: LocatedSpan) -> IResult<Located<Token>> {
    register_suffix(input, "y", Register::Y)
}

fn operand(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    let loc_imm = location.clone();
    let am_imm = map(preceded(char('#'), expression), move |expr| {
        Located::from(
            loc_imm.clone(),
            Token::Operand(Operand {
                expr: Box::new(expr),
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
            Located::from(
                loc_abs.clone(),
                Token::Operand(Operand {
                    expr: Box::new(expr),
                    addressing_mode: AddressingMode::AbsoluteOrZP,
                    suffix: suffix.map(Box::new),
                }),
            )
        },
    );

    let loc_ind = location.clone();
    let am_ind = map(
        tuple((
            delimited(char('('), expression, char(')')),
            optional_suffix(),
        )),
        move |(expr, suffix)| {
            Located::from(
                loc_ind.clone(),
                Token::Operand(Operand {
                    expr: Box::new(expr),
                    addressing_mode: AddressingMode::OuterIndirect,
                    suffix: suffix.map(Box::new),
                }),
            )
        },
    );

    let loc_outer_ind = location;
    let am_outer_ind = map(
        delimited(char('('), tuple((expression, optional_suffix())), char(')')),
        move |(expr, suffix)| {
            Located::from(
                loc_outer_ind.clone(),
                Token::Operand(Operand {
                    expr: Box::new(expr),
                    addressing_mode: AddressingMode::Indirect,
                    suffix: suffix.map(Box::new),
                }),
            )
        },
    );

    alt((am_imm, am_abs, am_ind, am_outer_ind))(input)
}

fn instruction(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    let instruction = tuple((mnemonic, opt(ws(operand))));

    map(instruction, move |(mnemonic, operand)| {
        let instruction = Instruction {
            mnemonic,
            operand: operand.map(Box::new),
        };
        Located::from(location.clone(), Token::Instruction(instruction))
    })(input)
}

fn error(input: LocatedSpan) -> IResult<Located<Token>> {
    map(
        take_till1(|c| c == ')' || c == '\n' || c == '\r'),
        |span: LocatedSpan| {
            let err = ParseError::UnexpectedError {
                location: Location::from(&span),
                message: format!("unexpected '{}'", span.fragment()),
            };
            span.extra.report_error(err);
            Located::from(Location::from(&span), Token::Error)
        },
    )(input)
}

fn label(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map(
        tuple((
            identifier_name,
            expect(char(':'), "labels should end with ':'"),
        )),
        move |(id, _)| {
            let id = Identifier(id.fragment());
            Located::from(location.clone(), Token::Label(id))
        },
    )(input)
}

fn data(input: LocatedSpan) -> IResult<Located<Token>> {
    let location = Location::from(&input);

    map(
        tuple((
            alt((
                map(tag_no_case(".byte"), |_| 1),
                map(tag_no_case(".word"), |_| 2),
                map(tag_no_case(".dword"), |_| 4),
            )),
            expect(
                many0(alt((terminated(expression, char(',')), expression))),
                "expected expression",
            ),
        )),
        move |(sz, exp)| Located::from(location.clone(), Token::Data(exp.unwrap_or_default(), sz)),
    )(input)
}

fn statement(input: LocatedSpan) -> IResult<Located<Token>> {
    alt((instruction, label, data, error))(input)
}

fn source_file(input: LocatedSpan) -> IResult<Vec<Located<Token>>> {
    terminated(
        many0(map(
            pair(ws(statement), many0(tuple((opt(char('\r')), char('\n'))))),
            |(expr, _)| expr,
        )),
        preceded(expect(not(anychar), "expected EOF"), rest),
    )(input)
}

fn number(input: LocatedSpan) -> IResult<Located<Expression>> {
    let location = Location::from(&input);

    let hex_location = location.clone();
    let dec_location = location;
    alt((
        preceded(
            char('$'),
            map_opt(
                expect(hex_digit1, "expected hexadecimal value"),
                move |input| {
                    let res = input.map(|i| usize::from_str_radix(i.fragment(), 16).ok());
                    res.map(|val| {
                        Located::from(
                            hex_location.clone(),
                            Expression::Number(val.unwrap(), NumberType::Hex),
                        )
                    })
                },
            ),
        ),
        map_opt(digit1, move |input: LocatedSpan| {
            let res = Some(usize::from_str_radix(input.fragment(), 10).ok());
            res.map(|val| {
                Located::from(
                    dec_location.clone(),
                    Expression::Number(val.unwrap(), NumberType::Dec),
                )
            })
        }),
    ))(input)
}

fn expression_parens(input: LocatedSpan) -> IResult<Located<Expression>> {
    let location = Location::from(&input);

    delimited(
        multispace0,
        delimited(
            tag("["),
            map(expression, move |e| {
                Located::from(location.clone(), Expression::ExprParens(Box::new(e)))
            }),
            tag("]"),
        ),
        multispace0,
    )(input)
}

fn current_pc(input: LocatedSpan) -> IResult<Located<Expression>> {
    let location = Location::from(&input);

    ws(map(char('*'), move |_| {
        Located::from(location.clone(), Expression::CurrentProgramCounter)
    }))(input)
}

fn expression_factor(input: LocatedSpan) -> IResult<Located<Expression>> {
    ws(alt((number, identifier, current_pc, expression_parens)))(input)
}

enum Operation {
    Add,
    Sub,
    Mul,
    Div,
}

fn fold_expressions<'a>(
    initial: Located<'a, Expression<'a>>,
    remainder: Vec<(Operation, Located<'a, Expression<'a>>)>,
) -> Located<'a, Expression<'a>> {
    remainder.into_iter().fold(initial, |acc, pair| {
        let (oper, expr) = pair;

        match oper {
            Operation::Add => Located::from(
                acc.location.clone(),
                Expression::BinaryAdd(Box::new(acc), Box::new(expr)),
            ),
            Operation::Sub => Located::from(
                acc.location.clone(),
                Expression::BinarySub(Box::new(acc), Box::new(expr)),
            ),
            Operation::Mul => Located::from(
                acc.location.clone(),
                Expression::BinaryMul(Box::new(acc), Box::new(expr)),
            ),
            Operation::Div => Located::from(
                acc.location.clone(),
                Expression::BinaryDiv(Box::new(acc), Box::new(expr)),
            ),
        }
    })
}

fn expression_term(input: LocatedSpan) -> IResult<Located<Expression>> {
    let (input, initial) = expression_factor(input)?;
    let (input, remainder) = many0(alt((
        |input| {
            let (input, mul) = preceded(tag("*"), expression_factor)(input)?;
            Ok((input, (Operation::Mul, mul)))
        },
        |input| {
            let (input, div) = preceded(tag("/"), expression_factor)(input)?;
            Ok((input, (Operation::Div, div)))
        },
    )))(input)?;

    Ok((input, fold_expressions(initial, remainder)))
}

pub fn expression(input: LocatedSpan) -> IResult<Located<Expression>> {
    let (input, initial) = expression_term(input)?;
    let (input, remainder) = many0(alt((
        |input| {
            let (input, add) = preceded(tag("+"), expression_term)(input)?;
            Ok((input, (Operation::Add, add)))
        },
        |input| {
            let (input, sub) = preceded(tag("-"), expression_term)(input)?;
            Ok((input, (Operation::Sub, sub)))
        },
    )))(input)?;

    Ok((input, fold_expressions(initial, remainder)))
}

pub fn parse<'a>(filename: &'a str, source: &'a str) -> MosResult<Vec<Located<'a, Token<'a>>>> {
    let errors = Rc::new(RefCell::new(Vec::new()));
    let input = LocatedSpan::new_extra(
        source,
        State {
            filename,
            errors: errors.clone(),
        },
    );
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
    fn parse_expression() {
        check(
            "lda  1   +   [  $ff  * 12367 ] / foo  ",
            "LDA 1 + [$ff * 12367] / foo",
        );
    }

    #[test]
    fn parse_addressing_modes() {
        check("lda #123", "LDA #123");
        check("lda 12345", "LDA 12345");
        check("lda 12345, x", "LDA 12345, x");
        check("lda 12345, y", "LDA 12345, y");
        check("lda (123), x", "LDA (123), x");
        check("lda (123, x)", "LDA (123, x)");
    }

    #[test]
    fn parse_current_pc() {
        check("lda *", "LDA *");
        check("lda * - 3", "LDA * - 3");
    }

    #[test]
    fn parse_address_modifiers() {
        check("lda #<foo", "LDA #<foo");
        check("lda #>foo", "LDA #>foo");
        check_err("lda #!foo", "test.asm:1:5: error: unexpected '#!foo'");
    }

    #[test]
    fn parse_data() -> MosResult<()> {
        let expr = parse(
            "test.asm",
            ".byte 123\n.word foo\n.dword 12345678\n.word 1 + 2, 3, 4 * 4",
        )?;
        let mut e = expr.iter();
        assert_eq!(format!("{}", e.next().unwrap().data), ".byte 123");
        assert_eq!(format!("{}", e.next().unwrap().data), ".word foo");
        assert_eq!(format!("{}", e.next().unwrap().data), ".dword 12345678");
        assert_eq!(
            format!("{}", e.next().unwrap().data),
            ".word 1 + 2, 3, 4 * 4"
        );
        Ok(())
    }

    fn check(src: &str, expected: &str) {
        let expr = parse("test.asm", src).ok().unwrap();
        let e = expr.get(0).unwrap();
        assert_eq!(format!("{}", e.data), expected.to_string());
    }

    fn check_err(src: &str, expected: &str) {
        let err = parse("test.asm", src).err().unwrap();
        assert_eq!(err.to_string(), expected.to_string());
    }
}
