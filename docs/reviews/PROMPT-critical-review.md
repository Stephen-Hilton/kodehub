You are a senior staff engineer doing a **critical review** of the Kodehub pilot in this repository.
Read CLAUDE.md, then docs/00-vision.md through docs/08-poc-dataset.md, then docs/decisions.md, then the
code under crates/ (and app/ if present) and examples/auth-poc/.

Scope for this review: milestones {{SCOPE}}.

Look for, in priority order:
1. Violations of the hard rules / invariants in CLAUDE.md (and whether tests actually prove them).
2. Places where an acceptance criterion in docs/07-milestones.md is claimed but not truly met or tested.
3. Correctness bugs (races, transactions that are not atomic, wrong AQL semantics, cache-key mistakes,
   determinism leaks, security issues such as credential handling or command injection).
4. Design flaws that will hurt later milestones (M5 agents/changesets, M6 UI, M7 releases).
5. Simplifications / areas of improvement that are worth it for a pilot (not gold-plating).

Be concrete: cite file:line, explain the failure scenario, and propose the fix. Rank by severity
(critical / high / medium / low). Skip style nits. Do not modify any file except the review file.

Write your review to {{OUT}} as Markdown with sections: Summary, Findings (numbered, each with
severity, location, problem, scenario, recommendation), and "Things done well" (short).
