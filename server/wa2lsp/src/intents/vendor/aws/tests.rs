#[cfg(test)]
mod tests {
	use std::{
		env,
		path::Path,
		sync::{Arc, OnceLock},
	};

	use url::Url;

	use crate::{
		iaac::cloudformation::{
			spec_cache::SpecCacheManager, spec_source::SpecSource, spec_store::SpecStore,
		},
		intents::{
			kernel::{AssertFailure, Kernel},
			model::{EntityId, Model, Query, print_model_as_tree},
			vendor::{DocumentFormat, Method, Vendor},
		},
	};

	fn project_and_analyse(yaml: &str) -> (Model, Vec<AssertFailure>) {
		let kernel = Kernel::new();
		let uri = Url::parse("file:///test.yaml").unwrap();
		let result = kernel
			.analyse(
				yaml,
				&uri,
				DocumentFormat::Yaml,
				Vendor::Aws,
				Method::CloudFormation,
			)
			.expect("analysis should succeed");
		(result.model, result.failures)
	}

	/// Check if a store has evidence of a given fact type
	fn has_evidence(model: &Model, store: EntityId, fact_type_name: &str) -> bool {
		let evidence_type = match model.resolve("core:Evidence") {
			Some(t) => t,
			None => return false,
		};

		let fact_type = match model.resolve(fact_type_name) {
			Some(t) => t,
			None => return false,
		};

		// Check if store has Evidence child containing the fact type
		for evidence_id in model.children(store) {
			if model.has_type(evidence_id, evidence_type) {
				for fact_id in model.children(evidence_id) {
					if model.has_type(fact_id, fact_type) {
						return true;
					}
				}
			}
		}

		false
	}

	/// Check if any failure has the given area
	fn has_failure_with_area(model: &Model, failures: &[AssertFailure], area_name: &str) -> bool {
		let area_id = match model.resolve(area_name) {
			Some(id) => id,
			None => return false,
		};
		failures.iter().any(|f| f.area == Some(area_id))
	}

	/// Count failures with the given area
	fn count_failures_with_area(
		model: &Model,
		failures: &[AssertFailure],
		area_name: &str,
	) -> usize {
		let area_id = match model.resolve(area_name) {
			Some(id) => id,
			None => return 0,
		};
		failures.iter().filter(|f| f.area == Some(area_id)).count()
	}

	/// Loads the real CFN spec (blocking). Cached after first call.
	/// Panics if spec can't be loaded.
	pub fn use_latest_cfn_spec() -> Arc<SpecStore> {
		static CACHED_SPEC: OnceLock<Arc<SpecStore>> = OnceLock::new();

		CACHED_SPEC
			.get_or_init(|| {
				let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
				rt.block_on(async {
					let source = SpecSource::for_region_schemas("us-east-1").expect("spec source");
					let cache_dir = dirs::cache_dir()
						.unwrap_or_else(std::env::temp_dir)
						.join("wa2")
						.join("cfn-spec");
					let manager = SpecCacheManager::new(source, &cache_dir);
					manager.load_registry_spec_store().await.expect("load spec")
				})
			})
			.clone()
	}

	fn test_uri() -> Url {
		Url::parse("file:///tmp/test.yaml").unwrap()
	}

	#[test]
	fn test_non_critical_data_no_backup_required() {
		// NonCritical data doesn't need backup
		let cfn_text = r#"
Resources:
  TempBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags:
        - Key: DataSensitivity
          Value: Confidential
        - Key: DataCriticality
          Value: NonCritical
"#;

		let (model, failures) = project_and_analyse(cfn_text);

		eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("Failures:\n===\n{:?}", failures);

		// No failures expected - tags are present
		assert!(failures.is_empty()); // wrong as don't understand its non critical yet
		//assert!(failures.is_empty());
	}

	#[test]
	fn test_mission_critical_needs_backup() {
		let cfn_text = r#"
Resources:
  CriticalBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags:
        - Key: DataSensitivity
          Value: Confidential
        - Key: DataCriticality
          Value: MissionCritical
"#;

		let (model, failures) = project_and_analyse(cfn_text);

		//eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));
		//eprintln!("Failures:\n===\n{:?}", failures);

		// Tags are present, but no resilience evidence yet
		// The ensure_critical_stores_are_protected rule would fire if we had data:Criticality evidence
		// For now, tags alone don't create evidence - we need the ~() conversion operator
		// So this test may need adjustment based on current rule behavior
	}

	#[test]
	fn test_non_store_resources_no_guidance() {
		let cfn_text = r#"
Resources:
  MyLambda:
    Type: AWS::Lambda::Function
    Properties:
      Role: arn::iam::123456789012
      Runtime: python3.9
      Handler: index.handler
      Code:
        ZipFile: |
          def handler(event, context):
            return "hello"
  MyRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement: []
  MyQueue:
    Type: AWS::SQS::Queue
"#;

		let (model, failures) = project_and_analyse(cfn_text);

		//eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));
		//eprintln!("Failures:\n===\n{:?}", failures);

		// Lambda is Run, Queue is Move - neither are Store
		// Only Store requires DataSensitivity/DataCriticality tags
		let stores = model.query(&Query::descendant("core:Store"));
		assert!(stores.is_empty());

		// No store-related failures
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataSensitivity"
		));
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataCriticality"
		));
	}

	#[test]
	fn test_query_finds_stores() {
		let cfn_text = r#"
Resources:
  Bucket1:
    Type: AWS::S3::Bucket
  Bucket2:
    Type: AWS::S3::Bucket
  Lambda1:
    Type: AWS::Lambda::Function
    Properties:
      Role: arn::iam::123456789012
      Runtime: python3.9
      Handler: index.handler
      Code:
        ZipFile: "def handler(e,c): pass"
"#;

		let (model, failures) = project_and_analyse(cfn_text);

		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));
		eprintln!("Failures:\n===\n{:?}", failures);

		let stores = model.query(&Query::descendant("core:Store"));
		assert_eq!(stores.len(), 2);

		let runs = model.query(&Query::descendant("core:Run"));
		assert_eq!(runs.len(), 1);

		// Both buckets should have tag failures
		assert_eq!(
			count_failures_with_area(&model, &failures, "my:DataSensitivity"),
			2
		);
		assert_eq!(
			count_failures_with_area(&model, &failures, "my:DataSensitivity"),
			2
		);
	}

	#[test]
	fn test_evidence_query() {
		let cfn_text = r#"
Resources:
  Bucket1:
    Type: AWS::S3::Bucket
    Properties:
      Tags:
        - Key: DataSensitivity
          Value: Confidential
        - Key: DataCriticality
          Value: BusinessCritical
      VersioningConfiguration:
        Status: Enabled
      ReplicationConfiguration:
        Role: !GetAtt ReplicationRole.Arn
        Rules:
          - Status: Enabled
            Destination:
              Bucket: arn:aws:s3:::destination-bucket
  ReplicationRole:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: "2012-10-17"
        Statement: []
"#;

		let (model, failures) = project_and_analyse(cfn_text);

		//eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));
		eprintln!("Failures:\n===\n{:?}", failures);

		// Find the core:Store node that sources from Bucket1
		let bucket_cfn = model.resolve("Bucket1").expect("bucket should exist");
		let core_source = model
			.resolve("core:source")
			.expect("core:source should exist");

		// Find core:Node with core:source -> Bucket1
		let stores = model.query(&Query::descendant("core:Store"));
		let store_node = stores
			.iter()
			.find(|&&node| {
				model
					.get_all(node, core_source)
					.iter()
					.any(|v| v.as_entity() == Some(bucket_cfn))
			})
			.expect("should find core:Store for Bucket1");

		assert!(has_evidence(&model, *store_node, "data:isResilient"));
		assert!(!has_evidence(&model, *store_node, "data:SomethingElse"));

		// No tag failures (tags present)
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataSensitivity"
		));
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataCriticality"
		));
	}

	#[test]
	fn test_model_display() {
		let cfn_text = r#"
Resources:
  TestBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags:
        - Key: Environment
          Value: Test
"#;

		let (model, failures) = project_and_analyse(cfn_text);

		//eprintln!("Failures:\n===\n{:?}", failures);
		let display = format!("{}", model);

		assert!(display.contains("TestBucket"));
		assert!(display.contains("core:Store"));
	}

	#[test]
	fn tutorial_step_0() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/0.naive.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let (model, failures) = project_and_analyse(&cfn_text);

		eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("Failures:\n===\n{:?}", failures);

		// Should have failures for missing tags
		assert!(has_failure_with_area(
			&model,
			&failures,
			"my:DataSensitivity"
		));
		assert!(has_failure_with_area(
			&model,
			&failures,
			"my:DataCriticality"
		));
	}

	#[test]
	fn tutorial_step_1() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/1.calm.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let (model, failures) = project_and_analyse(&cfn_text);

		//eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));
		//eprintln!("Failures:\n===\n{:?}", failures);

		// Should have failures for missing tags on multiple stores
		assert!(has_failure_with_area(
			&model,
			&failures,
			"my:DataSensitivity"
		));
		assert!(has_failure_with_area(
			&model,
			&failures,
			"my:DataCriticality"
		));
	}

	#[test]
	fn tutorial_step_2() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/2.wa2tags.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let (model, failures) = project_and_analyse(&cfn_text);

		//eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));
		//eprintln!("Failures:\n===\n{:?}", failures);

		// Tags present, no tag failures
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataSensitivity"
		));
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataCriticality"
		));

		// But might have criticality evidence failures (needs resilience)
		// This depends on whether we derive data:Criticality from tags yet
	}

	#[test]
	fn tutorial_step_3() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/3.with-replication.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let (model, failures) = project_and_analyse(&cfn_text);

		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));
		eprintln!("Failures:\n===\n{:?}", failures);

		// No tag-related failures (tags present on both buckets)
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataSensitivity"
		));
		assert!(!has_failure_with_area(
			&model,
			&failures,
			"my:DataCriticality"
		));

		// TODO: Until ~() is implemented, ALL DataCriticality tags create evidence,
		// so DataBucketLogs (NonCritical) triggers ensure_critical_stores_are_protected.
		// Once ~() exists, only BusinessCritical/MissionCritical should require resilience.
		assert!(failures.is_empty());
		assert!(!has_failure_with_area(&model, &failures, "data:isResilient"));
      panic!();
	}
}
