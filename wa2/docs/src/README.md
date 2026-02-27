

# Introduction
Products are under constant pressure to change.
Architectures must evolve with them.

Yet ensuring systems follow best practice is still largely manual.

Best practices are in long PDFs, written without your context.
Reviews happen late, risking expensive mistakes and incidents.
All systems are distributed now, but best practice knowledge is not.
Teams have hybrid, multi-vendor systems - but our tools reason about them one property at a time.

Well-Architected 2 (WA2) is an architecture reasoning system.

WA2 creates a graph of your system and evaluates it against your intent.

As you build or evolve architectures, WA2 guides you - explaining best practices, what they imply, and how their consequences ripple through your architecture.

Instead of asking
* Have you backed up this S3 bucket?
WA2 determines
* Are your *critical* stores protected from data loss?

## What WA2 is
WA2 consists of:
* **Intents language**: a small language for expressing architectural policies.
* **Framework**: vendor-independent best practices built on architectural concepts.
* **Extension**: editor integration that guides you around problems as you build.
* **CLI**: enforcement in CI/CD. [not done!]
* **Book**: this guide, explaining both the thinking and the tool.

## The Big Idea

WA2 separates:
* How a system is implemented

*from*
* What it must guarantee

Rules add evidence to a shared graph.
Policies evaluate that evidence.

Vendor-specific logic produces facts.
Architectural intent consumes them.

This keeps governance clean and portable.

## Why This Matters
Architecture has grown more complex.
Our tooling has not kept up.

WA2 changes how we think about architecture:
* Architecture becomes queryable.
* Best practices become executable.
* Governance becomes scalable.
* Vendor specifics become interchangeable.
* Developers get guidance in context.

# Current Scope

* Today WA2 supports AWS CloudFormation (JSON & YAML).
* It is designed to support additional systems over time.