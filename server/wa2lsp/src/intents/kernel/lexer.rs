//! Lexer for WA2 language using logos

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::Path;

use logos::Logos;

#[derive(Logos, Debug, Clone, PartialEq)]
pub enum Token {
	// Keywords
	#[token("namespace")]
	KwNamespace,
	#[token("struct")]
	KwStruct,
	#[token("enum")]
	KwEnum,
	#[token("use")]
	KwUse,
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
	#[token("true")]
	KwTrue,
	#[token("false")]
	KwFalse,
	#[token("policy")]
	KwPolicy,
	#[token("must")]
	KwMust,
	#[token("should")]
	KwShould,
	#[token("may")]
	KwMay,
	#[token("query")]
	KwQuery,
	#[token("add")]
	KwAdd,
	#[token("for")]
	KwFor,
	#[token("if")]
	KwIf,
	#[token("else")]
	KwElse,
	#[token("match")]
	KwMatch,
	#[token("as")]
	KwAs,
	#[token("derive")]
	KwDerive,
	#[token("profile")]
	KwProfile,

	// Symbols
	#[token("@")]
	At,
	#[token("#")]
	Hash,
	#[token("{")]
	LBrace,
	#[token("}")]
	RBrace,
	#[token("(")]
	LParen,
	#[token(")")]
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
	#[token("=>")]
	FatArrow,
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

	// Skip whitespace and comments
	#[regex(r"[ \t\r\n]+", logos::skip)]
	#[regex(r"//[^\n]*", logos::skip)]
	_Skip,
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
	fn lex_rule_new_syntax() {
		let src = r#"rule test { let x = query(cfn:Resource) }"#;
		let source = Wa2Source::from_str(src);
		let tokens: Vec<_> = source.lexer().filter_map(|r| r.ok()).collect();

		assert!(tokens.contains(&Token::KwRule));
		assert!(tokens.contains(&Token::KwLet));
		assert!(tokens.contains(&Token::KwQuery));
	}

	#[test]
	fn lex_for_if_match() {
		let src = r#"for x in items { if cond { match v as(T, should) { A => true } } }"#;
		let source = Wa2Source::from_str(src);
		let tokens: Vec<_> = source.lexer().filter_map(|r| r.ok()).collect();

		assert!(tokens.contains(&Token::KwFor));
		assert!(tokens.contains(&Token::KwIf));
		assert!(tokens.contains(&Token::KwMatch));
		assert!(tokens.contains(&Token::KwAs));
		assert!(tokens.contains(&Token::KwShould));
		assert!(tokens.contains(&Token::FatArrow));
	}

	#[test]
	fn lex_fat_arrow() {
		let src = r#"A => true"#;
		let source = Wa2Source::from_str(src);
		let mut lex = source.lexer();

		assert_eq!(lex.next(), Some(Ok(Token::Ident("A".into()))));
		assert_eq!(lex.next(), Some(Ok(Token::FatArrow)));
		assert_eq!(lex.next(), Some(Ok(Token::KwTrue)));
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
	fn lex_comment() {
		let src = r#"// this is a comment
struct Foo {}"#;
		let source = Wa2Source::from_str(src);
		let mut lex = source.lexer();

		assert_eq!(lex.next(), Some(Ok(Token::KwStruct)));
		assert_eq!(lex.next(), Some(Ok(Token::Ident("Foo".into()))));
	}

	#[test]
	fn lex_path_with_slash() {
		let src = r#"query(store/core:source)"#;
		let source = Wa2Source::from_str(src);
		let tokens: Vec<_> = source.lexer().filter_map(|r| r.ok()).collect();

		assert!(tokens.contains(&Token::KwQuery));
		assert!(tokens.contains(&Token::LParen));
		assert!(tokens.contains(&Token::Ident("store".into())));
		assert!(tokens.contains(&Token::Slash));
		assert!(tokens.contains(&Token::Ident("core".into())));
		assert!(tokens.contains(&Token::Colon));
		assert!(tokens.contains(&Token::Ident("source".into())));
		assert!(tokens.contains(&Token::RParen));
	}
}
