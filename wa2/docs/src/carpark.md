---- car park
## Value to you
Today WA2 supports AWS Cloudformation, but it is designed to be vendor independant:
* lightning fast template validation (Rust LSP)
* Education and guidence on adopting best practices
* Framework with vendor independent architecture best practices
* Framework language allows you to implement your own rules, policies and governance

## How it works
WA2 is an extension for your your editor or IDE (as a LSP).
It automatically parses and validates Infrastructure-As-Code (IaC),
currently supporting Cloudformation (JSON and YAML formats).

## history
In 2015, AWS publised the Well-Architected Framework...

## Design
A humble ontological graph for architecture.
design observation: rules add evidence to the graph,
which triggers other rules to derive further facts.
this decouples framework policies from vendor specifics -
the framework can require data protection for critical stores
without understanding tagging, CFN, AWS Services, or S3 replication.