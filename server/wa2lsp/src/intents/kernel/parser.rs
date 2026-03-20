//! Recursive descent parser for WA2 language

use std::collections::HashSet;

use crate::intents::kernel::ast::*;
use crate::intents::kernel::lexer::{LogosLexer, Token, Wa2Source};

#[derive(Debug)]
pub struct ParseError {
	pub message: String,
	pub span: Span,
}

/// Resolver function type - returns true if the name is a namespace
pub type Resolver<'a> = Box<dyn Fn(&str) -> bool + 'a>;

/// Default resolver that never resolves (for simple parsing)
pub fn no_resolver() -> Resolver<'static> {
	Box::new(|_| false)
}

pub struct Parser<'src> {
	lexer: LogosLexer<'src>,
	current: Option<Token>,
	span: Span,
	resolver: Resolver<'src>,
	declared_namespaces: HashSet<String>,
	namespace_stack: Vec<String>,
}

impl<'src> Parser<'src> {
	pub fn new(mut lexer: LogosLexer<'src>, resolver: Resolver<'src>) -> Self {
		let current = lexer.next().and_then(|r| r.ok());
		let span = lexer.span();
		Self {
			lexer,
			current,
			span,
			resolver,
			declared_namespaces: HashSet::new(),
			namespace_stack: Vec::new(),
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

	/// Check if a name is a known namespace (from resolver or declared in this file)
	fn is_namespace(&self, name: &str) -> bool {
		(self.resolver)(name) || self.declared_namespaces.contains(name)
	}

	/// Get the current namespace path (for nested namespace declarations)
	fn current_namespace_path(&self) -> String {
		self.namespace_stack.join(":")
	}

	/// Compute full namespace path for a new namespace being declared
	fn full_namespace_path(&self, local_name: &str) -> String {
		if self.namespace_stack.is_empty() {
			local_name.to_string()
		} else {
			format!("{}:{}", self.current_namespace_path(), local_name)
		}
	}
}

/// Parse a WA2 source into an AST (without resolver - for tests)
pub fn parse(lexer: LogosLexer) -> Result<Ast, ParseError> {
	parse_with_resolver(lexer, no_resolver())
}

/// Parse from source string with model resolver
pub fn parse_with_model<'src>(
	source: &'src Wa2Source<'src>,
	resolver: Resolver<'src>,
) -> Result<Ast, ParseError> {
	parse_with_resolver(source.lexer(), resolver)
}

/// Parse a WA2 source into an AST with a resolver for namespaces
pub fn parse_with_resolver<'a>(
	lexer: LogosLexer<'a>,
	resolver: Resolver<'a>,
) -> Result<Ast, ParseError> {
	let mut parser = Parser::new(lexer, resolver);
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
		Some(Token::KwUse) => parse_use(p).map(Item::Use),
		Some(Token::KwStruct) => parse_struct(p, annotations).map(Item::Struct),
		Some(Token::KwEnum) => parse_enum(p, annotations).map(Item::Enum),
		Some(Token::KwType) => parse_type_decl(p, annotations).map(Item::Type),
		Some(Token::KwPredicate) => parse_predicate(p).map(Item::Predicate),
		Some(Token::KwInstance) => parse_instance(p).map(Item::Instance),
		Some(Token::KwRule) => parse_rule(p).map(Item::Rule),
		Some(Token::KwDerive) => parse_derive(p).map(Item::Derive),
		Some(Token::KwPolicy) => parse_policy(p).map(Item::Policy),
		Some(Token::KwProfile) => parse_profile_or_selection(p),
		other => Err(ParseError {
			message: format!("expected item, got {:?}", other),
			span: p.span.clone(),
		}),
	}
}

fn parse_use(p: &mut Parser) -> Result<Use, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwUse)?;
	let namespace = parse_qualified_name(p)?;

	Ok(Use {
		namespace,
		span: start..p.span.end,
	})
}

fn parse_policy(p: &mut Parser) -> Result<Policy, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwPolicy)?;
	let name = p.expect_ident()?;
	p.expect(Token::LBrace)?;

	let mut bindings = Vec::new();
	while !p.at(&Token::RBrace) {
		bindings.push(parse_policy_binding(p)?);
	}

	p.expect(Token::RBrace)?;

	Ok(Policy {
		name,
		bindings,
		span: start..p.span.end,
	})
}

fn parse_policy_binding(p: &mut Parser) -> Result<PolicyBinding, ParseError> {
	let start = p.span.start;

	let modal = match &p.current {
		Some(Token::KwMust) => {
			p.advance();
			Modal::Must
		}
		Some(Token::KwShould) => {
			p.advance();
			Modal::Should
		}
		Some(Token::KwMay) => {
			p.advance();
			Modal::May
		}
		other => {
			return Err(ParseError {
				message: format!("expected modal (must/should/may), got {:?}", other),
				span: p.span.clone(),
			});
		}
	};

	let rule_name = parse_qualified_name(p)?;

	Ok(PolicyBinding {
		modal,
		rule_name,
		span: start..p.span.end,
	})
}

fn parse_profile_or_selection(p: &mut Parser) -> Result<Item, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwProfile)?;
	let name = parse_qualified_name(p)?;

	// Check if this is a declaration (has brace) or selection (bare)
	if p.at(&Token::LBrace) {
		p.advance();

		let mut policies = Vec::new();
		while !p.at(&Token::RBrace) {
			p.expect(Token::KwPolicy)?;
			let policy_name = parse_qualified_name(p)?;
			policies.push(policy_name);
		}

		p.expect(Token::RBrace)?;

		Ok(Item::Profile(Profile {
			name,
			policies,
			span: start..p.span.end,
		}))
	} else {
		// Bare profile selection
		Ok(Item::ProfileSelection(ProfileSelection {
			name,
			span: start..p.span.end,
		}))
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

	// Register this namespace before parsing contents
	let full_path = p.full_namespace_path(&name);
	p.declared_namespaces.insert(full_path);

	// Push onto stack for nested namespaces
	p.namespace_stack.push(name.clone());

	p.expect(Token::LBrace)?;

	let mut items = Vec::new();
	while !p.at(&Token::RBrace) {
		items.push(parse_item(p)?);
	}

	p.expect(Token::RBrace)?;

	// Pop from stack
	p.namespace_stack.pop();

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

fn parse_derive(p: &mut Parser) -> Result<Derive, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwDerive)?;
	let name = p.expect_ident()?;
	p.expect(Token::LBrace)?;

	let mut body = Vec::new();
	while !p.at(&Token::RBrace) {
		body.push(parse_statement(p)?);
	}

	p.expect(Token::RBrace)?;

	Ok(Derive {
		name,
		body,
		span: start..p.span.end,
	})
}

fn parse_statement(p: &mut Parser) -> Result<Statement, ParseError> {
	match &p.current {
		Some(Token::KwLet) => parse_let_stmt(p).map(Statement::Let),
		Some(Token::KwFor) => parse_for_stmt(p).map(Statement::For),
		Some(Token::KwIf) => parse_if_stmt(p).map(Statement::If),
		Some(Token::KwMust) => parse_modal_stmt(p, Modal::Must).map(Statement::Modal),
		Some(Token::KwShould) => parse_modal_stmt(p, Modal::Should).map(Statement::Modal),
		Some(Token::KwMay) => parse_modal_stmt(p, Modal::May).map(Statement::Modal),
		Some(Token::KwAdd) => parse_add_stmt_keyword(p).map(Statement::Add),
		Some(Token::At) => parse_assert_stmt(p).map(Statement::Assert),
		other => Err(ParseError {
			message: format!("expected statement, got {:?}", other),
			span: p.span.clone(),
		}),
	}
}

fn parse_modal_stmt(p: &mut Parser, modal: Modal) -> Result<ModalStmt, ParseError> {
	let start = p.span.start;
	// Consume the modal keyword (must/should/may)
	p.advance();
	let expr = parse_expr(p)?;

	// Optional metadata block
	let metadata = if p.at(&Token::LBrace) {
		Some(parse_must_metadata(p)?)
	} else {
		None
	};

	Ok(ModalStmt {
		modal,
		expr,
		metadata,
		span: start..p.span.end,
	})
}

fn parse_for_stmt(p: &mut Parser) -> Result<ForStmt, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwFor)?;
	let var = p.expect_ident()?;
	p.expect(Token::KwIn)?;
	let collection = parse_expr(p)?;
	p.expect(Token::LBrace)?;

	let mut body = Vec::new();
	while !p.at(&Token::RBrace) {
		body.push(parse_statement(p)?);
	}

	p.expect(Token::RBrace)?;

	Ok(ForStmt {
		var,
		collection,
		body,
		span: start..p.span.end,
	})
}

fn parse_if_stmt(p: &mut Parser) -> Result<IfStmt, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwIf)?;
	let condition = parse_expr(p)?;
	p.expect(Token::LBrace)?;

	let mut then_body = Vec::new();
	while !p.at(&Token::RBrace) {
		then_body.push(parse_statement(p)?);
	}

	p.expect(Token::RBrace)?;

	let else_body = if p.at(&Token::KwElse) {
		p.advance();
		p.expect(Token::LBrace)?;

		let mut body = Vec::new();
		while !p.at(&Token::RBrace) {
			body.push(parse_statement(p)?);
		}

		p.expect(Token::RBrace)?;
		Some(body)
	} else {
		None
	};

	Ok(IfStmt {
		condition,
		then_body,
		else_body,
		span: start..p.span.end,
	})
}

fn parse_add_stmt_keyword(p: &mut Parser) -> Result<AddStmt, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwAdd)?;
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

fn parse_must_metadata(p: &mut Parser) -> Result<ModalMetadata, ParseError> {
	let start = p.span.start;
	p.expect(Token::LBrace)?;

	let mut subject = None;
	let mut area = None;
	let mut message = None;

	while !p.at(&Token::RBrace) {
		let field_name = p.expect_ident()?;
		p.expect(Token::Colon)?;

		match field_name.as_str() {
			"subject" => {
				subject = Some(parse_expr(p)?);
			}
			"area" => {
				area = Some(parse_qualified_name(p)?);
			}
			"message" => match &p.current {
				Some(Token::StringLiteral(s)) => {
					message = Some(s.clone());
					p.advance();
				}
				other => {
					return Err(ParseError {
						message: format!("expected string for message, got {:?}", other),
						span: p.span.clone(),
					});
				}
			},
			other => {
				return Err(ParseError {
					message: format!("unknown must metadata field: {}", other),
					span: p.span.clone(),
				});
			}
		}

		// Optional comma between fields
		if p.at(&Token::Comma) {
			p.advance();
		}
	}

	p.expect(Token::RBrace)?;

	Ok(ModalMetadata {
		subject,
		area,
		message,
		span: start..p.span.end,
	})
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
	let expr = parse_primary_expr(p)?;

	// Check for trailing as(Type) - standalone validation
	if p.at(&Token::KwAs) {
		let start = p.span.start;
		p.advance();
		p.expect(Token::LParen)?;
		let target_type = parse_qualified_name(p)?;
		p.expect(Token::RParen)?;

		return Ok(Expr::As(Box::new(AsValidation {
			inner: expr,
			target_type,
			span: start..p.span.end,
		})));
	}

	Ok(expr)
}

fn parse_primary_expr(p: &mut Parser) -> Result<Expr, ParseError> {
	match &p.current {
		Some(Token::Underscore) => {
			let span = p.span.clone();
			p.advance();
			Ok(Expr::Blank(span))
		}
		Some(Token::KwQuery) => parse_query_expr_keyword(p),
		Some(Token::KwAdd) => parse_add_expr_keyword(p),
		Some(Token::KwMatch) => parse_match_expr(p),
		Some(Token::StringLiteral(s)) => {
			let s = s.clone();
			let span = p.span.clone();
			p.advance();
			Ok(Expr::String(s, span))
		}
		Some(Token::KwTrue) => {
			let span = p.span.clone();
			p.advance();
			Ok(Expr::Bool(true, span))
		}
		Some(Token::KwFalse) => {
			let span = p.span.clone();
			p.advance();
			Ok(Expr::Bool(false, span))
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

fn parse_add_expr_keyword(p: &mut Parser) -> Result<Expr, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwAdd)?;
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

fn parse_query_expr_keyword(p: &mut Parser) -> Result<Expr, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwQuery)?;
	p.expect(Token::LParen)?;
	let path = parse_query_path(p)?;
	p.expect(Token::RParen)?;

	Ok(Expr::Query(QueryExpr {
		path,
		span: start..p.span.end,
	}))
}

fn parse_match_expr(p: &mut Parser) -> Result<Expr, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwMatch)?;
	let value = parse_primary_expr(p)?; // Changed from parse_expr

	// Optional as(Type, mode)
	let as_type = if p.at(&Token::KwAs) {
		Some(parse_as_expr(p)?)
	} else {
		None
	};

	p.expect(Token::LBrace)?;

	let mut arms = Vec::new();
	while !p.at(&Token::RBrace) {
		arms.push(parse_match_arm(p)?);
		// Optional comma between arms
		if p.at(&Token::Comma) {
			p.advance();
		}
	}

	p.expect(Token::RBrace)?;

	Ok(Expr::Match(Box::new(MatchExpr {
		value,
		as_type,
		arms,
		span: start..p.span.end,
	})))
}

fn parse_as_expr(p: &mut Parser) -> Result<AsExpr, ParseError> {
	let start = p.span.start;
	p.expect(Token::KwAs)?;
	p.expect(Token::LParen)?;
	let target_type = parse_qualified_name(p)?;
	p.expect(Token::Comma)?;

	let mode = match &p.current {
		Some(Token::KwMust) => {
			p.advance();
			Modal::Must
		}
		Some(Token::KwShould) => {
			p.advance();
			Modal::Should
		}
		Some(Token::KwMay) => {
			p.advance();
			Modal::May
		}
		other => {
			return Err(ParseError {
				message: format!("expected 'must', 'should', or 'may', got {:?}", other),
				span: p.span.clone(),
			});
		}
	};

	p.expect(Token::RParen)?;

	Ok(AsExpr {
		target_type,
		mode,
		span: start..p.span.end,
	})
}

fn parse_match_arm(p: &mut Parser) -> Result<MatchArm, ParseError> {
	let start = p.span.start;
	let mut patterns = Vec::new();

	// Parse patterns (comma-separated)
	loop {
		patterns.push(parse_match_pattern(p)?);
		if p.at(&Token::Comma) && !p.at(&Token::FatArrow) {
			// Look ahead - if next is FatArrow, stop
			p.advance();
			// Check if we're now at FatArrow or another pattern
			if p.at(&Token::FatArrow) {
				break;
			}
		} else {
			break;
		}
	}

	p.expect(Token::FatArrow)?;
	let result = parse_expr(p)?;

	Ok(MatchArm {
		patterns,
		result,
		span: start..p.span.end,
	})
}

fn parse_match_pattern(p: &mut Parser) -> Result<MatchPattern, ParseError> {
	if p.at(&Token::KwElse) {
		p.advance();
		Ok(MatchPattern::Else)
	} else if p.at_ident() {
		let name = p.expect_ident()?;
		Ok(MatchPattern::Variant(name))
	} else {
		Err(ParseError {
			message: format!("expected pattern (identifier or else), got {:?}", p.current),
			span: p.span.clone(),
		})
	}
}

fn parse_query_path(p: &mut Parser) -> Result<QueryPath, ParseError> {
	let start = p.span.start;
	let mut steps = Vec::new();

	// First step: type name or variable (e.g., core:Store or store)
	if p.at_name() {
		let qname = parse_qualified_name(p)?;

		// Collect predicates for first step
		let mut predicates = Vec::new();
		while p.at(&Token::LBracket) {
			p.advance();
			predicates.push(parse_query_predicate(p)?);
			p.expect(Token::RBracket)?;
		}

		steps.push(QueryStep {
			axis: Axis::Child,
			node_test: Some(qname),
			predicates,
			span: start..p.span.end,
		});
	}

	// Then handle /step segments
	while p.at(&Token::Slash) {
		steps.push(parse_query_step(p)?);
	}

	Ok(QueryPath {
		steps,
		span: start..p.span.end,
	})
}

fn parse_query_step(p: &mut Parser) -> Result<QueryStep, ParseError> {
	let start = p.span.start;

	// Expect /
	p.expect(Token::Slash)?;

	// Node test (type name or wildcard)
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
		axis: Axis::Child,
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

/// Parse qualified name using resolver to determine namespace depth
fn parse_qualified_name(p: &mut Parser) -> Result<QualifiedName, ParseError> {
	let start = p.span.start;
	let first = p.expect_name()?;

	if !p.at(&Token::Colon) {
		// Simple name, no namespace
		return Ok(QualifiedName {
			namespace: None,
			name: first,
			span: start..p.span.end,
		});
	}

	// We have at least one colon - check if first part is a namespace
	if p.is_namespace(&first) {
		// It's a namespace, consume more parts while they're namespaces
		let mut namespace_parts = vec![first];

		while p.at(&Token::Colon) {
			p.advance(); // consume :
			let next = p.expect_name()?;
			let candidate = format!("{}:{}", namespace_parts.join(":"), next);

			if p.at(&Token::Colon) && p.is_namespace(&candidate) {
				// Still a namespace, continue
				namespace_parts.push(next);
			} else {
				// Not a namespace, this is the final name
				return Ok(QualifiedName {
					namespace: Some(namespace_parts.join(":")),
					name: next,
					span: start..p.span.end,
				});
			}
		}

		// Consumed all parts as namespaces - error, need a final name
		Err(ParseError {
			message: "expected name after namespace".to_string(),
			span: p.span.clone(),
		})
	} else {
		// First part is not a namespace, treat as simple 2-part qualified name
		p.advance(); // consume :
		let second = p.expect_name()?;
		Ok(QualifiedName {
			namespace: Some(first),
			name: second,
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
		Some(Token::KwTrue) => {
			p.advance();
			Ok(Literal::Bool(true))
		}
		Some(Token::KwFalse) => {
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
			let stores = query(cfn:Resource)
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
	fn parse_nested_namespace_declared_in_file() {
		let src = r#"
namespace aws {
	namespace cfn {
		type Resource
	}
}

rule test {
	let x = query(aws:cfn:Resource)
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 2);

		// Check the rule parsed the nested namespace correctly
		match &ast.items[1] {
			Item::Rule(r) => {
				assert_eq!(r.name, "test");
				match &r.body[0] {
					Statement::Let(let_stmt) => match &let_stmt.value {
						Expr::Query(q) => {
							let qname = q.path.steps[0].node_test.as_ref().unwrap();
							assert_eq!(qname.namespace, Some("aws:cfn".to_string()));
							assert_eq!(qname.name, "Resource");
						}
						_ => panic!("expected query"),
					},
					_ => panic!("expected let"),
				}
			}
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_nested_namespace_with_resolver() {
		let src = r#"
rule test {
	let x = query(aws:cfn:Resource)
}
"#;
		let source = Wa2Source::from_str(src);

		// Create resolver that knows about aws and aws:cfn namespaces (from model)
		let resolver: Resolver = Box::new(|name| matches!(name, "aws" | "aws:cfn"));

		let ast = parse_with_resolver(source.lexer(), resolver).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Rule(r) => {
				assert_eq!(r.name, "test");
				match &r.body[0] {
					Statement::Let(let_stmt) => match &let_stmt.value {
						Expr::Query(q) => {
							let qname = q.path.steps[0].node_test.as_ref().unwrap();
							assert_eq!(qname.namespace, Some("aws:cfn".to_string()));
							assert_eq!(qname.name, "Resource");
						}
						_ => panic!("expected query"),
					},
					_ => panic!("expected let"),
				}
			}
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_instance_with_resolver() {
		let src = r#"instance core:workload: core:Workload"#;
		let source = Wa2Source::from_str(src);

		// Resolver knows core is a namespace
		let resolver: Resolver = Box::new(|name| name == "core");

		let ast = parse_with_resolver(source.lexer(), resolver).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Instance(i) => {
				assert_eq!(i.name.namespace, Some("core".to_string()));
				assert_eq!(i.name.name, "workload");
				assert_eq!(i.ty.namespace, Some("core".to_string()));
				assert_eq!(i.ty.name, "Workload");
			}
			_ => panic!("expected instance"),
		}
	}

	#[test]
	fn parse_query_path_with_nested_namespace() {
		let src = r#"
namespace aws {
	namespace cfn {
		type Resource
	}
}
namespace core {
	predicate source
	type Store
}

rule test {
	let x = query(core:Store[core:source/aws:cfn:Resource])
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		// Items: 0=aws namespace, 1=core namespace, 2=rule test
		match &ast.items[2] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Let(let_stmt) => match &let_stmt.value {
					Expr::Query(q) => {
						// First step: core:Store
						let step0 = &q.path.steps[0];
						let qname0 = step0.node_test.as_ref().unwrap();
						assert_eq!(qname0.namespace, Some("core".to_string()));
						assert_eq!(qname0.name, "Store");

						// Predicate path: core:source/aws:cfn:Resource
						let pred = &step0.predicates[0];
						match pred {
							QueryPredicate::Exists(path) => {
								// First step in predicate: core:source
								let ps0 = &path.steps[0];
								let pqn0 = ps0.node_test.as_ref().unwrap();
								assert_eq!(pqn0.namespace, Some("core".to_string()));
								assert_eq!(pqn0.name, "source");

								// Second step in predicate: aws:cfn:Resource
								let ps1 = &path.steps[1];
								let pqn1 = ps1.node_test.as_ref().unwrap();
								assert_eq!(pqn1.namespace, Some("aws:cfn".to_string()));
								assert_eq!(pqn1.name, "Resource");
							}
							_ => panic!("expected exists predicate"),
						}
					}
					_ => panic!("expected query"),
				},
				_ => panic!("expected let"),
			},
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_bootstrap() {
		let src = super::super::EMBEDDED_BOOTSTRAP;
		let source = Wa2Source::from_str(src);

		// For bootstrap, namespaces are declared in the file itself
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
namespace core {}
namespace aws {}

rule test {
	let x = query(core:Store[core:source/aws:Rules/*/aws:Status = "Enabled"])
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
		let src = r#"query(core:Store[core:source/aws:Rules/*/aws:Status = "Enabled"])"#;
		let source = Wa2Source::from_str(src);
		let lex = source.lexer();

		for (i, tok) in lex.enumerate() {
			eprintln!("{}: {:?}", i, tok);
		}
	}

	#[test]
	fn compare_query_parsing_with_and_without_predicate() {
		// Query that works
		let src1 = r#"
namespace core {}
namespace aws { namespace cfn {} }

rule test {
    let stores = query(core:Store)
}
"#;

		// Query that doesn't work
		let src2 = r#"
namespace core {}
namespace aws { namespace cfn {} }

rule test {
    let stores = query(core:Store[core:source/aws:cfn:Resource])
}
"#;

		let source1 = Wa2Source::from_str(src1);
		let ast1 = parse(source1.lexer()).unwrap();

		let source2 = Wa2Source::from_str(src2);
		let ast2 = parse(source2.lexer()).unwrap();

		eprintln!("=== Query without predicate ===");
		if let Item::Rule(r) = &ast1.items[2] {
			if let Statement::Let(l) = &r.body[0] {
				if let Expr::Query(q1) = &l.value {
					eprintln!("Steps: {}", q1.path.steps.len());
					for (i, step) in q1.path.steps.iter().enumerate() {
						let name = step
							.node_test
							.as_ref()
							.map(|n| n.to_string())
							.unwrap_or("*".to_string());
						eprintln!(
							"  Step {}: {} (predicates: {})",
							i,
							name,
							step.predicates.len()
						);
					}
				}
			}
		}

		eprintln!("\n=== Query with predicate ===");
		if let Item::Rule(r) = &ast2.items[2] {
			if let Statement::Let(l) = &r.body[0] {
				if let Expr::Query(q2) = &l.value {
					eprintln!("Steps: {}", q2.path.steps.len());
					for (i, step) in q2.path.steps.iter().enumerate() {
						let name = step
							.node_test
							.as_ref()
							.map(|n| n.to_string())
							.unwrap_or("*".to_string());
						eprintln!(
							"  Step {}: {} (predicates: {})",
							i,
							name,
							step.predicates.len()
						);

						for (j, pred) in step.predicates.iter().enumerate() {
							match pred {
								QueryPredicate::Exists(path) => {
									eprintln!("    Predicate {}: Exists", j);
									for (k, pstep) in path.steps.iter().enumerate() {
										let pname = pstep
											.node_test
											.as_ref()
											.map(|n| {
												format!("ns={:?} name={}", n.namespace, n.name)
											})
											.unwrap_or("*".to_string());
										eprintln!("      Path step {}: {}", k, pname);
									}
								}
								QueryPredicate::Eq(path, val) => {
									eprintln!("    Predicate {}: Eq({:?})", j, val);
									for (k, pstep) in path.steps.iter().enumerate() {
										let pname = pstep
											.node_test
											.as_ref()
											.map(|n| {
												format!("ns={:?} name={}", n.namespace, n.name)
											})
											.unwrap_or("*".to_string());
										eprintln!("      Path step {}: {}", k, pname);
									}
								}
								QueryPredicate::In(path, vals) => {
									eprintln!("    Predicate {}: In({:?})", j, vals);
									for (k, pstep) in path.steps.iter().enumerate() {
										let pname = pstep
											.node_test
											.as_ref()
											.map(|n| {
												format!("ns={:?} name={}", n.namespace, n.name)
											})
											.unwrap_or("*".to_string());
										eprintln!("      Path step {}: {}", k, pname);
									}
								}
							}
						}
					}
				}
			}
		}
	}

	#[test]
	fn test_query_with_nested_namespace_predicate() {
		use crate::intents::kernel::ast::*;
		use crate::intents::kernel::query::QueryEngine;
		use crate::intents::model::Model;

		// Build a model with the structure we expect
		let mut model = Model::bootstrap();

		// Create namespaces
		model.ensure_namespace("core").unwrap();
		model.ensure_namespace("aws").unwrap();
		model.ensure_namespace("aws:cfn").unwrap();

		// Create types
		model.apply("core:Store", "wa2:type", "wa2:Type").unwrap();
		model
			.apply("aws:cfn:Resource", "wa2:type", "wa2:Type")
			.unwrap();
		model
			.apply("core:source", "wa2:type", "wa2:Predicate")
			.unwrap();

		// Create a workload as root
		let workload = model.ensure_entity("core:workload").unwrap();
		model
			.apply_to(workload, "wa2:type", "core:Workload")
			.unwrap();
		model.set_root(workload);

		// Create a CFN resource
		let bucket = model.ensure_entity("Bucket1").unwrap();
		model
			.apply_to(bucket, "wa2:type", "aws:cfn:Resource")
			.unwrap();

		// Create a Store that points to the CFN resource
		let store = model.blank();
		model.apply_to(store, "wa2:type", "core:Store").unwrap();
		model.apply_entity(store, "core:source", bucket).unwrap();
		model.apply_entity(workload, "wa2:contains", store).unwrap();

		eprintln!("Model:\n{}", model);

		let engine = QueryEngine::new();

		// Test 1: Simple query for core:Store (should work)
		let path1 = QueryPath {
			steps: vec![QueryStep {
				axis: Axis::Child,
				node_test: Some(QualifiedName {
					namespace: Some("core".to_string()),
					name: "Store".to_string(),
					span: 0..0,
				}),
				predicates: vec![],
				span: 0..0,
			}],
			span: 0..0,
		};

		let results1 = engine.execute(&model, &path1).unwrap();
		eprintln!("Query core:Store => {} results", results1.len());
		assert_eq!(results1.len(), 1, "Should find 1 Store");

		// Test 2: Query with predicate for core:source/aws:cfn:Resource
		let path2 = QueryPath {
			steps: vec![QueryStep {
				axis: Axis::Child,
				node_test: Some(QualifiedName {
					namespace: Some("core".to_string()),
					name: "Store".to_string(),
					span: 0..0,
				}),
				predicates: vec![QueryPredicate::Exists(QueryPath {
					steps: vec![
						QueryStep {
							axis: Axis::Child,
							node_test: Some(QualifiedName {
								namespace: Some("core".to_string()),
								name: "source".to_string(),
								span: 0..0,
							}),
							predicates: vec![],
							span: 0..0,
						},
						QueryStep {
							axis: Axis::Child,
							node_test: Some(QualifiedName {
								namespace: Some("aws:cfn".to_string()),
								name: "Resource".to_string(),
								span: 0..0,
							}),
							predicates: vec![],
							span: 0..0,
						},
					],
					span: 0..0,
				})],
				span: 0..0,
			}],
			span: 0..0,
		};

		let results2 = engine.execute(&model, &path2).unwrap();
		eprintln!(
			"Query core:Store[core:source/aws:cfn:Resource] => {} results",
			results2.len()
		);
		assert_eq!(results2.len(), 1, "Should find 1 Store with predicate");
	}

	#[test]
	fn parse_use_statement() {
		let src = r#"use core"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Use(u) => {
				assert_eq!(u.namespace.namespace, None);
				assert_eq!(u.namespace.name, "core");
			}
			_ => panic!("expected use"),
		}
	}

	#[test]
	fn parse_use_nested_namespace() {
		let src = r#"
namespace aws { namespace cfn {} }
use aws:cfn
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 2);
		match &ast.items[1] {
			Item::Use(u) => {
				assert_eq!(u.namespace.namespace, Some("aws".to_string()));
				assert_eq!(u.namespace.name, "cfn");
			}
			_ => panic!("expected use"),
		}
	}

	#[test]
	fn parse_multiple_use_statements() {
		let src = r#"
use core
use aws
use aws:cfn
use data
"#;
		let source = Wa2Source::from_str(src);
		// Resolver knows these namespaces exist
		let resolver: Resolver =
			Box::new(|name| matches!(name, "core" | "aws" | "aws:cfn" | "data"));
		let ast = parse_with_resolver(source.lexer(), resolver).unwrap();

		assert_eq!(ast.items.len(), 4);
		for item in &ast.items {
			assert!(matches!(item, Item::Use(_)));
		}
	}

	#[test]
	fn parse_derive() {
		let src = r#"
derive stores {
	let x = query(aws:cfn:Resource)
	for s in x {
		let node = add(_, wa2:type, core:Store)
	}
}
"#;
		let source = Wa2Source::from_str(src);
		let resolver: Resolver =
			Box::new(|name| matches!(name, "aws" | "aws:cfn" | "core" | "wa2"));
		let ast = parse_with_resolver(source.lexer(), resolver).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Derive(d) => {
				assert_eq!(d.name, "stores");
				assert_eq!(d.body.len(), 2);
			}
			_ => panic!("expected derive"),
		}
	}

	#[test]
	fn parse_profile_declaration() {
		let src = r#"
profile default {
	policy protect_critical_data
	policy classify_data
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::Profile(p) => {
				assert_eq!(p.name.name, "default");
				assert_eq!(p.policies.len(), 2);
				assert_eq!(p.policies[0].name, "protect_critical_data");
				assert_eq!(p.policies[1].name, "classify_data");
			}
			_ => panic!("expected profile"),
		}
	}

	#[test]
	fn parse_profile_selection() {
		let src = r#"profile default"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 1);
		match &ast.items[0] {
			Item::ProfileSelection(s) => {
				assert_eq!(s.name.name, "default");
			}
			_ => panic!("expected profile selection"),
		}
	}

	#[test]
	fn parse_profile_qualified_selection() {
		let src = r#"
namespace core {}
profile core:default
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		assert_eq!(ast.items.len(), 2);
		match &ast.items[1] {
			Item::ProfileSelection(s) => {
				assert_eq!(s.name.namespace, Some("core".to_string()));
				assert_eq!(s.name.name, "default");
			}
			_ => panic!("expected profile selection"),
		}
	}

	#[test]
	fn parse_should_statement() {
		let src = r#"
rule test {
	should query(core:Store) {
		subject: x,
		message: "warning message"
	}
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		match &ast.items[0] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Modal(m) => {
					assert_eq!(m.modal, Modal::Should);
				}
				_ => panic!("expected must statement"),
			},
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_may_statement() {
		let src = r#"
rule test {
	may query(core:Store) {
		message: "optional check"
	}
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		match &ast.items[0] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Modal(m) => {
					assert_eq!(m.modal, Modal::May);
				}
				_ => panic!("expected must statement"),
			},
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_match_with_else() {
		let src = r#"
rule test {
	let x = match query(v/value) {
		Critical => true,
		else => false
	}
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		match &ast.items[0] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Let(l) => match &l.value {
					Expr::Match(m) => {
						assert_eq!(m.arms.len(), 2);
						assert!(matches!(m.arms[0].patterns[0], MatchPattern::Variant(_)));
						assert!(matches!(m.arms[1].patterns[0], MatchPattern::Else));
					}
					_ => panic!("expected match"),
				},
				_ => panic!("expected let"),
			},
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_match_with_as_must() {
		let src = r#"
namespace my {}

rule test {
    let x = match query(v/value) as(my:Type, must) {
        A => true,
        else => false
    }
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		match &ast.items[1] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Let(l) => match &l.value {
					Expr::Match(m) => {
						assert!(m.as_type.is_some());
						let as_expr = m.as_type.as_ref().unwrap();
						assert_eq!(as_expr.target_type.namespace, Some("my".to_string()));
						assert_eq!(as_expr.target_type.name, "Type");
						assert_eq!(as_expr.mode, Modal::Must);
					}
					_ => panic!("expected match"),
				},
				_ => panic!("expected let"),
			},
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_match_with_as_should() {
		let src = r#"
namespace my {}

rule test {
    let x = match query(v/value) as(my:Type, should) {
        A => true,
        else => false
    }
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		match &ast.items[1] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Let(l) => match &l.value {
					Expr::Match(m) => {
						assert!(m.as_type.is_some());
						let as_expr = m.as_type.as_ref().unwrap();
						assert_eq!(as_expr.mode, Modal::Should);
					}
					_ => panic!("expected match"),
				},
				_ => panic!("expected let"),
			},
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_match_with_as_may() {
		let src = r#"
namespace my {}

rule test {
    let x = match query(v/value) as(my:Type, may) {
        A => true,
        else => false
    }
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		match &ast.items[1] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Let(l) => match &l.value {
					Expr::Match(m) => {
						assert!(m.as_type.is_some());
						let as_expr = m.as_type.as_ref().unwrap();
						assert_eq!(as_expr.mode, Modal::May);
					}
					_ => panic!("expected match"),
				},
				_ => panic!("expected let"),
			},
			_ => panic!("expected rule"),
		}
	}

	#[test]
	fn parse_match_without_as() {
		let src = r#"
rule test {
    let x = match query(v/value) {
        A => true,
        else => false
    }
}
"#;
		let source = Wa2Source::from_str(src);
		let ast = parse(source.lexer()).unwrap();

		match &ast.items[0] {
			Item::Rule(r) => match &r.body[0] {
				Statement::Let(l) => match &l.value {
					Expr::Match(m) => {
						assert!(m.as_type.is_none());
					}
					_ => panic!("expected match"),
				},
				_ => panic!("expected let"),
			},
			_ => panic!("expected rule"),
		}
	}
}
