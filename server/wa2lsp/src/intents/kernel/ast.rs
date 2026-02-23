//! AST types for WA2 language

use std::ops::Range;

/// Span in source
pub type Span = Range<usize>;

/// Top-level AST
#[derive(Debug, Default)]
pub struct Ast {
	pub items: Vec<Item>,
}

/// Top-level items
#[derive(Debug, Clone)]
pub enum Item {
	Namespace(Namespace),
	Struct(Struct),
	Enum(Enum),
	Type(TypeDecl),
	Predicate(Predicate),
	Instance(Instance),
	Rule(Rule),
}

/// Namespace block
#[derive(Debug, Clone)]
pub struct Namespace {
	pub name: String,
	pub items: Vec<Item>,
	pub span: Span,
}

/// Struct declaration
#[derive(Debug, Clone)]
pub struct Struct {
	pub name: String,
	pub fields: Vec<Field>,
	pub annotations: Vec<Annotation>,
	pub span: Span,
}

/// Field in a struct
#[derive(Debug, Clone)]
pub struct Field {
	pub name: String,
	pub ty: TypeRef,
	pub span: Span,
}

/// Enum declaration
#[derive(Debug, Clone)]
pub struct Enum {
	pub name: String,
	pub variants: Vec<String>,
	pub annotations: Vec<Annotation>,
	pub span: Span,
}

/// Simple type declaration (type alias or marker)
#[derive(Debug, Clone)]
pub struct TypeDecl {
	pub name: String,
	pub annotations: Vec<Annotation>,
	pub span: Span,
}

/// Predicate declaration
#[derive(Debug, Clone)]
pub struct Predicate {
	pub name: String,
	pub span: Span,
}

/// Instance declaration
#[derive(Debug, Clone)]
pub struct Instance {
	pub name: QualifiedName,
	pub ty: QualifiedName,
	pub span: Span,
}

/// Type reference
#[derive(Debug, Clone)]
pub struct TypeRef {
	pub name: QualifiedName,
	pub array: bool,
	pub optional: bool,
	pub span: Span,
}

/// Qualified name (e.g., core:Store, aws:type)
#[derive(Debug, Clone)]
pub struct QualifiedName {
	pub namespace: Option<String>,
	pub name: String,
	pub span: Span,
}

impl QualifiedName {
	pub fn to_string(&self) -> String {
		match &self.namespace {
			Some(ns) => format!("{}:{}", ns, self.name),
			None => self.name.clone(),
		}
	}
}

/// Annotation (e.g., @(description = "..."))
#[derive(Debug, Clone)]
pub struct Annotation {
	pub path: Option<QualifiedName>,
	pub args: Vec<AnnotationArg>,
	pub span: Span,
}

/// Annotation argument
#[derive(Debug, Clone)]
pub struct AnnotationArg {
	pub name: String,
	pub value: Literal,
}

/// Literal value
#[derive(Debug, Clone)]
pub enum Literal {
	String(String),
	Number(i64),
	Bool(bool),
}

/// Rule declaration
#[derive(Debug, Clone)]
pub struct Rule {
	pub name: String,
	pub body: Vec<Statement>,
	pub span: Span,
}

/// Statement in a rule
#[derive(Debug, Clone)]
pub enum Statement {
	Let(LetStmt),
	Add(AddStmt),
	Iterate(IterateStmt),
	Assert(AssertStmt),
}

/// Let binding: let x = ?(...)
#[derive(Debug, Clone)]
pub struct LetStmt {
	pub name: String,
	pub value: Expr,
	pub span: Span,
}

/// Add statement: +(s, p, o)
#[derive(Debug, Clone)]
pub struct AddStmt {
	pub subject: Expr,
	pub predicate: QualifiedName,
	pub object: Expr,
	pub span: Span,
}

/// Iterate statement: *(x in xs, { ... })
#[derive(Debug, Clone)]
pub struct IterateStmt {
	pub var: String,
	pub collection: Expr,
	pub body: Vec<Statement>,
	pub span: Span,
}

/// Assert statement: @#assert(expr)
#[derive(Debug, Clone)]
pub struct AssertStmt {
	pub expr: Expr,
	pub span: Span,
}

/// Expression
#[derive(Debug, Clone)]
pub enum Expr {
	/// Variable reference
	Var(String, Span),
	/// Blank node: _
	Blank(Span),
	/// Query: ?(//core:Store[...])
	Query(QueryExpr),
   /// Add expression: +(s, p, o) - returns subject
	Add(Box<AddExpr>),
	/// Qualified name literal: core:Store
	QName(QualifiedName),
	/// String literal
	String(String, Span),
	/// empty(expr) builtin
	Empty(Box<Expr>, Span),
}

/// Query expression: ?(path)
#[derive(Debug, Clone)]
pub struct QueryExpr {
	pub path: QueryPath,
	pub span: Span,
}

/// Query path: //core:Store[predicate]
#[derive(Debug, Clone)]
pub struct QueryPath {
	pub steps: Vec<QueryStep>,
	pub span: Span,
}

/// Query step
#[derive(Debug, Clone)]
pub struct QueryStep {
	pub axis: Axis,
	pub node_test: Option<QualifiedName>,
	pub predicates: Vec<QueryPredicate>,
	pub span: Span,
}

/// Axis in query
#[derive(Debug, Clone)]
pub enum Axis {
	Child,      // /
	Descendant, // //
	DescendantOrSelf,
}

/// Predicate in query: [aws:type = "..."] or [aws:type in (...)]
#[derive(Debug, Clone)]
pub enum QueryPredicate {
	Eq(QueryPath, Literal),
	In(QueryPath, Vec<Literal>),
	Exists(QueryPath),
}

/// Add expression: +(s, p, o)
#[derive(Debug, Clone)]
pub struct AddExpr {
	pub subject: Expr,
	pub predicate: QualifiedName,
	pub object: Expr,
	pub span: Span,
}
