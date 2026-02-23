//! Lexer for WA2 language using logos

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::Path;

use logos::{FilterResult, Logos};

#[derive(Default, Clone)]
pub struct LexerState {
	paren_depth: usize,
}

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(extras = LexerState)]
pub enum Token {
	// Keywords
	#[token("namespace")]
	KwNamespace,
	#[token("struct")]
	KwStruct,
	#[token("enum")]
	KwEnum,
	#[token("type")]
	KwType,
	#[token("predicate")]
	KwPredicate,
	#[token("instance")]
	KwInstance,
	#[token("rule")]
	KwRule,
	#[token("let")]
	KwLet,
	#[token("in")]
	KwIn,

	// Symbols
	#[token("@")]
	At,
	#[token("#")]
	Hash,
	#[token("{")]
	LBrace,
	#[token("}")]
	RBrace,
	#[token("(", on_lparen)]
	LParen,
	#[token(")", on_rparen)]
	RParen,
	#[token("[")]
	LBracket,
	#[token("]")]
	RBracket,
	#[token(":")]
	Colon,
	#[token(",")]
	Comma,
	#[token(".")]
	Dot,
	#[token("=")]
	Eq,
	#[token("+")]
	Plus,
	#[token("?")]
	Question,
	#[token("*")]
	Star,
	#[token("_", priority = 3)]
	Underscore,
	#[token("/")]
	Slash,

	// Double slash - context dependent
	#[regex(r"//", on_double_slash)]
	DoubleSlash,

	// Identifiers and literals
	#[regex(r"[A-Za-z_][A-Za-z0-9_]*", |lex| lex.slice().to_owned(), priority = 2)]
	Ident(String),

	#[regex(r#""([^"\\]|\\.)*""#, |lex| {
        let s = lex.slice();
        s[1..s.len()-1].to_owned()
    })]
	StringLiteral(String),

	#[regex(r"[0-9]+", |lex| lex.slice().parse().ok())]
	NumberLiteral(i64),

	// Skip whitespace and block comments
	#[regex(r"[ \t\r\n]+", logos::skip)]
	_Skip,
}

fn on_lparen(lex: &mut logos::Lexer<Token>) -> FilterResult<(), ()> {
	lex.extras.paren_depth += 1;
	FilterResult::Emit(())
}

fn on_rparen(lex: &mut logos::Lexer<Token>) -> FilterResult<(), ()> {
	lex.extras.paren_depth = lex.extras.paren_depth.saturating_sub(1);
	FilterResult::Emit(())
}

fn on_double_slash(lex: &mut logos::Lexer<Token>) -> FilterResult<(), ()> {
	if lex.extras.paren_depth > 0 {
		// Inside parens: emit as DoubleSlash token (path term)
		FilterResult::Emit(())
	} else {
		// Outside parens: treat as comment, skip to end of line
		let remainder = lex.remainder();
		let newline_pos = remainder.find('\n').unwrap_or(remainder.len());
		lex.bump(newline_pos);
		FilterResult::Skip
	}
}

/// Type alias for convenience
pub type LogosLexer<'src> = logos::Lexer<'src, Token>;

/// Source wrapper that can own or borrow WA2 text
pub struct Wa2Source<'src> {
	src: Cow<'src, str>,
}

impl<'src> Wa2Source<'src> {
	pub fn from_str(src: &'src str) -> Self {
		Wa2Source {
			src: Cow::Borrowed(src),
		}
	}

	pub fn from_string(src: String) -> Self {
		Wa2Source {
			src: Cow::Owned(src),
		}
	}

	pub fn from_file(path: impl AsRef<Path>) -> io::Result<Self> {
		let contents = fs::read_to_string(path)?;
		Ok(Wa2Source {
			src: Cow::Owned(contents),
		})
	}

	pub fn lexer(&self) -> LogosLexer<'_> {
		Token::lexer(&self.src)
	}

	pub fn as_str(&self) -> &str {
		&self.src
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lex_struct() {
		let src = r#"struct Workload { nodes: Node[] }"#;
		let source = Wa2Source::from_str(src);
		let mut lex = source.lexer();

		assert_eq!(lex.next(), Some(Ok(Token::KwStruct)));
		assert_eq!(lex.next(), Some(Ok(Token::Ident("Workload".into()))));
		assert_eq!(lex.next(), Some(Ok(Token::LBrace)));
		assert_eq!(lex.next(), Some(Ok(Token::Ident("nodes".into()))));
		assert_eq!(lex.next(), Some(Ok(Token::Colon)));
		assert_eq!(lex.next(), Some(Ok(Token::Ident("Node".into()))));
		assert_eq!(lex.next(), Some(Ok(Token::LBracket)));
		assert_eq!(lex.next(), Some(Ok(Token::RBracket)));
		assert_eq!(lex.next(), Some(Ok(Token::RBrace)));
		assert_eq!(lex.next(), None);
	}

	#[test]
	fn lex_rule() {
		let src = r#"rule derive_stores { let x = ?(//cfn:Resource) }"#;
		let source = Wa2Source::from_str(src);
		let tokens: Vec<_> = source.lexer().collect();

		assert!(tokens.iter().any(|t| matches!(t, Ok(Token::KwRule))));
		assert!(tokens.iter().any(|t| matches!(t, Ok(Token::KwLet))));
		assert!(tokens.iter().any(|t| matches!(t, Ok(Token::Question))));
		assert!(tokens.iter().any(|t| matches!(t, Ok(Token::DoubleSlash))));
	}

	#[test]
	fn lex_qualified_name() {
		let src = r#"core:Store"#;
		let source = Wa2Source::from_str(src);
		let mut lex = source.lexer();

		assert_eq!(lex.next(), Some(Ok(Token::Ident("core".into()))));
		assert_eq!(lex.next(), Some(Ok(Token::Colon)));
		assert_eq!(lex.next(), Some(Ok(Token::Ident("Store".into()))));
	}

	#[test]
	fn lex_annotation() {
		let src = r#"@(description = "test")"#;
		let source = Wa2Source::from_str(src);
		let mut lex = source.lexer();

		assert_eq!(lex.next(), Some(Ok(Token::At)));
		assert_eq!(lex.next(), Some(Ok(Token::LParen)));
		assert_eq!(lex.next(), Some(Ok(Token::Ident("description".into()))));
		assert_eq!(lex.next(), Some(Ok(Token::Eq)));
		assert_eq!(lex.next(), Some(Ok(Token::StringLiteral("test".into()))));
		assert_eq!(lex.next(), Some(Ok(Token::RParen)));
	}

	#[test]
	fn lex_comment_outside_parens() {
		let src = r#"// this is a comment
struct Foo {}"#;
		let source = Wa2Source::from_str(src);
		let mut lex = source.lexer();

		// Comment should be skipped, first token is struct
		assert_eq!(lex.next(), Some(Ok(Token::KwStruct)));
		assert_eq!(lex.next(), Some(Ok(Token::Ident("Foo".into()))));
	}

	#[test]
	fn lex_double_slash_inside_parens() {
		let src = r#"?(//cfn:Resource)"#;
		let source = Wa2Source::from_str(src);
		let tokens: Vec<_> = source.lexer().map(|r| r.ok()).collect();

		assert!(tokens.contains(&Some(Token::Question)));
		assert!(tokens.contains(&Some(Token::LParen)));
		assert!(tokens.contains(&Some(Token::DoubleSlash)));
		assert!(tokens.contains(&Some(Token::Ident("cfn".into()))));
		assert!(tokens.contains(&Some(Token::Colon)));
		assert!(tokens.contains(&Some(Token::Ident("Resource".into()))));
		assert!(tokens.contains(&Some(Token::RParen)));
	}

	#[test]
	fn lex_mixed_comments_and_paths() {
		let src = r#"
// this is a comment
rule test {
    let x = ?(//core:Store)  // another comment
}
"#;
		let source = Wa2Source::from_str(src);
		let tokens: Vec<_> = source.lexer().filter_map(|r| r.ok()).collect();

		// Should have rule, test, {, let, x, =, ?, (, //, core, :, Store, ), }
		assert!(tokens.contains(&Token::KwRule));
		assert!(tokens.contains(&Token::KwLet));
		assert!(tokens.contains(&Token::DoubleSlash));
		assert!(tokens.contains(&Token::Ident("core".into())));
		assert!(tokens.contains(&Token::Ident("Store".into())));
	}
}
