// use id_arena::Id;

use super::system::FocusTaxonomy;

// /// unique handle to a `Node` instance
// pub type NodeId = Id<Node>;
// /// A node is a simple abstract component in a `System`.
// ///
// /// This is doing part of a system, it carries out kind of purpose.
// #[derive(Debug)]
// pub struct Node {
// 	/// the name of the node
// 	pub name: String,
// 	/// The kind of node for this instance
// 	pub kind: NodeKind,
// 	/// Annotations attached to this instance.
// 	pub annotations: Vec<Annotation>,
// }

// impl Node {
// 	pub fn has_tag(&self, tag: FocusTaxonomy) -> bool {
// 		self.annotations
// 			.iter()
// 			.any(|a| matches!(a, Annotation::Tagged(tv) if tv.tag == tag))
// 	}

// 	pub fn has_tag_value<'a, I>(&self, tag: FocusTaxonomy, values: I) -> bool
// 	where
// 		I: IntoIterator<Item = Option<&'a str>>,
// 	{
// 		let values: Vec<Option<&'a str>> = values.into_iter().collect();

// 		self.annotations.iter().any(|a| {
// 			let Annotation::Tagged(tv) = a else {
// 				return false;
// 			};
// 			if tv.tag != tag {
// 				return false;
// 			}

// 			values.iter().any(|v| match v {
// 				None => true,
// 				Some(expected) => tv.value.as_deref() == Some(*expected),
// 			})
// 		})
// 	}

// 	pub fn has_evidence(&self, tag: FocusTaxonomy) -> bool {
// 		self.annotations
// 			.iter()
// 			.any(|a| matches!(a, Annotation::Evidence(tv) if *tv == tag))
// 	}
// }

// /// Type of `Node` that we can arrange to describe a `System`.
// ///
// /// Each variant represents a distinct conceptual element in the system.
// #[derive(Debug)]
// pub enum NodeKind {
// 	/// An execution/run, represented as a filled circle ●
// 	Run,

// 	/// A state/storage, represented as a filled square ■
// 	Store,

// 	/// A transfer/movement, represented as a filled triangle ►
// 	Move,
// }

// impl NodeKind {
// 	/// Returns the Unicode glyph that visually represents this `NodeKind`.
// 	///
// 	/// These glyphs are monochrome, widely supported geometric symbols.
// 	/// The returned value is a static string slice with no allocation.
// 	pub const fn glyph(&self) -> &'static str {
// 		match self {
// 			NodeKind::Run => "●",   // U+25CF BLACK CIRCLE
// 			NodeKind::Store => "■", // U+25A0 BLACK SQUARE
// 			NodeKind::Move => "►",  // U+25BA BLACK RIGHT-POINTING POINTER
// 		}
// 	}
// }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Annotation {
	Source(SourceReference),
	Tagged(TaggedValue),
	Evidence(FocusTaxonomy),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReference {
	pub file: String,
	pub line: u32,
	pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedValue {
	pub tag: FocusTaxonomy,
	pub value: Option<String>,
}
