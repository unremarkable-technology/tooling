#[cfg(test)]
mod tests {
	// ─── Tests ───
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
			guidance::{FocusTaxonomy, guidance, has_evidence},
			model::{Model, Query, print_model_as_tree},
			vendor::{DocumentFormat, Method, Vendor, get_projector},
		},
	};

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

		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();

		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);

		assert!(guides.is_empty());
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

		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);

		assert_eq!(guides.len(), 1);
		assert!(matches!(guides[0].focus, FocusTaxonomy::DataResiliance));
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

		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);

		// Lambda is Run, Queue is Move - neither are Store
		// Only Store requires DataSensitivity/DataCriticality tags
		let stores = model.query(&Query::descendant("core:Store"));
		assert!(stores.is_empty());
		assert!(guides.is_empty());
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

		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);

		let stores = model.query(&Query::descendant("core:Store"));
		assert_eq!(stores.len(), 2);

		let runs = model.query(&Query::descendant("core:Run"));
		assert_eq!(runs.len(), 1);
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

		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();

		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);

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

		assert!(has_evidence(&model, *store_node, "DataResiliance"));
		assert!(!has_evidence(&model, *store_node, "SomethingElse"));
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

		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();
		//eprintln!("\nModel:\n===\n{}", &model);

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);
		let display = format!("{}", model);

		assert!(display.contains("TestBucket"));
		assert!(display.contains("core:Store"));
	}

	#[test]
	fn tutorial_step_0() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/0.naive.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();

		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);
		assert_eq!(guides.len(), 2, "should fail");
		assert!(matches!(guides[0].focus, FocusTaxonomy::DataSensitivity));
		assert!(matches!(guides[1].focus, FocusTaxonomy::DataCriticality));
	}

	#[test]
	fn tutorial_step_1() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/1.calm.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();

		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);
		assert_eq!(guides.len(), 4, "should fail: not tagged");
		assert!(matches!(guides[0].focus, FocusTaxonomy::DataSensitivity));
		assert!(matches!(guides[1].focus, FocusTaxonomy::DataCriticality));
	}

	#[test]
	fn tutorial_step_2() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/2.wa2tags.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();

		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);
		assert_eq!(guides.len(), 1, "should fail: not tagged");
		assert!(matches!(guides[0].focus, FocusTaxonomy::DataResiliance));
	}

	#[test]
	fn tutorial_step_3() {
		println!("{}", env::current_dir().unwrap().display());
		let path = Path::new("../../examples/tutorial/3.with-replication.yaml");
		let cfn_text = std::fs::read_to_string(path).unwrap();
		let projector = get_projector(Vendor::Aws, Method::CloudFormation);
		let mut model = Model::bootstrap();
		projector
			.project_into(&mut model, &cfn_text, &test_uri(), DocumentFormat::Yaml)
			.unwrap();

		//eprintln!("\nModel:\n===\n{}", &model);
		eprintln!("\nModel:\n===\n{}", print_model_as_tree(&model));

		// GUIDANCE: is guidance required?
		let guides = guidance(&model);
		eprintln!("Guidance:\n===\n{:?}", guides);
		assert!(guides.is_empty(), "all good");
		panic!();
	}
}
