use std::cell::RefCell;

use crate::parser::mnemonic::mnemonic;
use nom::bytes::complete::{is_not, tag, tag_no_case, take_till1, take_until};
use nom::character::complete::{
    alpha1, alphanumeric1, anychar, char, digit1, hex_digit1, newline, space1,
};
use nom::combinator::{all_consuming, map, map_opt, not, opt, recognize, rest};
use nom::{branch::alt, character::complete::multispace0};

use nom::multi::many0;
use nom::sequence::{delimited, pair, preceded, terminated, tuple};

mod ast;
mod mnemonic;

use crate::errors::AsmError;
pub use ast::*;
pub use mnemonic::*;

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
                let err = AsmError::Parser {
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

fn ws<'a, F>(inner: F) -> impl FnMut(LocatedSpan<'a>) -> IResult<Token>
where
    F: FnMut(LocatedSpan<'a>) -> IResult<Token>,
{
    map(
        tuple((whitespace_or_comment(), inner, whitespace_or_comment())),
        |(l, i, r)| {
            if l.is_empty() && r.is_empty() {
                i
            } else {
                Token::Ws((l, Box::new(i), r))
            }
        },
    )
}

fn identifier(input: LocatedSpan) -> IResult<Token> {
    let id = recognize(pair(
        alt((alpha1, tag("_"))),
        many0(alt((alphanumeric1, tag("_")))),
    ));

    map(id, |span: LocatedSpan| {
        Token::Identifier(Identifier(span.fragment().to_string()))
    })(input)
}

fn register_suffix<'a>(
    input: LocatedSpan<'a>,
    reg: &'a str,
    map_to: Register,
) -> IResult<'a, Token> {
    preceded(
        char(','),
        ws(map(tag_no_case(reg), |_| Token::RegisterSuffix(map_to))),
    )(input)
}

fn register_x_suffix(input: LocatedSpan) -> IResult<Token> {
    register_suffix(input, "x", Register::X)
}

fn register_y_suffix(input: LocatedSpan) -> IResult<Token> {
    register_suffix(input, "y", Register::Y)
}

fn operand(input: LocatedSpan) -> IResult<Token> {
    let am_imm = map(preceded(char('#'), expression), |expr| {
        Token::Operand(Operand {
            expr: Box::new(expr),
            addressing_mode: AddressingMode::Immediate,
            suffix: None,
        })
    });

    let optional_suffix = || opt(alt((register_x_suffix, register_y_suffix)));

    let am_abs = map(tuple((expression, optional_suffix())), |(expr, suffix)| {
        Token::Operand(Operand {
            expr: Box::new(expr),
            addressing_mode: AddressingMode::AbsoluteOrZP,
            suffix: suffix.map(Box::new),
        })
    });

    let am_ind = map(
        tuple((
            delimited(char('('), expression, char(')')),
            optional_suffix(),
        )),
        |(expr, suffix)| {
            Token::Operand(Operand {
                expr: Box::new(expr),
                addressing_mode: AddressingMode::OuterIndirect,
                suffix: suffix.map(Box::new),
            })
        },
    );

    let am_outer_ind = map(
        delimited(char('('), tuple((expression, optional_suffix())), char(')')),
        |(expr, suffix)| {
            Token::Operand(Operand {
                expr: Box::new(expr),
                addressing_mode: AddressingMode::Indirect,
                suffix: suffix.map(Box::new),
            })
        },
    );

    alt((am_imm, am_abs, am_ind, am_outer_ind))(input)
}

fn instruction(input: LocatedSpan) -> IResult<Token> {
    let location = Location::from(&input);

    let instruction = tuple((mnemonic, opt(ws(operand))));

    map(instruction, move |(mnemonic, operand)| {
        let instruction = Instruction {
            location,
            mnemonic,
            operand: operand.map(Box::new),
        };
        Token::Instruction(instruction)
    })(input)
}

fn error(input: LocatedSpan) -> IResult<Token> {
    map(
        take_till1(|c| c == ')' || c == '\n' || c == '\r'),
        |span: LocatedSpan| {
            let err = AsmError::Parser {
                location: Location::from(&span),
                message: format!("unexpected `{}`", span.fragment()),
            };
            span.extra.report_error(err);
            Token::Error
        },
    )(input)
}

fn label(input: LocatedSpan) -> IResult<Token> {
    map(
        tuple((identifier, expect(char(':'), "labels should end with ':'"))),
        |(id, _)| match id {
            Token::Identifier(id) => Token::Label(id),
            _ => panic!(),
        },
    )(input)
}

fn data(input: LocatedSpan) -> IResult<Token> {
    map(
        tuple((
            alt((
                map(tag_no_case(".byte"), |_| 1),
                map(tag_no_case(".word"), |_| 2),
                map(tag_no_case(".dword"), |_| 4),
            )),
            expect(expression, "expected expression"),
        )),
        |(sz, exp)| Token::Data(exp.map(Box::new), sz),
    )(input)
}

fn statement(input: LocatedSpan) -> IResult<Token> {
    alt((instruction, label, data, error))(input)
}

fn source_file(input: LocatedSpan) -> IResult<Vec<Token>> {
    terminated(
        many0(map(pair(ws(statement), many0(newline)), |(expr, _)| expr)),
        preceded(expect(not(anychar), "expected EOF"), rest),
    )(input)
}

fn number(input: LocatedSpan) -> IResult<Token> {
    map(
        alt((
            preceded(
                char('$'),
                map_opt(expect(hex_digit1, "expected hexadecimal value"), |input| {
                    input.map(|i| usize::from_str_radix(i.fragment(), 16).ok())
                }),
            ),
            map_opt(digit1, |input: LocatedSpan| {
                Some(usize::from_str_radix(input.fragment(), 10).ok())
            }),
        )),
        |val| Token::Number(val.unwrap()),
    )(input)
}

fn expression_parens(input: LocatedSpan) -> IResult<Token> {
    delimited(
        multispace0,
        delimited(
            tag("["),
            map(expression, |e| Token::ExprParens(Box::new(e))),
            tag("]"),
        ),
        multispace0,
    )(input)
}

fn expression_factor(input: LocatedSpan) -> IResult<Token> {
    ws(alt((number, identifier, expression_parens)))(input)
}

enum Operation {
    Add,
    Sub,
    Mul,
    Div,
}

fn fold_expressions(initial: Token, remainder: Vec<(Operation, Token)>) -> Token {
    remainder.into_iter().fold(initial, |acc, pair| {
        let (oper, expr) = pair;

        match oper {
            Operation::Add => Token::BinaryAdd(Box::new(acc), Box::new(expr)),
            Operation::Sub => Token::BinarySub(Box::new(acc), Box::new(expr)),
            Operation::Mul => Token::BinaryMul(Box::new(acc), Box::new(expr)),
            Operation::Div => Token::BinaryDiv(Box::new(acc), Box::new(expr)),
        }
    })
}

fn expression_term(input: LocatedSpan) -> IResult<Token> {
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

pub fn expression(input: LocatedSpan) -> IResult<Token> {
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

pub fn parse(source: &str) -> (Vec<Token>, Vec<AsmError>) {
    let errors = RefCell::new(Vec::new());
    let input = LocatedSpan::new_extra(source, State { errors: &errors });
    let (_, expr) = all_consuming(source_file)(input).expect("parser cannot fail");
    (expr, errors.into_inner())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_expression() {
        check(
            "lda  1   +   [  $ff  * 12367 ] / foo  ",
            "LDA 1 + [255 * 12367] / foo",
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
    fn parse_data() {
        let expr = parse(".byte 123\n.word foo\n.dword 12345678\n.word 1 + 2");
        let mut e = expr.0.iter();
        assert_eq!(format!("{}", e.next().unwrap()), ".byte 123");
        assert_eq!(format!("{}", e.next().unwrap()), ".word foo");
        assert_eq!(format!("{}", e.next().unwrap()), ".dword 12345678");
        assert_eq!(format!("{}", e.next().unwrap()), ".word 1 + 2");
    }

    fn check(src: &str, expected: &str) {
        let expr = dbg!(parse(src));
        let e = expr.0.get(0).unwrap();
        assert_eq!(format!("{}", e), format!("\t\t{}", expected));
    }
}
