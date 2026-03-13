# WA2 — Intent-First Architecture in VS Code

Define intent.  
Attach evidence.  
Enforce architecture — not just configuration.

WA2 brings **Well-Architected thinking directly into your editor**, validating CloudFormation in real time while modelling architectural intent.

Built by the creator of the AWS Well-Architected Framework.

---

## Why WA2?

Most tools validate configuration.

WA2 models architecture.

Instead of asking:

> “Is encryption enabled?”

WA2 asks:

> “What is this resource for?”

Then enforces controls appropriate to that declared purpose.

**Intent → Obligation → Evidence**

---

## ✨ Core Capabilities

### 🧠 Intent-Based Architecture

- Declare **DataSensitivity** and **DataCriticality**
- Generate architectural obligations from declared intent
- Attach evidence to resources
- Get guidance that adapts to purpose, not just resource type

---

### ⚙️ Real-Time CloudFormation Validation

WA2 includes a high-performance CloudFormation validator built in Rust.

- 1000+ AWS resource types (CloudFormation Registry schemas)
- Property validation & type checking
- All intrinsic functions (Ref, GetAtt, Sub, Join, If, FindInMap, and more)
- AWS::LanguageExtensions (Fn::ForEach, Transform support)
- SAM / Serverless transforms
- Precise line & column diagnostics

Sub-second validation inside VS Code.

---

## 🏗️ Example

### Without intent:

```yaml
Resources:
  DataBucket:
    Type: AWS::S3::Bucket
```

WA2 prompts you to classify the resource.

---

### With intent declared:

```yaml
Resources:
  DataBucket:
    Type: AWS::S3::Bucket
    Properties:
      Tags:
        - Key: DataCriticality
          Value: BusinessCritical
```

WA2 derives architectural obligations:

> Business critical stores must be resilient.

Guidance appears inline — tied to declared intent.

---

## 🧩 Framework as Language

WA2 best practices are not hard-coded.

The framework logic is expressed in a small, purpose-built language.

This allows:

- 90%+ of guidance to be defined outside the engine
- Inspectable, modifiable architectural rules
- Vendor-agnostic modelling
- Traceable chains from source → classification → policy → evidence

Framework as code.  
Not framework as binary.

---

## ⚡ Performance

WA2 validates typical templates in ~0.37 seconds,  
significantly faster than many existing editor-based validators.

Built in Rust for predictable performance, low memory overhead, and fast parsing of large templates.

Designed for consistent sub-second feedback as you build.

---

## 🚀 Quick Start

1. Install from the VS Code Marketplace  
2. Open any `.yaml`, `.yml`, or `.json` CloudFormation file  
3. Start declaring intent  

---

## 🔗 Links

- GitHub: https://github.com/unremarkable-technology/tooling  
- Issues: https://github.com/unremarkable-technology/tooling/issues  

---

Shift architecture left.

---

## ☕ Support WA2

If you find WA2 useful and want to support continued development, you can buy me a coffee:

<a href="https://buymeacoffee.com/fitz_xyz" target="_blank">
  <img src="https://cdn.buymeacoffee.com/buttons/default-orange.png" height="28">
</a>
