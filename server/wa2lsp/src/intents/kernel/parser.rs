//! Recursive descent parser for WA2 language

use crate::intents::kernel::ast::*;
use crate::intents::kernel::lexer::{LogosLexer, Token};

#[derive(Debug)]
pub struct ParseError {
	pub message: String,
	pub span: Span,
}

pub struct Parser<'src> {
	lexer: LogosLexer<'src>,
	current: Option<Token>,
	span: Span,
}

impl<'src> Parser<'src> {
	pub fn new(mut lexer: LogosLexer<'src>) -> Self {
		let current = lexer.next().and_then(|r| r.ok());
		let span = lexer.span();
		Self {
			lexer,
			current,
			span,
		}
	}

	fn advance(&mut self) {
		self.current = self.lexer.next().and_then(|r| r.ok());
		self.span = self.lexer.span();
	}

	fn expect(&mut self, expected: Token) -> Result<(), ParseError> {
		if self.current.as_ref() == Some(&expected) {
			self.advance();
			Ok(())
		} else {
			Err(ParseError {
				message: format!("expected {:?}, got {:?}", expected, self.current),
				span: self.span.clone(),
			})
		}
	}

	fn expect_ident(&mut self) -> Result<String, ParseError> {
		match self.current.take() {
			Some(Token::Ident(name)) => {
				self.advance();
				Ok(name)
			}
			other => Err(ParseError {
				message: format!("expected identifier, got {:?}", other),
				span: self.span.clone(),
			}),
		}
	}

	fn expect_name(&mut self) -> Result<String, ParseError> {
		match &self.current {
			Some(Token::Ident(name)) => {
				let name = name.clone();
				self.advance();
				Ok(name)
			}
			// Allow keywords as names in certain contexts
			Some(Token::KwType) => {
				self.advance();
				Ok("type".to_string())
			}
			Some(Token::KwIn) => {
				self.advance();
				Ok("in".to_string())
			}
			Some(Token::KwNamespace) => {
				self.advance();
				Ok("namespace".to_string())
			}
			Some(Token::KwStruct) => {
				self.advance();
				Ok("struct".to_string())
			}
			Some(Token::KwEnum) => {
				self.advance();
				Ok("enum".to_string())
			}
			Some(Token::KwPredicate) => {
				self.advance();
				Ok("predicate".to_string())
			}
			Some(Token::KwInstance) => {
				self.advance();
				Ok("instance".to_string())
			}
			Some(Token::KwRule) => {
				self.advance();
				Ok("rule".to_string())
			}
			Some(Token::KwLet) => {
				self.advance();
				Ok("let".to_string())
			}
			other => Err(ParseError {
				message: format!("expected name, got {:?}", other),
				span: self.span.clone(),
			}),
		}
	}

	fn at_name(&self) -> bool {
		matches!(
			self.current,
			Some(Token::Ident(_))
				| Some(Token::KwType)
				| Some(Token::KwIn)
				| Some(Token::KwNamespace)
				| Some(Token::KwStruct)
				| Some(Token::KwEnum)
				| Some(Token::KwPredicate)
				| Some(Token::KwInstance)
				| Some(Token::KwRule)
				| Some(Token::KwLet)
		)
	}

	fn at(&self, token: &Token) -> bool {
		self.current.as_ref() == Some(token)
	}

	fn at_ident(&self) -> bool {
		matches!(self.current, Some(Token::Ident(_)))
	}
}

/// Parse a WA2 source into an AST
pub fn parse(lexer: LogosLexer) -> Result<Ast, ParseError> {
	let mut parser = Parser::new(lexer);
	let mut items = Vec::new();

	while parser.current.is_some() {
		items.push(parse_item(&mut parser)?);
	}

	Ok(Ast { items })
}

fn parse_item(p: &mut Parser) -> Result<Item, ParseError> {
	// Collect annotations first
	let annotations = parse_annotations(p)?;

	match &p.current {
		Some(Token::KwNamespace) => parse_namespace(p).map(Item::Namespace),
		Some(Token::KwStruct) => parse_struct(p, annotations).map(Item::Struct),
		Some(Token::KwEnum) => parse_enum(p, annotations).map(Item::Enum),
		Some(Token::KwType) => parse_type_decl(p, annotations).map(Item::Type),
		Some(Token::KwPredicate) => parse_predicate(p).map(Item::Predicate),
		Some(Token::KwInstance) => parse_instance(p).map(Item::Instance),
		Some(Token::KwRule) => parse_rule(p).map(Item::Rule),
		other => Err(ParseError {
			message: format!("expected item, got {:?}", other),
			span: p.span.clone(),
		}),
	}
}

fn parse_annotations(p: &mut Parser) -> Result<Vec<Annotation>, ParseError> {
	let mut annotations = Vec::new();

	while p.at(&Token::At) {
		let start = p.span.start;
		p.advance(); // @

		// Optional path: @#path or @(...)
		let path = if p.at(&Token::Hash) {
			p.advance();
			Some(parse_qualified_name(p)?)
		} else {
			None
		};

		p.expect(Token::LParen)?;

		let mut args = Vec::new();
		while !p.at(&Token::RParen) {
			let name = p.expect_ident()?;
			p.expect(Token::Eq)?;
			let value = parse_literal(p)?;
			args.push(AnnotationArg { name, value });

			if p.at(&Token::Comma) {
				p.advance();
			}
		}

		p.expect(Token::RParen)?;

		annotations.push(Annotation {
			path,
			args,
			span: start..p.span.end,
		});
	}

	Ok(annotations)
}

fn parse_namespace(p: &mut Parser) -> Result<Namespace, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwNamespace)?;
	let name = p.expect_ident()?;
	p.expect(Token::LBrace)?;

	let mut items = Vec::new();
	while !p.at(&Token::RBrace) {
		items.push(parse_item(p)?);
	}

	p.expect(Token::RBrace)?;

	Ok(Namespace {
		name,
		items,
		span: start..p.span.end,
	})
}

fn parse_struct(p: &mut Parser, annotations: Vec<Annotation>) -> Result<Struct, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwStruct)?;
	let name = p.expect_ident()?;
	p.expect(Token::LBrace)?;

	let mut fields = Vec::new();
	while !p.at(&Token::RBrace) {
		fields.push(parse_field(p)?);
	}

	p.expect(Token::RBrace)?;

	Ok(Struct {
		name,
		fields,
		annotations,
		span: start..p.span.end,
	})
}

fn parse_field(p: &mut Parser) -> Result<Field, ParseError> {
	let start = p.span.start;
	let name = p.expect_name()?;
	p.expect(Token::Colon)?;
	let ty = parse_type_ref(p)?;

	Ok(Field {
		name,
		ty,
		span: start..p.span.end,
	})
}

fn parse_type_ref(p: &mut Parser) -> Result<TypeRef, ParseError> {
	let start = p.span.start;
	let name = parse_qualified_name(p)?;

	let array = if p.at(&Token::LBracket) {
		p.advance();
		p.expect(Token::RBracket)?;
		true
	} else {
		false
	};

	let optional = if p.at(&Token::Question) {
		p.advance();
		true
	} else {
		false
	};

	Ok(TypeRef {
		name,
		array,
		optional,
		span: start..p.span.end,
	})
}

fn parse_enum(p: &mut Parser, annotations: Vec<Annotation>) -> Result<Enum, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwEnum)?;
	let name = p.expect_ident()?;
	p.expect(Token::LBrace)?;

	let mut variants = Vec::new();
	while !p.at(&Token::RBrace) {
		variants.push(p.expect_ident()?);
		if p.at(&Token::Comma) {
			p.advance();
		}
	}

	p.expect(Token::RBrace)?;

	Ok(Enum {
		name,
		variants,
		annotations,
		span: start..p.span.end,
	})
}

fn parse_type_decl(p: &mut Parser, annotations: Vec<Annotation>) -> Result<TypeDecl, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwType)?;
	let name = p.expect_ident()?;

	Ok(TypeDecl {
		name,
		annotations,
		span: start..p.span.end,
	})
}

fn parse_predicate(p: &mut Parser) -> Result<Predicate, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwPredicate)?;
	let name = p.expect_name()?;

	Ok(Predicate {
		name,
		span: start..p.span.end,
	})
}

fn parse_instance(p: &mut Parser) -> Result<Instance, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwInstance)?;
	let name = parse_qualified_name(p)?;
	p.expect(Token::Colon)?;
	let ty = parse_qualified_name(p)?;

	Ok(Instance {
		name,
		ty,
		span: start..p.span.end,
	})
}

fn parse_rule(p: &mut Parser) -> Result<Rule, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwRule)?;
	let name = p.expect_ident()?;
	p.expect(Token::LBrace)?;

	let mut body = Vec::new();
	while !p.at(&Token::RBrace) {
		body.push(parse_statement(p)?);
	}

	p.expect(Token::RBrace)?;

	Ok(Rule {
		name,
		body,
		span: start..p.span.end,
	})
}

fn parse_statement(p: &mut Parser) -> Result<Statement, ParseError> {
	match &p.current {
		Some(Token::KwLet) => parse_let_stmt(p).map(Statement::Let),
		Some(Token::Plus) => parse_add_stmt(p).map(Statement::Add),
		Some(Token::Star) => parse_iterate_stmt(p).map(Statement::Iterate),
		Some(Token::At) => parse_assert_stmt(p).map(Statement::Assert),
		other => Err(ParseError {
			message: format!("expected statement, got {:?}", other),
			span: p.span.clone(),
		}),
	}
}

fn parse_let_stmt(p: &mut Parser) -> Result<LetStmt, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwLet)?;
	let name = p.expect_ident()?;
	p.expect(Token::Eq)?;
	let value = parse_expr(p)?;

	Ok(LetStmt {
		name,
		value,
		span: start..p.span.end,
	})
}

fn parse_add_stmt(p: &mut Parser) -> Result<AddStmt, ParseError> {
	let start = p.span.start;
	p.expect(Token::Plus)?;
	p.expect(Token::LParen)?;
	let subject = parse_expr(p)?;
	p.expect(Token::Comma)?;
	let predicate = parse_qualified_name(p)?;
	p.expect(Token::Comma)?;
	let object = parse_expr(p)?;
	p.expect(Token::RParen)?;

	Ok(AddStmt {
		subject,
		predicate,
		object,
		span: start..p.span.end,
	})
}

fn parse_iterate_stmt(p: &mut Parser) -> Result<IterateStmt, ParseError> {
	let start = p.span.start;
	p.expect(Token::Star)?;
	p.expect(Token::LParen)?;
	let var = p.expect_ident()?;
	p.expect(Token::KwIn)?;
	let collection = parse_expr(p)?;
	p.expect(Token::Comma)?;
	p.expect(Token::LBrace)?;

	let mut body = Vec::new();
	while !p.at(&Token::RBrace) {
		body.push(parse_statement(p)?);
	}

	p.expect(Token::RBrace)?;
	p.expect(Token::RParen)?;

	Ok(IterateStmt {
		var,
		collection,
		body,
		span: start..p.span.end,
	})
}

fn parse_assert_stmt(p: &mut Parser) -> Result<AssertStmt, ParseError> {
	let start = p.span.start;
	p.expect(Token::At)?;
	p.expect(Token::Hash)?;

	// Expect "assert"
	let name = p.expect_ident()?;
	if name != "assert" {
		return Err(ParseError {
			message: format!("expected 'assert', got '{}'", name),
			span: p.span.clone(),
		});
	}

	p.expect(Token::LParen)?;
	let expr = parse_expr(p)?;
	p.expect(Token::RParen)?;

	Ok(AssertStmt {
		expr,
		span: start..p.span.end,
	})
}

fn parse_expr(p: &mut Parser) -> Result<Expr, ParseError> {
	match &p.current {
		Some(Token::Underscore) => {
			let span = p.span.clone();
			p.advance();
			Ok(Expr::Blank(span))
		}
		Some(Token::Question) => parse_query_expr(p),
		Some(Token::Plus) => {
			// +(s, p, o) as expression returns the subject
			parse_add_expr(p)
		}
		Some(Token::StringLiteral(s)) => {
			let s = s.clone();
			let span = p.span.clone();
			p.advance();
			Ok(Expr::String(s, span))
		}
		Some(Token::Ident(name)) if name == "empty" => {
			let start = p.span.start;
			p.advance();
			p.expect(Token::LParen)?;
			let inner = parse_expr(p)?;
			p.expect(Token::RParen)?;
			Ok(Expr::Empty(Box::new(inner), start..p.span.end))
		}
		Some(Token::Ident(_)) => {
			let qname = parse_qualified_name(p)?;
			// Check if it's actually a variable (no colon seen)
			if qname.namespace.is_none() {
				Ok(Expr::Var(qname.name, qname.span))
			} else {
				Ok(Expr::QName(qname))
			}
		}
		other => Err(ParseError {
			message: format!("expected expression, got {:?}", other),
			span: p.span.clone(),
		}),
	}
}

fn parse_query_expr(p: &mut Parser) -> Result<Expr, ParseError> {
	let start = p.span.start;
	p.expect(Token::Question)?;
	p.expect(Token::LParen)?;
	let path = parse_query_path(p)?;
	p.expect(Token::RParen)?;

	Ok(Expr::Query(QueryExpr {
		path,
		span: start..p.span.end,
	}))
}

fn parse_add_expr(p: &mut Parser) -> Result<Expr, ParseError> {
	let start = p.span.start;
	p.expect(Token::Plus)?;
	p.expect(Token::LParen)?;
	let subject = parse_expr(p)?;
	p.expect(Token::Comma)?;
	let predicate = parse_qualified_name(p)?;
	p.expect(Token::Comma)?;
	let object = parse_expr(p)?;
	p.expect(Token::RParen)?;

	Ok(Expr::Add(Box::new(AddExpr {
		subject,
		predicate,
		object,
		span: start..p.span.end,
	})))
}

fn parse_query_path(p: &mut Parser) -> Result<QueryPath, ParseError> {
	let start = p.span.start;
	let mut steps = Vec::new();

	while p.at(&Token::Slash) || p.at(&Token::DoubleSlash) {
		steps.push(parse_query_step(p)?);
	}

	Ok(QueryPath {
		steps,
		span: start..p.span.end,
	})
}

fn parse_query_step(p: &mut Parser) -> Result<QueryStep, ParseError> {
	let start = p.span.start;

	// Determine axis
	let axis = if p.at(&Token::DoubleSlash) {
		p.advance();
		Axis::Descendant
	} else if p.at(&Token::Slash) {
		p.advance();
		Axis::Child
	} else {
		return Err(ParseError {
			message: format!("expected / or //, got {:?}", p.current),
			span: p.span.clone(),
		});
	};

	// Node test (type name)
	let node_test = if p.at_name() {
		Some(parse_qualified_name(p)?)
	} else if p.at(&Token::Star) {
		p.advance();
		None
	} else {
		None
	};

	// Predicates
	let mut predicates = Vec::new();
	while p.at(&Token::LBracket) {
		p.advance();
		predicates.push(parse_query_predicate(p)?);
		p.expect(Token::RBracket)?;
	}

	Ok(QueryStep {
		axis,
		node_test,
		predicates,
		span: start..p.span.end,
	})
}

fn parse_query_predicate(p: &mut Parser) -> Result<QueryPredicate, ParseError> {
	let path = parse_query_path_inline(p)?;

	if p.at(&Token::Eq) {
		p.advance();
		let value = parse_literal(p)?;
		Ok(QueryPredicate::Eq(path, value))
	} else if p.at(&Token::KwIn) {
		p.advance();
		p.expect(Token::LParen)?;
		let mut values = Vec::new();
		while !p.at(&Token::RParen) {
			values.push(parse_literal(p)?);
			if p.at(&Token::Comma) {
				p.advance();
			}
		}
		p.expect(Token::RParen)?;
		Ok(QueryPredicate::In(path, values))
	} else {
		Ok(QueryPredicate::Exists(path))
	}
}

fn parse_query_path_inline(p: &mut Parser) -> Result<QueryPath, ParseError> {
	let start = p.span.start;
	let mut steps = Vec::new();

	// First step doesn't require leading slash
	if p.at_name() {
		let qname = parse_qualified_name(p)?;
		steps.push(QueryStep {
			axis: Axis::Child,
			node_test: Some(qname),
			predicates: Vec::new(),
			span: start..p.span.end,
		});
	} else if p.at(&Token::Star) {
		p.advance();
		steps.push(QueryStep {
			axis: Axis::Child,
			node_test: None,
			predicates: Vec::new(),
			span: start..p.span.end,
		});
	}

	// Additional steps
	while p.at(&Token::Slash) {
		p.advance();
		let axis = if p.at(&Token::Slash) {
			p.advance();
			Axis::Descendant
		} else {
			Axis::Child
		};

		let node_test = if p.at_name() {
			Some(parse_qualified_name(p)?)
		} else if p.at(&Token::Star) {
			p.advance();
			None
		} else {
			None
		};

		steps.push(QueryStep {
			axis,
			node_test,
			predicates: Vec::new(),
			span: start..p.span.end,
		});
	}

	Ok(QueryPath {
		steps,
		span: start..p.span.end,
	})
}

fn parse_qualified_name(p: &mut Parser) -> Result<QualifiedName, ParseError> {
	let start = p.span.start;
	let first = p.expect_name()?;

	if p.at(&Token::Colon) {
		p.advance();
		let second = p.expect_name()?;
		Ok(QualifiedName {
			namespace: Some(first),
			name: second,
			span: start..p.span.end,
		})
	} else {
		Ok(QualifiedName {
			namespace: None,
			name: first,
			span: start..p.span.end,
		})
	}
}

fn parse_literal(p: &mut Parser) -> Result<Literal, ParseError> {
	match &p.current {
		Some(Token::StringLiteral(s)) => {
			let s = s.clone();
			p.advance();
			Ok(Literal::String(s))
		}
		Some(Token::NumberLiteral(n)) => {
			let n = *n;
			p.advance();
			Ok(Literal::Number(n))
		}
		Some(Token::Ident(s)) if s == "true" => {
			p.advance();
			Ok(Literal::Bool(true))
		}
		Some(Token::Ident(s)) if s == "false" => {
			p.advance();
			Ok(Literal::Bool(false))
		}
		other => Err(ParseError {
			message: format!("expected literal, got {:?}", other),
			span: p.span.clone(),
		}),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::intents::kernel::lexer::Wa2Source;

	#[test]
	fn parse_simple_struct() {
		let src = r#"struct Workload { nodes: Node[] }"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Struct(s) => {
				assert_eq!(s.name, "Workload");
				assert_eq!(s.fields.len(), 1);
				assert_eq!(s.fields[0].name, "nodes");
				assert!(s.fields[0].ty.array);
			}
			_ => panic!("expected struct"),
		}
	}

	#[test]
	fn parse_enum() {
		let src = r#"enum Node { Store, Run, Move }"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Enum(e) => {
				assert_eq!(e.name, "Node");
				assert_eq!(e.variants, vec!["Store", "Run", "Move"]);
			}
			_ => panic!("expected enum"),
		}
	}

	#[test]
	fn parse_namespace() {
		let src = r#"namespace core { type Template predicate source }"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Namespace(ns) => {
				assert_eq!(ns.name, "core");
				assert_eq!(ns.items.len(), 2);
			}
			_ => panic!("expected namespace"),
		}
	}

	#[test]
	fn parse_instance() {
		let src = r#"instance core:workload: core:Workload"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Instance(i) => {
				assert_eq!(i.name.to_string(), "core:workload");
				assert_eq!(i.ty.to_string(), "core:Workload");
			}
			_ => panic!("expected instance"),
		}
	}

	#[test]
	fn parse_simple_rule() {
		let src = r#"
            rule derive_stores {
                let stores = ?(//cfn:Resource)
            }
        "#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Rule(r) => {
				assert_eq!(r.name, "derive_stores");
				assert_eq!(r.body.len(), 1);
			}
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_bootstrap() {
		let src = include_str!("../../../../../wa2/core/v0.1/bootstrap.wa2");
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer());

		match ast {
			Ok(ast) => {
				eprintln!("Parsed {} items", ast.items.len());
				for item in &ast.items {
					eprintln!("  {:?}", std::mem::discriminant(item));
				}
			}
			Err(e) => {
				// Find the line
				let lines: Vec<&str> = src.lines().collect();
				let mut offset = 0;
				for (i, line) in lines.iter().enumerate() {
					if offset + line.len() >= e.span.start {
						eprintln!("Parse error at line {}: {}", i + 1, line);
						break;
					}
					offset += line.len() + 1;
				}
				panic!("Parse error: {:?}", e);
			}
		}
	}

	#[test]
	fn parse_path_with_wildcard() {
		let src = r#"
rule test {
    let x = ?(//core:Store[core:source/aws:Rules/*/aws:Status = "Enabled"])
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer());

		match ast {
			Ok(_) => eprintln!("Parsed OK"),
			Err(e) => panic!("Parse error: {:?}", e),
		}
	}

	#[test]
	fn debug_lex_predicate() {
		let src = r#"?(//core:Store[core:source/aws:Rules/*/aws:Status = "Enabled"])"#;
		let source = Wa2Source::from_str(src);
		let lex = source.lexer();

		for (i, tok) in lex.enumerate() {
			eprintln!("{}: {:?}", i, tok);
		}
	}
}
