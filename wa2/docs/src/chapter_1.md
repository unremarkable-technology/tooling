# Chapter 1
Let’s start with the classic “Hello, world” example.
To do that we need a few things:
1. WA2 installed - so we can run the example
2. A stack template, written in CloudFormation YAML to evaluate
3. Example run
4. A policy, written in WA2 the _Intent_ language, to say what we want to evaluate for
5. A rule that generates architectural evidence from our CloudFormation implementation.

by the end of this chapter you will understand
* WA2 does not check raw properties.
* It checks architectural facts.
* Architectural facts can be derived from vendor-specific implementations.

## 1 Installation
(todo)

## 2 A minimal stack
This is a very simple AWS stack. It creates an S3 bucket.
```
AWSTemplateFormatVersion: "2010-09-09"

Resources:
  DataBucket:
    Type: AWS::S3::Bucket
```

## 3 A minimal policy
Our minimal policy says that every CloudFormation resource must be classified.
It does not define how classification is implemented — only that it must exist.
```wa2
policy require_classification {
  must classified
}

rule classified {
  let resources = query(aws:cfn:Resource)

  for r in resources {
    must query(r/data:Criticality) {
      subject: r,
      message: "Resource must be classified"
    }
  }
}
```

## 4 Run WA2
```
$ wa --stack stack.yaml --intent myintent.wa2

Stack stack.yaml: parsed and validated successfully.
Intent myintent.wa2: parsed and validated successfully.

Failed: stack does not satisfy intent.

Causes:
✖ require_classification.classified
 - Subject: DataBucket
 - Message: Resource must be classified
```

WA2 is not checking for a specific tag.
It is checking whether architectural evidence of classification exists.
Right now, no such evidence has been generated.

The policy requires a fact. We have not yet told WA2 how that fact is produced. The policy fails — correctly.

## 5 Generating evidence
Now we define how classification is expressed in our CloudFormation implementation.
In this example, we express classification using a _DataCriticality_ tag:

```
rule classification_from_tag {
  let resources = query(aws:cfn:Resource)

  for r in resources {
    let tag = query(r/aws:Tags/*[aws:Key = "DataCriticality"])

    if tag {
      add(r, data:Criticality)
    }
  }
}
```

## 6 Fix the stack
Update the stack to include the classification tag:
```
AWSTemplateFormatVersion: "2010-09-09"

Resources:
  DataBucket:
    Type: AWS::S3::Bucket
    Tags:
      - Key: DataCriticality
        Value: Important
```

Let’s check the stack again:
```
$ wa --stack stack.yaml --intent myintent.wa2

Stack stack.yaml: parsed and validated successfully.
Intent myintent.wa2: parsed and validated successfully.

Success: stack does satisfy intent.
```

The policy is satisfied because the required architectural fact now exists.