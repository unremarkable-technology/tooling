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
	Use(Use),
	Struct(Struct),
	Enum(Enum),
	Type(TypeDecl),
	Predicate(Predicate),
	Instance(Instance),
	Rule(Rule),
	Derive(Derive),
	Policy(Policy),
	Profile(Profile),
	ProfileSelection(ProfileSelection),
}

/// Namespace block
#[derive(Debug, Clone)]
pub struct Namespace {
	pub name: String,
	pub items: Vec<Item>,
	pub span: Span,
}

/// Use declaration - imports a namespace
#[derive(Debug, Clone)]
pub struct Use {
	pub namespace: QualifiedName,
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
	For(ForStmt),
	Assert(AssertStmt),
	Modal(ModalStmt),
	If(IfStmt),
}

/// Let binding: let x = query(...)
#[derive(Debug, Clone)]
pub struct LetStmt {
	pub name: String,
	pub value: Expr,
	pub span: Span,
}

/// Add statement: add(s, p, o)
#[derive(Debug, Clone)]
pub struct AddStmt {
	pub subject: Expr,
	pub predicate: QualifiedName,
	pub object: Expr,
	pub span: Span,
}

/// For statement (new syntax): for x in xs { ... }
#[derive(Debug, Clone)]
pub struct ForStmt {
	pub var: String,
	pub collection: Expr,
	pub body: Vec<Statement>,
	pub span: Span,
}

/// If statement: if cond { ... } else { ... }
#[derive(Debug, Clone)]
pub struct IfStmt {
	pub condition: Expr,
	pub then_body: Vec<Statement>,
	pub else_body: Option<Vec<Statement>>,
	pub span: Span,
}

/// Derive declaration - model building, no must allowed
#[derive(Debug, Clone)]
pub struct Derive {
	pub name: String,
	pub body: Vec<Statement>,
	pub span: Span,
}

/// Assert statement: @#assert(expr)
#[derive(Debug, Clone)]
pub struct AssertStmt {
	pub expr: Expr,
	pub span: Span,
}

/// Must/should/may statement with modal
#[derive(Debug, Clone)]
pub struct ModalStmt {
	pub modal: Modal,
	pub expr: Expr,
	pub metadata: Option<MustMetadata>,
	pub span: Span,
}

/// Metadata block for must statement
#[derive(Debug, Clone)]
pub struct MustMetadata {
	pub subject: Option<Expr>,
	pub area: Option<QualifiedName>,
	pub message: Option<String>,
	pub span: Span,
}

/// Expression
#[derive(Debug, Clone)]
pub enum Expr {
	/// Variable reference
	Var(String, Span),
	/// Blank node: _
	Blank(Span),
	/// Query: query(core:Store[...])
	Query(QueryExpr),
	/// Add expression: add(s, p, o) - returns subject
	Add(Box<AddExpr>),
	/// Qualified name literal: core:Store
	QName(QualifiedName),
	/// String literal
	String(String, Span),
	/// Boolean literal
	Bool(bool, Span),
	/// empty(expr) builtin
	Empty(Box<Expr>, Span),
	/// Match expression: match v as(T, strict) { ... }
	Match(Box<MatchExpr>),
   /// Standalone as validation: query(...) as(Type)
    As(Box<AsValidation>),
}

/// Query expression: query(path)
#[derive(Debug, Clone)]
pub struct QueryExpr {
	pub path: QueryPath,
	pub span: Span,
}

/// Query path: core:Store[predicate]
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
	Child, // /
	Descendant,
	DescendantOrSelf,
}

/// Predicate in query: [aws:type = "..."] or [aws:type in (...)]
#[derive(Debug, Clone)]
pub enum QueryPredicate {
	Eq(QueryPath, Literal),
	In(QueryPath, Vec<Literal>),
	Exists(QueryPath),
}

/// Add expression: add(s, p, o)
#[derive(Debug, Clone)]
pub struct AddExpr {
	pub subject: Expr,
	pub predicate: QualifiedName,
	pub object: Expr,
	pub span: Span,
}

/// Match expression: match v as(T, strict) { A, B => true, _ => false }
#[derive(Debug, Clone)]
pub struct MatchExpr {
	pub value: Expr,
	pub as_type: Option<AsExpr>,
	pub arms: Vec<MatchArm>,
	pub span: Span,
}

/// As expression for type coercion: as(Type, strict)
#[derive(Debug, Clone)]
pub struct AsExpr {
	pub target_type: QualifiedName,
	pub mode: Modal,
	pub span: Span,
}

/// Standalone as validation: expr as(Type)
#[derive(Debug, Clone)]
pub struct AsValidation {
    pub inner: Expr,
    pub target_type: QualifiedName,
    pub span: Span,
}

/// Match arm: Pattern => result
#[derive(Debug, Clone)]
pub struct MatchArm {
	pub patterns: Vec<MatchPattern>,
	pub result: Expr,
	pub span: Span,
}

/// Match pattern
#[derive(Debug, Clone)]
pub enum MatchPattern {
	/// Named variant: MissionCritical
	Variant(String),
	/// Catch-all: else
	Else,
}

/// Modal verb for policy bindings
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Modal {
	Must,
	Should,
	May,
}

/// Policy binding: modal + rule reference
#[derive(Debug, Clone)]
pub struct PolicyBinding {
	pub modal: Modal,
	pub rule_name: QualifiedName,
	pub span: Span,
}

/// Policy declaration
#[derive(Debug, Clone)]
pub struct Policy {
	pub name: String,
	pub bindings: Vec<PolicyBinding>,
	pub span: Span,
}

/// Profile declaration - bundles policies
#[derive(Debug, Clone)]
pub struct Profile {
	pub name: QualifiedName,
	pub policies: Vec<QualifiedName>,
	pub span: Span,
}

/// Profile selection - bare profile name
#[derive(Debug, Clone)]
pub struct ProfileSelection {
	pub name: QualifiedName,
	pub span: Span,
}
