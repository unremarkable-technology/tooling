use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio::time::sleep;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::core_engine::CoreEngine;
use crate::iaac::cloudformation::spec_cache::SpecCacheManager;
use crate::iaac::cloudformation::spec_source::SpecSource;

/// backend: LSP-facing adapter that holds the engine and schedules work
pub struct Backend {
	client: Client,
	engine: Arc<Mutex<CoreEngine>>,
	notify: Arc<Notify>,
	// queue of events (generation, uri); multiple entries for the same uri
	// are coalesced by the analyser loop.
	event_queue: Arc<Mutex<VecDeque<Url>>>,
}

impl Backend {
	/// bump generation, enqueue an event for this uri, and notify the analyser
	fn enqueue_event(&self, uri: &Url) {
		// enqueue this event for the analyser
		{
			let mut queue = self.event_queue.lock().unwrap();
			queue.push_back(uri.clone());
		}

		// tell background analyser there is work todo
		self.notify.notify_one();
	}
}

impl Backend {
	pub fn new(client: Client) -> Self {
		// core engine, task notification and atomic generation counter
		let engine = Arc::new(Mutex::new(CoreEngine::new()));
		let notify = Arc::new(Notify::new());
		let event_queue: Arc<Mutex<VecDeque<Url>>> = Arc::new(Mutex::new(VecDeque::new()));

		Backend {
			client,
			engine: engine.clone(),
			notify: notify.clone(),
			event_queue: event_queue.clone(),
		}
	}
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
	/// being asked to initialize, what capabilities do we have
	async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
		Ok(InitializeResult {
			capabilities: ServerCapabilities {
				text_document_sync: Some(TextDocumentSyncCapability::Options(
					TextDocumentSyncOptions {
						open_close: Some(true),
						change: Some(TextDocumentSyncKind::FULL),
						..Default::default()
					},
				)),
				code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
				definition_provider: Some(OneOf::Left(true)),
				hover_provider: Some(HoverProviderCapability::Simple(true)),
				..Default::default()
			},
			server_info: Some(ServerInfo {
				name: "wa2-lsp".to_string(),
				version: Some(env!("CARGO_PKG_VERSION").to_string()),
			}),
		})
	}

	/// initialized done, spawns the analyser
	async fn initialized(&self, _params: InitializedParams) {
		self.client
			.log_message(MessageType::INFO, "WA2 LSP server initialized")
			.await;

		// clone the arcs protecting resources
		let client = self.client.clone();
		let engine = self.engine.clone();
		let notify = self.notify.clone();
		let event_queue = self.event_queue.clone();

		// kick off spec loading in background.
		{
			let client = client.clone();
			let engine = engine.clone();

			tokio::spawn(async move {
				// Simple region choice for now; you can make this configurable later.
				let region = "us-east-1"; // US East (N. Virginia)
				//let region = "eu-west-2"; // London, matches your TZ / likely usage.

				let source = match SpecSource::for_region_schemas(region) {
					Ok(s) => s,
					Err(err) => {
						client
							.log_message(
								MessageType::ERROR,
								format!("WA2: failed to create SpecSource for {region}: {err}"),
							)
							.await;
						return;
					}
				};

				// Decide cache directory: e.g. ~/.cache/wa2/cfn-spec
				let cache_dir = dirs::cache_dir()
					.unwrap_or_else(std::env::temp_dir)
					.join("wa2")
					.join("cfn-spec");

				let manager = SpecCacheManager::new(source, &cache_dir);

				match manager.load_registry_spec_store().await {
					Ok(spec_store) => {
						{
							let mut guard = engine.lock().unwrap();
							guard.set_spec_store(spec_store);
						}

						client
							.log_message(
								MessageType::INFO,
								format!(
									"WA2: CloudFormation spec loaded and cached in {:?}",
									cache_dir
								),
							)
							.await;
					}
					Err(err) => {
						client
							.log_message(
								MessageType::ERROR,
								format!(
									"WA2: failed to load CloudFormation spec (no validation from spec): {err}"
								),
							)
							.await;
					}
				}
			});
		}

		// listen for work via notify
		tokio::spawn(async move {
			analyser_loop(client, engine, notify, event_queue).await;
		});
	}

	/// graceful shutdown request
	async fn shutdown(&self) -> Result<()> {
		Ok(())
	}

	/// new document opened
	/// capture the event, but do no work on main thread
	async fn did_open(&self, params: DidOpenTextDocumentParams) {
		// capture that a new document has been opened
		let uri = params.text_document.uri;
		let text = params.text_document.text;
		let language_id = params.text_document.language_id;

		// forward to core engine (under lock)
		{
			let mut engine = self.engine.lock().unwrap();
			engine.on_open(uri.clone(), text, language_id);
		}

		// enqueue analysis event
		self.enqueue_event(&uri);

		let message = format!("doc_open: {}", uri);
		self.client.log_message(MessageType::INFO, message).await;
	}

	/// document changed
	/// capture the event, but do no work on main thread
	async fn did_change(&self, params: DidChangeTextDocumentParams) {
		let uri = params.text_document.uri;
		let changes = params.content_changes;

		// take the last change's text as the full document.
		let new_text = changes.last().map(|c| c.text.clone()).unwrap_or_default();

		// forward to core engine (under lock)
		{
			let mut engine = self.engine.lock().unwrap();
			engine.on_change(uri.clone(), new_text);
		}

		// enqueue analysis event
		self.enqueue_event(&uri);

		let message = format!("doc_change: {}", uri);
		self.client.log_message(MessageType::INFO, message).await;
	}

	/// document saved, re-run analysis on save
	/// capture the event, but do no work on main thread
	async fn did_save(&self, params: DidSaveTextDocumentParams) {
		let uri = params.text_document.uri;

		// forward to core engine (under lock)
		{
			let mut engine = self.engine.lock().unwrap();
			engine.on_save(&uri);
		}

		// enqueue analysis event
		self.enqueue_event(&uri);

		let message = format!("doc_save: {}", uri);
		self.client.log_message(MessageType::INFO, message).await;
	}

	async fn did_close(&self, _params: DidCloseTextDocumentParams) {
		// no-op for now
	}

	async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
		let uri = params.text_document.uri;

		let mut actions = Vec::new();

		// Check each diagnostic in the range
		for diagnostic in &params.context.diagnostics {
			// Extract suggestion from diagnostic data
			if let Some(ref data) = diagnostic.data {
				if let Ok(obj) = serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(
					data.clone(),
				) {
					if let Some(kind) = obj.get("kind").and_then(|v| v.as_str()) {
						let (title, new_text) = match kind {
							"getatt" => {
								let target =
									obj.get("target").and_then(|v| v.as_str()).unwrap_or("");
								let attribute =
									obj.get("attribute").and_then(|v| v.as_str()).unwrap_or("");
								(
									format!("Change attribute to '{}'", attribute),
									format!("{}.{}", target, attribute), // Just Target.Attribute
								)
							}
							"ref" => {
								let target =
									obj.get("target").and_then(|v| v.as_str()).unwrap_or("");
								(
									format!("Change to '{}'", target),
									target.to_string(), // Just the target
								)
							}
							"property" => {
								let property =
									obj.get("property").and_then(|v| v.as_str()).unwrap_or("");
								(format!("Change to '{}'", property), property.to_string())
							}
							_ => continue,
						};

						let edit = TextEdit {
							range: diagnostic.range,
							new_text,
						};

						let mut changes = HashMap::new();
						changes.insert(uri.clone(), vec![edit]);

						let workspace_edit = WorkspaceEdit {
							changes: Some(changes),
							..Default::default()
						};

						let action = CodeAction {
							title,
							kind: Some(CodeActionKind::QUICKFIX),
							diagnostics: Some(vec![diagnostic.clone()]),
							edit: Some(workspace_edit),
							..Default::default()
						};

						actions.push(CodeActionOrCommand::CodeAction(action));
					}
				} else {
					eprintln!("Failed to parse data as map");
				}
			} else {
				eprintln!("No data in diagnostic");
			}
		}

		if actions.is_empty() {
			Ok(None)
		} else {
			Ok(Some(actions))
		}
	}

	async fn goto_definition(
		&self,
		params: GotoDefinitionParams,
	) -> Result<Option<GotoDefinitionResponse>> {
		let uri = params.text_document_position_params.text_document.uri;
		let position = params.text_document_position_params.position;

		self.client
			.log_message(
				MessageType::INFO,
				format!("goto_definition(m): uri={}, position={:?}", uri, position),
			)
			.await;

		let location = {
			let engine = self.engine.lock().unwrap();
			engine.goto_definition(&uri, position)
		};

		self.client
			.log_message(
				MessageType::INFO,
				format!("goto_definition result: {:?}", location),
			)
			.await;

		Ok(location.map(GotoDefinitionResponse::Scalar))
	}

	async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
		let uri = params.text_document_position_params.text_document.uri;
		let position = params.text_document_position_params.position;

		// Get diagnostics at this position
		let hover_content = {
			let engine = self.engine.lock().unwrap();
			engine.get_hover_content(&uri, position)
		};

		Ok(hover_content.map(|content| Hover {
			contents: HoverContents::Markup(MarkupContent {
				kind: MarkupKind::Markdown,
				value: content,
			}),
			range: None,
		}))
	}
}

/// listens for work to do via notify, processing any
/// changes that require us to analyse again
async fn analyser_loop(
	client: Client,
	engine: Arc<Mutex<CoreEngine>>,
	notify: Arc<Notify>,
	event_queue: Arc<Mutex<VecDeque<Url>>>,
) {
	const TIME_BUDGET: Duration = Duration::from_millis(50);
	// we pick 2x the average inter-key interval (2 x 120 = 240)
	// assumes programmers are fast typist, and when pausing for more
	// than two IKIs they have probably stoppped, started thinking, etc
	// https://userinterfaces.aalto.fi/136Mkeystrokes/resources/chi-18-analysis.pdf#:~:text=For%20fast%20typists,%20the%20average%20IKI%20is
	const IDLE_DELAY: Duration = Duration::from_millis(240);

	loop {
		// Wait until something changes
		notify.notified().await;

		// Debounce: wait for an idle gap so that multiple quick edits
		// can be coalesced into a single analysis pass.
		sleep(IDLE_DELAY).await;

		let start = Instant::now();

		loop {
			/* TECHDEBT: we exceed our budget, rather than avoiding that.
			 * we should know how long a task takes, and exit if we don't
			 * have sufficient time to execute it this time.
			 * this also implies no task is allowed which requires > TIME_BUDGET
			 */
			// we have run out of time, need to wait until next cycle
			if start.elapsed() > TIME_BUDGET {
				// If there's still work left in the queue, schedule another
				// pass before we go back to waiting on notify. This ensures
				// leftover events are eventually processed even if no new
				// edits arrive.
				let has_remaining_work = {
					let queue = event_queue.lock().unwrap();
					!queue.is_empty()
				};
				if has_remaining_work {
					notify.notify_one();
				}

				let used_ms = start.elapsed().as_millis();
				let message = format!(
					"analyser_loop: TIME_BUDGET exhausted {}ms vs {}ms",
					used_ms,
					TIME_BUDGET.as_millis()
				);
				client.log_message(MessageType::INFO, message).await;
				break;
			}

			// Pop one event from the front of the queue. If the queue is
			// empty, we're done for this pass.
			let maybe_event = {
				let mut queue = event_queue.lock().unwrap();
				queue.pop_front()
			};

			let uri = match maybe_event {
				Some(ev) => ev,
				None => break, // no more work this pass
			};

			// Coalesce: remove any remaining events for this uri from the
			// queue. We consider them "covered" by this analysis run.
			{
				let mut queue = event_queue.lock().unwrap();
				queue.retain(|u| u != &uri);
			}

			// Analyse this uri using the latest text snapshot. We treat
			// "no text" as a processed event (doc was closed / removed).
			let parse_result = {
				let mut engine_guard = engine.lock().unwrap();
				engine_guard.analyse_document_fast(&uri)
			};

			match parse_result {
				Ok(_template) => {
					// Fast path succeeded with no diagnostics
					// Run slow path (WA2 guidance)
					let wa2_diagnostics = {
						let mut engine_guard = engine.lock().unwrap();
						engine_guard.analyse_document_slow(&uri)
					};

					// Publish WA2 guidance diagnostics
					if !wa2_diagnostics.is_empty() {
						let message = format!("doc_analyse_slow: {} (WA2 guidance)", uri);
						client.log_message(MessageType::INFO, message).await;

						client
							.publish_diagnostics(uri.clone(), wa2_diagnostics, None)
							.await;
					} else {
						// Template is perfect - no diagnostics at all
						client
							.publish_diagnostics(uri.clone(), Vec::new(), None)
							.await;
					}
				}
				Err(diagnostics) => {
					let message = format!("doc_analyse_fast: {}", uri);
					client.log_message(MessageType::INFO, message).await;

					client
						.publish_diagnostics(uri.clone(), diagnostics, None)
						.await;
				}
			}
		}
	}
}
