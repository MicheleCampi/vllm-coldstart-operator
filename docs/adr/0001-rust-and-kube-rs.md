# ADR-0001: Build the operator in Rust with kube-rs

- Status: Accepted
- Date: 2026-05-31

## Context

This operator needs a control plane: a CRD, a reconcile loop, owned-resource management, and a status machine. The dominant ecosystem for building Kubernetes operators is Go — Kubernetes itself, controller-runtime, kubebuilder, and operator-sdk are all Go, and the overwhelming majority of production operators are written in it. Choosing the implementation language is therefore a real decision, not a default, and it deserves a record.

The realistic options were:

1. **Go with controller-runtime / kubebuilder** — the ecosystem standard.
2. **Rust with kube-rs** — a mature but smaller operator framework.

## Decision

Build the operator in Rust with kube-rs.

## Rationale

The decision rests on three points, in order of weight.

1. **The operator model matters more than the language.** What a reviewer or employer should take from this project is that the author understands the Kubernetes control-plane model: custom resources, the reconcile loop, server-side apply, owner references and garbage collection, and the status subresource. That understanding is language-agnostic. Demonstrating it in Rust — where the type system makes the resource shapes and the reconcile contract explicit — is if anything a stronger demonstration that the model is understood rather than scaffolded.

2. **Continuity with the project's existing tooling.** This operator is the operational half of a cold-start research line whose measurement tools (vllm-coldstart-probe, inferscope) are already in Rust. Keeping the operator in the same language keeps the toolchain, the idioms, and the maintainer's productivity coherent across the whole line, rather than splitting it across two ecosystems for one component.

3. **kube-rs is mature enough not to be an obstacle.** kube-rs 2.x provides the CustomResource derive, a Controller runtime with watches and owned-resource retriggering, server-side apply, and status subresource support — everything this operator needs. The reconcile loop, ownership GC, and status machine were all implemented and verified end-to-end against a live cluster without hitting framework limitations.

## Consequences

### What we accept by not using Go

This is the honest cost of the decision, stated plainly:

- **A smaller ecosystem.** Go's operator tooling is more mature: kubebuilder scaffolds projects and RBAC, controller-runtime has a larger body of examples, envtest provides a standard integration-testing harness. In Rust these are either lighter or assembled by hand (here, an end-to-end test on an ephemeral kind cluster in CI plays the role envtest would in Go).
- **A smaller hiring overlap.** Most teams operating Kubernetes write their controllers in Go. A Rust operator demonstrates the model but is not in the language those teams maintain day to day.

### Why we accept it

The project's purpose is to demonstrate that cold start can be made a first-class control-plane signal, and to demonstrate control-plane competence. Both goals are served by a correct, tested, well-documented operator in the language the author is most effective in. The cost — a less mature framework and a narrower language overlap — is outweighed, for a portfolio control plane built and maintained by one person, by correctness, coherence, and clarity.

A reviewer who maintains operators in Go should read this project as evidence that the author understands the operator model and can pick up controller-runtime quickly, not as a claim that Rust is the better choice for their stack.
